#include "popup_window_host.h"

#include <gtk/gtk.h>
#ifdef GDK_WINDOWING_WAYLAND
#include <gdk/gdkwayland.h>
#endif
#include <math.h>

// =============================================================================
// 规格常量 — 必须与跨端弹窗契约『原生窗口规格』章节保持一致：宽 520 固定、
// 初始高 600，逻辑像素（GTK 逻辑像素即 gtk 坐标，HiDPI 由 GTK scale factor
// 自动处理，这里的数值不需要按显示器缩放手动换算）；resize() 的高度上限为
// 所在显示器工作区高度的 90%。
// =============================================================================

static const gint kPopupLogicalWidth = 520;
static const gint kPopupDefaultHeight = 600;
static const gdouble kResizeWorkareaFraction = 0.9;

// =============================================================================
// 控制器状态
// =============================================================================

struct _PopupWindowHost {
  // fluxdown/popup_host 通道，注册在主引擎 messenger 上，本结构体持有引用。
  FlMethodChannel* host_channel;

  // 懒创建的弹窗顶层窗口；GTK 部件树拥有其生命周期，这里只是弱引用（唯一
  // 的销毁点在 popup_window_host_free，进程退出前）。NULL 表示尚未创建。
  GtkWindow* popup_window;
  // popup_window 下唯一的子部件，承载第二个 Flutter 引擎；随 popup_window
  // 一起被 GTK 销毁，这里同样只是弱引用。
  FlView* popup_view;
  // fluxdown/popup_child 通道，注册在弹窗引擎 messenger 上，本结构体持有
  // 引用；与 popup_window/popup_view 同生共死，NULL 表示尚未创建。
  FlMethodChannel* child_channel;

  // 弹窗 Dart 是否已经调用过 ready()。整个进程生命周期内只会由 FALSE 翻到
  // TRUE 一次——弹窗引擎懒创建后常驻复用，不会重新经历一次 Dart 首帧。
  gboolean popup_ready;
  // ready() 到达前暂存的最新载荷 JSON；投递后立即清空。只在“窗口已创建但
  // 弹窗 Dart 尚未 ready”这段短暂窗口期内可能非 NULL。
  gchar* pending_payload;

  // reveal 兜底定时器 source id（0 = 未武装）。show 后弹窗 Dart 迟迟不发
  // reveal（引擎冷启动异常/卡死）时按当前尺寸强制显示，保证窗口永远弹得
  // 出来。
  guint reveal_timeout_id;
};

// reveal 兜底超时（毫秒）——与 Windows/macOS 宿主保持一致。
static const guint kRevealFallbackTimeoutMs = 3000;

// =============================================================================
// 工作区辅助（居中 / resize 高度 clamp 共用）
// =============================================================================

// 主屏工作区——契约要求 Linux/macOS 居中于主屏（只有 Windows 才是居中于光
// 标所在显示器）。取不到显示器信息（无头环境等）时退化为一个不至于把窗口
// 移出屏幕的合理默认值。
static void get_primary_workarea(GdkRectangle* out) {
  GdkDisplay* display = gdk_display_get_default();
  GdkMonitor* monitor =
      display != nullptr ? gdk_display_get_primary_monitor(display) : nullptr;
  if (monitor == nullptr && display != nullptr &&
      gdk_display_get_n_monitors(display) > 0) {
    monitor = gdk_display_get_monitor(display, 0);
  }
  if (monitor != nullptr) {
    gdk_monitor_get_workarea(monitor, out);
    return;
  }
  out->x = 0;
  out->y = 0;
  out->width = 1280;
  out->height = 720;
}

// resize() 的 clamp 基准用弹窗实际所在的显示器（而不是固定的主屏）——多屏
// 时 90% 上限应该跟着窗口走。窗口尚未 realize（拿不到 GdkWindow）时退化为
// 主屏工作区。
static void get_popup_workarea(PopupWindowHost* self, GdkRectangle* out) {
  if (self->popup_window != nullptr) {
    GdkWindow* gdk_window =
        gtk_widget_get_window(GTK_WIDGET(self->popup_window));
    GdkDisplay* display = gdk_display_get_default();
    if (gdk_window != nullptr && display != nullptr) {
      GdkMonitor* monitor =
          gdk_display_get_monitor_at_window(display, gdk_window);
      if (monitor != nullptr) {
        gdk_monitor_get_workarea(monitor, out);
        return;
      }
    }
  }
  get_primary_workarea(out);
}

// =============================================================================
// pickFolder 的 initialDir 容错：目录不存在时逐级向上找最近的存在目录。
// =============================================================================

static gchar* resolve_existing_dir(const gchar* initial_dir) {
  if (initial_dir == nullptr || initial_dir[0] == '\0') {
    return nullptr;
  }
  gchar* candidate = g_strdup(initial_dir);
  while (candidate != nullptr) {
    if (g_file_test(candidate, G_FILE_TEST_IS_DIR)) {
      return candidate;
    }
    gchar* parent = g_path_get_dirname(candidate);
    gboolean reached_root = g_strcmp0(parent, candidate) == 0;
    g_free(candidate);
    if (reached_root) {
      // 已经到路径根节点仍然不存在（理论上 "/" 总是存在，这里只是防御性
      // 收尾避免死循环）——放弃解析，交给 GTK 用它自己的默认目录。
      g_free(parent);
      return nullptr;
    }
    candidate = parent;
  }
  return nullptr;
}

// =============================================================================
// 载荷投递辅助
// =============================================================================

// 向弹窗引擎投递 setPayload。fl_method_channel_invoke_method 在返回前就已
// 经把参数同步编码进待发消息、不再持有引用，这里新建的 FlValue 不需要比这
// 次调用活得更久。
static void send_set_payload(PopupWindowHost* self, const gchar* payload_json) {
  if (self->child_channel == nullptr) {
    return;
  }
  g_autoptr(FlValue) args = fl_value_new_string(payload_json);
  fl_method_channel_invoke_method(self->child_channel, "setPayload", args,
                                  nullptr, nullptr, nullptr);
}

// =============================================================================
// reveal 兜底定时器 / 统一隐藏辅助
// =============================================================================

// present_popup 定义在下方（依赖 popup_view），此处前置声明供 reveal
// 处理器与兜底回调使用。
static void present_popup(PopupWindowHost* self);

static void cancel_reveal_fallback(PopupWindowHost* self) {
  if (self->reveal_timeout_id != 0) {
    g_source_remove(self->reveal_timeout_id);
    self->reveal_timeout_id = 0;
  }
}

// reveal 超时未到达（弹窗引擎冷启动异常/卡死）——按当前尺寸强制显示，
// 保证窗口永远弹得出来（等价旧版 show 立即显示的行为）。
static gboolean reveal_fallback_cb(gpointer user_data) {
  PopupWindowHost* self = (PopupWindowHost*)user_data;
  self->reveal_timeout_id = 0;
  g_warning("[popup-host] reveal timed out, force presenting popup");
  present_popup(self);
  return G_SOURCE_REMOVE;
}

// 统一隐藏入口：隐藏窗口并作废 pending 的 reveal 兜底。
static void popup_hide(PopupWindowHost* self) {
  cancel_reveal_fallback(self);
  if (self->popup_window != nullptr) {
    gtk_widget_hide(GTK_WIDGET(self->popup_window));
  }
}

// =============================================================================
// 系统级关闭（窗口管理器发起的 delete-event，例如 Alt+F4）
// =============================================================================

// 必须在 ensure_popup_window() 里赶在 gtk_widget_realize(popup_view) 之前
// 完成连接：FlView 自己的 realize_cb（shell/platform/linux/fl_view.cc）也
// 会在同一个顶层窗口上连接一个 "delete-event" 处理器，无条件调用
// fl_platform_plugin_request_app_exit() 请求退出整个引擎——这是 Flutter 桌
// 面生命周期对常规主窗口的默认行为，但对我们的弹窗完全不适用。GTK 对返回
// gboolean 的信号（"delete-event" 在内）使用
// g_signal_accumulator_true_handled 语义：一旦某个已连接的处理器返回
// TRUE，同一次信号发送里排在它之后的处理器（含 FlView 内部那个）就不会再
// 被调用。只要本处理器先连接、且返回 TRUE，就能保证系统级关闭永远只被解
// 读成 cancel()，绝不会误触发退出整个应用。
static gboolean popup_window_delete_event_cb(GtkWidget* /*widget*/,
                                             GdkEvent* /*event*/,
                                             gpointer user_data) {
  PopupWindowHost* self = (PopupWindowHost*)user_data;
  popup_hide(self);
  fl_method_channel_invoke_method(self->host_channel, "onClosed", nullptr,
                                  nullptr, nullptr, nullptr);
  return TRUE;  // 阻止 GTK 默认处理（销毁窗口）。
}

// =============================================================================
// 弹窗引擎通道 fluxdown/popup_child 处理器
// =============================================================================

static FlMethodResponse* handle_child_ready(PopupWindowHost* self) {
  self->popup_ready = TRUE;
  if (self->pending_payload != nullptr) {
    send_set_payload(self, self->pending_payload);
    g_clear_pointer(&self->pending_payload, g_free);
  }
  return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
}

static FlMethodResponse* handle_child_submit(PopupWindowHost* self,
                                             FlValue* args) {
  if (args == nullptr || fl_value_get_type(args) != FL_VALUE_TYPE_STRING) {
    return FL_METHOD_RESPONSE(fl_method_error_response_new(
        "bad_args", "submit requires a JSON string result", nullptr));
  }
  popup_hide(self);
  // args 是这次 submit 调用的参数、由 FlMethodCall 拥有；
  // fl_method_channel_invoke_method 只在本次调用内同步编码读取、不保留引
  // 用，可以直接透传给 onResult，不必先落地成一份新的字符串副本。
  fl_method_channel_invoke_method(self->host_channel, "onResult", args,
                                  nullptr, nullptr, nullptr);
  return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
}

static FlMethodResponse* handle_child_cancel(PopupWindowHost* self) {
  popup_hide(self);
  fl_method_channel_invoke_method(self->host_channel, "onClosed", nullptr,
                                  nullptr, nullptr, nullptr);
  return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
}

// startDrag：坐标/按钮/时间戳优先取自 gtk_get_current_event()。但实际几乎
// 总是 NULL——method call 由平台通道经 GLib 主循环的消息分发触发，并不处
// 在真实输入事件的调用栈内（Dart 手势识别到发起这次 native 调用之间，至少
// 隔着一次事件循环）——因此下面“查询当前指针位置”的分支才是常态路径，不
// 是纯防御性代码。
static FlMethodResponse* handle_child_start_drag(PopupWindowHost* self) {
  if (self->popup_window == nullptr) {
    return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
  }

  gint button = 1;
  gint root_x = 0, root_y = 0;
  gboolean have_coords = FALSE;

  GdkEvent* event = gtk_get_current_event();
  if (event != nullptr) {
    guint event_button = 0;
    if (gdk_event_get_button(event, &event_button)) {
      button = (gint)event_button;
    }
    gdouble event_root_x = 0.0, event_root_y = 0.0;
    if (gdk_event_get_root_coords(event, &event_root_x, &event_root_y)) {
      root_x = (gint)lround(event_root_x);
      root_y = (gint)lround(event_root_y);
      have_coords = TRUE;
    }
    gdk_event_free(event);
  }

  if (!have_coords) {
    // 已知风险：Wayland 下 xdg_toplevel 的 move 请求要求 serial 对应一个
    // 仍然有效的隐式抓取；这种事后用当前指针位置重建坐标发起的调用，严格
    // 的合成器可能会静默忽略——契约明确要求"容错 NULL"，这里已是能做到的
    // 最佳退化。
    GdkDisplay* display =
        gtk_widget_get_display(GTK_WIDGET(self->popup_window));
    GdkSeat* seat =
        display != nullptr ? gdk_display_get_default_seat(display) : nullptr;
    GdkDevice* pointer = seat != nullptr ? gdk_seat_get_pointer(seat) : nullptr;
    if (pointer != nullptr) {
      GdkScreen* screen = nullptr;
      gdk_device_get_position(pointer, &screen, &root_x, &root_y);
    }
  }

  // gtk_get_current_event_time() 内部已经处理了“没有当前事件”的情况（退
  // 化为 GDK_CURRENT_TIME），不需要在这里再手写一次 NULL 分支。
  guint32 timestamp = gtk_get_current_event_time();
  gtk_window_begin_move_drag(GTK_WINDOW(self->popup_window), button, root_x,
                             root_y, timestamp);
  return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
}

// resize/reveal 共用的高度解析 + 应用（clamp 到工作区 90%，宽度固定、
// 顶边不动：GtkWindow 默认 NORTH_WEST 重力下 resize 只伸缩右/下边）。
// 返回 nullptr = 成功；否则为错误响应。
static FlMethodResponse* apply_logical_height(PopupWindowHost* self,
                                              FlValue* args,
                                              const gchar* method_name) {
  if (args == nullptr || fl_value_get_type(args) != FL_VALUE_TYPE_MAP) {
    g_autofree gchar* msg =
        g_strdup_printf("%s requires a map", method_name);
    return FL_METHOD_RESPONSE(
        fl_method_error_response_new("bad_args", msg, nullptr));
  }
  FlValue* height_value = fl_value_lookup_string(args, "height");
  if (height_value == nullptr ||
      fl_value_get_type(height_value) != FL_VALUE_TYPE_FLOAT) {
    g_autofree gchar* msg =
        g_strdup_printf("%s: missing/invalid height", method_name);
    return FL_METHOD_RESPONSE(
        fl_method_error_response_new("bad_args", msg, nullptr));
  }

  GdkRectangle workarea;
  get_popup_workarea(self, &workarea);
  gint max_height = (gint)lround(workarea.height * kResizeWorkareaFraction);
  gint target_height = (gint)lround(fl_value_get_float(height_value));
  if (target_height < 1) {
    target_height = 1;
  }
  if (max_height > 0 && target_height > max_height) {
    target_height = max_height;
  }
  gtk_window_resize(GTK_WINDOW(self->popup_window), kPopupLogicalWidth,
                    target_height);
  return nullptr;
}

static FlMethodResponse* handle_child_resize(PopupWindowHost* self,
                                             FlValue* args) {
  if (self->popup_window == nullptr) {
    return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
  }
  FlMethodResponse* error = apply_logical_height(self, args, "resize");
  if (error != nullptr) {
    return error;
  }
  return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
}

// reveal 握手：新载荷首帧已就绪——设高完成后显示并激活（解除兜底定时
// 器）。窗口已可见时等价一次 resize + 重新前置。
static FlMethodResponse* handle_child_reveal(PopupWindowHost* self,
                                             FlValue* args) {
  if (self->popup_window == nullptr) {
    return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
  }
  FlMethodResponse* error = apply_logical_height(self, args, "reveal");
  if (error != nullptr) {
    return error;
  }
  cancel_reveal_fallback(self);
  present_popup(self);
  return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
}

// =============================================================================
// pickFolder（异步：真正的 respond 发生在文件对话框的 "response" 信号里）
// =============================================================================

// GtkFileChooserNative "response" 信号回调。dialog 就是发出信号的那个对话
// 框实例；user_data 是 handle_child_pick_folder 里额外 g_object_ref 过的
// method_call，这里用 g_autoptr 归还这份引用。
static void pick_folder_response_cb(GtkNativeDialog* dialog, gint response_id,
                                    gpointer user_data) {
  g_autoptr(FlMethodCall) method_call = FL_METHOD_CALL(user_data);

  g_autoptr(FlMethodResponse) response = nullptr;
  if (response_id == GTK_RESPONSE_ACCEPT) {
    g_autofree gchar* folder =
        gtk_file_chooser_get_filename(GTK_FILE_CHOOSER(dialog));
    response = FL_METHOD_RESPONSE(fl_method_success_response_new(
        folder != nullptr ? fl_value_new_string(folder) : nullptr));
  } else {
    // GTK_RESPONSE_CANCEL 或对话框被意外关闭（GTK_RESPONSE_DELETE_EVENT）
    // 统一按取消处理，返回 null。
    response = FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
  }

  g_autoptr(GError) error = nullptr;
  if (!fl_method_call_respond(method_call, response, &error)) {
    g_warning("Failed to send popup_child pickFolder response: %s",
             error->message);
  }

  g_object_unref(dialog);  // 归还创建时持有的那份非浮动引用。
}

static void handle_child_pick_folder(PopupWindowHost* self,
                                     FlMethodCall* method_call, FlValue* args) {
  if (self->popup_window == nullptr || args == nullptr ||
      fl_value_get_type(args) != FL_VALUE_TYPE_MAP) {
    g_autoptr(FlMethodResponse) response = FL_METHOD_RESPONSE(
        fl_method_error_response_new("bad_args", "pickFolder requires a map",
                                     nullptr));
    g_autoptr(GError) error = nullptr;
    if (!fl_method_call_respond(method_call, response, &error)) {
      g_warning("Failed to send popup_child pickFolder response: %s",
               error->message);
    }
    return;
  }

  FlValue* title_value = fl_value_lookup_string(args, "title");
  const gchar* title = (title_value != nullptr &&
                       fl_value_get_type(title_value) == FL_VALUE_TYPE_STRING)
                          ? fl_value_get_string(title_value)
                          : nullptr;

  FlValue* initial_dir_value = fl_value_lookup_string(args, "initialDir");
  const gchar* initial_dir =
      (initial_dir_value != nullptr &&
      fl_value_get_type(initial_dir_value) == FL_VALUE_TYPE_STRING)
          ? fl_value_get_string(initial_dir_value)
          : nullptr;

  // GtkFileChooserNative 直接继承 GObject（不是 GInitiallyUnowned），
  // gtk_file_chooser_native_new 返回的是一份普通、非浮动的引用——这里刻意
  // 不用 g_autoptr：它要活到 pick_folder_response_cb 的异步 "response" 信
  // 号触发之后才 g_object_unref。
  GtkFileChooserNative* dialog = gtk_file_chooser_native_new(
      title, self->popup_window, GTK_FILE_CHOOSER_ACTION_SELECT_FOLDER,
      nullptr, nullptr);

  g_autofree gchar* resolved_dir = resolve_existing_dir(initial_dir);
  if (resolved_dir != nullptr) {
    gtk_file_chooser_set_current_folder(GTK_FILE_CHOOSER(dialog), resolved_dir);
  }

  // message_cb（fl_method_channel.cc）用 g_autoptr 持有 method_call，这次
  // 同步的 handler 调用一返回它就会被释放；要跨到异步的 "response" 信号才
  // 真正结束这次调用，必须在这里自己多拿一份引用，交给
  // pick_folder_response_cb 用 g_autoptr(FlMethodCall) 归还。
  g_signal_connect(dialog, "response", G_CALLBACK(pick_folder_response_cb),
                   g_object_ref(method_call));
  gtk_native_dialog_show(GTK_NATIVE_DIALOG(dialog));
}

static void child_method_call_cb(FlMethodChannel* /*channel*/,
                                 FlMethodCall* method_call, gpointer user_data) {
  PopupWindowHost* self = (PopupWindowHost*)user_data;
  const gchar* method = fl_method_call_get_name(method_call);
  FlValue* args = fl_method_call_get_args(method_call);

  if (g_strcmp0(method, "pickFolder") == 0) {
    // 异步方法：真正的 fl_method_call_respond 发生在
    // pick_folder_response_cb 里，这里提前返回，不落入下面统一收尾的
    // respond。
    handle_child_pick_folder(self, method_call, args);
    return;
  }

  g_autoptr(FlMethodResponse) response = nullptr;
  if (g_strcmp0(method, "ready") == 0) {
    response = handle_child_ready(self);
  } else if (g_strcmp0(method, "submit") == 0) {
    response = handle_child_submit(self, args);
  } else if (g_strcmp0(method, "cancel") == 0) {
    response = handle_child_cancel(self);
  } else if (g_strcmp0(method, "startDrag") == 0) {
    response = handle_child_start_drag(self);
  } else if (g_strcmp0(method, "resize") == 0) {
    response = handle_child_resize(self, args);
  } else if (g_strcmp0(method, "reveal") == 0) {
    response = handle_child_reveal(self, args);
  } else {
    response = FL_METHOD_RESPONSE(fl_method_not_implemented_response_new());
  }

  g_autoptr(GError) error = nullptr;
  if (!fl_method_call_respond(method_call, response, &error)) {
    g_warning("Failed to send popup_child response: %s", error->message);
  }
}

// =============================================================================
// 弹窗窗口 + 第二个 Flutter 引擎（懒创建，之后常驻复用）
// =============================================================================

static void ensure_popup_window(PopupWindowHost* self) {
  if (self->popup_window != nullptr) {
    return;
  }

  GtkWindow* window = GTK_WINDOW(gtk_window_new(GTK_WINDOW_TOPLEVEL));
  self->popup_window = window;

  // ── Wayland 理论降级清单（一次性记录，便于支持排查；均为"降级不破坏"）──
  // 1. 居中定位：Wayland 无全局坐标协议，set_position(CENTER) 被合成器忽
  //    略，落点由合成器策略决定（多数居中或级联）。
  // 2. keep_above / skip_taskbar / skip_pager：均为 X11 EWMH 语义，Wayland
  //    下被 GDK 静默忽略——弹窗可能不置顶、可能出现在任务栏/概览中。
  // 3. 显示即获焦：外部请求到达时前台是浏览器（另一进程），本进程拿不到
  //    xdg-activation 的用户交互 token，GNOME 等合成器的防焦点抢占会拒绝
  //    聚焦（可能只弹"FluxDown 已就绪"通知）——用户需点击一次弹窗才能输
  //    入。GTK3 层无解，与主窗口对话框回退路径受同一限制，不构成方案回退
  //    理由。
  // 4. startDrag：begin_move_drag 的 serial 重建可能被严格合成器忽略（见
  //    handle_child_start_drag 注释）。
  // 5. gtk_window_resize / 引擎渲染 / delete-event 守卫 / 文件选择器
  //    （GtkFileChooserNative 走 portal）在 Wayland 下语义完整，无降级。
  //    仓库先例：悬浮球按 wayland_degradation_service 整体禁用，弹窗则保
  //    持可用——降级后仍是完整可交互表单。
#ifdef GDK_WINDOWING_WAYLAND
  {
    GdkDisplay* display = gdk_display_get_default();
    if (display != nullptr && GDK_IS_WAYLAND_DISPLAY(display)) {
      g_message(
          "[popup-host] Wayland session: centering/keep-above/skip-taskbar/"
          "focus-on-show/start-drag are compositor-dependent degradations");
    }
  }
#endif

  // 无边框卡片式小窗：Dart 侧自绘标题拖动区与关闭按钮，原生只负责窗口本
  // 身。置顶、不进任务栏/切换器，类型提示为 DIALOG。
  gtk_window_set_decorated(window, FALSE);
  gtk_window_set_keep_above(window, TRUE);
  gtk_window_set_skip_taskbar_hint(window, TRUE);
  gtk_window_set_skip_pager_hint(window, TRUE);
  gtk_window_set_type_hint(window, GDK_WINDOW_TYPE_HINT_DIALOG);
  // 注意：必须保持 resizable=TRUE。GTK3 对非可调尺寸窗口会让尺寸跟随子部
  // 件的 natural size 并忽略 default_size/gtk_window_resize —— FlView 的
  // natural size 为 0，设 FALSE 会导致窗口塌缩且 Dart 驱动的 resize() 失
  // 效。无边框窗口本就没有用户拖边调尺寸的入口，不需要 FALSE 来禁用。
  gtk_window_set_title(window, "FluxDown Quick Download");
  // 520x600 逻辑像素——只在窗口第一次显示时生效（GTK 语义：隐藏后再显示会
  // 保留上次尺寸），后续高度变化由 Dart 经 resize() 驱动。
  gtk_window_set_default_size(window, kPopupLogicalWidth, kPopupDefaultHeight);
  gtk_window_set_position(window, GTK_WIN_POS_CENTER);

  // 必须先于下面 gtk_widget_realize(popup_view) 完成连接，理由见
  // popup_window_delete_event_cb 顶部的注释：FlView 的 realize_cb 会在同一
  // 个顶层窗口上连接它自己的 delete-event 处理器，谁先连接谁先被调用。
  g_signal_connect(window, "delete-event",
                   G_CALLBACK(popup_window_delete_event_cb), self);

  g_autoptr(FlDartProject) project = fl_dart_project_new();
  // fl_dart_project_set_dart_entrypoint_arguments()
  // （shell/platform/linux/fl_dart_project.cc）内部用 g_strdupv(argv) 深拷
  // 贝整个数组（含每个字符串），调用一返回就不再依赖 argv——栈上局部数组
  // 即可，不需要 static/堆分配保活。
  char* argv[] = {(char*)"--quick-popup", nullptr};
  fl_dart_project_set_dart_entrypoint_arguments(project, argv);

  FlView* view = fl_view_new(project);
  self->popup_view = view;

  GdkRGBA background_color;
  gdk_rgba_parse(&background_color, "#000000");
  fl_view_set_background_color(view, &background_color);

  gtk_widget_show(GTK_WIDGET(view));
  gtk_container_add(GTK_CONTAINER(window), GTK_WIDGET(view));

  // 弹窗引擎零插件注册：不调用 fl_register_plugins()。第二个引擎的所有原
  // 生能力都经下面这条 fluxdown/popup_child 通道由本文件提供，不加载任何
  // 生成的插件 registrant，也不触碰 Rust。
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  self->child_channel = fl_method_channel_new(
      fl_engine_get_binary_messenger(fl_view_get_engine(view)),
      "fluxdown/popup_child", FL_METHOD_CODEC(codec));
  fl_method_channel_set_method_call_handler(self->child_channel,
                                            child_method_call_cb, self,
                                            nullptr);

  // 显式 realize 触发 FlView::realize_cb（内部调用 fl_engine_start）：对照
  // my_application.cc 里主引擎 view 的同一处理方式——不需要等顶层窗口先显
  // 示，realize 子部件会顺带把尚未 realize 的顶层窗口一并 realize（创建
  // GdkWindow 但不 map），弹窗引擎因此可以在窗口真正可见之前就开始运行、
  // 渲染首帧。
  gtk_widget_realize(GTK_WIDGET(view));
}

// 每次 show() 都重新声明一次居中约束：GTK 把每一次“隐藏后再显示”都当作
// 新一轮初始定位处理，这样即使用户此前用 startDrag 把窗口拖到别处，下次
// 弹出也会回到屏幕中央。Wayland 下 gtk_window_set_position 没有对应协议、
// 合成器直接忽略，是已知且可接受的降级（契约原文）。
//
// 对隐藏窗口调用 gtk_window_resize 设置的是下次显示时的尺寸——reveal 到
// 达时会按内容实高覆盖，这里的默认高度只在兜底路径（reveal 超时）可见。
static void reset_placement(PopupWindowHost* self) {
  gtk_window_resize(self->popup_window, kPopupLogicalWidth,
                    kPopupDefaultHeight);
  gtk_window_set_position(self->popup_window, GTK_WIN_POS_CENTER);
}

// 显示并激活（reveal 握手的显示端；也是兜底定时器的强制显示入口）。
static void present_popup(PopupWindowHost* self) {
  if (self->popup_window == nullptr) {
    return;
  }
  gtk_window_present(self->popup_window);
  // 显示即获得键盘焦点（文本输入必需）：present() 负责窗口级别的激活/抢
  // 焦点，这里再显式把焦点交给 FlView，确保 Flutter 侧文本框不需要额外点
  // 击就能直接输入（对照 my_application.cc 主窗口 activate() 末尾的同一处
  // 理）。
  gtk_widget_grab_focus(GTK_WIDGET(self->popup_view));
}

// =============================================================================
// 主引擎通道 fluxdown/popup_host 处理器
// =============================================================================

static FlMethodResponse* handle_host_show(PopupWindowHost* self, FlValue* args) {
  if (args == nullptr || fl_value_get_type(args) != FL_VALUE_TYPE_STRING) {
    return FL_METHOD_RESPONSE(fl_method_error_response_new(
        "bad_args", "show requires a JSON string payload", nullptr));
  }

  ensure_popup_window(self);

  // 复用时先藏起旧表单画面并重置定位；窗口保持隐藏，由弹窗 Dart 在新载荷
  // 首帧就绪后经 reveal 一次到位「设高 + 显示」（reveal 握手）。
  popup_hide(self);
  reset_placement(self);

  if (self->popup_ready) {
    send_set_payload(self, fl_value_get_string(args));
  } else {
    g_free(self->pending_payload);
    self->pending_payload = g_strdup(fl_value_get_string(args));
  }

  // reveal 兜底：Dart 侧异常时超时强制显示（等价旧版立即显示行为）。
  self->reveal_timeout_id = g_timeout_add(kRevealFallbackTimeoutMs,
                                          reveal_fallback_cb, self);

  return FL_METHOD_RESPONSE(
      fl_method_success_response_new(fl_value_new_bool(TRUE)));
}

static FlMethodResponse* handle_host_close(PopupWindowHost* self) {
  // 契约：close 只隐藏，不回调 onClosed。
  popup_hide(self);
  return FL_METHOD_RESPONSE(fl_method_success_response_new(nullptr));
}

// 小窗可见期间新到的外部请求 — 转发给弹窗引擎合入当前表单（append
// 模式）。窗口实际不可见/引擎未就绪时返回 false，Dart 侧复位失步状态。
static FlMethodResponse* handle_host_append(PopupWindowHost* self,
                                            FlValue* args) {
  if (args == nullptr || fl_value_get_type(args) != FL_VALUE_TYPE_STRING) {
    return FL_METHOD_RESPONSE(fl_method_error_response_new(
        "bad_args", "append requires a URL text string", nullptr));
  }
  const gboolean can_append =
      self->popup_window != nullptr &&
      gtk_widget_get_visible(GTK_WIDGET(self->popup_window)) &&
      self->popup_ready && self->child_channel != nullptr;
  if (can_append) {
    fl_method_channel_invoke_method(self->child_channel, "appendPayload",
                                    args, nullptr, nullptr, nullptr);
  }
  return FL_METHOD_RESPONSE(
      fl_method_success_response_new(fl_value_new_bool(can_append)));
}

static void host_method_call_cb(FlMethodChannel* /*channel*/,
                                FlMethodCall* method_call, gpointer user_data) {
  PopupWindowHost* self = (PopupWindowHost*)user_data;
  const gchar* method = fl_method_call_get_name(method_call);
  FlValue* args = fl_method_call_get_args(method_call);

  g_autoptr(FlMethodResponse) response = nullptr;
  if (g_strcmp0(method, "show") == 0) {
    response = handle_host_show(self, args);
  } else if (g_strcmp0(method, "close") == 0) {
    response = handle_host_close(self);
  } else if (g_strcmp0(method, "append") == 0) {
    response = handle_host_append(self, args);
  } else {
    response = FL_METHOD_RESPONSE(fl_method_not_implemented_response_new());
  }

  g_autoptr(GError) error = nullptr;
  if (!fl_method_call_respond(method_call, response, &error)) {
    g_warning("Failed to send popup_host response: %s", error->message);
  }
}

// =============================================================================
// 公开 API
// =============================================================================

PopupWindowHost* popup_window_host_new(FlBinaryMessenger* main_messenger) {
  PopupWindowHost* self = g_new0(PopupWindowHost, 1);

  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  self->host_channel = fl_method_channel_new(
      main_messenger, "fluxdown/popup_host", FL_METHOD_CODEC(codec));
  fl_method_channel_set_method_call_handler(self->host_channel,
                                            host_method_call_cb, self,
                                            nullptr);
  return self;
}

void popup_window_host_free(PopupWindowHost* self) {
  if (self == nullptr) {
    return;
  }
  if (self->popup_window != nullptr) {
    // 唯一的销毁点：进程退出前。子部件 popup_view 随之一并销毁，不需要单
    // 独处理。
    gtk_widget_destroy(GTK_WIDGET(self->popup_window));
  }
  if (self->child_channel != nullptr) {
    fl_method_channel_set_method_call_handler(self->child_channel, nullptr,
                                              nullptr, nullptr);
    g_object_unref(self->child_channel);
  }
  if (self->host_channel != nullptr) {
    fl_method_channel_set_method_call_handler(self->host_channel, nullptr,
                                              nullptr, nullptr);
    g_object_unref(self->host_channel);
  }
  cancel_reveal_fallback(self);
  g_free(self->pending_payload);
  g_free(self);
}
