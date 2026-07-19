// 预解析清单选择弹窗 — 主窗口 ShadDialog 外壳。
//
// 触发路径：new_download_dialog.dart / quick_download_dialog.dart 对单条
// http(s) 非磁力/种子链接先经 ResolvePreviewClient 探测是否为多文件清单；
// 命中后弹出本对话框，底层表单保持不动。取消 → 回到表单；确认 → 发
// CreateTaskGroup，两层对话框一起关闭（由调用方在 Future 完成后关闭底层
// 表单，本文件只负责自己的 Navigator.pop）。
//
// 视图主体（下钻导航/筛选/高级选项/底栏）在 manifest_select_view.dart，
// 与独立快速下载小窗共用；本文件只做主窗口侧的三件接线：
// ShadDialog 外壳、FilePickerService 目录选择、CreateTaskGroup 信号发送。

import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../models/download_queue.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'manifest_select_view.dart';
import 'quick_download_form.dart' show QuickQueueOption;

/// 弹出清单选择框。
///
/// 返回 `true` = 用户确认并已发出 [CreateTaskGroup]（调用方应关闭底层的
/// 新建下载表单对话框）；返回 `false` = 用户取消（表单对话框保持打开）。
Future<bool> showManifestSelectDialog(
  BuildContext context, {
  required List<DownloadQueue> queues,
  required ResolvePreviewResult manifest,
  required String sourceUrl,
  required String initialSaveDir,
  required String initialQueueId,
  required int segments,
  required String cookies,
  required String referrer,
  required String userAgent,
  required String proxyUrl,
  required Map<String, String> extraHeaders,
  required bool ignoreTlsErrors,
}) async {
  final result = await showShadDialog<bool>(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    barrierDismissible: false,
    animateIn: const [],
    animateOut: const [],
    builder: (context) => ShadDialog(
      constraints: const BoxConstraints(maxWidth: 780, maxHeight: 620),
      padding: EdgeInsets.zero,
      scrollable: false,
      radius: AppMetrics.of(context).brDialog,
      child: SizedBox(
        width: 780,
        height: 620,
        child: ManifestSelectView(
          queues: [
            for (final q in queues)
              QuickQueueOption(
                queueId: q.queueId,
                name: q.name,
                defaultSegments: q.defaultSegments,
              ),
          ],
          manifest: manifest,
          sourceUrl: sourceUrl,
          initialSaveDir: initialSaveDir,
          initialQueueId: initialQueueId,
          segments: segments,
          cookies: cookies,
          referrer: referrer,
          userAgent: userAgent,
          proxyUrl: proxyUrl,
          extraHeaders: extraHeaders,
          ignoreTlsErrors: ignoreTlsErrors,
          pickDirectory: FilePickerService.pickDirectory,
          onConfirm: (sub) {
            CreateTaskGroup(
              sourceUrl: sub.sourceUrl,
              groupName: sub.groupName,
              saveDir: sub.saveDir,
              queueId: sub.queueId,
              segments: sub.segments,
              cookies: sub.cookies,
              referrer: sub.referrer,
              userAgent: sub.userAgent,
              proxyUrl: sub.proxyUrl,
              extraHeaders: sub.extraHeaders,
              ignoreTlsErrors: sub.ignoreTlsErrors,
              startPaused: sub.startPaused,
              items: sub.items,
            ).sendSignalToRust();
            Navigator.of(context).pop(true);
          },
          onCancel: () => Navigator.of(context).pop(false),
        ),
      ),
    ),
  );
  return result ?? false;
}
