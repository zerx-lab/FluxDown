import 'dart:ffi';
import 'dart:ui' as ui;

import 'package:ffi/ffi.dart';

/// Win32 API bindings for sub-window management.
///
/// 子窗口不能使用 window_manager（全局 channel 会覆盖主窗口导致崩溃），
/// 因此通过 dart:ffi 直接调用 Win32 API 来控制子窗口。
/// 这是 Flutter Windows 桌面开发中处理平台原生能力的标准做法。

// --- Win32 constants (names follow Win32 API convention) ---
// ignore_for_file: constant_identifier_names
const int _SWP_NOZORDER = 0x0004;
const int _SWP_NOMOVE = 0x0002;
const int _SWP_NOSIZE = 0x0001;
const int _HWND_TOPMOST = -1;
const int _HWND_NOTOPMOST = -2;
const int _GWL_STYLE = -16;
const int _WS_THICKFRAME = 0x00040000;
const int _WS_MAXIMIZEBOX = 0x00010000;
const int _WS_CAPTION = 0x00C00000;
const int _WS_EX_TOOLWINDOW = 0x00000080;
const int _WS_EX_APPWINDOW = 0x00040000;
const int _GWL_EXSTYLE = -20;
const int _SWP_FRAMECHANGED = 0x0020;
const int _SM_CXSCREEN = 0;
const int _SM_CYSCREEN = 1;
const int _SW_SHOW = 5;
const int _WM_CLOSE = 0x0010;

// --- Win32 function typedefs ---
typedef _GetActiveWindowNative = IntPtr Function();
typedef _GetActiveWindowDart = int Function();

typedef _SetWindowPosNative =
    Int32 Function(
      IntPtr hWnd,
      IntPtr hWndInsertAfter,
      Int32 x,
      Int32 y,
      Int32 cx,
      Int32 cy,
      Uint32 uFlags,
    );
typedef _SetWindowPosDart =
    int Function(
      int hWnd,
      int hWndInsertAfter,
      int x,
      int y,
      int cx,
      int cy,
      int uFlags,
    );

typedef _SetWindowTextNative =
    Int32 Function(IntPtr hWnd, Pointer<Utf16> lpString);
typedef _SetWindowTextDart = int Function(int hWnd, Pointer<Utf16> lpString);

typedef _GetWindowLongNative = IntPtr Function(IntPtr hWnd, Int32 nIndex);
typedef _GetWindowLongDart = int Function(int hWnd, int nIndex);

typedef _SetWindowLongNative =
    IntPtr Function(IntPtr hWnd, Int32 nIndex, IntPtr dwNewLong);
typedef _SetWindowLongDart = int Function(int hWnd, int nIndex, int dwNewLong);

typedef _GetSystemMetricsNative = Int32 Function(Int32 nIndex);
typedef _GetSystemMetricsDart = int Function(int nIndex);

typedef _SetForegroundWindowNative = Int32 Function(IntPtr hWnd);
typedef _SetForegroundWindowDart = int Function(int hWnd);

typedef _ShowWindowNative = Int32 Function(IntPtr hWnd, Int32 nCmdShow);
typedef _ShowWindowDart = int Function(int hWnd, int nCmdShow);

typedef _PostMessageNative =
    Int32 Function(IntPtr hWnd, Uint32 msg, IntPtr wParam, IntPtr lParam);
typedef _PostMessageDart =
    int Function(int hWnd, int msg, int wParam, int lParam);

typedef _GetDpiForWindowNative = Uint32 Function(IntPtr hWnd);
typedef _GetDpiForWindowDart = int Function(int hWnd);

// --- Lazy-loaded Win32 functions ---
final _user32 = DynamicLibrary.open('user32.dll');

final _getActiveWindow = _user32
    .lookupFunction<_GetActiveWindowNative, _GetActiveWindowDart>(
      'GetActiveWindow',
    );

final _setWindowPos = _user32
    .lookupFunction<_SetWindowPosNative, _SetWindowPosDart>('SetWindowPos');

final _setWindowText = _user32
    .lookupFunction<_SetWindowTextNative, _SetWindowTextDart>('SetWindowTextW');

final _getWindowLong = _user32
    .lookupFunction<_GetWindowLongNative, _GetWindowLongDart>(
      'GetWindowLongPtrW',
    );

final _setWindowLong = _user32
    .lookupFunction<_SetWindowLongNative, _SetWindowLongDart>(
      'SetWindowLongPtrW',
    );

final _getSystemMetrics = _user32
    .lookupFunction<_GetSystemMetricsNative, _GetSystemMetricsDart>(
      'GetSystemMetrics',
    );

final _setForegroundWindow = _user32
    .lookupFunction<_SetForegroundWindowNative, _SetForegroundWindowDart>(
      'SetForegroundWindow',
    );

final _showWindow = _user32.lookupFunction<_ShowWindowNative, _ShowWindowDart>(
  'ShowWindow',
);

final _postMessage = _user32
    .lookupFunction<_PostMessageNative, _PostMessageDart>('PostMessageW');

// GetDpiForWindow (Windows 10 1607+)
final _getDpiForWindow = _user32
    .lookupFunction<_GetDpiForWindowNative, _GetDpiForWindowDart>(
      'GetDpiForWindow',
    );

/// Sub-window helper — 通过 Win32 API 控制子窗口。
///
/// 使用前必须在子窗口的 `initState` 或 `_initWindow` 中调用
/// [SubWindowUtils.init] 获取当前窗口句柄。
class SubWindowUtils {
  SubWindowUtils._();

  static int _hwnd = 0;

  /// 初始化 — 获取当前窗口的 HWND。
  /// 必须在窗口已经创建之后调用（通常在 initState + addPostFrameCallback 中）。
  static void init() {
    _hwnd = _getActiveWindow();
  }

  /// 当前窗口句柄是否已获取
  static bool get isInitialized => _hwnd != 0;

  /// 设置窗口大小（逻辑像素，自动处理 DPI 缩放）
  static void setSize(ui.Size size) {
    if (_hwnd == 0) return;
    final dpi = _getDpiForWindow(_hwnd);
    final scale = dpi / 96.0;
    final w = (size.width * scale).round();
    final h = (size.height * scale).round();
    _setWindowPos(_hwnd, 0, 0, 0, w, h, _SWP_NOZORDER | _SWP_NOMOVE);
  }

  /// 设置窗口位置（逻辑像素，自动处理 DPI 缩放）
  static void setPosition(ui.Offset offset) {
    if (_hwnd == 0) return;
    final dpi = _getDpiForWindow(_hwnd);
    final scale = dpi / 96.0;
    final x = (offset.dx * scale).round();
    final y = (offset.dy * scale).round();
    _setWindowPos(_hwnd, 0, x, y, 0, 0, _SWP_NOZORDER | _SWP_NOSIZE);
  }

  /// 窗口居中
  static void center() {
    if (_hwnd == 0) return;

    final screenW = _getSystemMetrics(_SM_CXSCREEN);
    final screenH = _getSystemMetrics(_SM_CYSCREEN);

    // 读取当前窗口尺寸 — 通过 RECT
    final rect = calloc<_RECT>();
    try {
      final ok = _getWindowRect(_hwnd, rect.cast());
      if (ok != 0) {
        final winW = rect.ref.right - rect.ref.left;
        final winH = rect.ref.bottom - rect.ref.top;
        final x = (screenW - winW) ~/ 2;
        final y = (screenH - winH) ~/ 2;
        _setWindowPos(_hwnd, 0, x, y, 0, 0, _SWP_NOZORDER | _SWP_NOSIZE);
      }
    } finally {
      calloc.free(rect);
    }
  }

  /// 设置窗口标题
  static void setTitle(String title) {
    if (_hwnd == 0) return;
    final ptr = title.toNativeUtf16();
    _setWindowText(_hwnd, ptr);
    calloc.free(ptr);
  }

  /// 设置窗口是否置顶
  static void setAlwaysOnTop(bool value) {
    if (_hwnd == 0) return;
    final insertAfter = value ? _HWND_TOPMOST : _HWND_NOTOPMOST;
    _setWindowPos(_hwnd, insertAfter, 0, 0, 0, 0, _SWP_NOMOVE | _SWP_NOSIZE);
  }

  /// 移除原生标题栏和窗口边框 — 子窗口使用自绘标题栏时调用。
  /// 同时移除 WS_CAPTION（标题栏）和 WS_THICKFRAME（粗边框），
  /// 确保客户区完全占满窗口，无非客户区像素占用。
  static void removeCaption() {
    if (_hwnd == 0) return;
    var style = _getWindowLong(_hwnd, _GWL_STYLE);
    style &= ~_WS_CAPTION;
    style &= ~_WS_THICKFRAME;
    style &= ~_WS_MAXIMIZEBOX;
    _setWindowLong(_hwnd, _GWL_STYLE, style);
    // SWP_FRAMECHANGED 强制系统重新计算窗口非客户区
    _setWindowPos(
      _hwnd,
      0,
      0,
      0,
      0,
      0,
      _SWP_NOZORDER | _SWP_NOMOVE | _SWP_NOSIZE | _SWP_FRAMECHANGED,
    );
  }

  /// 设置窗口是否可缩放
  static void setResizable(bool value) {
    if (_hwnd == 0) return;
    var style = _getWindowLong(_hwnd, _GWL_STYLE);
    if (value) {
      style |= _WS_THICKFRAME | _WS_MAXIMIZEBOX;
    } else {
      style &= ~_WS_THICKFRAME;
      style &= ~_WS_MAXIMIZEBOX;
    }
    _setWindowLong(_hwnd, _GWL_STYLE, style);
    // 通知系统重新计算非客户区
    _setWindowPos(
      _hwnd,
      0,
      0,
      0,
      0,
      0,
      _SWP_NOZORDER | _SWP_NOMOVE | _SWP_NOSIZE | _SWP_FRAMECHANGED,
    );
  }

  /// 设置是否跳过任务栏显示
  static void setSkipTaskbar(bool value) {
    if (_hwnd == 0) return;
    var exStyle = _getWindowLong(_hwnd, _GWL_EXSTYLE);
    if (value) {
      exStyle |= _WS_EX_TOOLWINDOW;
      exStyle &= ~_WS_EX_APPWINDOW;
    } else {
      exStyle &= ~_WS_EX_TOOLWINDOW;
      exStyle |= _WS_EX_APPWINDOW;
    }
    _setWindowLong(_hwnd, _GWL_EXSTYLE, exStyle);
  }

  /// 窗口获取焦点
  static void focus() {
    if (_hwnd == 0) return;
    _setForegroundWindow(_hwnd);
  }

  /// 显示窗口
  static void show() {
    if (_hwnd == 0) return;
    _showWindow(_hwnd, _SW_SHOW);
  }

  /// 关闭窗口（发送 WM_CLOSE）
  static void close() {
    if (_hwnd == 0) return;
    _postMessage(_hwnd, _WM_CLOSE, 0, 0);
  }
}

// --- RECT struct for GetWindowRect ---
final class _RECT extends Struct {
  @Int32()
  external int left;

  @Int32()
  external int top;

  @Int32()
  external int right;

  @Int32()
  external int bottom;
}

typedef _GetWindowRectNative =
    Int32 Function(IntPtr hWnd, Pointer<_RECT> lpRect);
typedef _GetWindowRectDart = int Function(int hWnd, Pointer<_RECT> lpRect);

final _getWindowRect = _user32
    .lookupFunction<_GetWindowRectNative, _GetWindowRectDart>('GetWindowRect');
