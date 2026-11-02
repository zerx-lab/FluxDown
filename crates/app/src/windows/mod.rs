//! 窗口注册表与界面进程退出判定。
//!
//! 所有顶层窗口按 [`WindowKey`] 去重；最后一个用户窗口关闭时界面进程退出（后台是否驻留由
//! agent 决定，见 `crate::lifecycle`）。gpui 不会在最后一个窗口关闭时自动退出，这里是唯一的
//! 「关窗即退出」判定点。

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use gpui::{
    AnyWindowHandle, App, DisplayId, Entity, Global, Render, Subscription, Window, WindowHandle,
    WindowId, WindowOptions,
};
use gpui_component::WindowExt as _;
use serde_json::{Value, json};

use crate::{agent_client::AgentClient, app::Desktop};

mod bounds;
pub use bounds::RememberedWindow;

pub mod file_conflict;
pub mod group_detail;
pub mod main;
pub mod new_download;
pub mod plugin;
pub mod progress;
pub mod queue_manager;
pub mod selection;
pub mod settings;
pub mod task_detail;
pub mod update_prompt;

#[cfg(test)]
mod tests;

/// 顶层窗口身份：同 key 只允许一个实例。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum WindowKey {
    Main,
    Settings,
    NewDownload,
    QueueManager,
    Selection(String),
    /// 所有待确认的「文件已存在」请求聚合在这一个窗口里。
    FileConflicts,
    /// 自动完成提醒复用进度窗口，一个窗口内导航有界完成集合。
    Completion,
    TaskDetail(String),
    GroupDetail(String),
    /// 独立下载进度 / 完成窗口（每任务一个）。
    Progress(String),
    /// 更新窗口（不依赖主窗口）：只在用户主动时打开（菜单「检查更新…」、顶栏更新按钮）。
    UpdatePrompt,
    /// 插件桌面界面（contributes.views 的 desktop 入口，gpui-shell 运行），key 为 `<identity>/<viewId>`。
    Plugin(String),
}

pub struct WindowRegistry {
    open: HashMap<WindowKey, AnyWindowHandle>,
    ids: HashMap<WindowId, WindowKey>,
    /// 正在显示「下载仍在进行」确认框的窗口：重复 ⌘W / ⌘Q 不叠第二个对话框。
    confirming: HashSet<WindowId>,
    /// 窗口边界记忆（最新值 + 防抖中尚未落盘的值，退出时强制写一次）。
    bounds: Rc<RefCell<bounds::BoundsMemory>>,
    _closed_sub: Subscription,
    _quit_sub: Subscription,
}

impl Global for WindowRegistry {}

impl WindowRegistry {
    /// 安装全局注册表：窗口关闭清理 + 退出判定 + 退出时落盘边界。
    pub fn init(cx: &mut App, client: Arc<AgentClient>) {
        let closed_sub = cx.on_window_closed(Self::on_window_closed);
        let bounds = Rc::new(RefCell::new(bounds::BoundsMemory::default()));
        let quit_bounds = Rc::clone(&bounds);
        let quit_sub = cx.on_app_quit(move |_| {
            let pending = quit_bounds.borrow_mut().drain_pending();
            let client = Arc::clone(&client);
            async move {
                for (key, value) in pending {
                    if let Err(error) = patch_local_preference(&client, key, value).await {
                        // 退出时继续提交其他窗口的边界，失败不能假作已落盘。
                        log::warn!(
                            "failed to persist window bounds on exit {key}: {:?}",
                            error.code
                        );
                    }
                }
            }
        });
        cx.set_global(Self {
            open: HashMap::new(),
            ids: HashMap::new(),
            confirming: HashSet::new(),
            bounds,
            _closed_sub: closed_sub,
            _quit_sub: quit_sub,
        });
    }

    fn on_window_closed(cx: &mut App, window_id: WindowId) {
        let (last_closed, closed_key) = {
            let registry = cx.global_mut::<Self>();
            let key = registry.ids.remove(&window_id);
            if let Some(key) = key.as_ref() {
                registry.open.remove(key);
            }
            registry.confirming.remove(&window_id);
            (registry.open.is_empty(), key)
        };
        if let Some(WindowKey::Completion) = closed_key {
            crate::completion_windows::window_closed(cx);
        }
        if last_closed {
            crate::lifecycle::quit_ui(cx);
        }
    }

    /// 承载全局提示（配对确认框、登录失效通知）的窗口：当前活动的主 / 设置窗口优先，
    /// 其次主窗口，再次设置窗口；这两类窗口都由 `Root` 托管浮层。
    pub fn overlay_window(cx: &App) -> Option<AnyWindowHandle> {
        let registry = cx.global::<Self>();
        let main = registry.open.get(&WindowKey::Main).copied();
        let settings = registry.open.get(&WindowKey::Settings).copied();
        cx.active_window()
            .filter(|active| Some(*active) == main || Some(*active) == settings)
            .or(main)
            .or(settings)
    }

    /// 打开或聚焦窗口。已开 → 延后 `activate_window` 并返回 `None`。
    pub fn open_or_focus<V: Render>(
        cx: &mut App,
        key: WindowKey,
        options: WindowOptions,
        build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    ) -> Option<WindowHandle<V>> {
        if let Some(handle) = cx.global::<Self>().open.get(&key).copied() {
            // 快捷键可能正在该窗口自己的 update 栈内分发；此时句柄暂不可更新，
            // 不代表窗口已关闭。退出当前栈后再聚焦，登记只由 on_window_closed 清理。
            cx.defer(move |cx| {
                if let Err(error) = handle.update(cx, |_, window, _| window.activate_window()) {
                    log::debug!("window closed before deferred activation: {error:#}");
                }
            });
            return None;
        }
        let label = format!("{key:?}");
        let opened_at = std::time::Instant::now();
        let options = crate::app_icon::window_options(options, cx);
        let result = cx.open_window(options, move |window, cx| {
            crate::logging::observe_new_window(label, opened_at, window, cx);
            // 「跟随系统」主题：系统明暗切换时重新解析；外观变化不会触发整窗刷新，必须显式订阅。
            window
                .observe_window_appearance(fluxdown_ui_theme::sync_system_theme)
                .detach();
            build(window, cx)
        });
        match result {
            Ok(handle) => {
                let any: AnyWindowHandle = handle.into();
                let registry = cx.global_mut::<Self>();
                registry.ids.insert(any.window_id(), key.clone());
                registry.open.insert(key, any);
                // 窗口类图标来自 exe 资源；非默认应用图标要请 agent 给新窗口补设（日志即可，不阻塞开窗）。
                #[cfg(windows)]
                crate::app_icon::sync_window_icons(cx);
                Some(handle)
            }
            Err(error) => {
                log::error!("failed to open FluxDown window {key:?}: {error:#}");
                None
            }
        }
    }

    pub fn close(cx: &mut App, key: &WindowKey) {
        if let Some(handle) = cx.global::<Self>().open.get(key).copied()
            && let Err(error) = handle.update(cx, |_, window, _| window.remove_window())
        {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    }

    #[must_use]
    pub fn is_open(cx: &App, key: &WindowKey) -> bool {
        cx.global::<Self>().open.contains_key(key)
    }

    #[must_use]
    pub fn handle(cx: &App, key: &WindowKey) -> Option<AnyWindowHandle> {
        cx.global::<Self>().open.get(key).copied()
    }

    /// 主窗口所在的显示器；无主窗口时调用方应让 GPUI 回退到主显示器。
    #[must_use]
    pub fn main_display_id(cx: &mut App) -> Option<DisplayId> {
        Self::handle(cx, &WindowKey::Main)
            .and_then(|handle| {
                handle
                    .update(cx, |_, window, cx| window.display(cx))
                    .ok()
                    .flatten()
            })
            .map(|display| display.id())
    }

    /// 窗口 id → 注册 key（未注册的窗口返回 `None`）。
    #[must_use]
    pub fn key_of(cx: &App, id: WindowId) -> Option<WindowKey> {
        cx.global::<Self>().ids.get(&id).cloned()
    }

    /// 当前获得焦点的窗口。macOS 的 `cx.active_window()` 只认 `NSWindow`，`Floating` /
    /// `PopUp` 是 `NSPanel`（选择框）会返回 `None`，此时按 gpui 记录的 key 态扫描。
    /// 只能在 defer 之后调用：正在 update 栈内的窗口不在 `cx.windows` 里，扫描会漏掉它。
    #[must_use]
    pub fn focused_window(cx: &mut App) -> Option<AnyWindowHandle> {
        cx.active_window().or_else(|| {
            cx.windows().into_iter().find(|handle| {
                handle
                    .update(cx, |_, window, _| window.is_window_active())
                    .unwrap_or(false)
            })
        })
    }

    /// 关闭当前活动窗口：主窗口走 [`main::should_close`] 关闭策略（与原生关闭按钮一致，
    /// `remove_window` 不会触发 `windowShouldClose:`），其他窗口直接关。
    ///
    /// 键盘触发时正处于该窗口自己的 update 栈内（窗口已被从 `cx.windows` 取走），同步
    /// `handle.update` 会拿不到窗口而静默失败；必须 defer 到本轮 update 结束再操作。
    pub fn close_active_window(cx: &mut App) {
        cx.defer(|cx| {
            let Some(handle) = Self::focused_window(cx) else {
                return;
            };
            let is_main = Self::key_of(cx, handle.window_id()) == Some(WindowKey::Main);
            if let Err(error) = handle.update(cx, |_, window, cx| {
                if !is_main || main::should_close(window, cx) {
                    window.remove_window();
                }
            }) {
                log::debug!("view or window released before lifecycle update: {error:#}");
            }
        });
    }

    /// 用户窗口数量。
    #[must_use]
    pub fn open_count(cx: &App) -> usize {
        cx.global::<Self>().open.len()
    }
}

/// 请求激活应用与窗口；前台切换最终由系统决定，并不保证强制置前。
///
/// - macOS：应用激活与 `makeKeyAndOrderFront` 是不同操作，都经 GPUI 请求。
/// - Windows：GPUI 调用 `SetForegroundWindow`，仍受系统前台策略约束。
/// - X11：发送 `_NET_ACTIVE_WINDOW`；额外请求 urgency 提示。
/// - Wayland：申请 xdg-activation token，合成器可拒绝；GPUI 0.3.7 的
///   `request_attention` 是空实现，不能承诺任务栏提示或抢焦点。
pub fn bring_to_front(window: &mut Window, cx: &mut App) {
    cx.activate(true);
    window.activate_window();
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    window.request_attention();
}

/// 「下载仍在进行」确认框：用户确认后执行 `on_ok`（关窗 / 退出）。同一窗口已在提示中
/// （重复 ⌘W / ⌘Q、再点关闭按钮）则不再叠第二个对话框。
pub fn confirm_active_tasks(
    window: &mut Window,
    cx: &mut App,
    on_ok: impl Fn(&mut Window, &mut App) + 'static,
) {
    let id = window.window_handle().window_id();
    if !cx.global_mut::<WindowRegistry>().confirming.insert(id) {
        return;
    }
    let translator = Desktop::global(cx).translator.read(cx).clone();
    let title = translator.text("closeWithActiveTasksTitle").to_owned();
    let hint = translator.text("closeWithActiveTasksHint").to_owned();
    let ok = translator.text("menuQuit").to_owned();
    let cancel = translator.text("cancel").to_owned();
    let on_ok = Rc::new(on_ok);
    window.open_alert_dialog(cx, move |dialog, _, cx| {
        let on_ok = Rc::clone(&on_ok);
        dialog
            .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
            .description(hint.clone())
            .footer(fluxdown_ui_components::dialog_footer(
                Some(cancel.clone().into()),
                ok.clone(),
                fluxdown_ui_components::DialogIntent::Confirm,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                on_ok(window, cx);
                true
            })
            .on_close(move |_, _, cx| {
                cx.global_mut::<WindowRegistry>().confirming.remove(&id);
            })
    });
}

/// `agent.preferences.patch` 设备本地写入（`sync:false`，不进云同步）。
pub async fn patch_local_preference(
    client: &AgentClient,
    key: &str,
    value: Value,
) -> Result<(), fluxdown_protocol::RpcErrorData> {
    client
        .call::<Value, Value>(
            fluxdown_protocol::method::AGENT_PREFERENCES_PATCH,
            Some(json!({ "values": { key: value }, "sync": false })),
        )
        .await
        .map(|_| ())
}
