/// Linux Wayland 降级服务（方案 S3.4）。
///
/// Wayland 会话下悬浮球不可用（xdg-shell 禁止客户端定位/置顶，
/// GNOME/Mutter 不支持 zwlr_layer_shell_v1），本服务提供两条替代通道：
///
/// 1. **需求2 落点**：托盘 `setTitle("N↓ 12.4MB/s")` — Linux SNI Title
///    真实生效（tray_manager_plugin.cc handle_set_title）。原生 GNOME 无
///    AppIndicator 扩展时托盘整体不可见（已知缺口，设置页文案已说明）。
/// 2. **需求3 落点（形态③）**：主窗口从隐藏恢复可见时自动读一次剪贴板，
///    含新 URL → QuickDownloadDialog 预填。
///    为什么不做后台轮询：Wayland wl_data_device 协议只向持焦点 surface
///    投递 clipboard offer，失焦（=主窗隐藏）时 Clipboard.getData 读不到
///    新内容（flutter engine#155741 多环境复现）— 后台监听架构性不可用。
library;

import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tray_manager/tray_manager.dart';

import '../../models/download_controller.dart';
import '../../models/download_task.dart';
import '../../models/settings_provider.dart';
import '../../widgets/quick_download_dialog.dart';
import '../log_service.dart';
import 'floating_ball_service.dart';

const _tag = 'WaylandDegrade';

final _urlRegex = RegExp(r'(https?|ftp)://\S+', caseSensitive: false);

/// Wayland 降级通道 — 仅在 Linux 且能力探测为 wayland 时激活。
class WaylandDegradationService {
  WaylandDegradationService._();
  static final instance = WaylandDegradationService._();

  DownloadController? _downloads;
  SettingsProvider? _settings;
  GlobalKey<NavigatorState>? _navigatorKey;

  String _lastTrayTitle = '';
  String _lastClipboardValue = '';
  bool _active = false;

  /// 初始化 — FloatingBallService 收到 onCapability=wayland 后调用。
  void activate({
    required SettingsProvider settings,
    required GlobalKey<NavigatorState> navigatorKey,
  }) {
    if (_active || !Platform.isLinux) return;
    final downloads = DownloadController.globalInstance;
    if (downloads == null) {
      logInfo(_tag, 'activate deferred: DownloadController not ready');
      return;
    }
    _downloads = downloads;
    _settings = settings;
    _navigatorKey = navigatorKey;
    downloads.addListener(_updateTrayTitle);
    _active = true;
    logInfo(_tag, 'activated (wayland degradation mode)');
  }

  void deactivate() {
    if (!_active) return;
    _downloads?.removeListener(_updateTrayTitle);
    _downloads = null;
    _active = false;
  }

  /// 需求2 降级落点：托盘 Title 显示 "N↓ 12.4MB/s"。
  ///
  /// 变化检测：格式化文本相同则跳过（上游已节流 500ms）。
  void _updateTrayTitle() {
    final d = _downloads;
    if (d == null) return;
    final active = d.activeCount;
    final title = active > 0
        ? '$active↓ ${DownloadTask.formatBytes(d.totalDownloadSpeed)}/s'
        : '';
    if (title == _lastTrayTitle) return;
    _lastTrayTitle = title;
    unawaited(
      trayManager.setTitle(title).catchError((Object e) {
        logError(_tag, 'setTitle failed', e);
      }),
    );
  }

  /// 需求3 降级落点（形态③）：主窗恢复可见时读一次剪贴板。
  ///
  /// 由 main.dart 的 onWindowFocus 调用。前台有焦点时读取可靠
  /// （规避 Wayland 失焦门控）。
  Future<void> checkClipboardOnRestore() async {
    if (!_active) return;
    if (_settings?.clipboardWatchEnabled != true) return;
    try {
      final data = await Clipboard.getData(Clipboard.kTextPlain);
      final text = data?.text ?? '';
      if (text.isEmpty || text == _lastClipboardValue) return;
      _lastClipboardValue = text;

      final urls = _urlRegex
          .allMatches(text)
          .map((m) => m.group(0)!)
          .toList();
      if (urls.isEmpty) return;

      final ctx = _navigatorKey?.currentContext;
      if (ctx == null || !ctx.mounted) return;
      final settings = SettingsProvider.globalInstance ?? _settings!;
      logInfo(_tag, 'clipboard URL detected on restore: ${urls.length}');
      showQuickDownloadDialog(
        ctx,
        url: urls.join('\n'),
        filename: '',
        fileSize: 0,
        mimeType: '',
        cookies: '',
        defaultSaveDir: settings.effectiveDefaultSaveDir,
        defaultQueueId: settings.defaultQueueId,
      );
    } catch (e, stack) {
      logError(_tag, 'clipboard check failed', e, stack);
    }
  }
}

/// FloatingBallService 的 Wayland 降级挂钩扩展。
extension WaylandDegradationHook on FloatingBallService {
  /// Linux 能力确定为 wayland 后激活降级通道。
  void activateWaylandDegradation({
    required SettingsProvider settings,
    required GlobalKey<NavigatorState> navigatorKey,
  }) {
    WaylandDegradationService.instance.activate(
      settings: settings,
      navigatorKey: navigatorKey,
    );
  }
}
