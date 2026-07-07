import 'dart:async';
import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';
import '../../i18n/locale_provider.dart';
import '../../models/download_task.dart';
import '../../theme/flux_theme_tokens.dart';
import '../log_service.dart';
import '../native_overlay/win32_layered_window.dart';
import '../open_folder.dart';
import 'toast_card_renderer.dart';
import 'win32_bindings.dart';

const _tag = 'Win32Toast';

// =============================================================================
// 生命周期阶段
// =============================================================================

enum _Phase { fadeIn, autoClose, fadeOut }

// =============================================================================
// 窗口状态（纯 Dart，无 Win32 消息参与）
// =============================================================================

class _ToastState {
  final String filePath;
  final void Function() onOpenFile;
  final void Function() onOpenFolder;
  final void Function() onDismissed;

  _Phase phase = _Phase.fadeIn;
  int alpha = 0;
  bool isHovered = false;
  bool prevMouseDown = false; // 上一个 tick 时鼠标是否按下
  int hoveredButton = 0; // 0=无 1=关闭 2=打开文件夹 3=打开文件

  // autoClose 阶段开始时间
  DateTime? autoCloseStart;

  // ── 分层窗口位图资源（窗口销毁时释放）──
  // 4 张 hover 变体 DIB（索引与 hoveredButton 对应）
  final List<int> hBitmaps = [];
  int memDC = 0;

  /// 当前贴入窗口的变体索引（-1 = 尚未贴过）
  int currentVariant = -1;

  // 物理尺寸（DPI 缩放后）
  int scaledW = 0;
  int scaledH = 0;

  // 屏幕位置（UpdateLayeredWindow 需要显式传递）
  int screenX = 0;
  int screenY = 0;

  // 命中测试区域（物理像素，客户端坐标）
  int closeX1 = 0, closeY1 = 0, closeX2 = 0, closeY2 = 0;
  int folderX1 = 0, folderY1 = 0, folderX2 = 0, folderY2 = 0;
  int fileX1 = 0, fileY1 = 0, fileX2 = 0, fileY2 = 0;

  _ToastState({
    required this.filePath,
    required this.onOpenFile,
    required this.onOpenFolder,
    required this.onDismissed,
  });
}

// =============================================================================
// 全局状态
// =============================================================================

final Map<int, _ToastState> _states = {}; // hwnd → state
const String _className = 'FluxDownToast_v3';

// 复用的 POINT 指针：避免每 tick calloc/free（16ms × 整个 Toast 生命周期）
final _sharedCursorPt = calloc<POINT>();

// =============================================================================
// 批次数据
// =============================================================================

/// 一个通知批次 — 包含代表任务（显示用）和批次总数
class _ToastBatch {
  final DownloadTask representative;
  final int count;

  _ToastBatch(this.representative, this.count);
}

// =============================================================================
// Win32ToastWindow — 公开 API
// =============================================================================

/// Win32 悬浮通知窗口 — 主显示器工作区右下角。
///
/// ## 设计原则
///
/// 为避免 "Cannot invoke native callback outside an isolate" 崩溃：
/// - WndProc 直接使用 `DefWindowProcW` 原生函数指针（纯 Win32，无 Dart 回调）
/// - 所有状态机逻辑（淡入/倒计时/淡出）由 Dart `Timer.periodic` 驱动
/// - 鼠标输入通过 `GetCursorPos` + `GetAsyncKeyState` 轮询实现
///
/// ## 渲染
///
/// 卡片由 Flutter 主引擎**离屏光栅化**（[renderToastCardVariants]）：
/// 与 App 主窗口共享同一套主题 token / MiSans 字体 / 渲染管线，
/// UI 观感与应用内完全一致。位图经 `UpdateLayeredWindow`
/// （per-pixel alpha）整图贴入分层窗口 — 真 alpha 圆角+阴影，
/// 无需 SetWindowRgn 硬裁剪。
///
/// 4 张 hover 变体在显示前一次性渲染完成，Toast 生命周期内
/// tick 只做位图切换与整窗 alpha 渐变，无异步渲染竞态。
class Win32ToastWindow {
  Win32ToastWindow._();
  static final instance = Win32ToastWindow._();

  /// 当前主题 token（由 NotificationService 在冲刷前注入）
  FluxThemeTokens? themeTokens;

  final List<_ToastBatch> _queue = [];
  bool _showing = false;
  Timer? _masterTimer;

  /// 是否有正在显示或等待显示的 Toast（退出前等待用）。
  bool get hasActive => _showing || _queue.isNotEmpty;

  /// 将一批下载任务加入显示队列（800ms 防抖后由 NotificationService 调用）。
  ///
  /// 批次中的所有任务对应一个 Toast：
  /// - count=1 → 显示文件名 + "下载完成"
  /// - count>1 → 显示代表文件名 + "N个文件已下载"
  ///
  /// 已有批次在排队时，新批次直接并入队尾批次（更新代表任务与计数），
  /// 避免高频完成时连续弹出多张卡片。
  void enqueueBatch(List<DownloadTask> tasks) {
    if (!Platform.isWindows || tasks.isEmpty) return;
    if (_queue.isNotEmpty) {
      final tail = _queue.removeLast();
      _queue.add(_ToastBatch(tasks.last, tail.count + tasks.length));
      logInfo(
        _tag,
        'enqueueBatch: merged ${tasks.length} into tail batch, '
        'queueSize=${_queue.length}',
      );
      _tryShowNext();
      return;
    }
    final batch = _ToastBatch(tasks.last, tasks.length);
    _queue.add(batch);
    logInfo(
      _tag,
      'enqueueBatch: count=${tasks.length}, '
      'representative=${tasks.last.fileName}, queueSize=${_queue.length}',
    );
    _tryShowNext();
  }

  /// 销毁所有 Toast（应用退出时调用）
  void destroyAll() {
    _masterTimer?.cancel();
    _masterTimer = null;
    final hwnds = List<int>.from(_states.keys);
    for (final hwnd in hwnds) {
      _releaseWindowResources(hwnd);
      try {
        destroyWindow(hwnd);
      } catch (e) {
        logError(_tag, 'destroyAll: destroyWindow($hwnd) failed', e);
      }
    }
    _states.clear();
    _queue.clear();
    _showing = false;
    logInfo(_tag, 'destroyAll: done');
  }

  void _tryShowNext() {
    if (_showing || _queue.isEmpty) return;
    final batch = _queue.removeAt(0);
    _showing = true;
    _showBatch(batch);
  }

  Future<void> _showBatch(_ToastBatch batch) async {
    try {
      ensureLayeredWindowClass(_className);
      await _createToastWindow(batch);
      _ensureMasterTimer();
    } catch (e, stack) {
      logError(_tag, 'showBatch failed', e, stack);
      _showing = false;
      Future.delayed(const Duration(milliseconds: 500), _tryShowNext);
    }
  }

  void _ensureMasterTimer() {
    if (_masterTimer?.isActive == true) return;
    _masterTimer = Timer.periodic(
      const Duration(milliseconds: 16),
      _onMasterTick,
    );
  }

  void _onMasterTick(Timer _) {
    if (_states.isEmpty) {
      _masterTimer?.cancel();
      _masterTimer = null;
      return;
    }
    final entries = Map<int, _ToastState>.from(_states);
    for (final MapEntry(:key, :value) in entries.entries) {
      _processWindowTick(key, value);
    }
  }

  void _onToastDismissed() {
    _showing = false;
    Future.delayed(const Duration(milliseconds: 400), _tryShowNext);
  }
}

// =============================================================================
// 创建窗口 — 离屏渲染 Flutter 卡片 → DIB → UpdateLayeredWindow
// =============================================================================

Future<void> _createToastWindow(_ToastBatch batch) async {
  final task = batch.representative;

  // ── 1. 组装渲染 spec（主题 token 来自 NotificationService 注入）──────────
  final s = currentS;
  final tokens =
      Win32ToastWindow.instance.themeTokens ?? FluxThemeTokens.defaultDark();
  final spec = ToastCardSpec(
    title: batch.count > 1
        ? s.batchDownloadCompleted(batch.count)
        : s.downloadCompleted,
    fileName: task.fileName,
    fileExt: task.fileExtension,
    subtitle: batch.count > 1
        ? s.andMoreFiles(batch.count - 1)
        : task.sizeText,
    openFolderLabel: s.openFileFolder,
    openFileLabel: s.openFile,
    tokens: tokens,
  );

  // ── 2. 主屏工作区 + DPI → 计算物理尺寸与位置 ────────────────────────────
  final workArea = calloc<RECT>();
  final int waRight;
  final int waBottom;
  try {
    systemParametersInfoW(SPI_GETWORKAREA, 0, workArea.cast(), 0);
    waRight = workArea.ref.right;
    waBottom = workArea.ref.bottom;
  } finally {
    calloc.free(workArea);
  }

  // ── 3. 创建隐藏分层窗口（先建窗才能查该窗口的 DPI）──────────────────────
  final classNamePtr = _className.toNativeUtf16();
  final titlePtr = ''.toNativeUtf16();
  final int hwnd;
  try {
    final exStyle =
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED;
    hwnd = createWindowExW(
      exStyle,
      classNamePtr,
      titlePtr,
      WS_POPUP,
      waRight - kToastWindowW.round(),
      waBottom - kToastWindowH.round(),
      kToastWindowW.round(),
      kToastWindowH.round(),
      0,
      0,
      getModuleHandleW(nullptr),
      nullptr,
    );
  } finally {
    calloc.free(classNamePtr);
    calloc.free(titlePtr);
  }
  if (hwnd == 0) throw StateError('CreateWindowExW returned 0');

  try {
    // ── 4. DPI 感知 → 离屏渲染 4 张 hover 变体 ────────────────────────────
    final dpi = getDpiForWindow(hwnd);
    final scale = dpi / 96.0;
    final images = await renderToastCardVariants(spec, scale: scale);

    final scaledW = images.first.width;
    final scaledH = images.first.height;
    // 阴影出血区已含在窗口内，卡片距工作区边缘 = shadowPad 的富余，
    // 窗口本体直接贴右下角再留 4px 呼吸位。
    final margin = (4 * scale).round();
    final x = waRight - scaledW - margin;
    final y = waBottom - scaledH - margin;

    // ── 5. 构建状态 + DIB 资源 ────────────────────────────────────────────
    final filePath = task.filePath;
    final state = _ToastState(
      filePath: filePath,
      onOpenFile: () => openFile(filePath),
      onOpenFolder: () => openFolder(filePath),
      onDismissed: Win32ToastWindow.instance._onToastDismissed,
    );
    state.scaledW = scaledW;
    state.scaledH = scaledH;
    state.screenX = x;
    state.screenY = y;
    _calcHitAreas(state, scale);

    state.memDC = createCompatibleDC(0);
    if (state.memDC == 0) throw StateError('CreateCompatibleDC failed');
    for (final img in images) {
      state.hBitmaps.add(
        createDibFromBgra(img.width, img.height, img.bgraPremultiplied),
      );
    }

    _states[hwnd] = state;

    // ── 6. 贴入 base 变体（alpha=0 不可见），Timer 驱动淡入 ───────────────
    _pushVariant(hwnd, state, 0);
    showWindow(hwnd, SW_SHOWNOACTIVATE);

    logInfo(
      _tag,
      'toast created hwnd=$hwnd, dpi=$dpi, '
      'scale=$scale, size=${scaledW}x$scaledH',
    );
  } catch (e) {
    // 渲染/资源失败 → 回收半成品窗口
    final state = _states.remove(hwnd);
    if (state != null) _freeStateResources(state);
    try {
      destroyWindow(hwnd);
    } catch (_) {}
    rethrow;
  }
}

// （DIB 创建已抽取至 native_overlay/win32_layered_window.dart）

/// 命中区域：渲染器逻辑坐标 × DPI scale → 物理像素
void _calcHitAreas(_ToastState state, double scale) {
  state.closeX1 = (kToastHitClose.left * scale).round();
  state.closeY1 = (kToastHitClose.top * scale).round();
  state.closeX2 = (kToastHitClose.right * scale).round();
  state.closeY2 = (kToastHitClose.bottom * scale).round();

  state.folderX1 = (kToastHitFolder.left * scale).round();
  state.folderY1 = (kToastHitFolder.top * scale).round();
  state.folderX2 = (kToastHitFolder.right * scale).round();
  state.folderY2 = (kToastHitFolder.bottom * scale).round();

  state.fileX1 = (kToastHitFile.left * scale).round();
  state.fileY1 = (kToastHitFile.top * scale).round();
  state.fileX2 = (kToastHitFile.right * scale).round();
  state.fileY2 = (kToastHitFile.bottom * scale).round();
}

// =============================================================================
// UpdateLayeredWindow 贴图 / 整窗 alpha
// =============================================================================

/// 把变体位图贴入分层窗口（同时应用当前整窗 alpha）。
void _pushVariant(int hwnd, _ToastState state, int variant) {
  if (variant < 0 || variant >= state.hBitmaps.length) return;
  pushLayeredBitmap(
    hwnd: hwnd,
    memDC: state.memDC,
    hBitmap: state.hBitmaps[variant],
    screenX: state.screenX,
    screenY: state.screenY,
    width: state.scaledW,
    height: state.scaledH,
    alpha: state.alpha,
  );
  state.currentVariant = variant;
}

// =============================================================================
// 主 Tick — 在 Dart Timer 回调中执行（isolate 已激活）
// =============================================================================

void _processWindowTick(int hwnd, _ToastState state) {
  // ── 1. 更新悬停状态（复用 _sharedCursorPt，避免每 tick malloc/free）──────
  getCursorPos(_sharedCursorPt);
  screenToClient(hwnd, _sharedCursorPt);
  final cx = _sharedCursorPt.ref.x;
  final cy = _sharedCursorPt.ref.y;

  state.isHovered =
      cx >= 0 && cy >= 0 && cx < state.scaledW && cy < state.scaledH;

  if (state.isHovered) {
    state.hoveredButton = _hitTest(state, cx, cy);
  } else {
    state.hoveredButton = 0;
  }

  // ── 2. 检测点击（检测下降沿：上次未按 → 本次按下）──────────────────────
  final vkState = getAsyncKeyState(VK_LBUTTON);
  final isCurrentlyDown = (vkState & 0x8000) != 0;
  final wasJustPressed = !state.prevMouseDown && isCurrentlyDown;
  state.prevMouseDown = isCurrentlyDown;

  if (wasJustPressed && state.isHovered && state.phase != _Phase.fadeOut) {
    _handleClick(hwnd, state, cx, cy);
    return; // handleClick 可能已移除 state，直接 return
  }

  // ── 3. 阶段状态机 ──────────────────────────────────────────────────────
  var alphaChanged = false;
  switch (state.phase) {
    case _Phase.fadeIn:
      state.alpha = (state.alpha + 20).clamp(0, 255);
      alphaChanged = true;
      if (state.alpha >= 255) {
        state.phase = _Phase.autoClose;
        state.autoCloseStart = DateTime.now();
      }

    case _Phase.autoClose:
      if (state.isHovered) {
        // 悬停时重置倒计时
        state.autoCloseStart = DateTime.now();
      } else {
        final elapsed = DateTime.now().difference(
          state.autoCloseStart ?? DateTime.now(),
        );
        if (elapsed.inSeconds >= 8) {
          state.phase = _Phase.fadeOut;
        }
      }

    case _Phase.fadeOut:
      state.alpha = (state.alpha - 20).clamp(0, 255);
      alphaChanged = true;
      if (state.alpha <= 0) {
        _destroyToast(hwnd, state);
        return;
      }
  }

  // ── 4. 变体或 alpha 变化 → 重贴（UpdateLayeredWindow 同时携带两者）──────
  if (state.hoveredButton != state.currentVariant || alphaChanged) {
    _pushVariant(hwnd, state, state.hoveredButton);
  }
}

int _hitTest(_ToastState state, int x, int y) {
  if (x >= state.closeX1 &&
      x < state.closeX2 &&
      y >= state.closeY1 &&
      y < state.closeY2) {
    return 1;
  }
  if (x >= state.folderX1 &&
      x < state.folderX2 &&
      y >= state.folderY1 &&
      y < state.folderY2) {
    return 2;
  }
  if (x >= state.fileX1 &&
      x < state.fileX2 &&
      y >= state.fileY1 &&
      y < state.fileY2) {
    return 3;
  }
  return 0;
}

void _handleClick(int hwnd, _ToastState state, int cx, int cy) {
  final btn = _hitTest(state, cx, cy);
  if (btn == 1) {
    // 关闭
    state.phase = _Phase.fadeOut;
  } else if (btn == 2) {
    // 打开文件夹
    scheduleMicrotask(state.onOpenFolder);
    state.phase = _Phase.fadeOut;
  } else if (btn == 3) {
    // 打开文件
    scheduleMicrotask(state.onOpenFile);
    state.phase = _Phase.fadeOut;
  }
}

void _destroyToast(int hwnd, _ToastState state) {
  _states.remove(hwnd);
  _releaseWindowResources(hwnd, state: state);
  try {
    destroyWindow(hwnd);
  } catch (e) {
    logError(_tag, '_destroyToast: destroyWindow($hwnd) failed', e);
  }
  state.onDismissed();
  logInfo(_tag, 'toast destroyed hwnd=$hwnd');
}

void _releaseWindowResources(int hwnd, {_ToastState? state}) {
  final s = state ?? _states[hwnd];
  if (s == null) return;
  _freeStateResources(s);
}

void _freeStateResources(_ToastState s) {
  if (s.memDC != 0) {
    deleteDC(s.memDC);
    s.memDC = 0;
  }
  for (final hBitmap in s.hBitmaps) {
    if (hBitmap != 0) deleteObject(hBitmap);
  }
  s.hBitmaps.clear();
}
