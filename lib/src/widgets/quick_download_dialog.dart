import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/settings_provider.dart';
import '../services/file_picker_service.dart';
import '../services/quick_download_submitter.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'quick_download_form.dart';

/// 浏览器扩展下载请求的快速确认对话框（主窗口内回退路径）。
///
/// 外部唤起的首选路径是独立小窗（popup_window_service.dart）；
/// 本对话框用于原生宿主不可用时的回退，以及悬浮球拖链等主窗口内入口。
///
/// 表单主体与独立小窗共用 [QuickDownloadForm]；
/// 支持多行 URL 输入，批量/单条信号发送统一走 quick_download_submitter。
void showQuickDownloadDialog(
  BuildContext context, {
  required String url,
  required String filename,
  required int fileSize,
  required String mimeType,
  required String cookies,
  required String defaultSaveDir,
  String referrer = '',
  String defaultQueueId = '',
  bool saveDirFromRequest = false,
  String audioUrl = '',
}) {
  // 根据已知文件名自动匹配分类保存目录
  // （请求方显式指定目录时尊重之，跳过匹配）
  var saveDir = defaultSaveDir;
  if (!saveDirFromRequest) {
    final matched =
        SettingsProvider.globalInstance?.resolveCategorySaveDir(
          filename,
          url: url,
        ) ??
        '';
    if (matched.isNotEmpty) saveDir = matched;
  }

  showShadDialog(
    context: context,
    barrierColor: AppColors.of(context).dialogBarrier,
    animateIn: const [],
    animateOut: const [],
    builder: (context) => _QuickDownloadDialogShell(
      url: url,
      filename: filename,
      fileSize: fileSize,
      mimeType: mimeType,
      cookies: cookies,
      referrer: referrer,
      saveDir: saveDir,
      defaultQueueId: defaultQueueId,
      audioUrl: audioUrl,
    ),
  );
}

/// 对话框外壳：ShadDialog 标题/描述 + 共享表单主体。
class _QuickDownloadDialogShell extends StatelessWidget {
  final String url;
  final String filename;
  final int fileSize;
  final String mimeType;
  final String cookies;
  final String referrer;
  final String saveDir;
  final String defaultQueueId;
  final String audioUrl;

  const _QuickDownloadDialogShell({
    required this.url,
    required this.filename,
    required this.fileSize,
    required this.mimeType,
    required this.cookies,
    required this.referrer,
    required this.saveDir,
    required this.defaultQueueId,
    this.audioUrl = '',
  });

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final m = AppMetrics.of(context);

    return ShadDialog(
      title: Row(
        children: [
          Container(
            width: 28,
            height: 28,
            decoration: BoxDecoration(
              color: m.soft(c.accent),
              borderRadius: m.brMd,
            ),
            child: Icon(LucideIcons.download, size: 14, color: c.accent),
          ),
          const SizedBox(width: 10),
          Text(s.newDownload),
          const SizedBox(width: 8),
          if (fileSize > 0)
            QuickInfoTag(
              text: formatQuickFileSize(fileSize, unknownLabel: s.unknownSize),
              c: c,
            ),
          if (fileSize > 0 && mimeType.isNotEmpty) const SizedBox(width: 6),
          if (mimeType.isNotEmpty)
            Flexible(child: QuickInfoTag(text: mimeType, c: c)),
        ],
      ),
      description: Text(s.fromBrowserExtension),
      child: Padding(
        padding: const EdgeInsets.only(top: 16),
        child: QuickDownloadForm(
          initialUrl: url,
          initialFileName: filename,
          initialSaveDir: saveDir,
          defaultQueueId: defaultQueueId,
          initialCookies: cookies,
          initialAudioUrl: audioUrl,
          host: const _MainWindowFormHost(),
          onSubmit: (result) {
            submitQuickDownload(
              result: result,
              referrer: referrer,
              hintFileSize: fileSize,
            );
            Navigator.of(context).pop();
          },
          onCancel: () => Navigator.of(context).pop(),
        ),
      ),
    );
  }
}

/// 主窗口表单宿主 — 环境数据读全局单例，目录选择走 file_selector 插件。
class _MainWindowFormHost implements QuickDownloadFormHost {
  const _MainWindowFormHost();

  @override
  List<QuickQueueOption> get queues => [
    for (final q in DownloadController.globalInstance?.queues ?? const [])
      QuickQueueOption(
        queueId: q.queueId,
        name: q.name,
        defaultSegments: q.defaultSegments,
      ),
  ];

  @override
  int get defaultSegments =>
      SettingsProvider.globalInstance?.defaultSegments ?? 0;

  @override
  String get lastDialogThreads =>
      SettingsProvider.globalInstance?.lastDialogThreads ?? '';

  @override
  Future<String?> pickDirectory({
    required String dialogTitle,
    String? initialDirectory,
  }) {
    return FilePickerService.pickDirectory(
      dialogTitle: dialogTitle,
      initialDirectory: initialDirectory,
    );
  }
}
