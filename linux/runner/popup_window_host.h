#ifndef RUNNER_POPUP_WINDOW_HOST_H_
#define RUNNER_POPUP_WINDOW_HOST_H_

#include <flutter_linux/flutter_linux.h>

G_BEGIN_DECLS

// 外部唤起下载小窗的原生宿主控制器（跨端弹窗契约 v1：外部下载请求——浏览器
// 扩展 / aria2 RPC / 管理 API——唤起时，弹出独立原生小窗承载第二个 Flutter
// 引擎渲染快速下载表单）。
//
// 职责：
// - 在主引擎 messenger 上注册 fluxdown/popup_host 通道，响应
//   show/close/relay，并把 onResult/onClosed/onRelay 中继回主引擎 Dart。
// - 懒创建承载第二个 Flutter 引擎（--quick-popup 入口、零插件注册、不初
//   始化 Rust）的无边框 GTK 顶层窗口，并在该引擎自己的 messenger 上注册
//   fluxdown/popup_child 通道，响应 ready/submit/cancel/pickFolder/
//   startDrag/resize/relay（resize 支持可选 width），并把 setPayload/
//   onRelay 投递给弹窗 Dart。
// - 窗口与引擎一旦创建即常驻复用：之后的每次外部请求都只 hide/show，绝不
//   重建——历史上 desktop_multi_window 频繁建销 isolate 曾导致 0xc0000005
//   崩溃（commit 39d6c74），本实现刻意规避同类模式。
//
// 和 floating_ball_window.h 一样，这不是一个注册过的 GObject 类型，只是一
// 个用 GLib 习惯管理生命周期的不透明控制器结构体。
typedef struct _PopupWindowHost PopupWindowHost;

// 创建控制器并在 |main_messenger|（主引擎的 FlBinaryMessenger）上安装
// fluxdown/popup_host 的 method call handler。弹窗窗口/弹窗引擎本身在首次
// 收到 show 请求时才懒创建。
PopupWindowHost* popup_window_host_new(FlBinaryMessenger* main_messenger);

// 销毁弹窗窗口（若已创建；随之一并销毁弹窗引擎/第二个 FlView）、卸载两个
// 通道的 method call handler 并释放 |self|。仅应在进程退出前调用一次；安全
// 接受 NULL。
void popup_window_host_free(PopupWindowHost* self);

G_END_DECLS

#endif  // RUNNER_POPUP_WINDOW_HOST_H_
