import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../models/download_controller.dart';
import '../models/download_queue.dart';
import '../models/settings_provider.dart';
import '../services/cloud/cloud_auth_service.dart';
import '../services/file_picker_service.dart';
import '../services/quick_download_submitter.dart';
import '../services/resolve_preview_client.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'manifest_select_dialog.dart';
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
///
/// 有状态：提交单条 http(s) 链接时先经 [ResolvePreviewClient] 探测多文件
/// 清单（与新建下载对话框同一门控/90s 超时/取消/回退语义）；命中 → 弹
/// [showManifestSelectDialog]，确认后两层一起关闭；无清单/超时 → 零差异
/// 落入原 [submitQuickDownload] 提交路径。
class _QuickDownloadDialogShell extends StatefulWidget {
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
  State<_QuickDownloadDialogShell> createState() =>
      _QuickDownloadDialogShellState();
}

class _QuickDownloadDialogShellState extends State<_QuickDownloadDialogShell> {
  /// 当前等待中的预解析；非 null 时表单动作区进入 loading 态。
  ResolvePreviewHandle? _previewHandle;

  @override
  void dispose() {
    _previewHandle?.cancel();
    _previewHandle = null;
    super.dispose();
  }

  void _cancelResolve() {
    final handle = _previewHandle;
    if (handle == null) return;
    setState(() => _previewHandle = null);
    handle.cancel();
  }

  Future<void> _onSubmit(QuickDownloadFormResult result) async {
    // 单条 http(s) 链接先探测多文件清单。音视频轨对请求除外——
    // 清单建组无法承载 audioUrl 合并语义，维持原单任务路径。
    final entries = parseQuickDownloadEntries(result.urlText);
    if (widget.audioUrl.isEmpty &&
        _previewHandle == null &&
        entries.length == 1 &&
        isManifestPreviewableUrl(entries.first.url)) {
      // 镜像 new_download_dialog 的提交点偏好记录——清单路径确认后不再
      // 经过 submitQuickDownload（无清单回退时那里会以同值重记，幂等）。
      SettingsProvider.globalInstance?.recordLastSaveDir(result.saveDir);
      if (result.threadsUserModified) {
        SettingsProvider.globalInstance?.setLastDialogThreads(
          result.segments > 0 ? result.segments.toString() : 'auto',
        );
      }
      final handle = ResolvePreviewClient.start(
        url: entries.first.url,
        cookies: result.cookies,
        referrer: widget.referrer,
        userAgent: result.userAgent,
        extraHeaders: result.extraHeaders,
      );
      setState(() => _previewHandle = handle);
      final manifest = await handle.future;
      if (!mounted) return;
      setState(() => _previewHandle = null);
      if (handle.cancelled) return; // 取消等待：表单保持可编辑
      if (manifest != null) {
        // 有清单 → 弹选择框（本对话框保持底层）；确认发出 CreateTaskGroup
        // 并两层一起关闭，取消则回到本表单（未被改动，可编辑重新提交）。
        final created = await showManifestSelectDialog(
          context,
          queues: DownloadController.globalInstance?.queues ?? const [],
          manifest: manifest,
          sourceUrl: entries.first.url,
          initialSaveDir: result.saveDir,
          initialQueueId: result.queueId.isEmpty
              ? kMainQueueId
              : result.queueId,
          segments: result.segments,
          cookies: result.cookies,
          referrer: widget.referrer,
          userAgent: result.userAgent,
          proxyUrl: result.proxyUrl,
          extraHeaders: result.extraHeaders,
          ignoreTlsErrors: result.ignoreTlsErrors,
        );
        if (created && mounted) Navigator.of(context).pop();
        return;
      }
      // 无清单/error/超时 → 落入下方原提交路径（行为零差异）。
    }
    submitQuickDownload(
      result: result,
      referrer: widget.referrer,
      hintFileSize: widget.fileSize,
    );
    if (mounted) Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final m = AppMetrics.of(context);

    return ShadDialog(
      // 同 new_download_dialog：焦点圈画在控件外侧，滚动视图会裁切，
      // 左右各让 6px 进 scrollPadding 保住焦点圈。
      padding: const EdgeInsets.fromLTRB(18, 24, 18, 24),
      scrollPadding: const EdgeInsets.symmetric(horizontal: 6),
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
          if (widget.fileSize > 0)
            QuickInfoTag(
              text: formatQuickFileSize(
                widget.fileSize,
                unknownLabel: s.unknownSize,
              ),
              c: c,
            ),
          if (widget.fileSize > 0 && widget.mimeType.isNotEmpty)
            const SizedBox(width: 6),
          if (widget.mimeType.isNotEmpty)
            Flexible(child: QuickInfoTag(text: widget.mimeType, c: c)),
        ],
      ),
      description: Text(
        _previewHandle != null
            ? s.manifestResolvingLabel
            : s.fromBrowserExtension,
      ),
      child: Padding(
        padding: const EdgeInsets.only(top: 16),
        child: QuickDownloadForm(
          initialUrl: widget.url,
          initialFileName: widget.filename,
          initialSaveDir: widget.saveDir,
          defaultQueueId: widget.defaultQueueId,
          initialCookies: widget.cookies,
          initialAudioUrl: widget.audioUrl,
          host: const _MainWindowFormHost(),
          resolving: _previewHandle != null,
          onCancelResolve: _cancelResolve,
          onSubmit: _onSubmit,
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
  List<QuickDeviceOption> get devices => [
    for (final d in CloudAuthService.instance.remoteDevices)
      QuickDeviceOption(
        deviceId: d.deviceId,
        name: d.name,
        platform: d.platform,
        isOnline: d.isOnline,
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
