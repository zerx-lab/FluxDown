/// 快速下载表单结果的统一提交器（主 isolate）。
///
/// 主窗口对话框与外部唤起独立小窗共用：解析多行 URL、记录用户偏好
/// （上次保存目录 / 上次线程数）、按单条/批量分别发送
/// [ConfirmExternalDownload] / [BatchCreateTask] 信号。
library;

import '../bindings/bindings.dart';
import '../models/download_queue.dart';
import '../models/settings_provider.dart';
import '../widgets/quick_download_form.dart';
import 'log_service.dart';

const _tag = 'QuickSubmit';

/// 解析表单结果并发送下载信号。
///
/// [referrer] / [hintFileSize] 为外部请求上下文（保存在主引擎侧，
/// 不经过表单流转）；cookies 由表单携带（预填浏览器捕获值，用户可编辑）。
void submitQuickDownload({
  required QuickDownloadFormResult result,
  required String referrer,
  required int hintFileSize,
  String audioUrlOverride = '',
}) {
  final saveDir = result.saveDir.trim();
  if (saveDir.isEmpty) {
    logError(_tag, 'empty save dir, dropping submit');
    return;
  }
  final entries = parseQuickDownloadEntries(result.urlText);
  if (entries.isEmpty) {
    logError(_tag, 'no valid entries, dropping submit');
    return;
  }

  // 稍后下载且未选队列 → 落入内置「稍后下载」队列，等「启动队列」批量恢复；
  // 已显式选择队列则尊重选择（暂停加入该队列）。
  final queueId = (result.startLater && result.queueId.isEmpty)
      ? kLaterQueueId
      : result.queueId;

  // 记录本次保存位置，供"跟随上次保存位置"开关使用
  SettingsProvider.globalInstance?.recordLastSaveDir(saveDir);

  // 记住用户本次选择的线程数，下次新建时沿用
  if (result.threadsUserModified) {
    SettingsProvider.globalInstance?.setLastDialogThreads(
      result.segments > 0 ? result.segments.toString() : 'auto',
    );
  }

  if (entries.length == 1) {
    final entry = entries.first;
    final fileName = result.rename.isNotEmpty ? result.rename : entry.fileName;
    // audioUrl 走独立通道透传（外部 track-pair 请求，不在 URL 文本里），
    // 优先 result（表单透传的原始值），回退文本解析出的 checksum= 轨对。
    final audioUrl = audioUrlOverride.isNotEmpty
        ? audioUrlOverride
        : (result.audioUrl.isNotEmpty ? result.audioUrl : entry.audioUrl);
    // 校验值：高级选项手填的优先，否则回退 URL 文本里的 checksum= 选项行
    final checksum = result.checksum.isNotEmpty
        ? result.checksum
        : entry.checksum;
    if (checksum.isEmpty) {
      // 单条无校验 — 使用 ConfirmExternalDownload（保留 Rust 侧按 URL
      // 缓存的请求上下文与 hintFileSize，免二次探测）
      ConfirmExternalDownload(
        url: entry.url,
        saveDir: saveDir,
        fileName: fileName,
        segments: result.segments,
        cookies: result.cookies,
        referrer: referrer,
        hintFileSize: hintFileSize,
        proxyUrl: result.proxyUrl,
        userAgent: result.userAgent,
        queueId: queueId,
        ignoreTlsErrors: result.ignoreTlsErrors,
        startPaused: result.startLater,
        audioUrl: audioUrl,
        extraHeaders: result.extraHeaders,
      ).sendSignalToRust();
    } else {
      // 单条带校验 — ConfirmExternalDownload 信号无 checksum 字段，
      // 走 BatchCreateTask 单元素路径（UrlEntry 携带 checksum；
      // 代价仅是丢失 hintFileSize，由元数据探测补齐）
      BatchCreateTask(
        entries: [
          UrlEntry(
            url: entry.url,
            fileName: fileName,
            checksum: checksum,
            audioUrl: audioUrl,
          ),
        ],
        saveDir: saveDir,
        segments: result.segments,
        proxyUrl: result.proxyUrl,
        userAgent: result.userAgent,
        queueId: queueId,
        ignoreTlsErrors: result.ignoreTlsErrors,
        startPaused: result.startLater,
        cookies: result.cookies,
        referrer: referrer,
        extraHeaders: result.extraHeaders,
      ).sendSignalToRust();
    }
  } else {
    // 多条 — 使用 BatchCreateTask（携带每条的 fileName/checksum）
    BatchCreateTask(
      entries: entries
          .map(
            (e) => UrlEntry(
              url: e.url,
              fileName: e.fileName,
              checksum: e.checksum,
              audioUrl: e.audioUrl,
            ),
          )
          .toList(),
      saveDir: saveDir,
      segments: result.segments,
      proxyUrl: result.proxyUrl,
      userAgent: result.userAgent,
      queueId: queueId,
      ignoreTlsErrors: result.ignoreTlsErrors,
      startPaused: result.startLater,
      cookies: result.cookies,
      referrer: referrer,
      extraHeaders: result.extraHeaders,
    ).sendSignalToRust();
  }
}
