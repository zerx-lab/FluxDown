//! 独立下载进度 / 完成窗口：任务进度与自动完成集合共用 FluxDown 辅助窗口和业务视图。
//! 标题随进度与导航刷新，可调整大小、可最小化；共用设备本地的尺寸、位置与显示器记忆。
//! 开关时机由 [`crate::progress_windows`] 与 [`crate::completion_windows`] 决定。
//!
//! - macOS：自动进度面板不激活应用，但允许点按激活；完成窗口保持非激活。
//! - Windows：仅完成窗口使用 `WindowKind::PopUp`（`WS_EX_TOPMOST`）。
//! - Linux X11：PopUp 是不能拖动的 override-redirect 窗口；保持普通窗口，完成时请求注意。

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use fluxdown_ui_downloads::{
    COMPLETION_WINDOW_PREF, PROGRESS_WINDOW_INITIAL_HEIGHT, PROGRESS_WINDOW_WIDTH,
    ProgressWindowEvent, ProgressWindowPrefs, ProgressWindowView,
};
use fluxdown_ui_shell::{AuxiliaryWindowView, auxiliary_window_options};
use gpui::{
    App, AppContext as _, Context, Entity, SharedString, Subscription, Task, Window, WindowKind,
    px, size,
};
use gpui_component::Root;

use crate::{
    app::Desktop,
    downloads_port::AgentDownloadsPort,
    session::attach,
    windows::{RememberedWindow, WindowKey, WindowRegistry},
};

/// 打开文件 / 文件夹后等待对方接管前台的上限，从点击起算。
const HANDOFF_CLOSE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Show task progress without activating the app.
pub(crate) fn open_automatic(cx: &mut App, task_id: String) {
    let key = WindowKey::Progress(task_id.clone());
    if !WindowRegistry::is_open(cx, &key) {
        #[cfg(target_os = "macos")]
        let prior_windows = objc2::MainThreadMarker::new().map(|main| {
            let app = objc2_app_kit::NSApplication::sharedApplication(main);
            let windows = app.windows();
            (app, windows)
        });
        let progress = open_window(cx, key, task_id, None);
        #[cfg(target_os = "macos")]
        if progress.is_some()
            && let Some((app, before)) = prior_windows
        {
            let after = app.windows();
            let mut added = after
                .iter()
                .filter(|window| !before.iter().any(|old| std::ptr::eq(&*old, &**window)));
            // Synchronous creation should add exactly one window. If that is
            // ambiguous, don't change the ordering of an unrelated window.
            if let Some(window) = added.next()
                && added.next().is_none()
                && let Ok(panel) = window.downcast::<objc2_app_kit::NSPanel>()
            {
                panel.setBecomesKeyOnlyIfNeeded(false);
                panel.setHidesOnDeactivate(false);
                panel.orderFrontRegardless();
            }
        }
        drop(progress);
    }
}

/// 自动完成复用原有进度窗口，不激活应用，也不聚焦已存在的完成窗口。
pub(crate) fn open_completion(
    cx: &mut App,
    task_ids: Vec<String>,
    total: usize,
) -> Option<Entity<ProgressWindowView>> {
    if WindowRegistry::is_open(cx, &WindowKey::Completion) {
        return None;
    }
    let first = task_ids.first()?.clone();
    open_window(cx, WindowKey::Completion, first, Some((task_ids, total)))
}

fn open_window(
    cx: &mut App,
    key: WindowKey,
    task_id: String,
    completed: Option<(Vec<String>, usize)>,
) -> Option<Entity<ProgressWindowView>> {
    let desktop = Desktop::global(cx);
    let translator = desktop.translator.clone();
    let session = desktop.session.clone();
    let client = desktop.client.clone();
    let title = translator.read(cx).text("progressWindowTitle").to_owned();
    let settings = desktop.settings_store.clone();
    let show_completion =
        ProgressWindowPrefs::from_preferences(settings.read(cx).preferences()).completion;

    let mut options = auxiliary_window_options(title);
    let min_size = size(
        px(PROGRESS_WINDOW_WIDTH),
        px(PROGRESS_WINDOW_INITIAL_HEIGHT),
    );
    options.window_min_size = Some(min_size);
    options.is_resizable = true;
    options.is_minimizable = true;
    WindowRegistry::restore_bounds(RememberedWindow::Progress, &mut options, min_size, cx);
    options.focus = false;
    if cfg!(target_os = "macos") && completed.is_none() {
        options.kind = WindowKind::Floating;
    } else if completed.is_some() && cfg!(any(target_os = "macos", target_os = "windows")) {
        options.kind = WindowKind::PopUp;
    }

    let mut opened_view = None;
    let completion = key == WindowKey::Completion;
    let opened = WindowRegistry::open_or_focus(cx, key.clone(), options, |window, cx| {
        let port = Arc::new(AgentDownloadsPort::new(client.clone()));
        let view = cx.new(|cx| {
            cx.observe(&settings, |view: &mut ProgressWindowView, settings, cx| {
                let completion =
                    ProgressWindowPrefs::from_preferences(settings.read(cx).preferences())
                        .completion;
                view.set_show_completion(completion, cx);
            })
            .detach();
            ProgressWindowView::new(
                translator.clone(),
                task_id.clone(),
                port,
                show_completion,
                cx,
            )
        });
        attach(&session, &view, cx);
        if let Some((task_ids, total)) = completed {
            view.update(cx, |view, cx| view.append_completed(task_ids, total, cx));
        }
        let window_view = cx.new(|cx| {
            AuxiliaryWindowView::new(
                translator.clone(),
                "progressWindowTitle",
                view.clone().into(),
                cx,
            )
        });
        if let Some(title) = view.read(cx).title() {
            window.set_window_title(&title);
            window_view.update(cx, |chrome, cx| {
                chrome.set_title(Some(SharedString::from(title)), cx);
            });
        }

        let root = cx.new(|cx| {
            let root = Root::new(window_view.clone(), window, cx);
            let hand_off = Rc::new(RefCell::new(HandOff::default()));
            cx.observe_in(&view, window, move |_, view, window, cx| {
                let Some(title) = view.read(cx).title() else {
                    return;
                };
                window.set_window_title(&title);
                window_view.update(cx, |chrome, cx| {
                    chrome.set_title(Some(SharedString::from(title)), cx);
                });
            })
            .detach();
            cx.subscribe_in(&view, window, move |_, view, event, window, cx| {
                on_view_event(view, event, completion, &hand_off, window, cx);
            })
            .detach();
            root
        });
        WindowRegistry::persist_bounds(RememberedWindow::Progress, client, &root, window, cx);
        opened_view = Some(view);
        root
    });
    if cfg!(any(target_os = "linux", target_os = "freebsd"))
        && completion
        && let Some(handle) = opened
        && let Err(error) = handle.update(cx, |_, window, _| window.request_attention())
    {
        log::debug!("completion window released before requesting attention: {error:#}");
    }
    opened_view
}

/// 进度视图的事件：关窗、打开文件 / 文件夹的前台交接，以及完成窗口偏好。
fn on_view_event(
    view: &Entity<ProgressWindowView>,
    event: &ProgressWindowEvent,
    completion: bool,
    hand_off: &Rc<RefCell<HandOff>>,
    window: &mut Window,
    cx: &mut Context<Root>,
) {
    match event {
        ProgressWindowEvent::Close => window.remove_window(),
        ProgressWindowEvent::HandOffStarted => {
            if completion {
                crate::completion_windows::window_closing(cx);
            }
            // 窗口可能先于打开命令结束关闭：命令登记为关窗后仍须完成的请求，
            // 关掉最后一个窗口时界面等它结束再退出。
            if let Some(task) = view.update(cx, |view, _| view.take_hand_off_task()) {
                crate::lifecycle::keep_alive(cx, task).detach();
            }
            begin_hand_off(hand_off, window, cx);
        }
        ProgressWindowEvent::HandOffAborted => {
            if completion {
                crate::completion_windows::window_kept(cx);
            }
            // 丢弃失活订阅与兜底计时：窗口保留。
            drop(hand_off.take());
        }
        ProgressWindowEvent::HandedOff => {
            if completion {
                crate::completion_windows::window_closing(cx);
            }
            finish_hand_off(hand_off, window, cx);
        }
        ProgressWindowEvent::ShowCompletionChanged(value) => {
            Desktop::set_pref(cx, COMPLETION_WINDOW_PREF, (*value).into());
        }
        ProgressWindowEvent::OpenMainWindow => {
            if completion {
                crate::completion_windows::window_closing(cx);
            }
            // 先显示主窗口再关本窗口：避免关掉最后一个窗口时界面退出。
            crate::windows::main::reveal(cx);
            window.remove_window();
        }
    }
}

/// 打开文件 / 文件夹的前台交接（每个窗口至多一个）。本应用仍在前台时直接关闭当前窗口，系统会
/// 把本应用的下一个窗口（通常是主窗口）提到前面、压住刚打开的程序；所以等窗口失活（对方已成为
/// 前台）再关。点击即开始等待；兜底时限也从点击起算，但打开命令成功前不因时限关窗。
#[derive(Default)]
struct HandOff {
    /// 兜底时限已到。
    expired: bool,
    /// 打开命令已成功。
    succeeded: bool,
    /// 窗口失活即关窗；`None` = 没有进行中的交接。丢弃即取消。
    activation: Option<Subscription>,
    /// 兜底计时，只为持有；丢弃即取消。
    _timer: Option<Task<()>>,
}

impl HandOff {
    /// 兜底时限到：命令已成功则关窗，否则等结果。
    fn expire(&mut self) -> bool {
        self.expired = true;
        self.succeeded
    }

    /// 命令成功：窗口已失活或时限已到则立即关窗，否则继续等对方接管前台。
    fn succeed(&mut self, window_active: bool) -> bool {
        self.succeeded = true;
        self.expired || !window_active
    }
}

fn begin_hand_off(state: &Rc<RefCell<HandOff>>, window: &mut Window, cx: &mut Context<Root>) {
    let activation = cx.observe_window_activation(window, |_, window, _| {
        if !window.is_window_active() {
            window.remove_window();
        }
    });
    // 计时持弱引用：状态与计时互不延长对方的生命周期。
    let weak = Rc::downgrade(state);
    let timer = cx.spawn_in(window, async move |_, cx| {
        cx.background_executor().timer(HANDOFF_CLOSE_TIMEOUT).await;
        let Some(state) = weak.upgrade() else {
            return;
        };
        let close = state.borrow_mut().expire();
        if close && let Err(error) = cx.update(|window, _| window.remove_window()) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    });
    // 旧交接（若有）随替换丢弃，其订阅与计时一并取消。
    drop(state.replace(HandOff {
        activation: Some(activation),
        _timer: Some(timer),
        ..HandOff::default()
    }));
}

fn finish_hand_off(state: &Rc<RefCell<HandOff>>, window: &mut Window, cx: &mut Context<Root>) {
    if state.borrow().activation.is_none() {
        // 点击时预计窗口保留（完成集合里还有其它项），结果却要关窗：从现在开始等待。
        begin_hand_off(state, window, cx);
    }
    let close = state.borrow_mut().succeed(window.is_window_active());
    if close {
        window.remove_window();
    }
}

#[cfg(test)]
mod tests {
    use super::HandOff;

    #[test]
    fn handoff_closes_only_after_success_and_then_on_deactivation_or_expiry() {
        // 时限先到：命令未成功前不关（失败要显示在窗口里），成功即关。
        let mut slow = HandOff::default();
        assert!(!slow.expire());
        assert!(slow.succeed(true));

        // 成功时窗口仍在前台：等对方接管前台或时限到。
        let mut fast = HandOff::default();
        assert!(!fast.succeed(true));
        assert!(fast.expire());

        // 成功时对方已接管前台：立即关。
        assert!(HandOff::default().succeed(false));
    }
}
