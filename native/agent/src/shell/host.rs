//! 托盘宿主：主线程运行 tao 事件循环并持有托盘图标，agent runtime 在工作线程运行。
//!
//! - macOS 的状态栏图标必须在主线程、随 NSApp 事件循环运行；Windows / Linux 同样放主线程，
//!   三个平台只有一套线程模型。进程以 Accessory 策略运行（无 Dock 图标）。
//! - 托盘菜单 / 点击事件直接经 [`TrayAction`] 通道交给 runtime；runtime 经 [`TrayPort`]
//!   把展示状态投递回事件循环。
//! - 系统注销 / 关机（Windows `WM_ENDSESSION`、macOS `applicationWillTerminate`）时 tao 发出
//!   `LoopDestroyed` 后随即结束进程：在这里同步执行有时限的完全退出。
//! - Linux 需要图形会话与 StatusNotifier 宿主；缺失时不建托盘，后台随界面退出。

use std::panic::AssertUnwindSafe;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(any(windows, target_os = "macos"))]
use std::time::Instant;

use fluxdown_protocol::TrayUnavailableReason;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use tao::platform::run_return::EventLoopExtRunReturn;
use tokio::sync::mpsc;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[cfg(windows)]
use super::DownloadDot;
#[cfg(not(target_os = "macos"))]
use super::TrayIconSource;
use super::{ShellHost, TrayAction, TrayAvailability, TrayModel, TrayPort};
#[cfg(target_os = "macos")]
use crate::hotkey::{HotkeyBinding, HotkeyPort, HotkeySink, MainThreadHost};
use crate::runtime::AgentResult;

/// 系统注销时等待完全退出（daemon 优雅关停）的上限；Windows 约 5s 后强制结束进程。
const SESSION_END_BUDGET: Duration = Duration::from_millis(4500);
/// 菜单位置：显示窗口、分隔线、全部暂停、全部恢复之后插入「取消完成后关机」。
const CANCEL_SHUTDOWN_POSITION: usize = 4;
#[cfg(not(target_os = "macos"))]
const DEFAULT_TOOLTIP: &str = "FluxDown";
/// 托盘固定标识。Linux 上 tray-icon 把它拼成 SNI `Id`（`tray-icon tray app fluxdown`），
/// GNOME 顶栏整理类扩展按 `Id` 记住托盘项位置；缺省的 `<pid>-<序号>` 每次启动都会变（#269）。
const TRAY_ID: &str = "fluxdown";

#[cfg(target_os = "macos")]
const ICON_BYTES: &[u8] = include_bytes!("../../../../assets/logo/tray_iconTemplate.png");
#[cfg(target_os = "linux")]
const ICON_BYTES: &[u8] = include_bytes!("../../../../assets/logo/fluxdown_logo.png");
/// Windows 没有模板图标机制：深色任务栏用浅色图标，浅色任务栏用原色图标。
#[cfg(windows)]
const DARK_TASKBAR_ICON_BYTES: &[u8] = include_bytes!("../../../../assets/logo/logo_on_dark.png");
#[cfg(windows)]
const LIGHT_TASKBAR_ICON_BYTES: &[u8] = include_bytes!("../../../../assets/logo/fluxdown_logo.png");
#[cfg(windows)]
const THEME_POLL_INTERVAL: Duration = Duration::from_secs(5);

enum HostEvent {
    Apply(Box<TrayModel>),
    RuntimeFinished,
    /// 在主线程创建全局热键后端（仅 macOS）。
    #[cfg(target_os = "macos")]
    HotkeySpawn(Arc<dyn HotkeySink>),
    /// 向主线程的全局热键后端整表下发绑定（仅 macOS）。
    #[cfg(target_os = "macos")]
    HotkeyApply(Vec<HotkeyBinding>),
    /// 右键点击托盘图标后在主线程弹出菜单（仅 macOS，见 [`TrayUi::pop_up_menu`]）。
    #[cfg(target_os = "macos")]
    ShowMenu,
}

struct ProxyPort(Mutex<EventLoopProxy<HostEvent>>);

impl TrayPort for ProxyPort {
    fn apply(&self, model: &TrayModel) {
        let proxy = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if proxy
            .send_event(HostEvent::Apply(Box::new(model.clone())))
            .is_err()
        {
            tracing::trace!("tray event loop closed before model update");
        }
    }
}

/// 把全局热键后端放进主线程事件循环（macOS）：后端对象留在循环闭包里，这里只投递消息。
/// 同一通道 FIFO，保证创建先于其后的 `apply`。
#[cfg(target_os = "macos")]
#[derive(Clone)]
struct HotkeyLink(Arc<Mutex<EventLoopProxy<HostEvent>>>);

#[cfg(target_os = "macos")]
impl HotkeyLink {
    fn send(&self, event: HostEvent) {
        let proxy = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if proxy.send_event(event).is_err() {
            tracing::debug!("tray event loop closed before hotkey message");
        }
    }
}

#[cfg(target_os = "macos")]
impl MainThreadHost for HotkeyLink {
    fn spawn(&self, sink: Arc<dyn HotkeySink>) -> Box<dyn HotkeyPort> {
        self.send(HostEvent::HotkeySpawn(sink));
        Box::new(self.clone())
    }
}

#[cfg(target_os = "macos")]
impl HotkeyPort for HotkeyLink {
    fn apply(&self, bindings: Vec<HotkeyBinding>) {
        self.send(HostEvent::HotkeyApply(bindings));
    }
}

/// 在主线程运行托盘宿主；`run_runtime` 在工作线程阻塞运行 agent 并返回其结果。
pub fn run<F>(autostart: bool, run_runtime: F) -> AgentResult
where
    F: FnOnce(ShellHost) -> AgentResult + Send + 'static,
{
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return run_runtime(ShellHost::without_tray(
            TrayUnavailableReason::NoDisplay,
            autostart,
        ));
    }
    #[cfg(target_os = "macos")]
    crate::notification::initialize();

    // Linux 下 GTK 初始化失败时 tao 直接 panic；降级为无托盘运行而不是让 agent 起不来。
    let built =
        std::panic::catch_unwind(|| EventLoopBuilder::<HostEvent>::with_user_event().build());
    let Ok(mut event_loop) = built else {
        tracing::warn!("tray event loop unavailable; running without tray");
        return run_runtime(ShellHost::without_tray(
            TrayUnavailableReason::InitFailed,
            autostart,
        ));
    };
    configure_platform(&mut event_loop);

    let proxy = event_loop.create_proxy();
    let (actions_tx, actions_rx) = mpsc::unbounded_channel();
    let mut pending = Some((run_runtime, actions_rx));
    let mut ui: Option<TrayUi> = None;
    let mut runtime: Option<std::thread::JoinHandle<AgentResult>> = None;
    let mut done_rx: Option<std_mpsc::Receiver<()>> = None;
    let mut finished = false;
    let mut spawn_error: Option<std::io::Error> = None;
    #[cfg(windows)]
    let mut theme_deadline = Instant::now() + THEME_POLL_INTERVAL;
    // 全局热键后端（Carbon / NSEvent，只能在主线程）；宿主要求时在这里创建，随循环结束释放。
    #[cfg(target_os = "macos")]
    let mut hotkeys: Option<crate::platform::hotkey::macos::Backend> = None;

    event_loop.run_return(|event, _, control_flow| {
        if !matches!(
            *control_flow,
            ControlFlow::Exit | ControlFlow::ExitWithCode(_)
        ) {
            #[cfg(windows)]
            {
                *control_flow = ControlFlow::WaitUntil(theme_deadline);
            }
            #[cfg(not(windows))]
            {
                *control_flow = ControlFlow::Wait;
            }
        }
        match event {
            Event::NewEvents(StartCause::Init) => {
                let Some((run_runtime, actions_rx)) = pending.take() else {
                    return;
                };
                let availability = match TrayUi::create(&actions_tx, &proxy) {
                    Ok(created) => {
                        ui = Some(created);
                        TrayAvailability::Available
                    }
                    Err(reason) => TrayAvailability::Unavailable(reason),
                };
                let port = (availability == TrayAvailability::Available)
                    .then(|| Arc::new(ProxyPort(Mutex::new(proxy.clone()))) as Arc<dyn TrayPort>);
                #[cfg(target_os = "macos")]
                let hotkey_host: Option<Arc<dyn MainThreadHost>> =
                    Some(Arc::new(HotkeyLink(Arc::new(Mutex::new(proxy.clone())))));
                #[cfg(not(target_os = "macos"))]
                let hotkey_host: Option<Arc<dyn crate::hotkey::MainThreadHost>> = None;
                let host = ShellHost {
                    availability,
                    port,
                    actions: Some(actions_rx),
                    autostart,
                    hotkey_host,
                };
                let (done_tx, rx) = std_mpsc::channel();
                let finished_proxy = proxy.clone();
                let spawned = std::thread::Builder::new()
                    .name("fluxdown-agent-runtime".to_owned())
                    .spawn(move || {
                        let result = run_runtime(host);
                        if done_tx.send(()).is_err() {
                            tracing::debug!("tray host dropped its runtime completion receiver");
                        }
                        if finished_proxy.send_event(HostEvent::RuntimeFinished).is_err() {
                            tracing::debug!("tray event loop closed before runtime completion");
                        }
                        result
                    });
                match spawned {
                    Ok(handle) => {
                        runtime = Some(handle);
                        done_rx = Some(rx);
                    }
                    Err(error) => {
                        spawn_error = Some(error);
                        finished = true;
                        ui = None;
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            #[cfg(windows)]
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                theme_deadline = Instant::now() + THEME_POLL_INTERVAL;
                *control_flow = ControlFlow::WaitUntil(theme_deadline);
                if let Some(ui) = ui.as_mut() {
                    ui.sync_taskbar_theme();
                }
            }
            #[cfg(target_os = "macos")]
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if let Some(backend) = hotkeys.as_mut() {
                    backend.on_timer(Instant::now());
                }
            }
            #[cfg(target_os = "macos")]
            Event::UserEvent(HostEvent::HotkeySpawn(sink)) => {
                if hotkeys.is_some() {
                    tracing::debug!("global hotkey backend already running; spawn ignored");
                } else if let Some(main_thread) = objc2::MainThreadMarker::new() {
                    hotkeys = Some(crate::platform::hotkey::macos::Backend::new(
                        main_thread,
                        sink,
                    ));
                } else {
                    tracing::error!("global hotkey backend requested off the main thread");
                    sink.report(
                        fluxdown_protocol::shortcuts::GlobalShortcutBackend::Unavailable,
                        Vec::new(),
                    );
                }
            }
            #[cfg(target_os = "macos")]
            Event::UserEvent(HostEvent::HotkeyApply(bindings)) => match hotkeys.as_mut() {
                Some(backend) => backend.apply(bindings),
                None => tracing::debug!("global hotkey table dropped: backend not running"),
            },
            #[cfg(target_os = "macos")]
            Event::UserEvent(HostEvent::ShowMenu) => {
                if let Some(ui) = ui.as_ref() {
                    ui.pop_up_menu();
                }
            }
            Event::UserEvent(HostEvent::Apply(model)) => {
                if let Some(ui) = ui.as_mut() {
                    ui.apply(&model);
                }
            }
            Event::UserEvent(HostEvent::RuntimeFinished) => {
                finished = true;
                // 先销毁托盘图标（Windows 需要 NIM_DELETE，否则任务栏残留幽灵图标）再结束循环。
                ui = None;
                *control_flow = ControlFlow::Exit;
            }
            Event::Opened { urls } => {
                let urls = urls.iter().map(ToString::to_string).collect::<Vec<_>>();
                if actions_tx.send(TrayAction::OpenUrls(urls)).is_err() {
                    tracing::debug!("agent runtime closed before URL-open tray action");
                }
            }
            Event::Reopen { .. } => {
                if actions_tx.send(TrayAction::ShowWindow).is_err() {
                    tracing::debug!("agent runtime closed before window-reopen tray action");
                }
            }
            Event::LoopDestroyed => {
                if !finished {
                    // 系统结束会话：tao 在本回调返回后即结束进程，这里同步等完全退出。
                    if actions_tx.send(TrayAction::SessionEnd).is_err() {
                        tracing::debug!("agent runtime closed before session-end tray action");
                    }
                    if let Some(rx) = done_rx.as_ref() {
                        match rx.recv_timeout(SESSION_END_BUDGET) {
                            Ok(()) => tracing::debug!("agent runtime finished during session end"),
                            Err(std_mpsc::RecvTimeoutError::Timeout) => tracing::warn!(
                                budget_ms = SESSION_END_BUDGET.as_millis(),
                                "agent runtime exceeded session-end shutdown budget; daemon cleanup may be interrupted"
                            ),
                            Err(std_mpsc::RecvTimeoutError::Disconnected) => tracing::warn!(
                                "agent runtime completion channel closed without a session-end completion signal"
                            ),
                        }
                    }
                }
                ui = None;
            }
            _ => {}
        }
        #[cfg(target_os = "macos")]
        if !matches!(
            *control_flow,
            ControlFlow::Exit | ControlFlow::ExitWithCode(_)
        ) && let Some(deadline) = hotkeys
            .as_ref()
            .and_then(crate::platform::hotkey::macos::Backend::next_deadline)
        {
            // 仅在有待授权的双击条目时才设定时器；否则保持纯事件驱动。
            *control_flow = ControlFlow::WaitUntil(deadline);
        }
    });

    if let Some(error) = spawn_error {
        return Err(Box::new(error));
    }
    match runtime.map(std::thread::JoinHandle::join) {
        Some(Ok(result)) => result,
        Some(Err(_)) => Err("fluxdown-agent runtime thread panicked".into()),
        None => Ok(()),
    }
}

#[cfg(target_os = "macos")]
fn configure_platform(event_loop: &mut EventLoop<HostEvent>) {
    use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
}

#[cfg(not(target_os = "macos"))]
fn configure_platform(_event_loop: &mut EventLoop<HostEvent>) {}

#[derive(Clone)]
struct MenuIds {
    show: MenuId,
    settings: MenuId,
    pause: MenuId,
    resume: MenuId,
    cancel_shutdown: MenuId,
    restart: MenuId,
    quit: MenuId,
}

impl MenuIds {
    fn action(&self, id: &MenuId) -> Option<TrayAction> {
        if *id == self.show {
            Some(TrayAction::ShowWindow)
        } else if *id == self.settings {
            Some(TrayAction::ShowSettings)
        } else if *id == self.pause {
            Some(TrayAction::PauseAll)
        } else if *id == self.resume {
            Some(TrayAction::ResumeAll)
        } else if *id == self.cancel_shutdown {
            Some(TrayAction::CancelShutdown)
        } else if *id == self.restart {
            Some(TrayAction::Restart)
        } else if *id == self.quit {
            Some(TrayAction::Quit)
        } else {
            None
        }
    }
}

struct TrayUi {
    /// 字段按声明顺序析构：托盘（删除图标文件）必须先于 `_icon_dir`（删除目录）释放。
    icon: TrayIcon,
    #[cfg(target_os = "linux")]
    _icon_dir: linux::IconDir,
    menu: Menu,
    show: MenuItem,
    settings: MenuItem,
    pause: MenuItem,
    resume: MenuItem,
    cancel_shutdown: MenuItem,
    cancel_inserted: bool,
    restart: MenuItem,
    quit: MenuItem,
    applied: Option<TrayModel>,
    #[cfg(windows)]
    dark_taskbar: bool,
    /// 托盘正在显示用户选择的应用图标：此时不再按任务栏明暗换图。
    #[cfg(windows)]
    custom_icon: bool,
}

impl TrayUi {
    fn create(
        actions: &mpsc::UnboundedSender<TrayAction>,
        proxy: &EventLoopProxy<HostEvent>,
    ) -> Result<Self, TrayUnavailableReason> {
        #[cfg(target_os = "linux")]
        if !linux::status_notifier_host_present() {
            tracing::info!("no StatusNotifier host on the session bus; tray disabled");
            return Err(TrayUnavailableReason::NoHost);
        }
        // Linux 缺少 (ayatana-)appindicator 运行库时 tray-icon 直接 panic。
        let created = std::panic::catch_unwind(AssertUnwindSafe(Self::build));
        let ui = match created {
            Ok(Ok(ui)) => ui,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "could not create tray icon");
                return Err(TrayUnavailableReason::InitFailed);
            }
            Err(_) => {
                tracing::warn!("tray icon backend panicked during initialisation");
                return Err(TrayUnavailableReason::InitFailed);
            }
        };
        let ids = MenuIds {
            show: ui.show.id().clone(),
            settings: ui.settings.id().clone(),
            pause: ui.pause.id().clone(),
            resume: ui.resume.id().clone(),
            cancel_shutdown: ui.cancel_shutdown.id().clone(),
            restart: ui.restart.id().clone(),
            quit: ui.quit.id().clone(),
        };
        let menu_actions = actions.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(action) = ids.action(&event.id)
                && menu_actions.send(action).is_err()
            {
                tracing::debug!("agent runtime closed before tray menu action");
            }
        }));
        let click_actions = actions.clone();
        #[cfg(target_os = "macos")]
        let click_proxy = Mutex::new(proxy.clone());
        #[cfg(not(target_os = "macos"))]
        let _ = proxy;
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let TrayIconEvent::Click {
                button,
                button_state,
                ..
            } = event
            else {
                return;
            };
            match (button, button_state) {
                (MouseButton::Left, MouseButtonState::Up) => {
                    if click_actions.send(TrayAction::ShowWindow).is_err() {
                        tracing::trace!("agent runtime closed before tray click action");
                    }
                }
                #[cfg(target_os = "macos")]
                (MouseButton::Right, MouseButtonState::Down) => {
                    let proxy = click_proxy
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if proxy.send_event(HostEvent::ShowMenu).is_err() {
                        tracing::trace!("tray event loop closed before menu request");
                    }
                }
                _ => {}
            }
        }));
        Ok(ui)
    }

    /// macOS：`NSStatusItem` 一旦挂了菜单，AppKit 对任何点击都直接弹菜单，
    /// `menu_on_left_click(false)` 拦不住。因此平时不挂菜单，右键时临时挂上、弹出
    /// （`performClick` 在菜单关闭后才返回），再摘掉，左键才能走「显示窗口」。
    #[cfg(target_os = "macos")]
    fn pop_up_menu(&self) {
        self.icon.set_menu(Some(Box::new(self.menu.clone())));
        self.icon.show_menu();
        self.icon.set_menu(None);
    }

    fn build() -> Result<Self, String> {
        let show = MenuItem::new("Show Window", true, None);
        let settings = MenuItem::new("Settings", true, None);
        let pause = MenuItem::new("Pause All", true, None);
        let resume = MenuItem::new("Resume All", true, None);
        let cancel_shutdown = MenuItem::new("Cancel Shutdown", true, None);
        let restart = MenuItem::new("Restart", true, None);
        let quit = MenuItem::new("Exit", true, None);
        let menu = Menu::with_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &pause,
            &resume,
            &PredefinedMenuItem::separator(),
            &settings,
            &restart,
            &quit,
        ])
        .map_err(|error| error.to_string())?;
        #[cfg(windows)]
        let dark_taskbar = windows_theme::taskbar_is_dark();
        #[cfg(windows)]
        let bytes = if dark_taskbar {
            DARK_TASKBAR_ICON_BYTES
        } else {
            LIGHT_TASKBAR_ICON_BYTES
        };
        #[cfg(not(windows))]
        let bytes = ICON_BYTES;
        let builder = TrayIconBuilder::new().with_id(TRAY_ID);
        #[cfg(target_os = "linux")]
        let icon_dir = linux::IconDir::for_process();
        #[cfg(target_os = "linux")]
        let builder = builder.with_temp_dir_path(icon_dir.path());
        // macOS 不在状态栏项上常挂菜单，见 [`TrayUi::pop_up_menu`]。
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_menu(Box::new(menu.clone()));
        // macOS 不设 tooltip：macOS 27 上状态栏 tooltip 收不到移出事件会常驻（#953），
        // 倒计时改由菜单项展示（见 `tray_model::COUNTDOWN_IN_MENU`）。
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_tooltip(DEFAULT_TOOLTIP);
        let icon = builder
            .with_menu_on_left_click(false)
            .with_icon(decode_icon(bytes)?)
            .with_icon_as_template(cfg!(target_os = "macos"))
            .build()
            .map_err(|error| error.to_string())?;
        // 首个展示状态到达前保持隐藏：偏好关闭托盘时不应闪现。
        icon.set_visible(false).map_err(|error| error.to_string())?;
        Ok(Self {
            icon,
            #[cfg(target_os = "linux")]
            _icon_dir: icon_dir,
            menu,
            show,
            settings,
            pause,
            resume,
            cancel_shutdown,
            cancel_inserted: false,
            restart,
            quit,
            applied: None,
            #[cfg(windows)]
            dark_taskbar,
            #[cfg(windows)]
            custom_icon: false,
        })
    }

    fn apply(&mut self, model: &TrayModel) {
        if self.applied.as_ref() == Some(model) {
            return;
        }
        #[cfg(not(target_os = "macos"))]
        {
            let previous = self.applied.as_ref().map(|applied| &applied.icon);
            let icon_changed = previous.unwrap_or(&TrayIconSource::Default) != &model.icon;
            #[cfg(windows)]
            let icon_changed = icon_changed
                || self
                    .applied
                    .as_ref()
                    .and_then(|applied| applied.download_dot)
                    != model.download_dot;
            if icon_changed {
                #[cfg(windows)]
                self.apply_icon(&model.icon, model.download_dot);
                #[cfg(not(windows))]
                self.apply_icon(&model.icon);
            }
        }
        self.show.set_text(&model.show_window);
        self.settings.set_text(&model.settings);
        self.pause.set_text(&model.pause_all);
        self.resume.set_text(&model.resume_all);
        self.restart.set_text(&model.restart);
        self.quit.set_text(&model.quit);
        match (&model.cancel_shutdown, self.cancel_inserted) {
            (Some(label), inserted) => {
                self.cancel_shutdown.set_text(label);
                if !inserted {
                    match self
                        .menu
                        .insert(&self.cancel_shutdown, CANCEL_SHUTDOWN_POSITION)
                    {
                        Ok(()) => self.cancel_inserted = true,
                        Err(error) => tracing::warn!(error = %error, "tray menu insert failed"),
                    }
                }
            }
            (None, true) => match self.menu.remove(&self.cancel_shutdown) {
                Ok(()) => self.cancel_inserted = false,
                Err(error) => tracing::warn!(error = %error, "tray menu remove failed"),
            },
            (None, false) => {}
        }
        #[cfg(not(target_os = "macos"))]
        if let Err(error) = self.icon.set_tooltip(Some(&model.tooltip)) {
            tracing::debug!(error = %error, "tray tooltip unsupported");
        }
        if let Err(error) = self.icon.set_visible(model.visible) {
            tracing::warn!(error = %error, "could not change tray visibility");
        }
        self.applied = Some(model.clone());
    }

    /// 切换托盘图标：自选图标直接换图；回到默认时按当前任务栏明暗（Windows）/ 内置图（Linux）还原。
    #[cfg(not(target_os = "macos"))]
    fn apply_icon(&mut self, source: &TrayIconSource, #[cfg(windows)] dot: Option<DownloadDot>) {
        #[cfg(windows)]
        let dark = windows_theme::taskbar_is_dark();
        let bytes: &[u8] = match source {
            TrayIconSource::Image { png, .. } => png,
            #[cfg(windows)]
            TrayIconSource::Default => {
                if dark {
                    DARK_TASKBAR_ICON_BYTES
                } else {
                    LIGHT_TASKBAR_ICON_BYTES
                }
            }
            #[cfg(not(windows))]
            TrayIconSource::Default => ICON_BYTES,
        };
        #[cfg(windows)]
        let decoded = decode_icon_with_dot(bytes, dot);
        #[cfg(not(windows))]
        let decoded = decode_icon(bytes);
        match decoded {
            Ok(icon) => match self.icon.set_icon(Some(icon)) {
                Ok(()) => {
                    #[cfg(windows)]
                    {
                        self.custom_icon = matches!(source, TrayIconSource::Image { .. });
                        self.dark_taskbar = dark;
                    }
                }
                Err(error) => tracing::warn!(%error, "could not change tray icon"),
            },
            Err(error) => tracing::warn!(%error, "could not decode tray icon"),
        }
    }

    #[cfg(windows)]
    fn sync_taskbar_theme(&mut self) {
        if self.custom_icon {
            return;
        }
        let dark = windows_theme::taskbar_is_dark();
        if dark == self.dark_taskbar {
            return;
        }
        let dot = self.applied.as_ref().and_then(|model| model.download_dot);
        self.apply_icon(&TrayIconSource::Default, dot);
    }
}

fn decode_icon(bytes: &[u8]) -> Result<Icon, String> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height).map_err(|error| error.to_string())
}

#[cfg(windows)]
fn decode_icon_with_dot(bytes: &[u8], dot: Option<DownloadDot>) -> Result<Icon, String> {
    let Some(dot) = dot else {
        return decode_icon(bytes);
    };
    let mut image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?
        .into_rgba8();
    let color = match dot {
        DownloadDot::Downloading => [250, 204, 21, 255],
        DownloadDot::Completed => [34, 197, 94, 255],
        DownloadDot::Failed => [239, 68, 68, 255],
    };
    let (width, height) = image.dimensions();
    let radius = f64::from(width.min(height)) * 0.20;
    let center_x = f64::from(width) - radius;
    let center_y = f64::from(height) - radius;
    let inner_radius_squared = (radius * 0.78).powi(2);
    let outer_radius_squared = radius * radius;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let dx = f64::from(x) + 0.5 - center_x;
        let dy = f64::from(y) + 0.5 - center_y;
        let distance_squared = dx * dx + dy * dy;
        if distance_squared <= inner_radius_squared {
            *pixel = image::Rgba(color);
        } else if distance_squared <= outer_radius_squared {
            *pixel = image::Rgba([32, 32, 32, 255]);
        }
    }
    Icon::from_rgba(image.into_raw(), width, height).map_err(|error| error.to_string())
}

#[cfg(windows)]
mod windows_theme {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

    const PERSONALIZE_KEY: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";

    /// 任务栏（系统界面）是否深色；读不到时按 Windows 默认深色任务栏处理。
    pub fn taskbar_is_dark() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(PERSONALIZE_KEY, KEY_READ)
            .and_then(|key| key.get_value::<u32, _>("SystemUsesLightTheme"))
            .map_or(true, |light| light == 0)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::{Path, PathBuf};

    use gtk::gio;
    use gtk::glib;
    use gtk::glib::prelude::ToVariant;

    /// 本进程托盘图标的临时目录，随托盘释放删除。
    ///
    /// tray-icon 按托盘 id 给图标文件命名并在析构时删除；id 固定后，并存的另一个 agent
    /// （拿 `agent.lock` 失败退出前已建过托盘的重复启动、开发版与正式版并行）会写同一个
    /// 文件，先退出的一方删掉另一方仍在引用的图标。按 PID 分目录让文件只归本进程。
    pub struct IconDir(PathBuf);

    impl IconDir {
        /// `$XDG_RUNTIME_DIR/fluxdown-tray-<pid>`，无运行时目录时落在系统临时目录（同 tray-icon）。
        pub fn for_process() -> Self {
            let base = directories::BaseDirs::new()
                .and_then(|dirs| dirs.runtime_dir().map(Path::to_path_buf))
                .unwrap_or_else(std::env::temp_dir);
            Self(base.join(format!("fluxdown-tray-{}", std::process::id())))
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for IconDir {
        fn drop(&mut self) {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(
                    path = %self.0.display(),
                    %error,
                    "could not remove tray icon directory"
                ),
            }
        }
    }

    /// 会话总线上是否有 StatusNotifier 宿主（KDE、带 AppIndicator 扩展的 GNOME 等）；
    /// 没有时 appindicator 图标不可见，关闭窗口会让应用「消失」。
    pub fn status_notifier_host_present() -> bool {
        let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        else {
            return false;
        };
        let Ok(reply_type) = glib::VariantTy::new("(b)") else {
            return false;
        };
        connection
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameHasOwner",
                Some(&("org.kde.StatusNotifierWatcher",).to_variant()),
                Some(reply_type),
                gio::DBusCallFlags::NONE,
                1000,
                gio::Cancellable::NONE,
            )
            .ok()
            .and_then(|reply| reply.get::<(bool,)>())
            .is_some_and(|(present,)| present)
    }
}
