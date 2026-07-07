import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';

import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../models/settings_provider.dart';
import '../theme/flux_theme_tokens.dart';
import '../theme/theme_provider.dart';
import 'open_folder.dart';
import 'log_service.dart';
import 'win32_toast/win32_toast_window.dart';

const _tag = 'NotifySvc';

/// 下载完成通知服务 — 屏幕级弹窗，不依赖主窗口可见性。
///
/// ## 平台策略
///
/// - **Windows**: [Win32ToastWindow] 自定义悬浮窗，固定显示在**主显示器**
///   工作区右下角（`SPI_GETWORKAREA` 返回主屏工作区，天然满足多屏选主屏）。
///   卡片由 Flutter 主引擎离屏光栅化（与 App 同主题/字体/渲染管线），
///   经 `UpdateLayeredWindow` 贴入分层窗口；窗口侧纯 Win32 +
///   Dart Timer 驱动，零 Dart 原生回调，无 Isolate 竞态风险。
/// - **Linux**: D-Bus 系统通知（org.freedesktop.Notifications），
///   带"打开文件夹/打开文件"动作按钮（D-Bus spec `actions`，
///   GNOME/KDE 支持；轻量 DE 自动退化为仅点击通知体）。
///   Wayland 安全模型禁止客户端指定全局屏幕坐标，自定义右下角弹窗
///   在 GNOME Wayland 上不可实现，系统通知是唯一正确做法。
/// - **macOS**: UNUserNotification 系统通知（符合 HIG），
///   经 UNNotificationCategory 提供同款动作按钮。
///
/// 曾经的方案及弃用原因：
/// - desktop_multi_window 子窗口：Isolate 生命周期竞态 → 0xc0000005 崩溃；
/// - 主窗口内 OverlayEntry：依赖主窗口可见性，最小化/隐藏时不可见。
///
/// ## 高频完成防抖 + 合批
///
/// 1. 完成通知入队，启动 800ms 防抖定时器；
/// 2. 防抖窗口内的多个完成事件合并为一批（"N 个文件已下载"）；
/// 3. 持续密集完成时防抖会被反复重置 — 批次自首个任务入队起
///    最多等待 [_maxBatchWait]，超时强制冲刷，避免通知无限延迟；
/// 4. Windows 端 [Win32ToastWindow] 内部串行播放、相邻批次合并，
///    杜绝弹窗风暴。
class NotificationService {
  NotificationService._();
  static final instance = NotificationService._();

  static const _appUserModelId = 'Com.FluxDown.App';
  static const _appGuid = '4b648ba5-0b80-4bdb-b2a0-7f3b68c8e2b1';

  /// 防抖窗口：收集短时间内密集完成的任务
  static const _debounce = Duration(milliseconds: 800);

  /// 批次最长等待：防止持续下载流无限重置防抖定时器
  static const _maxBatchWait = Duration(seconds: 3);

  final FlutterLocalNotificationsPlugin _systemNotifications =
      FlutterLocalNotificationsPlugin();
  bool _systemReady = false;

  ThemeProvider? _themeProvider;

  // ---------------------------------------------------------------------------
  // 队列 + 防抖
  // ---------------------------------------------------------------------------

  /// 等待通知的任务队列
  final List<DownloadTask> _queue = [];

  /// 防抖定时器
  Timer? _batchTimer;

  /// 当前批次首个任务入队时间（最长等待守卫）
  DateTime? _batchStartedAt;

  /// 标记是否正在退出
  bool _shuttingDown = false;

  // ---------------------------------------------------------------------------
  // Lifecycle
  // ---------------------------------------------------------------------------

  /// 初始化通知服务。
  void init() {
    logInfo(_tag, 'initialized');
    if (!Platform.isWindows) {
      // Windows 通知全部由 Win32ToastWindow 处理，无需系统通知插件。
      _initSystemNotifications();
    }
  }

  /// 设置主题提供者 — Windows 端 Win32 Toast 据此选择深/浅色绘制。
  void setThemeProvider(ThemeProvider provider) {
    _themeProvider = provider;
  }

  /// 是否有待处理的通知
  bool get hasPending =>
      _queue.isNotEmpty ||
      (Platform.isWindows && Win32ToastWindow.instance.hasActive);

  /// 等待当前通知完成（用于退出前）。
  Future<void> waitForPending() async {
    logInfo(_tag, 'waitForPending: hasPending=$hasPending');
    if (!hasPending) return;
    // 给当前 Toast 淡出动画一点时间，然后由 shutdown 强制清理
    await Future.delayed(const Duration(milliseconds: 500));
  }

  /// 标记正在退出，停止接受新通知
  void shutdown() {
    logInfo(_tag, 'shutdown called');
    _shuttingDown = true;
    _batchTimer?.cancel();
    _queue.clear();
    if (Platform.isWindows) {
      Win32ToastWindow.instance.destroyAll();
    }
  }

  // ---------------------------------------------------------------------------
  // Public API
  // ---------------------------------------------------------------------------

  /// 显示下载完成的通知。
  ///
  /// 所有平台统一走 800ms 防抖队列，合并短时间内密集完成的任务后一次性派发：
  /// - Windows → 主显示器右下角 Win32 悬浮窗（无论主窗口是否可见）
  /// - Linux/macOS → 系统通知（单文件显示文件名，多文件显示汇总）
  void showDownloadComplete(DownloadTask task) {
    if (SettingsProvider.globalInstance?.notifyOnComplete == false) {
      logInfo(_tag, 'showDownloadComplete: skipped (notifyOnComplete=false)');
      return;
    }
    logInfo(
      _tag,
      'showDownloadComplete: file=${task.fileName}, shuttingDown=$_shuttingDown',
    );
    if (_shuttingDown) {
      logInfo(_tag, 'skipped (shuttingDown)');
      return;
    }
    _enqueueTask(task);
  }

  // ---------------------------------------------------------------------------
  // Internal
  // ---------------------------------------------------------------------------

  /// 入队 + 防抖（带最长等待守卫）。
  void _enqueueTask(DownloadTask task) {
    _queue.add(task);
    _batchStartedAt ??= DateTime.now();
    logInfo(_tag, 'queued, queueSize=${_queue.length}');

    final waited = DateTime.now().difference(_batchStartedAt!);
    if (waited >= _maxBatchWait) {
      // 持续密集完成 — 不再重置防抖，立即冲刷
      _batchTimer?.cancel();
      _flushQueue();
      return;
    }

    _batchTimer?.cancel();
    _batchTimer = Timer(_debounce, _flushQueue);
  }

  /// 冲刷队列 — 取出所有待处理任务，整批派发给平台通知路径。
  void _flushQueue() {
    _batchStartedAt = null;
    if (_queue.isEmpty || _shuttingDown) return;

    final batch = List<DownloadTask>.of(_queue);
    _queue.clear();
    logInfo(_tag, 'flushing ${batch.length} notifications');

    if (Platform.isWindows) {
      final provider = _themeProvider;
      final platformDark =
          WidgetsBinding.instance.platformDispatcher.platformBrightness ==
          Brightness.dark;
      final dark = switch (provider?.themeMode) {
        ThemeMode.dark => true,
        ThemeMode.light => false,
        _ => platformDark,
      };
      Win32ToastWindow.instance.themeTokens =
          provider?.tokensFor(dark: dark) ??
          (dark
              ? FluxThemeTokens.defaultDark()
              : FluxThemeTokens.defaultLight());
      Win32ToastWindow.instance.enqueueBatch(batch);
    } else {
      _showSystemBatch(batch);
    }
  }

  /// macOS 通知类别 ID（含动作按钮，initialize 时注册）
  static const _categorySingle = 'fluxdown_download_complete';
  static const _categoryBatch = 'fluxdown_batch_complete';

  /// Linux/macOS：整批一条系统通知（单文件显示文件名，多文件显示汇总），
  /// 带"打开文件夹/打开文件"动作按钮（批量仅"打开文件夹"）。
  ///
  /// 按钮支持度由桌面环境决定：Linux 侧 GNOME/KDE 的通知服务均实现
  /// D-Bus spec `actions` capability；不支持的轻量 DE 自动退化为
  /// 仅点击通知体（default action）。
  Future<void> _showSystemBatch(List<DownloadTask> batch) async {
    if (_shuttingDown) return;
    await _initSystemNotifications();
    if (!_systemReady) {
      logInfo(_tag, 'systemNotify: skipped — system not ready');
      return;
    }

    final task = batch.last;
    final s = currentS;
    final isBatch = batch.length > 1;
    final title = isBatch
        ? s.batchDownloadCompleted(batch.length)
        : s.downloadCompleted;
    final body = isBatch
        ? '${task.fileName} ${s.andMoreFiles(batch.length - 1)}'
        : task.fileName;
    final filePath = task.filePath;

    try {
      final details = NotificationDetails(
        linux: LinuxNotificationDetails(
          defaultActionName: 'open',
          actions: [
            LinuxNotificationAction(
              key: 'open_folder',
              label: s.openFileFolder,
            ),
            if (!isBatch)
              LinuxNotificationAction(key: 'open_file', label: s.openFile),
          ],
        ),
        macOS: DarwinNotificationDetails(
          categoryIdentifier: isBatch ? _categoryBatch : _categorySingle,
        ),
      );

      final notifId = task.id.hashCode;
      logInfo(
        _tag,
        'systemNotify: show(id=$notifId, title="$title", body="$body")',
      );
      await _systemNotifications.show(
        id: notifId,
        title: title,
        body: body,
        notificationDetails: details,
        payload: _buildPayload(
          // 默认动作（点击通知体）：批量→打开文件夹；单文件→打开文件。
          // 动作按钮回调经 actionId 覆盖 payload 中的 action。
          action: isBatch ? 'open_folder' : 'open_file',
          // openFolder（RevealFile）接受文件路径 — Rust 端自动定位父目录，
          // 故单文件场景传文件路径可同时服务两个按钮。
          filePath: isBatch ? task.saveDir : filePath,
        ),
      );
      logInfo(_tag, 'systemNotify: show() completed successfully');
    } catch (e, stack) {
      logError(_tag, 'systemNotify: show() failed', e, stack);
    }
  }

  Future<void> _initSystemNotifications() async {
    if (_systemReady || Platform.isWindows) return;
    logInfo(_tag, 'initSystem: starting initialization...');

    try {
      const linux = LinuxInitializationSettings(defaultActionName: 'open');
      // macOS: UNNotificationCategory 需在 initialize 时注册。
      // 注意：按钮文案捕获自初始化时刻的语言，运行中切换语言后
      // 新通知按钮仍为旧语言（系统限制 — category 不可重复注册），
      // 重启应用后生效。
      final s = currentS;
      final darwin = DarwinInitializationSettings(
        requestAlertPermission: false,
        requestBadgePermission: false,
        requestSoundPermission: false,
        notificationCategories: [
          DarwinNotificationCategory(
            _categorySingle,
            actions: [
              DarwinNotificationAction.plain('open_folder', s.openFileFolder),
              DarwinNotificationAction.plain(
                'open_file',
                s.openFile,
                options: {DarwinNotificationActionOption.foreground},
              ),
            ],
          ),
          DarwinNotificationCategory(
            _categoryBatch,
            actions: [
              DarwinNotificationAction.plain('open_folder', s.openFileFolder),
            ],
          ),
        ],
      );
      final settings = InitializationSettings(
        // Windows 字段保留默认配置：Windows 路径不会走到 initialize，
        // 但插件要求所有桌面平台配置齐全时才可安全调用。
        windows: const WindowsInitializationSettings(
          appName: 'FluxDown',
          appUserModelId: _appUserModelId,
          guid: _appGuid,
        ),
        linux: linux,
        macOS: darwin,
      );

      final result = await _systemNotifications.initialize(
        settings: settings,
        onDidReceiveNotificationResponse: _onSystemNotificationResponse,
      );
      logInfo(_tag, 'initSystem: initialize result=$result');

      // macOS: explicitly request notification permissions after initialize.
      // Without this step the system never prompts the user and all show()
      // calls silently fail.
      if (Platform.isMacOS) {
        final macPlugin = _systemNotifications
            .resolvePlatformSpecificImplementation<
              MacOSFlutterLocalNotificationsPlugin
            >();
        if (macPlugin != null) {
          final granted = await macPlugin.requestPermissions(
            alert: true,
            badge: true,
            sound: true,
          );
          logInfo(_tag, 'initSystem: macOS permission granted=$granted');
        } else {
          logInfo(_tag, 'initSystem: macOS plugin not resolved');
        }
      }

      _systemReady = true;
      logInfo(_tag, 'initSystem: success');
    } catch (e, stack) {
      logError(_tag, 'initSystem: FAILED', e, stack);
    }
  }

  String _buildPayload({required String action, required String filePath}) {
    return '$action|$filePath';
  }

  void _onSystemNotificationResponse(NotificationResponse response) {
    final payload = response.payload ?? '';
    final actionId = response.actionId ?? '';
    final parts = payload.split('|');
    final action = actionId.isNotEmpty
        ? actionId
        : (parts.isNotEmpty ? parts[0] : '');
    final filePath = parts.length > 1 ? parts[1] : '';
    if (action == 'open_file') {
      _openFile(filePath);
      return;
    }

    // Default: open folder for safety
    _openFolder(filePath);
  }

  Future<void> _openFile(String filePath) async {
    if (filePath.isEmpty) return;
    await openFile(filePath);
  }

  Future<void> _openFolder(String filePath) async {
    final resolved = filePath.isEmpty ? _resolveDefaultDir() : filePath;
    if (resolved.isEmpty) return;
    await openFolder(resolved);
  }

  String _resolveDefaultDir() {
    final home =
        Platform.environment['USERPROFILE'] ??
        Platform.environment['HOME'] ??
        '.';
    return '$home${Platform.pathSeparator}Downloads';
  }
}
