#include "popup_window_host.h"

#include <dwmapi.h>
#include <flutter_windows.h>
#include <shlobj.h>
#include <uxtheme.h>
#include <shobjidl.h>
#include <windows.h>

#include <algorithm>
#include <iostream>
#include <vector>

namespace {

// Win11 圆角属性 — 兼容旧 SDK 头文件
#ifndef DWMWA_WINDOW_CORNER_PREFERENCE
#define DWMWA_WINDOW_CORNER_PREFERENCE 33
#endif
constexpr UINT kDwmCornerRound = 2;  // DWMWCP_ROUND

// 窗口逻辑尺寸（96 DPI 基准，契约约定：宽固定、高由 Dart resize 驱动）
constexpr int kLogicalWidth = 520;
constexpr int kLogicalDefaultHeight = 600;

// reveal 兜底定时器：show 后弹窗 Dart 迟迟不发 reveal（引擎冷启动异常/
// 卡死）时按当前尺寸强制显示，保证窗口永远弹得出来。
constexpr UINT_PTR kRevealFallbackTimerId = 0x464C5250;  // 'FLRP'
constexpr UINT kRevealFallbackTimeoutMs = 3000;

// 窗口标题 — 仅用于调试识别（无边框窗口不显示标题栏）。
// 注意不得等于 L"FluxDown"：main.cpp 单实例逻辑用
// FindWindow(class, L"FluxDown") 定位主窗口，两者共享窗口类。
constexpr const wchar_t kPopupTitle[] = L"FluxDown Quick Download";

std::wstring Utf8ToWide(const std::string& utf8) {
  if (utf8.empty()) {
    return std::wstring();
  }
  const int len = ::MultiByteToWideChar(CP_UTF8, 0, utf8.data(),
                                        static_cast<int>(utf8.size()),
                                        nullptr, 0);
  std::wstring wide(len, L'\0');
  ::MultiByteToWideChar(CP_UTF8, 0, utf8.data(),
                        static_cast<int>(utf8.size()), wide.data(), len);
  return wide;
}

std::string WideToUtf8(const std::wstring& wide) {
  if (wide.empty()) {
    return std::string();
  }
  const int len = ::WideCharToMultiByte(CP_UTF8, 0, wide.data(),
                                        static_cast<int>(wide.size()),
                                        nullptr, 0, nullptr, nullptr);
  std::string utf8(len, '\0');
  ::WideCharToMultiByte(CP_UTF8, 0, wide.data(),
                        static_cast<int>(wide.size()), utf8.data(), len,
                        nullptr, nullptr);
  return utf8;
}

// 从 EncodableMap 取字符串参数（缺失/类型不符返回空串）
std::string GetStringArg(const flutter::EncodableValue* args,
                         const char* key) {
  const auto* map = std::get_if<flutter::EncodableMap>(args);
  if (!map) {
    return std::string();
  }
  auto it = map->find(flutter::EncodableValue(key));
  if (it == map->end()) {
    return std::string();
  }
  const auto* str = std::get_if<std::string>(&it->second);
  return str ? *str : std::string();
}

// 强制将 |hwnd| 激活到前台（浏览器在前台时 SetForegroundWindow 会被
// 系统拒绝）。经典 AttachThreadInput 技巧，与 Dart 侧主窗口激活同款。
void ForceActivate(HWND hwnd) {
  const HWND foreground = ::GetForegroundWindow();
  const DWORD current_thread = ::GetCurrentThreadId();
  DWORD foreground_thread = 0;
  if (foreground) {
    foreground_thread = ::GetWindowThreadProcessId(foreground, nullptr);
  }
  const bool attached =
      foreground_thread != 0 && foreground_thread != current_thread &&
      ::AttachThreadInput(current_thread, foreground_thread, TRUE);
  ::SetForegroundWindow(hwnd);
  ::SetFocus(hwnd);
  if (attached) {
    ::AttachThreadInput(current_thread, foreground_thread, FALSE);
  }
}

}  // namespace

PopupWindowHost::PopupWindowHost(flutter::BinaryMessenger* host_messenger) {
  host_channel_ =
      std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
          host_messenger, "fluxdown/popup_host",
          &flutter::StandardMethodCodec::GetInstance());
  host_channel_->SetMethodCallHandler(
      [this](const flutter::MethodCall<flutter::EncodableValue>& call,
             std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>>
                 result) {
        if (call.method_name() == "show") {
          const auto* payload = std::get_if<std::string>(call.arguments());
          if (!payload) {
            result->Error("bad-args", "show expects a JSON string payload");
            return;
          }
          HandleHostShow(*payload, std::move(result));
        } else if (call.method_name() == "append") {
          // 小窗可见期间新到的外部请求 — 转发给弹窗引擎合入当前表单。
          // 窗口实际不可见/引擎未就绪时返回 false，Dart 侧复位失步状态。
          const auto* url_text = std::get_if<std::string>(call.arguments());
          HWND hwnd = GetHandle();
          const bool can_append = url_text && hwnd &&
                                  ::IsWindowVisible(hwnd) && child_ready_ &&
                                  child_channel_ != nullptr;
          if (can_append) {
            child_channel_->InvokeMethod(
                "appendPayload",
                std::make_unique<flutter::EncodableValue>(*url_text));
          }
          result->Success(flutter::EncodableValue(can_append));
        } else if (call.method_name() == "close") {
          // 契约：close 只隐藏，不回调 onClosed
          HidePopup();
          result->Success();
        } else {
          result->NotImplemented();
        }
      });
}

PopupWindowHost::~PopupWindowHost() {}

void PopupWindowHost::HandleHostShow(
    const std::string& payload,
    std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>> result) {
  if (!EnsureWindow()) {
    // 创建失败 — Dart 侧回退到主窗口内对话框
    result->Success(flutter::EncodableValue(false));
    return;
  }

  // 每次 show 重置为默认尺寸并重新居中：避免继承上一条请求 resize
  // 后的高度残留。窗口保持隐藏（复用时先藏起旧表单画面），由弹窗 Dart
  // 在新载荷首帧就绪后经 reveal 一次到位「设高 + 显示」。
  HidePopup();
  ResetPlacement();

  if (child_ready_) {
    DeliverPayload(payload);
  } else {
    // 弹窗引擎尚在启动 — 暂存，待 ready 到达后投递
    pending_payload_ = payload;
  }

  // reveal 兜底：Dart 侧异常时超时强制显示（等价旧版立即显示行为）。
  ArmRevealFallback();
  result->Success(flutter::EncodableValue(true));
}

bool PopupWindowHost::EnsureWindow() {
  if (GetHandle() != nullptr) {
    return true;
  }

  // 与主引擎同一 Dart bundle；--quick-popup 让 main() 分发到弹窗入口。
  // DartProject 相对路径以可执行文件目录为基准（与 main.cpp 一致）。
  project_.emplace(L"data");
  project_->set_dart_entrypoint_arguments({"--quick-popup"});

  child_ready_ = false;
  pending_payload_.reset();

  // Create 内部按目标显示器 DPI 缩放尺寸并回调 OnCreate 启动第二引擎；
  // 位置随后由 ResetPlacement 修正，此处原点无关紧要。
  if (!Create(kPopupTitle,
              Point(100, 100),
              Size(kLogicalWidth, kLogicalDefaultHeight))) {
    // OnCreate 失败时 CreateWindow 可能已成功 — 必须销毁半创建的窗口，
    // 否则下次 EnsureWindow 会把残留句柄误判为"已就绪"，弹出一个
    // 无引擎视图、未应用弹窗样式的空白系统边框窗口。
    OutputDebugStringA("[PopupHost] EnsureWindow: Create failed, destroying stale window\n");
    std::cerr << "[PopupHost] EnsureWindow: Create failed, destroying stale window\n";
    Destroy();
    project_.reset();
    return false;
  }

  ApplyPopupStyles();
  return true;
}

bool PopupWindowHost::OnCreate() {
  if (!Win32Window::OnCreate()) {
    return false;
  }
  if (!project_.has_value()) {
    return false;
  }

  RECT frame = GetClientArea();
  flutter_controller_ = std::make_unique<flutter::FlutterViewController>(
      frame.right - frame.left, frame.bottom - frame.top, *project_);
  if (!flutter_controller_->engine() || !flutter_controller_->view()) {
    // 第二引擎/视图创建失败 — 打诊断日志便于 flutter run 控制台定位
    OutputDebugStringA("[PopupHost] OnCreate: popup engine/view creation failed\n");
    std::cerr << "[PopupHost] OnCreate: popup engine/view creation failed\n";
    flutter_controller_ = nullptr;
    return false;
  }

  // 契约：弹窗引擎零插件注册 — 故意不调用 RegisterPlugins。
  // 表单所需的目录选择/拖动/关闭能力全部由本宿主经通道提供。

  child_channel_ =
      std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
          flutter_controller_->engine()->messenger(), "fluxdown/popup_child",
          &flutter::StandardMethodCodec::GetInstance());
  child_channel_->SetMethodCallHandler(
      [this](const flutter::MethodCall<flutter::EncodableValue>& call,
             std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>>
                 result) { HandleChildCall(call, std::move(result)); });

  SetChildContent(flutter_controller_->view()->GetNativeWindow());
  flutter_controller_->ForceRedraw();
  return true;
}

void PopupWindowHost::OnDestroy() {
  // 注意：Win32Window::Create() 入口会先调用 Destroy() 做预清理，
  // 因此本函数会在 EnsureWindow 设置 project_ 之后、OnCreate 之前触发 —
  // 此处绝不能重置 project_（否则 OnCreate 拿不到 DartProject 必然失败），
  // 也不动 ready/载荷状态（由 EnsureWindow 统一管理），只释放引擎资源。
  child_channel_ = nullptr;
  flutter_controller_ = nullptr;
  Win32Window::OnDestroy();
}

void PopupWindowHost::HandleChildCall(
    const flutter::MethodCall<flutter::EncodableValue>& call,
    std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>> result) {
  const std::string& method = call.method_name();

  if (method == "ready") {
    // 弹窗 Dart 首帧就绪 — 投递暂存载荷
    child_ready_ = true;
    if (pending_payload_.has_value()) {
      DeliverPayload(*pending_payload_);
      pending_payload_.reset();
    }
    result->Success();
    return;
  }

  if (method == "submit") {
    const auto* json = std::get_if<std::string>(call.arguments());
    if (!json) {
      result->Error("bad-args", "submit expects a JSON string");
      return;
    }
    // 先隐藏（响应更快），再中继结果到主引擎
    HidePopup();
    host_channel_->InvokeMethod(
        "onResult", std::make_unique<flutter::EncodableValue>(*json));
    result->Success();
    return;
  }

  if (method == "cancel") {
    HidePopup();
    NotifyClosed();
    result->Success();
    return;
  }

  if (method == "pickFolder") {
    const std::wstring title =
        Utf8ToWide(GetStringArg(call.arguments(), "title"));
    const std::wstring initial_dir =
        Utf8ToWide(GetStringArg(call.arguments(), "initialDir"));
    auto picked = PickFolder(title, initial_dir);
    if (picked.has_value()) {
      result->Success(flutter::EncodableValue(WideToUtf8(*picked)));
    } else {
      result->Success();  // 用户取消 → null
    }
    return;
  }

  if (method == "startDrag") {
    // 经典无边框拖动技巧：释放鼠标捕获后伪造非客户区标题栏按下
    HWND hwnd = GetHandle();
    if (hwnd) {
      ::ReleaseCapture();
      ::SendMessage(hwnd, WM_NCLBUTTONDOWN, HTCAPTION, 0);
    }
    result->Success();
    return;
  }

  if (method == "resize" || method == "reveal") {
    double logical_height = 0;
    if (const auto* map =
            std::get_if<flutter::EncodableMap>(call.arguments())) {
      auto it = map->find(flutter::EncodableValue("height"));
      if (it != map->end()) {
        if (const auto* d = std::get_if<double>(&it->second)) {
          logical_height = *d;
        } else if (const auto* i = std::get_if<int32_t>(&it->second)) {
          logical_height = static_cast<double>(*i);
        }
      }
    }
    ApplyLogicalHeight(logical_height);
    if (method == "reveal") {
      // reveal 握手：新载荷首帧已就绪 — 设高完成后显示并激活。
      // ShowPopup 内部解除兜底定时器；窗口已可见时等价一次 resize。
      ShowPopup();
    }
    result->Success();
    return;
  }

  result->NotImplemented();
}

void PopupWindowHost::DeliverPayload(const std::string& payload) {
  if (child_channel_) {
    child_channel_->InvokeMethod(
        "setPayload", std::make_unique<flutter::EncodableValue>(payload));
  }
}

void PopupWindowHost::ApplyLogicalHeight(double logical_height) {
  HWND hwnd = GetHandle();
  if (!hwnd || logical_height <= 0) {
    return;
  }
  const UINT dpi = ::GetDpiForWindow(hwnd);
  const double scale = dpi / 96.0;
  int physical_height = static_cast<int>(logical_height * scale + 0.5);

  // clamp 到窗口所在显示器工作区高度的 90%
  HMONITOR monitor = ::MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
  MONITORINFO mi = {};
  mi.cbSize = sizeof(mi);
  if (::GetMonitorInfo(monitor, &mi)) {
    const int max_height =
        static_cast<int>((mi.rcWork.bottom - mi.rcWork.top) * 0.9);
    physical_height = (std::min)(physical_height, max_height);
  }

  RECT rect = {};
  ::GetWindowRect(hwnd, &rect);
  // 顶边不动，宽度固定
  ::SetWindowPos(hwnd, nullptr, 0, 0, rect.right - rect.left, physical_height,
                 SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
}

void PopupWindowHost::ArmRevealFallback() {
  HWND hwnd = GetHandle();
  if (hwnd) {
    // 同 id 重复 SetTimer = 重置超时，语义正好匹配连续 show。
    ::SetTimer(hwnd, kRevealFallbackTimerId, kRevealFallbackTimeoutMs,
               nullptr);
  }
}

void PopupWindowHost::CancelRevealFallback() {
  HWND hwnd = GetHandle();
  if (hwnd) {
    ::KillTimer(hwnd, kRevealFallbackTimerId);
  }
}

void PopupWindowHost::ResetPlacement() {
  HWND hwnd = GetHandle();
  if (!hwnd) {
    return;
  }

  // 居中于光标所在显示器工作区（外部请求通常来自浏览器 —
  // 光标所在屏即用户注意力所在屏），按该显示器 DPI 缩放。
  POINT cursor = {};
  ::GetCursorPos(&cursor);
  HMONITOR monitor = ::MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
  const UINT dpi = FlutterDesktopGetDpiForMonitor(monitor);
  const double scale = dpi / 96.0;
  const int width = static_cast<int>(kLogicalWidth * scale + 0.5);
  const int height = static_cast<int>(kLogicalDefaultHeight * scale + 0.5);

  MONITORINFO mi = {};
  mi.cbSize = sizeof(mi);
  ::GetMonitorInfo(monitor, &mi);
  const int x =
      mi.rcWork.left + ((mi.rcWork.right - mi.rcWork.left) - width) / 2;
  const int y =
      mi.rcWork.top + ((mi.rcWork.bottom - mi.rcWork.top) - height) / 2;

  ::SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE);
}

void PopupWindowHost::ShowPopup() {
  HWND hwnd = GetHandle();
  if (!hwnd) {
    return;
  }
  CancelRevealFallback();
  ::ShowWindow(hwnd, SW_SHOW);
  ForceActivate(hwnd);
}

void PopupWindowHost::HidePopup() {
  HWND hwnd = GetHandle();
  if (!hwnd) {
    return;
  }
  // 主引擎主动 close / 提交隐藏时，pending 的 reveal 兜底一并作废。
  CancelRevealFallback();
  if (::IsWindowVisible(hwnd)) {
    ::ShowWindow(hwnd, SW_HIDE);
  }
}

void PopupWindowHost::NotifyClosed() {
  host_channel_->InvokeMethod("onClosed", nullptr);
}

void PopupWindowHost::ApplyPopupStyles() {
  HWND hwnd = GetHandle();
  if (!hwnd) {
    return;
  }

  // 无边框弹窗：去掉 overlapped 边框/标题栏（Dart 侧自绘窗口 chrome）
  ::SetWindowLong(hwnd, GWL_STYLE, WS_POPUP | WS_CLIPCHILDREN);
  // 不占任务栏 + 置顶（Z-order 置顶在 ResetPlacement 中生效）
  const LONG ex_style = ::GetWindowLong(hwnd, GWL_EXSTYLE);
  ::SetWindowLong(hwnd, GWL_EXSTYLE, ex_style | WS_EX_TOOLWINDOW);
  ::SetWindowPos(hwnd, nullptr, 0, 0, 0, 0,
                 SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE |
                     SWP_FRAMECHANGED);

  // Win11 圆角（旧系统上该属性调用失败即为方角，无需分支）
  const UINT corner = kDwmCornerRound;
  ::DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner,
                          sizeof(corner));

  // 无边框窗口的 DWM 阴影：向客户区内侧扩展 1px 不可见 frame 即可让 DWM
  // 绘制标准窗口阴影（经典 borderless-shadow 技巧，Win10/11 通用）。
  // Flutter 视图不透明且铺满客户区，扩展的 frame 完全被内容盖住，
  // 视觉上只多出阴影本身。
  const MARGINS shadow_margins = {1, 1, 1, 1};
  ::DwmExtendFrameIntoClientArea(hwnd, &shadow_margins);
}

std::optional<std::wstring> PopupWindowHost::PickFolder(
    const std::wstring& title, const std::wstring& initial_dir) {
  // wWinMain 已 OleInitialize（STA）；模态对话框自带消息泵，
  // 在平台线程同步执行与 file_selector_windows 插件行为一致。
  IFileOpenDialog* dialog = nullptr;
  HRESULT hr = ::CoCreateInstance(CLSID_FileOpenDialog, nullptr, CLSCTX_ALL,
                                  IID_PPV_ARGS(&dialog));
  if (FAILED(hr) || !dialog) {
    return std::nullopt;
  }

  std::optional<std::wstring> picked;
  FILEOPENDIALOGOPTIONS options = 0;
  if (SUCCEEDED(dialog->GetOptions(&options))) {
    dialog->SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
  }
  if (!title.empty()) {
    dialog->SetTitle(title.c_str());
  }
  // 初始目录不存在时跳过 SetFolder（IFileDialog 对无效路径会报错）
  if (!initial_dir.empty()) {
    const DWORD attrs = ::GetFileAttributesW(initial_dir.c_str());
    if (attrs != INVALID_FILE_ATTRIBUTES &&
        (attrs & FILE_ATTRIBUTE_DIRECTORY)) {
      IShellItem* folder = nullptr;
      if (SUCCEEDED(::SHCreateItemFromParsingName(
              initial_dir.c_str(), nullptr, IID_PPV_ARGS(&folder)))) {
        dialog->SetFolder(folder);
        folder->Release();
      }
    }
  }

  if (SUCCEEDED(dialog->Show(GetHandle()))) {
    IShellItem* item = nullptr;
    if (SUCCEEDED(dialog->GetResult(&item)) && item) {
      PWSTR path = nullptr;
      if (SUCCEEDED(item->GetDisplayName(SIGDN_FILESYSPATH, &path)) && path) {
        picked = std::wstring(path);
        ::CoTaskMemFree(path);
      }
      item->Release();
    }
  }
  dialog->Release();
  return picked;
}

LRESULT PopupWindowHost::MessageHandler(HWND hwnd, UINT const message,
                                        WPARAM const wparam,
                                        LPARAM const lparam) noexcept {
  // 系统级关闭（Alt+F4 等）→ 语义 = cancel：隐藏 + 中继 onClosed，
  // 绝不销毁窗口/引擎（常驻复用约束）。
  if (message == WM_CLOSE) {
    HidePopup();
    NotifyClosed();
    return 0;
  }

  // reveal 兜底超时：弹窗 Dart 未按时发来 reveal — 按当前尺寸强制显示，
  // 保证窗口永远弹得出来（等价旧版 show 立即显示的行为）。
  if (message == WM_TIMER && wparam == kRevealFallbackTimerId) {
    ShowPopup();  // 内部先 KillTimer，杜绝重复触发
    return 0;
  }

  // SW_HIDE 只发 WM_SHOWWINDOW 不发 WM_SIZE(SIZE_MINIMIZED)，
  // 需伪造 WM_SIZE 让弹窗引擎在隐藏期间暂停 vsync（与 FlutterWindow
  // 同款处理 — 弹窗绝大部分时间处于隐藏状态，不处理会白烧 CPU）。
  if (message == WM_SHOWWINDOW && lparam == 0 && flutter_controller_) {
    if (wparam == FALSE) {
      window_hidden_ = true;
      ::PostMessage(hwnd, WM_SIZE, SIZE_MINIMIZED, 0);
    } else if (wparam == TRUE && window_hidden_) {
      window_hidden_ = false;
      RECT rect = GetClientArea();
      ::PostMessage(hwnd, WM_SIZE, SIZE_RESTORED,
                    MAKELPARAM(rect.right - rect.left,
                               rect.bottom - rect.top));
    }
  }

  // 转发给弹窗引擎（DPI 变更、键盘布局等嵌入器级消息处理）
  if (flutter_controller_) {
    std::optional<LRESULT> result =
        flutter_controller_->HandleTopLevelWindowProc(hwnd, message, wparam,
                                                      lparam);
    if (result) {
      return *result;
    }
  }

  switch (message) {
    case WM_FONTCHANGE:
      if (flutter_controller_) {
        flutter_controller_->engine()->ReloadSystemFonts();
      }
      break;
  }

  return Win32Window::MessageHandler(hwnd, message, wparam, lparam);
}
