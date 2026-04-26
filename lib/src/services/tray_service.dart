import 'dart:io';

import 'package:flutter/widgets.dart';
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

  // Windows 系统托盘图标路径（深/浅色任务栏各一套）
  String? _winTrayDarkPath; // 白色箭头 — 适配深色任务栏
  String? _winTrayLightPath; // 深蓝色箭头 — 适配浅色任务栏
  // 当前有效的深/浅色状态，初始值跟随系统，后续由 setIsDark() 驱动
  bool _isDark = false;

  /// 应用退出回调 — 由外部（如 _FluxDownAppState）设置以实现优雅退出。
  /// 回调中应等待待处理通知、销毁托盘、再销毁窗口。
  Future<void> Function()? onExitApp;

  /// 初始化系统托盘图标和菜单
  Future<void> init() async {
    logInfo(_tag, 'init called, _initialized=$_initialized');
    if (_initialized) return;
    _initialized = true;

    // 图标路径：Windows/Linux 使用绝对文件系统路径，macOS 使用 Flutter asset key
    // CMakeLists.txt 已配置将图标文件复制到 exe 同级目录
    final exeDir = File(Platform.resolvedExecutable).parent.path;
    final String iconPath;
    final bool isTemplate;
    if (Platform.isWindows) {
      // Windows: 根据系统亮暗模式选择初始托盘图标
      //   tray_win_dark.ico  = 白色箭头（深色任务栏）
      //   tray_win_light.ico = 深蓝色箭头（浅色任务栏）
      // CMakeLists.txt 已将两个文件复制到 exe 同级目录
      // 初始值使用系统亮度；启动后由 _FluxDownAppState 通过 setIsDark() 修正为 app 主题
      _winTrayDarkPath = p.join(exeDir, 'tray_win_dark.ico');
      _winTrayLightPath = p.join(exeDir, 'tray_win_light.ico');
      _isDark =
          WidgetsBinding.instance.platformDispatcher.platformBrightness ==
          Brightness.dark;
      iconPath = _windowsTrayIconPath();
      isTemplate = false;
    } else if (Platform.isMacOS) {
      // macOS: tray_manager 使用 rootBundle.load() 加载，需要 Flutter asset key
      // 使用单色模板图标，macOS 自动适配亮色/暗色菜单栏
      iconPath = 'assets/logo/tray_iconTemplate.png';
      isTemplate = true;
    } else {
      // Linux: exe is at <prefix>/bin/, flutter_assets at <prefix>/data/flutter_assets/
      iconPath = p.join(
        exeDir,
        'data',
        'flutter_assets',
        'assets',
        'logo',
        'fluxdown_logo.png',
      );
      isTemplate = false;
    }

    logInfo(_tag, 'setting icon: $iconPath (isTemplate=$isTemplate)');
    await trayManager.setIcon(iconPath, isTemplate: isTemplate);
    // setToolTip is not implemented on Linux
    if (!Platform.isLinux) {
      await trayManager.setToolTip('FluxDown');
    }

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

  // ─────────────────────────────────────────────
  // Windows 深/浅色模式托盘图标切换
  // ─────────────────────────────────────────────

  /// 返回当前 _isDark 对应的 Windows 托盘图标路径
  String _windowsTrayIconPath() {
    return _isDark ? (_winTrayDarkPath ?? '') : (_winTrayLightPath ?? '');
  }

  /// 由外部（_FluxDownAppState）在应用主题或系统亮度变化时调用，
  /// 将托盘图标切换为与 app 当前生效主题一致的深/浅色版本。
  Future<void> setIsDark(bool isDark) async {
    if (!Platform.isWindows) return;
    if (_isDark == isDark) return; // 无变化，跳过
    _isDark = isDark;
    if (!_initialized || _isExiting) return;
    final newPath = _windowsTrayIconPath();
    logInfo(_tag, 'setIsDark($isDark) → $newPath');
    try {
      await trayManager.setIcon(newPath, isTemplate: false);
    } catch (e, stack) {
      logError(_tag, 'setIsDark: failed to update tray icon', e, stack);
    }
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
