import 'dart:io';

import 'package:path/path.dart' as p;
import 'package:tray_manager/tray_manager.dart';
import 'package:window_manager/window_manager.dart';

import '../i18n/locale_provider.dart';
import 'log_service.dart';

const _tag = 'TrayService';

/// 系统托盘服务 — 管理托盘图标、菜单和事件
class TrayService with TrayListener {
  TrayService._();
  static final TrayService instance = TrayService._();

  bool _initialized = false;

  /// 是否正在退出过程中 — 防止重入和退出期间操作窗口
  bool _isExiting = false;

  /// 应用退出回调 — 由外部（如 _FluxDownAppState）设置以实现优雅退出。
  /// 回调中应等待待处理通知、销毁托盘、再销毁窗口。
  Future<void> Function()? onExitApp;

  /// 初始化系统托盘图标和菜单
  Future<void> init() async {
    logInfo(_tag, 'init called, _initialized=$_initialized');
    if (_initialized) return;
    _initialized = true;

    // 图标路径必须是相对于 exe 目录的路径或绝对路径
    // CMakeLists.txt 已配置将 app_icon.ico 复制到 exe 同级目录
    final exeDir = File(Platform.resolvedExecutable).parent.path;
    final iconPath = Platform.isWindows
        ? p.join(exeDir, 'app_icon.ico')
        : p.join(
            exeDir,
            'data',
            'flutter_assets',
            'assets',
            'logo',
            'fluxdown_logo.png',
          );

    logInfo(_tag, 'setting icon: $iconPath');
    await trayManager.setIcon(iconPath);
    await trayManager.setToolTip('FluxDown');

    final menu = Menu(
      items: [
        MenuItem(key: 'show_window', label: currentS.trayShowWindow),
        MenuItem.separator(),
        MenuItem(key: 'exit_app', label: currentS.trayExit),
      ],
    );
    await trayManager.setContextMenu(menu);
    trayManager.addListener(this);
    logInfo(_tag, 'init done');
  }

  /// 刷新托盘菜单文字（语言切换后调用）
  Future<void> refreshMenu() async {
    if (!_initialized) return;
    logInfo(_tag, 'refreshMenu called');
    final menu = Menu(
      items: [
        MenuItem(key: 'show_window', label: currentS.trayShowWindow),
        MenuItem.separator(),
        MenuItem(key: 'exit_app', label: currentS.trayExit),
      ],
    );
    await trayManager.setContextMenu(menu);
    logInfo(_tag, 'refreshMenu done');
  }

  /// 销毁托盘图标
  Future<void> destroy() async {
    logInfo(_tag, 'destroy called, _initialized=$_initialized');
    trayManager.removeListener(this);
    await trayManager.destroy();
    _initialized = false;
    logInfo(_tag, 'destroy done');
  }

  /// 显示窗口并聚焦
  Future<void> _showWindow() async {
    logInfo(_tag, '_showWindow called, _isExiting=$_isExiting');
    // 退出过程中不再操作窗口，避免在已 destroyed 的窗口上调用导致崩溃
    if (_isExiting) {
      logInfo(_tag, '_showWindow skipped (isExiting)');
      return;
    }
    try {
      logInfo(_tag, 'calling windowManager.show()...');
      await windowManager.show();
      logInfo(_tag, 'calling windowManager.focus()...');
      await windowManager.focus();
      logInfo(_tag, '_showWindow done');
    } catch (e, stack) {
      logError(_tag, '_showWindow error', e, stack);
    }
  }

  /// 隐藏窗口到托盘
  Future<void> hideToTray() async {
    logInfo(_tag, 'hideToTray called, _isExiting=$_isExiting');
    if (_isExiting) {
      logInfo(_tag, 'hideToTray skipped (isExiting)');
      return;
    }
    try {
      await windowManager.hide();
      logInfo(_tag, 'hideToTray done');
    } catch (e, stack) {
      logError(_tag, 'hideToTray error', e, stack);
    }
  }

  // ─────────────────────────────────────────────
  // TrayListener 回调
  // ─────────────────────────────────────────────

  @override
  void onTrayIconMouseDown() {
    logInfo(_tag, 'onTrayIconMouseDown, _isExiting=$_isExiting');
    // 退出中不响应托盘点击
    if (_isExiting) return;
    _showWindow();
  }

  @override
  void onTrayIconRightMouseDown() {
    logInfo(_tag, 'onTrayIconRightMouseDown, _isExiting=$_isExiting');
    if (_isExiting) return;
    trayManager.popUpContextMenu();
  }

  @override
  void onTrayIconRightMouseUp() {}

  @override
  void onTrayMenuItemClick(MenuItem menuItem) {
    logInfo(
      _tag,
      'onTrayMenuItemClick: key=${menuItem.key}, _isExiting=$_isExiting',
    );
    if (_isExiting) return;
    switch (menuItem.key) {
      case 'show_window':
        _showWindow();
      case 'exit_app':
        _handleExit();
    }
  }

  /// 优雅退出 — 通过回调通知上层执行完整清理流程
  Future<void> _handleExit() async {
    logInfo(_tag, '_handleExit called, _isExiting=$_isExiting');
    // 防止重入：多次点击退出不会重复执行
    if (_isExiting) return;
    _isExiting = true;

    try {
      if (onExitApp != null) {
        logInfo(_tag, 'calling onExitApp callback...');
        await onExitApp!();
        logInfo(_tag, 'onExitApp callback done');
      } else {
        // 兜底：如果没有设置回调，直接退出（但先清理托盘）
        logInfo(_tag, 'no onExitApp callback, direct destroy');
        await destroy();
        await windowManager.destroy();
      }
    } catch (e, stack) {
      logError(_tag, '_handleExit error', e, stack);
      // 出错也尝试强制退出
      try {
        await windowManager.destroy();
      } catch (_) {}
    }
  }
}
