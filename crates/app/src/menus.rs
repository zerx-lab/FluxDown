//! 应用菜单（macOS 原生 / Windows·Linux 标题栏菜单栏）、全局键位与应用级动作处理。

use std::collections::BTreeMap;

use fluxdown_ui_downloads::actions as dl;
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_shell::ShellView;
use gpui::{
    Action, App, Context, Entity, Focusable as _, Global, Menu, MenuItem, SharedString, Window,
};
use gpui_component::{GlobalState, menu::AppMenuBar};

use crate::{
    actions::{
        About, BringAllToFront, CheckUpdate, CloseWindow, Hide, HideOthers, MinimizeWindow,
        OpenLogsFolder, OpenSettings, OpenWebsite, Quit, ShowAll, ShowMainWindow,
        ToggleCommandPalette, ToggleFullScreen, ZoomWindow,
    },
    app::Desktop,
    windows::{WindowKey, WindowRegistry, confirm_active_tasks},
};

const WEBSITE_URL: &str = "https://fluxdown.com";

/// 设备本地偏好：Windows / Linux 标题栏是否显示应用菜单栏（缺省隐藏）。不在云同步目录内，
/// 写入走 `sync:false`，只影响本机；macOS 走原生菜单，不读此键。
pub(crate) const SHOW_MENU_BAR_PREF: &str = "ui.show_menu_bar";

/// Windows / Linux 标题栏菜单栏实体（所有主窗口共用；macOS 不创建）。
struct TitleMenuBar(Entity<AppMenuBar>);

impl Global for TitleMenuBar {}

fn t(translator: &Translator, key: &str) -> SharedString {
    SharedString::from(translator.text(key).to_owned())
}

/// 菜单树（macOS 原生菜单与标题栏菜单栏共用一份）。
pub fn build_menus(translator: &Translator) -> Vec<Menu> {
    let mut menus = Vec::with_capacity(7);
    if cfg!(target_os = "macos") {
        menus.push(Menu::new("FluxDown").items([
            MenuItem::action(t(translator, "menuAbout"), About),
            MenuItem::separator(),
            MenuItem::action(t(translator, "menuSettings"), OpenSettings),
            MenuItem::separator(),
            MenuItem::action(t(translator, "menuHide"), Hide),
            MenuItem::action(t(translator, "menuHideOthers"), HideOthers),
            MenuItem::action(t(translator, "menuShowAll"), ShowAll),
            MenuItem::separator(),
            MenuItem::action(t(translator, "menuQuit"), Quit),
        ]));
    }
    let mut file = vec![
        MenuItem::action(t(translator, "menuNewDownload"), dl::NewDownload),
        MenuItem::action(t(translator, "openTorrentFile"), dl::OpenTorrentFile),
        MenuItem::action(t(translator, "seedLocalTitle"), dl::SeedLocal),
    ];
    file.push(MenuItem::separator());
    if cfg!(target_os = "macos") {
        file.push(MenuItem::action(
            t(translator, "menuCloseWindow"),
            CloseWindow,
        ));
    } else {
        file.push(MenuItem::action(t(translator, "menuQuit"), Quit));
    }
    menus.push(Menu::new(t(translator, "menuFile")).items(file));
    menus.push(Menu::new(t(translator, "menuTasks")).items([
        MenuItem::action(t(translator, "pause"), dl::PauseSelected),
        MenuItem::action(t(translator, "resume"), dl::ResumeSelected),
        MenuItem::action(t(translator, "deleteTask"), dl::DeleteSelected),
        MenuItem::action(
            t(translator, "deleteTaskAndFile"),
            dl::DeleteSelectedWithFiles,
        ),
        MenuItem::separator(),
        MenuItem::action(t(translator, "selectAll"), dl::SelectAllTasks),
        MenuItem::action(t(translator, "pauseAll"), dl::PauseAll),
        MenuItem::action(t(translator, "resumeAll"), dl::ResumeAll),
        MenuItem::action(t(translator, "menuClearFinished"), dl::ClearFinished),
        MenuItem::separator(),
        MenuItem::action(t(translator, "openFile"), dl::OpenSelected),
        MenuItem::action(t(translator, "openFolder"), dl::RevealSelected),
        MenuItem::action(t(translator, "copyFileAction"), dl::CopySelectedFiles),
        MenuItem::action(t(translator, "copyUrl"), dl::CopySelectedUrl),
        MenuItem::action(t(translator, "btCopyMagnet"), dl::CopyMagnetSelected),
        MenuItem::action(t(translator, "btExportTorrent"), dl::ExportTorrentSelected),
        MenuItem::action(t(translator, "renameTask"), dl::RenameSelected),
        MenuItem::action(t(translator, "webChangeUrl"), dl::ChangeSelectedUrl),
        MenuItem::action(t(translator, "editThreads"), dl::ChangeSelectedSegments),
        MenuItem::action(t(translator, "verifyTaskIntegrity"), dl::VerifySelected),
        MenuItem::action(t(translator, "redownloadTask"), dl::RedownloadSelected),
        MenuItem::action(t(translator, "boostDownload"), dl::ToggleBoostSelected),
        MenuItem::action(t(translator, "menuOpenInWindow"), dl::OpenSelectedInWindow),
    ]));
    let mut view = vec![
        MenuItem::action(t(translator, "menuCycleDensity"), dl::CycleDensity),
        MenuItem::action(t(translator, "menuCycleGroupBy"), dl::CycleGroupBy),
        MenuItem::action(t(translator, "menuCycleSort"), dl::CycleSort),
        MenuItem::separator(),
        MenuItem::action(t(translator, "menuDetailPanel"), dl::ToggleDetailPanel),
    ];
    if cfg!(target_os = "macos") {
        view.push(MenuItem::separator());
        view.push(MenuItem::action(
            t(translator, "menuToggleFullScreen"),
            ToggleFullScreen,
        ));
    }
    menus.push(Menu::new(t(translator, "menuView")).items(view));
    let mut tools = Vec::with_capacity(4);
    if !cfg!(target_os = "macos") {
        tools.push(MenuItem::action(
            t(translator, "menuSettings"),
            OpenSettings,
        ));
    }
    tools.extend([
        MenuItem::action(t(translator, "manageQueueAction"), dl::OpenQueueManager),
        MenuItem::action(t(translator, "menuCheckForUpdates"), CheckUpdate),
        MenuItem::action(t(translator, "menuOpenLogsFolder"), OpenLogsFolder),
    ]);
    menus.push(Menu::new(t(translator, "menuTools")).items(tools));
    if cfg!(target_os = "macos") {
        menus.push(Menu::new(t(translator, "menuWindow")).items([
            MenuItem::action(t(translator, "menuMinimize"), MinimizeWindow),
            MenuItem::action(t(translator, "menuZoom"), ZoomWindow),
            MenuItem::separator(),
            MenuItem::action(t(translator, "menuBringAllToFront"), BringAllToFront),
        ]));
    }
    let mut help = vec![MenuItem::action(t(translator, "menuWebsite"), OpenWebsite)];
    if !cfg!(target_os = "macos") {
        help.push(MenuItem::action(t(translator, "menuAbout"), About));
    }
    menus.push(Menu::new(t(translator, "menuHelp")).items(help));
    menus
}

/// 装配应用菜单：macOS 原生菜单栏；Windows / Linux 另建标题栏菜单栏（是否显示由
/// [`SHOW_MENU_BAR_PREF`] 决定）。翻译变化时重建。
pub fn install(cx: &mut App, translator: &Entity<Translator>) {
    apply_menus(translator, cx);
    if !cfg!(target_os = "macos") {
        let bar = AppMenuBar::new(cx);
        cx.set_global(TitleMenuBar(bar));
    }
    cx.observe(translator, move |translator, cx| {
        apply_menus(&translator, cx);
    })
    .detach();
}

pub(crate) fn apply_menus(translator: &Entity<Translator>, cx: &mut App) {
    let translator = translator.read(cx).clone();
    cx.set_menus(build_menus(&translator));
    if cfg!(target_os = "macos") {
        return;
    }
    GlobalState::global_mut(cx).set_app_menus(
        build_menus(&translator)
            .into_iter()
            .map(Menu::owned)
            .collect(),
    );
    if let Some(TitleMenuBar(bar)) = cx.try_global::<TitleMenuBar>() {
        bar.clone().update(cx, |bar, cx| bar.reload(cx));
    }
}

/// 偏好 → 标题栏菜单栏可见性；幂等，窗口创建与每次偏好变化都走这里。
pub(crate) fn apply_menu_bar_visibility(
    shell: &mut ShellView,
    preferences: &BTreeMap<String, serde_json::Value>,
    cx: &mut Context<ShellView>,
) {
    let visible = preferences
        .get(SHOW_MENU_BAR_PREF)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let bar = visible
        .then(|| {
            cx.try_global::<TitleMenuBar>()
                .map(|TitleMenuBar(bar)| bar.clone())
        })
        .flatten();
    shell.set_menu_bar(bar, cx);
}

/// 应用级动作（不依赖具体窗口）。窗口级动作作用于当前活动窗口。
pub fn install_global_actions(cx: &mut App) {
    cx.on_action(|_: &OpenSettings, cx| crate::windows::settings::open(cx));
    cx.on_action(|_: &ShowMainWindow, cx| {
        crate::windows::main::open(cx);
        cx.activate(true);
    });
    cx.on_action(|_: &Quit, cx| request_quit(cx));
    cx.on_action(|_: &CloseWindow, cx| WindowRegistry::close_active_window(cx));
    cx.on_action(|_: &MinimizeWindow, cx| with_active_window(cx, |w| w.minimize_window()));
    cx.on_action(|_: &ZoomWindow, cx| with_active_window(cx, |w| w.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| {
        with_active_window(cx, |w| w.toggle_fullscreen());
    });
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
    cx.on_action(|_: &OpenWebsite, cx| cx.open_url(WEBSITE_URL));
    cx.on_action(|_: &dl::NewDownload, cx| crate::windows::new_download::open_default(cx));
    cx.on_action(|_: &dl::OpenQueueManager, cx| crate::windows::queue_manager::open(cx));
    cx.on_action(|_: &CheckUpdate, cx| crate::update_notices::check_now(cx));
    cx.on_action(|_: &OpenLogsFolder, cx| open_logs_folder(cx));
    cx.on_action(|_: &About, cx| {
        crate::windows::settings::reveal(
            cx,
            fluxdown_ui_settings::SettingsTarget {
                page: "about",
                tab: "",
                row: None,
            },
        );
    });
    cx.on_action(|_: &ToggleCommandPalette, cx| crate::command_palette::toggle(cx));
    install_download_fallbacks(cx);
}

/// 下载页动作的应用级兜底。这些动作的处理器挂在下载页根元素上，只有派发路径经过下载页才会
/// 命中；Windows / Linux 标题栏菜单以「打开菜单时的焦点」为派发目标，焦点为空（启动后未点过
/// 列表）、在标题栏搜索框或当前不在下载页时，动作只沿标题栏冒泡、无人处理。下载页处理过的
/// 动作不会冒泡到这里，因此焦点在下载页内时行为不变。
fn install_download_fallbacks(cx: &mut App) {
    forward_to_downloads::<dl::OpenTorrentFile>(cx);
    forward_to_downloads::<dl::SeedLocal>(cx);
    forward_to_downloads::<dl::PauseSelected>(cx);
    forward_to_downloads::<dl::ResumeSelected>(cx);
    forward_to_downloads::<dl::DeleteSelected>(cx);
    forward_to_downloads::<dl::DeleteSelectedWithFiles>(cx);
    forward_to_downloads::<dl::SelectAllTasks>(cx);
    forward_to_downloads::<dl::PauseAll>(cx);
    forward_to_downloads::<dl::ResumeAll>(cx);
    forward_to_downloads::<dl::ClearFinished>(cx);
    forward_to_downloads::<dl::OpenSelected>(cx);
    forward_to_downloads::<dl::RevealSelected>(cx);
    forward_to_downloads::<dl::CopySelectedFiles>(cx);
    forward_to_downloads::<dl::CopySelectedUrl>(cx);
    forward_to_downloads::<dl::CopyMagnetSelected>(cx);
    forward_to_downloads::<dl::ExportTorrentSelected>(cx);
    forward_to_downloads::<dl::RenameSelected>(cx);
    forward_to_downloads::<dl::ChangeSelectedUrl>(cx);
    forward_to_downloads::<dl::ChangeSelectedSegments>(cx);
    forward_to_downloads::<dl::VerifySelected>(cx);
    forward_to_downloads::<dl::RedownloadSelected>(cx);
    forward_to_downloads::<dl::ToggleBoostSelected>(cx);
    forward_to_downloads::<dl::OpenSelectedInWindow>(cx);
    forward_to_downloads::<dl::CycleDensity>(cx);
    forward_to_downloads::<dl::CycleGroupBy>(cx);
    forward_to_downloads::<dl::CycleSort>(cx);
    forward_to_downloads::<dl::ToggleDetailPanel>(cx);
}

fn forward_to_downloads<A: Action>(cx: &mut App) {
    cx.on_action(|action: &A, cx| {
        let action = action.boxed_clone();
        cx.defer(move |cx| dispatch_in_downloads(action, cx));
    });
}

/// 主窗口切到下载页、聚焦下载页根元素，再在其上派发动作。
///
/// 焦点已在下载页内说明下载页自己让出了该动作（如输入框聚焦时的字母键守卫），不再接管，
/// 否则快捷键会抢走输入框焦点。刚切换路由时下载页尚未进入派发树，等它渲染一帧后再派发：
/// 首个 next-frame 回调在该帧绘制前执行，第二个才能看到包含下载页的已渲染帧。
fn dispatch_in_downloads(action: Box<dyn Action>, cx: &mut App) {
    let Some(downloads) = Desktop::global(cx)
        .main_downloads
        .as_ref()
        .and_then(gpui::WeakEntity::upgrade)
    else {
        return;
    };
    let Some(handle) = WindowRegistry::handle(cx, &WindowKey::Main) else {
        return;
    };
    let focus = downloads.read(cx).focus_handle(cx);
    let rendered = Desktop::global(cx)
        .main_shell
        .as_ref()
        .and_then(gpui::WeakEntity::upgrade)
        .is_some_and(|shell| {
            shell.read(cx).active_route() == Some(&crate::activity::DOWNLOADS_ROUTE)
        });
    let declined = match handle.update(cx, |_, window, cx| focus.contains_focused(window, cx)) {
        Ok(declined) => declined,
        Err(error) => {
            log::debug!("main window released before menu action dispatch: {error:#}");
            return;
        }
    };
    if declined {
        return;
    }
    crate::command_palette::navigate_now(cx, crate::activity::DOWNLOADS_ROUTE);
    if let Err(error) = handle.update(cx, move |_, window, cx| {
        focus.focus(window, cx);
        if rendered {
            focus.dispatch_action(action.as_ref(), window, cx);
        } else {
            window.on_next_frame(move |window, _| {
                window.on_next_frame(move |window, cx| {
                    focus.dispatch_action(action.as_ref(), window, cx);
                });
            });
        }
    }) {
        log::debug!("main window released before menu action dispatch: {error:#}");
    }
}

/// 全局动作里操作活动窗口必须 defer：有活动窗口时，菜单点击与键盘一样经
/// `App::dispatch_action` 在该窗口的 update 栈内分发，同步 `handle.update` 拿不到窗口会静默失败。
fn with_active_window(cx: &mut App, f: impl FnOnce(&Window) + 'static) {
    cx.defer(move |cx| {
        if let Some(window) = WindowRegistry::focused_window(cx)
            && let Err(error) = window.update(cx, |_, window, _| f(window))
        {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    });
}

/// 退出（菜单 / ⌘Q）：完全退出，后台 agent 与 daemon 一起停止。有活跃任务时先在当前窗口
/// 确认「下载将暂停」。只退出界面、保留后台，走关闭窗口（托盘驻留时）。
pub fn request_quit(cx: &mut App) {
    if Desktop::active_task_count(cx) == 0 {
        crate::lifecycle::quit_everything(cx);
        return;
    }
    cx.defer(|cx| {
        let Some(window) = WindowRegistry::focused_window(cx)
            .or_else(|| WindowRegistry::handle(cx, &WindowKey::Main))
        else {
            crate::lifecycle::quit_everything(cx);
            return;
        };
        if let Err(error) = window.update(cx, |_, window, cx| {
            confirm_active_tasks(window, cx, |_window, cx| {
                crate::lifecycle::quit_everything(cx);
            });
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    });
}

fn open_logs_folder(cx: &mut App) {
    // 桌面端自身日志目录本地已知；agent 不可达时日志入口恰是最需要的。
    let dir = crate::app::agent_data_dir().join("logs");
    #[cfg(not(windows))]
    cx.reveal_path(&dir);
    #[cfg(windows)]
    cx.spawn(async move |_| {
        if let Err(error) = crate::shell_open::directory(dir.to_string_lossy().into_owned()).await {
            log::warn!("could not open desktop log directory: {error:?}");
        }
    })
    .detach();
}
