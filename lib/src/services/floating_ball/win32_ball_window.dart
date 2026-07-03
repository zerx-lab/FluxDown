/// Windows 悬浮球分层窗口（方案 S1.1/S1.3）。
///
/// 复用 win32_toast 已验证模式：WS_EX_LAYERED 分层窗口 +
/// UpdateLayeredWindow 贴图 + DefWindowProcW 直通（零 Dart 原生回调）+
/// GetCursorPos/GetAsyncKeyState 轮询交互。
///
/// 与 Toast 的差异：
/// - 常驻（无淡入淡出），状态机 = idle/hover/dragging；
/// - 输入轮询 150ms，拖动中提频 16ms；全屏检测/DPI 检测 2s（tick 分频归并）；
/// - 拖动：位移超 SM_CXDRAG/SM_CYDRAG → SetWindowPos(SWP_NOACTIVATE) 跟随；
/// - 全屏避让：前台窗口独占球所在屏 → SW_HIDE，恢复后重现。
library;

import 'dart:async';
import 'dart:ffi';
import 'dart:math' as math;

import 'package:ffi/ffi.dart';

import '../log_service.dart';
import '../native_overlay/win32_layered_window.dart';
import '../win32_toast/win32_bindings.dart';
import 'floating_ball_renderer.dart';

const _tag = 'Win32Ball';
const _className = 'FluxDownBall_v1';

/// 输入轮询间隔（空闲态，光标不在球上）
const _tickInterval = Duration(milliseconds: 150);

/// 提频间隔（光标悬停球上/按下/拖动中 — 8ms ≈ 125Hz，保证拖动跟手）
const _fastTickInterval = Duration(milliseconds: 8);

// ── 贴边收起（迅雷式）──

/// 吸附判定阈值（逻辑 px）：拖动释放时球边缘距屏幕左/右/上边 ≤ 此值 → 贴边
const double _kDockSnapThreshold = 12;

/// 收起后露出的边条宽度（逻辑 px）
const double _kDockRevealWidth = 14;

/// 展开态光标离开后延迟收起
const _collapseDelay = Duration(milliseconds: 800);

/// 收起/展开动画时长（ms，ease-out cubic）
const _kDockAnimMs = 160;

/// 贴边方向
enum _DockEdge { none, left, right, top }


/// Windows 悬浮球窗口 — 单实例常驻。
///
/// 回调（由 FloatingBallService 注入）：
/// - [onClicked]：单击（未超拖动阈值）
/// - [onMoved]：拖动结束，回传窗口左上角绝对像素坐标
/// - [onDpiChanged]：所在显示器 DPI 变化，需重渲染
class Win32BallWindow {
  Win32BallWindow._();
  static final instance = Win32BallWindow._();

  void Function()? onClicked;
  void Function(double x, double y)? onMoved;
  void Function(double scale)? onDpiChanged;

  /// 右键按下（命中球）。由 FloatingBallService 注入弹出上下文菜单。
  void Function()? onContextMenu;

  int _hwnd = 0;
  int _memDC = 0;
  int _hBitmap = 0;
  int _scaledW = 0;
  int _scaledH = 0;
  int _screenX = 0;
  int _screenY = 0;
  int _dpi = 96;

  Timer? _timer;

  // 拖动状态
  bool _prevMouseDown = false;
  bool _dragging = false;
  bool _pressedInside = false;
  int _pressX = 0;
  int _pressY = 0;
  int _pressWinX = 0;
  int _pressWinY = 0;

  // 全屏避让
  bool _hiddenByFullscreen = false;

  // 销毁转场保护（S0.5：抑制转场期坐标持久化）
  bool _destroying = false;

  // ── 贴边收起（迅雷式）──
  _DockEdge _dockEdge = _DockEdge.none;
  bool _collapsed = false;
  DateTime? _collapseDeadline; // 展开态下光标离开后的收起时刻
  // 动画：非 null = 进行中
  DateTime? _animStart;
  int _animFromX = 0, _animFromY = 0, _animToX = 0, _animToY = 0;

  final _cursorPt = calloc<POINT>();

  bool get isCreated => _hwnd != 0;
  int get hwnd => _hwnd;
  double get scale => _dpi / 96.0;

  /// 创建球窗口（幂等）。[x]/[y] 为窗口左上角绝对像素坐标。
  void create({required int x, required int y}) {
    if (_hwnd != 0) return;
    ensureLayeredWindowClass(_className);

    // 清理僵尸球窗口：热重启后旧 isolate 的窗口仍在（驱动它的 Dart Timer
    // 已死 → 不可拖动），本 isolate _hwnd==0 保证同类窗口必为僵尸。
    final zombieClassPtr = _className.toNativeUtf16();
    try {
      var zombie = findWindowW(zombieClassPtr, nullptr);
      var guard = 0;
      while (zombie != 0 && guard < 8) {
        logInfo(_tag, 'destroying zombie ball window hwnd=$zombie');
        if (destroyWindow(zombie) == 0) break;
        zombie = findWindowW(zombieClassPtr, nullptr);
        guard++;
      }
    } finally {
      calloc.free(zombieClassPtr);
    }

    final classNamePtr = _className.toNativeUtf16();
    final titlePtr = ''.toNativeUtf16();
    try {
      final exStyle =
          WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
      // 初始尺寸按 96dpi 估算，创建后按实际 DPI 重设
      _hwnd = createWindowExW(
        exStyle,
        classNamePtr,
        titlePtr,
        WS_POPUP,
        x,
        y,
        kBallWindowSize.round(),
        kBallWindowSize.round(),
        0,
        0,
        getModuleHandleW(nullptr),
        nullptr,
      );
    } finally {
      calloc.free(classNamePtr);
      calloc.free(titlePtr);
    }
    if (_hwnd == 0) throw StateError('CreateWindowExW returned 0 (ball)');

    _dpi = getDpiForWindow(_hwnd);
    _screenX = x;
    _screenY = y;
    _destroying = false;

    _memDC = createCompatibleDC(0);
    if (_memDC == 0) {
      destroyWindow(_hwnd);
      _hwnd = 0;
      throw StateError('CreateCompatibleDC failed (ball)');
    }

    logInfo(_tag, 'ball window created hwnd=$_hwnd dpi=$_dpi pos=($x,$y)');
  }

  /// 贴入新位图（premultiplied BGRA）。重复调用时先释放旧 DIB（防 GDI 泄漏）。
  void pushImage(BallImage image) {
    if (_hwnd == 0) return;
    final newBitmap = createDibFromBgra(
      image.width,
      image.height,
      image.toBgraPremultiplied(),
    );
    // 先建新再删旧 — 失败时不丢当前画面
    final old = _hBitmap;
    _hBitmap = newBitmap;
    _scaledW = image.width;
    _scaledH = image.height;
    if (old != 0) deleteObject(old);
    _push();
  }

  void _push() {
    if (_hwnd == 0 || _hBitmap == 0) return;
    pushLayeredBitmap(
      hwnd: _hwnd,
      memDC: _memDC,
      hBitmap: _hBitmap,
      screenX: _screenX,
      screenY: _screenY,
      width: _scaledW,
      height: _scaledH,
    );
  }

  /// 显示球并启动交互轮询。
  void show() {
    if (_hwnd == 0) return;
    showWindow(_hwnd, SW_SHOWNOACTIVATE);
    _ensureTimer(_tickInterval);
    _evaluateDock(); // 启动位置若已贴边（上次会话），直接进入 dock 态
    logInfo(_tag, 'ball shown');
  }

  /// 强制展开（外部拖放悬停到收起边条时由 Service 调用）。
  void expandIfCollapsed() {
    if (_dockEdge == _DockEdge.none || !_collapsed) return;
    _collapsed = false;
    _collapseDeadline = null;
    _startAnim(_dockedExpandedPos());
  }

  /// 隐藏球（不销毁资源）。
  void hide() {
    if (_hwnd == 0) return;
    _timer?.cancel();
    _timer = null;
    showWindow(_hwnd, SW_HIDE);
    logInfo(_tag, 'ball hidden');
  }

  /// 销毁窗口与全部 GDI 资源（幂等）。
  void destroy() {
    if (_hwnd == 0) return;
    _destroying = true;
    _timer?.cancel();
    _timer = null;

    // 拖动中销毁：以最后已知坐标为准同步回传（disable() 状态机语义）
    if (_dragging && !_hiddenByFullscreen) {
      onMoved?.call(_screenX.toDouble(), _screenY.toDouble());
    }
    _dragging = false;

    if (_memDC != 0) {
      deleteDC(_memDC);
      _memDC = 0;
    }
    if (_hBitmap != 0) {
      deleteObject(_hBitmap);
      _hBitmap = 0;
    }
    try {
      destroyWindow(_hwnd);
    } catch (e) {
      logError(_tag, 'destroyWindow failed', e);
    }
    _hwnd = 0;
    _hiddenByFullscreen = false;
    logInfo(_tag, 'ball destroyed');
  }

  /// 移动球到指定绝对像素坐标（外部校正用，如坐标校验吸附）。
  void moveTo(int x, int y) {
    if (_hwnd == 0) return;
    _screenX = x;
    _screenY = y;
    _cancelAnim();
    setWindowPos(
      _hwnd,
      0,
      x,
      y,
      0,
      0,
      SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
    );
    _push(); // UpdateLayeredWindow 位置由贴图携带，同步刷新
    _evaluateDock(); // 外部校正后重判贴边
  }

  // ===========================================================================
  // Tick — 输入轮询 + 分频慢检测
  // ===========================================================================

  void _ensureTimer(Duration interval) {
    if (_currentInterval == interval && _timer?.isActive == true) return;
    _currentInterval = interval;
    _timer?.cancel();
    _timer = Timer.periodic(interval, _onTick);
  }

  Duration? _currentInterval;
  DateTime _lastSlowCheck = DateTime.now();
  bool _prevRightDown = false;

  void _onTick(Timer _) {
    if (_hwnd == 0) return;

    // ── 慢速检测（全屏避让 + DPI）：按墙钟节流 ~2s，与 tick 频率解耦 ──
    final now = DateTime.now();
    if (now.difference(_lastSlowCheck).inMilliseconds >= 2000) {
      _lastSlowCheck = now;
      _checkFullscreenAvoidance();
      _checkDpiChange();
    }
    if (_hiddenByFullscreen) return;

    // ── 贴边动画推进 ──
    final animating = _stepAnim(now);

    // ── 鼠标输入 ──
    getCursorPos(_cursorPt);
    final mx = _cursorPt.ref.x;
    final my = _cursorPt.ref.y;
    final hovering = _hitBall(mx, my);

    // 悬停/按下/动画中提频；空闲降频省电。
    if (hovering || _pressedInside || animating || _collapseDeadline != null) {
      _ensureTimer(_fastTickInterval);
    } else {
      _ensureTimer(_tickInterval);
    }

    // ── 贴边收起/展开状态机 ──
    if (_dockEdge != _DockEdge.none && !_pressedInside && !animating) {
      if (_collapsed) {
        // 收起态：悬停露条 → 滑出展开
        if (hovering) {
          _collapsed = false;
          _startAnim(_dockedExpandedPos());
        }
      } else {
        // 展开态：光标离开 → 800ms 后滑回收起
        if (hovering) {
          _collapseDeadline = null;
        } else {
          _collapseDeadline ??= now.add(_collapseDelay);
          if (now.isAfter(_collapseDeadline!)) {
            _collapseDeadline = null;
            _collapsed = true;
            _startAnim(_dockedCollapsedPos());
          }
        }
      }
    }

    // ── 右键：按下沿检测 → 上下文菜单（收起态先展开）──
    final rightDown = (getAsyncKeyState(VK_RBUTTON) & 0x8000) != 0;
    final rightJustPressed = !_prevRightDown && rightDown;
    _prevRightDown = rightDown;
    if (rightJustPressed && hovering && !_dragging && !_collapsed) {
      onContextMenu?.call();
      return;
    }

    // ── 左键 ──
    final vkState = getAsyncKeyState(VK_LBUTTON);
    final isDown = (vkState & 0x8000) != 0;
    final justPressed = !_prevMouseDown && isDown;
    final justReleased = _prevMouseDown && !isDown;
    _prevMouseDown = isDown;

    if (justPressed && hovering && !_collapsed) {
      _pressedInside = true;
      _pressX = mx;
      _pressY = my;
      _pressWinX = _screenX;
      _pressWinY = _screenY;
      return;
    }

    if (_pressedInside && isDown) {
      // 位移阈值判定（系统拖动阈值，物理像素）
      final dx = mx - _pressX;
      final dy = my - _pressY;
      if (!_dragging) {
        final thX = getSystemMetrics(SM_CXDRAG);
        final thY = getSystemMetrics(SM_CYDRAG);
        if (dx.abs() > thX || dy.abs() > thY) {
          _dragging = true;
          _dockEdge = _DockEdge.none; // 拖离即解除贴边
          _collapseDeadline = null;
          _cancelAnim();
        }
      }
      if (_dragging) {
        final nx = _pressWinX + dx;
        final ny = _pressWinY + dy;
        if (nx != _screenX || ny != _screenY) {
          _screenX = nx;
          _screenY = ny;
          // 拖动中只移动窗口，不重贴位图（内容未变；重复
          // UpdateLayeredWindow 是 rev1 拖动卡顿的主因）。
          setWindowPos(
            _hwnd,
            0,
            nx,
            ny,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
          );
        }
      }
      return;
    }

    if (justReleased && _pressedInside) {
      final wasDragging = _dragging;
      _pressedInside = false;
      _dragging = false;
      if (wasDragging) {
        _evaluateDock(); // 释放时判定吸附（可能启动吸附动画）
        if (!_destroying) {
          onMoved?.call(_screenX.toDouble(), _screenY.toDouble());
        }
      } else {
        onClicked?.call();
      }
    }
  }

  // ===========================================================================
  // 贴边收起（迅雷式）
  // ===========================================================================

  /// 拖动释放/外部校正后判定：球边缘距所在屏工作区左/右/上边 ≤ 阈值 → 吸附。
  void _evaluateDock() {
    final wa = _monitorWorkArea();
    if (wa == null) return;
    final (waL, waT, waR, _) = wa;
    final threshold = (_kDockSnapThreshold * scale).round();

    _DockEdge edge = _DockEdge.none;
    if (_screenX - waL <= threshold) {
      edge = _DockEdge.left;
    } else if (waR - (_screenX + _scaledW) <= threshold) {
      edge = _DockEdge.right;
    } else if (_screenY - waT <= threshold) {
      edge = _DockEdge.top;
    }

    _dockEdge = edge;
    _collapsed = false;
    _collapseDeadline = null;
    if (edge != _DockEdge.none) {
      _startAnim(_dockedExpandedPos()); // 钉到边（吸附动画）
      logInfo(_tag, 'docked to $edge');
    }
  }

  /// 展开吸附位（球贴边缘，完整可见）
  (int, int) _dockedExpandedPos() {
    final wa = _monitorWorkArea();
    if (wa == null) return (_screenX, _screenY);
    final (waL, waT, waR, _) = wa;
    return switch (_dockEdge) {
      _DockEdge.left => (waL, _screenY),
      _DockEdge.right => (waR - _scaledW, _screenY),
      _DockEdge.top => (_screenX, waT),
      _DockEdge.none => (_screenX, _screenY),
    };
  }

  /// 收起位（只露 kDockRevealWidth 逻辑 px 的边条）
  (int, int) _dockedCollapsedPos() {
    final wa = _monitorWorkArea();
    if (wa == null) return (_screenX, _screenY);
    final (waL, waT, waR, _) = wa;
    final reveal = (_kDockRevealWidth * scale).round();
    return switch (_dockEdge) {
      _DockEdge.left => (waL - _scaledW + reveal, _screenY),
      _DockEdge.right => (waR - reveal, _screenY),
      _DockEdge.top => (_screenX, waT - _scaledH + reveal),
      _DockEdge.none => (_screenX, _screenY),
    };
  }

  /// 球所在显示器工作区 (left, top, right, bottom)
  (int, int, int, int)? _monitorWorkArea() {
    if (_hwnd == 0) return null;
    final monitor = monitorFromWindow(_hwnd, MONITOR_DEFAULTTONEAREST);
    final mi = calloc<MONITORINFO>();
    try {
      mi.ref.cbSize = sizeOf<MONITORINFO>();
      if (getMonitorInfoW(monitor, mi) == 0) return null;
      final w = mi.ref.rcWork;
      return (w.left, w.top, w.right, w.bottom);
    } finally {
      calloc.free(mi);
    }
  }

  void _startAnim((int, int) target) {
    final (tx, ty) = target;
    if (tx == _screenX && ty == _screenY) return;
    _animStart = DateTime.now();
    _animFromX = _screenX;
    _animFromY = _screenY;
    _animToX = tx;
    _animToY = ty;
    _ensureTimer(_fastTickInterval);
  }

  void _cancelAnim() => _animStart = null;

  /// 推进动画一帧；返回是否仍在动画中。
  bool _stepAnim(DateTime now) {
    final start = _animStart;
    if (start == null) return false;
    final t =
        now.difference(start).inMilliseconds / _kDockAnimMs.toDouble();
    if (t >= 1.0) {
      _animStart = null;
      _setPos(_animToX, _animToY);
      return false;
    }
    // ease-out cubic
    final e = 1 - math.pow(1 - t, 3).toDouble();
    _setPos(
      (_animFromX + (_animToX - _animFromX) * e).round(),
      (_animFromY + (_animToY - _animFromY) * e).round(),
    );
    return true;
  }

  void _setPos(int x, int y) {
    if (x == _screenX && y == _screenY) return;
    _screenX = x;
    _screenY = y;
    setWindowPos(
      _hwnd,
      0,
      x,
      y,
      0,
      0,
      SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
    );
  }

  /// 命中判定：
  /// - 收起态：窗口整矩形（露出的边条很窄，圆形判定会漏接悬停）
  /// - 常态：圆形（A7，圆外穿透）
  bool _hitBall(int screenX, int screenY) {
    if (_collapsed) {
      return screenX >= _screenX &&
          screenX < _screenX + _scaledW &&
          screenY >= _screenY &&
          screenY < _screenY + _scaledH;
    }
    final cx = _screenX + _scaledW / 2;
    final cy = _screenY + _scaledH / 2;
    final r = kBallHitRadius * scale;
    final dx = screenX - cx;
    final dy = screenY - cy;
    return dx * dx + dy * dy <= r * r;
  }

  // ===========================================================================
  // 右键上下文菜单（TrackPopupMenuEx 阻塞式，同 tray_manager 模式）
  // ===========================================================================

  /// 在光标处弹出原生菜单。[items] 为 (id, label)；id==0 表示分隔符。
  ///
  /// 返回选中项 id；0 = 用户取消。阻塞当前 isolate 直到菜单关闭
  /// （与托盘菜单同款交互模型，菜单存续期主窗口渲染暂停，可接受）。
  int showContextMenu(List<(int, String)> items) {
    if (_hwnd == 0) return 0;
    final hMenu = createPopupMenu();
    if (hMenu == 0) return 0;
    final ptrs = <Pointer<Utf16>>[];
    try {
      for (final (id, label) in items) {
        if (id == 0) {
          appendMenuW(hMenu, MF_SEPARATOR, 0, nullptr);
        } else {
          final p = label.toNativeUtf16();
          ptrs.add(p);
          appendMenuW(hMenu, MF_STRING, id, p);
        }
      }
      getCursorPos(_cursorPt);
      // MSDN 怪癖：不先 SetForegroundWindow，点击菜单外不会关闭菜单
      setForegroundWindow(_hwnd);
      return trackPopupMenuEx(
        hMenu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
        _cursorPt.ref.x,
        _cursorPt.ref.y,
        _hwnd,
        nullptr,
      );
    } finally {
      destroyMenu(hMenu);
      for (final p in ptrs) {
        calloc.free(p);
      }
    }
  }

  // ===========================================================================
  // 全屏避让（S1.3）
  // ===========================================================================

  void _checkFullscreenAvoidance() {
    if (_hwnd == 0) return;
    final shouldHide = _isForegroundFullscreenOnBallMonitor();
    if (shouldHide && !_hiddenByFullscreen) {
      _hiddenByFullscreen = true;
      showWindow(_hwnd, SW_HIDE);
      // 中断进行中的拖动（球已不可见）
      _pressedInside = false;
      _dragging = false;
      logInfo(_tag, 'hidden: fullscreen app on ball monitor');
    } else if (!shouldHide && _hiddenByFullscreen) {
      _hiddenByFullscreen = false;
      showWindow(_hwnd, SW_SHOWNOACTIVATE);
      _push();
      logInfo(_tag, 'restored: fullscreen app gone');
    }
  }

  /// Shell 自身的整屏无边框窗口 — 点击桌面/开始菜单/任务视图时会短暂成为
  /// 前台窗口，尺寸覆盖整屏且无 WS_CAPTION，但并非真正的全屏应用。
  static const _shellClassNames = <String>{
    'Progman', // 桌面（点击桌面空白处/图标）
    'WorkerW', // 桌面壁纸宿主（SHELLDLL_DefView 被挪入时）
    'Shell_TrayWnd', // 主任务栏
    'Shell_SecondaryTrayWnd', // 副屏任务栏
    'Windows.UI.Core.CoreWindow', // 开始菜单/搜索（Win10）
    'XamlExplorerHostIslandWindow', // 任务视图/Alt-Tab（Win11）
    'MultitaskingViewFrame', // 任务视图（Win10）
    'ForegroundStaging', // Alt-Tab 过渡占位窗口
    'TaskListThumbnailWnd', // 任务栏缩略图预览
  };

  /// 前台窗口是否在球所在显示器上独占全屏。
  ///
  /// 判定条件（rev3 R1/F1/F2 修订 + Shell/cloak 排除）：
  /// 1. 前台窗口非本进程；
  /// 2. 类名不属于 Shell 整屏窗口（点桌面时 Progman/WorkerW 会成为前台）；
  /// 3. 未被 DWM cloak（虚拟桌面切换/UWP 过渡态窗口不可见但可为前台）；
  /// 4. 与球在同一 HMONITOR；
  /// 5. 窗口矩形 = 该屏全分辨率（rcMonitor 非 rcWork）；
  /// 6. 窗口 style 缺 WS_CAPTION|WS_THICKFRAME（排除普通最大化窗口）。
  bool _isForegroundFullscreenOnBallMonitor() {
    final fg = getForegroundWindow();
    if (fg == 0 || fg == _hwnd) return false;

    // 条件 1：非本进程
    final pidPtr = calloc<Uint32>();
    try {
      getWindowThreadProcessId(fg, pidPtr);
      if (pidPtr.value == getCurrentProcessId()) return false;
    } finally {
      calloc.free(pidPtr);
    }

    // 条件 2：排除 Shell 整屏窗口（点击桌面 → Progman/WorkerW 成为前台，
    // 整屏 + 无边框，四条件全中 → 球被误隐藏）
    final clsBuf = calloc<Uint16>(64);
    try {
      final n = getClassNameW(fg, clsBuf.cast(), 64);
      if (n > 0) {
        final cls = clsBuf.cast<Utf16>().toDartString(length: n);
        if (_shellClassNames.contains(cls)) return false;
      }
    } finally {
      calloc.free(clsBuf);
    }

    // 条件 3：排除 DWM cloaked 窗口（其他虚拟桌面/UWP 挂起窗口，
    // 不可见但可短暂持有前台）
    final cloaked = calloc<Uint32>();
    try {
      if (dwmGetWindowAttribute(fg, DWMWA_CLOAKED, cloaked.cast(), 4) == 0 &&
          cloaked.value != 0) {
        return false;
      }
    } finally {
      calloc.free(cloaked);
    }

    // 条件 4：同一显示器
    final fgMonitor = monitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
    final ballMonitor = monitorFromWindow(_hwnd, MONITOR_DEFAULTTONEAREST);
    if (fgMonitor != ballMonitor) return false;

    // 条件 5：尺寸 = 全屏分辨率
    final rect = calloc<RECT>();
    final mi = calloc<MONITORINFO>();
    try {
      if (getWindowRect(fg, rect) == 0) return false;
      mi.ref.cbSize = sizeOf<MONITORINFO>();
      if (getMonitorInfoW(fgMonitor, mi) == 0) return false;
      final m = mi.ref.rcMonitor;
      final r = rect.ref;
      final coversMonitor =
          r.left <= m.left &&
          r.top <= m.top &&
          r.right >= m.right &&
          r.bottom >= m.bottom;
      if (!coversMonitor) return false;
    } finally {
      calloc.free(rect);
      calloc.free(mi);
    }

    // 条件 6：无边框（缺 CAPTION|THICKFRAME）
    final style = getWindowLongPtrW(fg, GWL_STYLE);
    if ((style & WS_CAPTION) == WS_CAPTION ||
        (style & WS_THICKFRAME) == WS_THICKFRAME) {
      return false;
    }
    return true;
  }

  // ===========================================================================
  // DPI 检测（S1.3）
  // ===========================================================================

  void _checkDpiChange() {
    if (_hwnd == 0) return;
    final now = getDpiForWindow(_hwnd);
    if (now != _dpi && now > 0) {
      logInfo(_tag, 'dpi changed $_dpi -> $now');
      _dpi = now;
      onDpiChanged?.call(scale);
    }
  }

  // ===========================================================================
  // 坐标校验（A7 — 启动/唤醒时调用）
  // ===========================================================================

  /// 校验坐标是否落在球当前所在显示器工作区内；越界返回吸附后的坐标。
  ///
  /// 静态方法 — 创建窗口前用主工作区校验。
  static (int, int) clampToWorkArea(int x, int y) {
    final workArea = calloc<RECT>();
    try {
      systemParametersInfoW(SPI_GETWORKAREA, 0, workArea.cast(), 0);
      final wa = workArea.ref;
      final size = kBallWindowSize.round();
      // 粗校验（-500~20000 范围外视为垃圾值）→ 默认停靠
      if (x < -500 || x > 20000 || y < -500 || y > 20000) {
        return _defaultDock(wa);
      }
      final cx = math.max(wa.left, math.min(x, wa.right - size));
      final cy = math.max(wa.top, math.min(y, wa.bottom - size));
      return (cx, cy);
    } finally {
      calloc.free(workArea);
    }
  }

  /// 默认停靠：主工作区右侧边缘、垂直 40% 高度、贴边 8px（A7）。
  static (int, int) _defaultDock(RECT wa) {
    final size = kBallWindowSize.round();
    final x = wa.right - size - 8;
    final y = wa.top + ((wa.bottom - wa.top) * 0.4).round();
    return (x, y);
  }

  /// 计算默认停靠坐标（首次启用，哨兵 -1 时）。
  static (int, int) defaultDockPosition() {
    final workArea = calloc<RECT>();
    try {
      systemParametersInfoW(SPI_GETWORKAREA, 0, workArea.cast(), 0);
      return _defaultDock(workArea.ref);
    } finally {
      calloc.free(workArea);
    }
  }
}
