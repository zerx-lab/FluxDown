import 'dart:async';
import 'dart:ffi' hide Size;
import 'dart:io';
import 'dart:math' as math;
import 'dart:ui' show PlatformDispatcher;

import 'package:ffi/ffi.dart';
import 'package:flutter/painting.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:window_manager/window_manager.dart';

import 'log_service.dart';
import 'win32_toast/win32_bindings.dart';

const _tag = 'WindowState';

// SharedPreferences 存储 key（原生层 first_frame_cb 显示窗口前，
// Dart 侧在 runApp 之前直接 await 读取并应用）
const _kWindowX = 'window_state_x';
const _kWindowY = 'window_state_y';
const _kWindowWidth = 'window_state_width';
const _kWindowHeight = 'window_state_height';
const _kWindowMaximized = 'window_state_maximized';
const _kPrefsInitTimeout = Duration(seconds: 3);

/// 窗口最小尺寸限制
const _kMinWidth = 900.0;
const _kMinHeight = 500.0;

/// 坐标合理性阈值：允许多显示器配置下的负坐标，但拒绝明显异常值。
///
/// 典型异常值来源：
///   - Windows 最小化 → (-32000, -32000)
///   - Linux 隐藏后 WM 返回的各种巨大负值
///
/// 合理范围：多显示器可能出现 -3840 左右的负坐标（4K 屏在左侧），
/// 但不会出现 -10000 以下的值。这里取 -500 作为保守下限，覆盖
/// 窗口部分拖出屏幕的正常场景。
const _kMinPosition = -500.0;
const _kMaxPosition = 20000.0;

/// 恢复位置时要求窗口与显示器的最小重叠边长（物理像素）。
/// 低于此值视为"实际不可见"（如 2560 宽屏幕上 x=2552 只剩 8px 贴边），
/// fallback 到居中。100px 足以看到并抓住标题栏。
const _kMinVisiblePx = 100;

/// 窗口状态持久化服务。
///
/// ## 启动阶段（解决闪烁问题的关键）
///
/// `waitUntilReadyToShow` 的回调参数类型是 `VoidCallback`（不是 `Future`），
/// 内部通过 `callback()` 同步调用，async 回调中的 `await` 操作全部变成
/// fire-and-forget，与原生层 `first_frame_cb` 的 `gtk_widget_show` 竞争，
/// 导致窗口以默认 1280×720 先显示再跳变。
///
/// 解决方案：**完全不依赖回调**，在 `runApp` 之前直接 `await` 调用
/// `loadState()` → `applyState()`，所有 method-channel 调用同步完成后
/// 才进入 Flutter 渲染循环。当 `first_frame_cb` 触发 show 时，
/// 窗口属性已经就位。
///
/// ## 运行时
///
/// 通过 WindowListener 回调持久化窗口位置、大小、最大化状态，
/// 使用 500ms 防抖避免拖拽/调整大小时频繁写入。
///
/// ## 隐藏到托盘的坐标保护
///
/// `window_manager` Linux 原生层的 `hide()` 实现中，在 `gtk_widget_hide()`
/// 之后会调用 `gtk_window_move()` 恢复位置，这会触发 `configure-event` →
/// Dart 层 `onWindowMoved()` → 防抖保存。但此时窗口已隐藏，
/// `getPosition()` 返回异常坐标（如 -32000），会覆盖 `saveNow()` 之前
/// 保存的正确值。
///
/// 防护措施：
///   1. `_save()` 检查窗口可见性，不可见时跳过
///   2. `_save()` 校验坐标合理性，异常值时跳过
///   3. `applyState()` 恢复位置前校验，异常时 fallback 到居中
class WindowStateService {
  WindowStateService._();

  static final WindowStateService instance = WindowStateService._();

  // ---------------------------------------------------------------------------
  // 启动阶段加载的窗口状态
  // ---------------------------------------------------------------------------

  double? _savedX;
  double? _savedY;
  double? _savedWidth;
  double? _savedHeight;
  bool _savedMaximized = false;

  /// 是否成功从 SharedPreferences 读取到了有效的宽高
  bool get hasSavedSize => _savedWidth != null && _savedHeight != null;

  /// 是否成功从 SharedPreferences 读取到了有效的位置
  bool get hasSavedPosition => _savedX != null && _savedY != null;

  /// 保存的窗口宽度（经过 clamp，至少 [_kMinWidth]）
  double get savedWidth =>
      (_savedWidth ?? 1280).clamp(_kMinWidth, double.infinity);

  /// 保存的窗口高度（经过 clamp，至少 [_kMinHeight]）
  double get savedHeight =>
      (_savedHeight ?? 720).clamp(_kMinHeight, double.infinity);

  // ---------------------------------------------------------------------------
  // 运行时状态
  // ---------------------------------------------------------------------------

  /// 防抖定时器
  Timer? _debounceTimer;

  /// 防抖延迟
  static const _debounceDuration = Duration(milliseconds: 500);

  /// 当前是否处于最大化状态
  bool _isMaximized = false;

  /// 最大化前的窗口位置和大小（最大化时保留正常尺寸用于持久化）
  Rect? _normalBounds;

  // ---------------------------------------------------------------------------
  // 启动阶段：加载 & 应用
  // ---------------------------------------------------------------------------

  /// 从 SharedPreferences 读取保存的窗口状态。
  ///
  /// 纯读取操作，不调用任何 windowManager API。
  /// 应在 `windowManager.ensureInitialized()` 之后调用。
  Future<void> loadState() async {
    try {
      final prefs = await SharedPreferences.getInstance().timeout(
        _kPrefsInitTimeout,
      );
      _savedX = prefs.getDouble(_kWindowX);
      _savedY = prefs.getDouble(_kWindowY);
      _savedWidth = prefs.getDouble(_kWindowWidth);
      _savedHeight = prefs.getDouble(_kWindowHeight);
      _savedMaximized = prefs.getBool(_kWindowMaximized) ?? false;

      _isMaximized = _savedMaximized;

      // 初始化 _normalBounds 供后续最大化场景使用
      if (hasSavedSize) {
        _normalBounds = Rect.fromLTWH(
          _savedX ?? 0,
          _savedY ?? 0,
          savedWidth,
          savedHeight,
        );
      }

      logInfo(
        _tag,
        'loaded: position=($_savedX, $_savedY), '
        'size=(${_savedWidth}x$_savedHeight), '
        'maximized=$_savedMaximized',
      );
    } catch (e, stack) {
      logError(_tag, 'failed to load window state', e, stack);
    }
  }

  /// 在 `runApp` 之前调用，直接 `await` 设置窗口属性。
  ///
  /// 按严格顺序执行：setSize → setPosition/center → maximize。
  /// 所有操作通过 method-channel 同步完成后才返回，
  /// 确保后续 `first_frame_cb` show 窗口时属性已就位。
  ///
  /// **不调用 show / focus** — 窗口显示由原生层 `first_frame_cb`
  /// （非 silentStart）或后续 `windowManager.show()`（从托盘恢复）控制。
  Future<void> applyState() async {
    try {
      // 1) 设置窗口大小
      if (hasSavedSize) {
        await windowManager.setSize(Size(savedWidth, savedHeight));
        logInfo(_tag, 'applied size: ${savedWidth}x$savedHeight');
      } else {
        // 首次启动：使用默认大小
        await windowManager.setSize(const Size(1280, 720));
        logInfo(_tag, 'applied default size: 1280x720');
      }

      // 2) 设置窗口位置（带合理性校验）
      if (hasSavedPosition) {
        final x = _savedX!;
        final y = _savedY!;
        if (_isPositionValid(x, y) &&
            _isRectOnAnyDisplay(x, y, savedWidth, savedHeight)) {
          await windowManager.setPosition(Offset(x, y));
          logInfo(_tag, 'applied position: ($x, $y)');
        } else {
          // 坐标异常（上次隐藏到托盘时被错误保存），或保存的矩形不再落在
          // 任何显示器上（如外接显示器已断开），fallback 到居中
          logInfo(
            _tag,
            'saved position ($x, $y) invalid or off-screen '
            '(size ${savedWidth}x$savedHeight), centering instead',
          );
          await windowManager.setAlignment(Alignment.center);
        }
      } else {
        // 没有保存的位置 → 居中
        await windowManager.setAlignment(Alignment.center);
        logInfo(_tag, 'applied center alignment (no saved position)');
      }

      // 3) 最大化（在 setSize/setPosition 之后）
      if (_savedMaximized) {
        await windowManager.maximize();
        logInfo(_tag, 'applied maximized');
      }
    } catch (e, stack) {
      logError(_tag, 'failed to apply window state', e, stack);
    }
  }

  // ---------------------------------------------------------------------------
  // 运行时：WindowListener 回调
  // ---------------------------------------------------------------------------

  /// 窗口移动时调用（防抖保存）
  void onMoved() {
    _debounceSave();
  }

  /// 窗口调整大小时调用（防抖保存）
  void onResized() {
    _debounceSave();
  }

  /// 窗口最大化时调用
  void onMaximized() {
    _isMaximized = true;
    _debounceSave();
  }

  /// 窗口从最大化恢复时调用
  void onUnmaximized() {
    _isMaximized = false;
    _debounceSave();
  }

  /// 立即保存当前窗口状态（用于退出/隐藏前确保状态持久化）。
  ///
  /// 调用方保证此时窗口仍然可见，因此跳过可见性检查直接保存。
  /// 同时取消待执行的防抖定时器，防止后续 `hide()` 触发的
  /// `configure-event` 产生新的延迟保存覆盖本次结果。
  Future<void> saveNow() async {
    _debounceTimer?.cancel();
    _debounceTimer = null;
    await _save(force: true);
  }

  /// 释放资源
  void dispose() {
    _debounceTimer?.cancel();
    _debounceTimer = null;
  }

  // ---------------------------------------------------------------------------
  // 内部方法
  // ---------------------------------------------------------------------------

  void _debounceSave() {
    _debounceTimer?.cancel();
    _debounceTimer = Timer(_debounceDuration, _save);
  }

  /// 持久化当前窗口状态。
  ///
  /// [force] 为 true 时跳过可见性检查（仅 [saveNow] 在隐藏前调用时使用）。
  /// 默认的防抖调用 force=false，会检查窗口可见性和坐标合理性，
  /// 防止 `hide()` 后异常坐标被写入。
  Future<void> _save({bool force = false}) async {
    try {
      // ── 防护层 1：窗口状态检查 ──
      // 以下两种状态下 getPosition() 返回的坐标不可靠，必须跳过：
      //   - 隐藏（hide）：Linux 上 gtk_widget_hide() 后 WM 返回异常负值
      //   - 最小化（iconify）：isVisible 仍为 true，但坐标同样不可靠
      if (!force) {
        final isVisible = await windowManager.isVisible();
        final isMinimized = await windowManager.isMinimized();
        if (!isVisible || isMinimized) {
          logInfo(
            _tag,
            'save skipped: visible=$isVisible, minimized=$isMinimized',
          );
          return;
        }
      }

      final prefs = await SharedPreferences.getInstance();

      await prefs.setBool(_kWindowMaximized, _isMaximized);

      // 最大化状态下不更新位置和大小（保留正常状态的值，
      // 以便恢复时使用正确的窗口尺寸而非全屏尺寸）
      if (_isMaximized) {
        if (_normalBounds != null) {
          await prefs.setDouble(_kWindowX, _normalBounds!.left);
          await prefs.setDouble(_kWindowY, _normalBounds!.top);
          await prefs.setDouble(_kWindowWidth, _normalBounds!.width);
          await prefs.setDouble(_kWindowHeight, _normalBounds!.height);
          logInfo(
            _tag,
            'saved (maximized=true, normal bounds='
            '${_normalBounds!.left},${_normalBounds!.top} '
            '${_normalBounds!.width}x${_normalBounds!.height})',
          );
        } else {
          logInfo(_tag, 'saved (maximized=true, no normal bounds)');
        }
        return;
      }

      // 非最大化 → 获取当前窗口位置和大小
      final position = await windowManager.getPosition();
      final size = await windowManager.getSize();

      // ── 防护层 2：坐标合理性校验 ──
      // 即使通过了可见性检查，仍验证坐标是否在合理范围内。
      // 某些 WM/合成器在特定时序下可能返回异常值。
      if (!_isPositionValid(position.dx, position.dy)) {
        logInfo(
          _tag,
          'save skipped: position (${position.dx}, ${position.dy}) '
          'out of valid range [$_kMinPosition, $_kMaxPosition]',
        );
        return;
      }

      _normalBounds = Rect.fromLTWH(
        position.dx,
        position.dy,
        size.width,
        size.height,
      );

      await prefs.setDouble(_kWindowX, position.dx);
      await prefs.setDouble(_kWindowY, position.dy);
      await prefs.setDouble(_kWindowWidth, size.width);
      await prefs.setDouble(_kWindowHeight, size.height);

      logInfo(
        _tag,
        'saved: position=(${position.dx}, ${position.dy}), '
        'size=(${size.width}x${size.height}), maximized=false',
      );
    } catch (e, stack) {
      logError(_tag, 'failed to save window state', e, stack);
    }
  }

  /// 检查坐标是否在合理范围内。
  ///
  /// 多显示器配置下坐标可以为负值（如左侧屏幕），但不应该
  /// 出现 -32000 这样的极端值（Windows 最小化）或 Linux 隐藏
  /// 后 WM 返回的异常坐标。
  static bool _isPositionValid(double x, double y) {
    return x >= _kMinPosition &&
        x <= _kMaxPosition &&
        y >= _kMinPosition &&
        y <= _kMaxPosition;
  }

  /// 检查保存的窗口矩形是否以可用的方式落在当前某个显示器上。
  ///
  /// 显示器拓扑变化（如拔掉 4K 外接屏后仅剩笔记本屏）时，历史坐标
  /// 可能整体落在虚拟屏幕之外——粗校验 [-500, 20000] 无法发现，
  /// 结果是任务栏有图标但窗口不可见。
  ///
  /// Windows 判定逻辑：
  ///   1. window_manager 保存/恢复的是逻辑像素（插件按 devicePixelRatio
  ///      换算），而 Win32 显示器 API 用物理虚拟屏坐标 → 先按当前窗口
  ///      DPR 换算。混合 DPI 多屏下不完全精确，但误差只会导致 fallback
  ///      居中（安全方向），不会产生不可见窗口。
  ///   2. `MonitorFromRect(MONITOR_DEFAULTTONULL)` 返回 NULL = 零相交。
  ///   3. 仅相交仍不够：2560 宽屏幕上 x=2552 只剩 8px 贴边，等于不可见。
  ///      取相交面积最大的显示器，要求重叠区至少 [_kMinVisiblePx]²
  ///      （足以看到并抓住标题栏）。
  ///
  /// 其余平台维持原行为（返回 true，交给 WM 处理）。
  static bool _isRectOnAnyDisplay(
    double x,
    double y,
    double width,
    double height,
  ) {
    if (!Platform.isWindows) return true;
    try {
      final dpr =
          PlatformDispatcher.instance.implicitView?.devicePixelRatio ?? 1.0;
      final rect = calloc<RECT>();
      final mi = calloc<MONITORINFO>();
      try {
        rect.ref.left = (x * dpr).round();
        rect.ref.top = (y * dpr).round();
        rect.ref.right = ((x + width) * dpr).round();
        rect.ref.bottom = ((y + height) * dpr).round();
        final monitor = monitorFromRect(rect, MONITOR_DEFAULTTONULL);
        if (monitor == 0) return false; // 与任何显示器零相交

        // MonitorFromRect 返回相交面积最大的显示器 → 校验重叠区大小
        mi.ref.cbSize = sizeOf<MONITORINFO>();
        if (getMonitorInfoW(monitor, mi) == 0) return true; // 查询失败不阻塞
        final m = mi.ref.rcMonitor;
        final overlapW =
            math.min(rect.ref.right, m.right) - math.max(rect.ref.left, m.left);
        final overlapH =
            math.min(rect.ref.bottom, m.bottom) - math.max(rect.ref.top, m.top);
        return overlapW >= _kMinVisiblePx && overlapH >= _kMinVisiblePx;
      } finally {
        calloc.free(rect);
        calloc.free(mi);
      }
    } catch (e) {
      // FFI 失败（理论上不会发生）时不阻塞恢复流程
      logInfo(_tag, 'monitor check failed, assuming on-screen: $e');
      return true;
    }
  }
}
