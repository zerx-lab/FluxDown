//! 主窗口：shell + 下载页 + RSS 页；关闭策略与边界持久化。

use std::{rc::Rc, sync::Arc};

use fluxdown_ui_account::AccountRailButton;
use fluxdown_ui_components::FluxIcon;
use fluxdown_ui_downloads::{DownloadHostActions, DownloadView};
use fluxdown_ui_rss::RssView;
use fluxdown_ui_settings::{PushHost, PushView, UpdateButton};
use fluxdown_ui_shell::{ShellAction, ShellChrome, ShellRoute, ShellView, main_window_options};
use gpui::{App, AppContext as _, Window, WindowHandle, px, size};
use gpui_component::{Icon, Root};

use crate::{
    activity::{self, ActivityEntry},
    app::Desktop,
    capability_ports::AgentRssPort,
    downloads_port::AgentDownloadsPort,
    session::attach,
    windows::{RememberedWindow, WindowKey, WindowRegistry, confirm_active_tasks},
};

const MAIN_WINDOW_SIZE: gpui::Size<gpui::Pixels> = size(px(1120.), px(760.));

/// 显示 / 恢复 / 聚焦主窗口；窗口已关闭时重建。
pub fn reveal(cx: &mut App) {
    open(cx);
    cx.activate(true);
}

/// 打开或聚焦主窗口。返回新建窗口句柄（已开时 `None`）。
pub fn open(cx: &mut App) -> Option<WindowHandle<Root>> {
    open_with(cx, true)
}

/// 开机自启（托盘不可见时）：新建的主窗口不抢焦点，随后由调用方最小化。
pub fn open_unfocused(cx: &mut App) -> Option<WindowHandle<Root>> {
    open_with(cx, false)
}

fn open_with(cx: &mut App, focus: bool) -> Option<WindowHandle<Root>> {
    let desktop = Desktop::global(cx);
    let translator = desktop.translator.clone();
    let session = desktop.session.clone();
    let client = desktop.client.clone();
    let settings_store = desktop.settings_store.clone();
    let mut options = main_window_options();
    options.focus = focus;
    WindowRegistry::restore_bounds(RememberedWindow::Main, &mut options, MAIN_WINDOW_SIZE, cx);

    let handle = WindowRegistry::open_or_focus(cx, WindowKey::Main, options, move |window, cx| {
        let downloads_port = Arc::new(AgentDownloadsPort::new(client.clone()));
        let downloads =
            cx.new(|cx| DownloadView::new(translator.clone(), downloads_port, window, cx));
        watch_import_suspension(&client, &downloads, cx);
        let rss_port = Arc::new(AgentRssPort::new(client.clone()));
        let rss = cx.new(|cx| RssView::new(translator.clone(), rss_port, window, cx));
        let push = cx.new(|cx| {
            PushView::new(
                translator.clone(),
                settings_store.clone(),
                PushHost {
                    open_account: Rc::new(|_, cx| activity::open_account(cx)),
                },
                cx,
            )
        });
        let downloads_title_bar = downloads.update(cx, |downloads, cx| downloads.new_title_bar(cx));

        let downloads_sidebar = downloads.read(cx).sidebar_state();
        let rss_sidebar = rss.read(cx).sidebar_state();
        // 推送通知页没有侧边菜单，不登记侧栏；RSS 的切换按钮由 shell 提供。
        let mut routes = Vec::new();
        let mut actions = Vec::new();
        for entry in ActivityEntry::ALL {
            let button_id = entry.button_id();
            let optional = entry.toggle().is_some();
            match entry {
                ActivityEntry::Downloads => routes.push(
                    ShellRoute::new(
                        crate::activity::DOWNLOADS_ROUTE,
                        button_id,
                        "activity-downloads-tooltip",
                        entry.label_key(),
                        Icon::new(FluxIcon::Download),
                        downloads.clone().into(),
                    )
                    .with_title_bar(downloads_title_bar.clone())
                    .with_sidebar(downloads_sidebar.clone())
                    .optional(optional),
                ),
                ActivityEntry::Rss => routes.push(
                    ShellRoute::new(
                        crate::activity::RSS_ROUTE,
                        button_id,
                        "activity-rss-tooltip",
                        entry.label_key(),
                        Icon::new(FluxIcon::Rss),
                        rss.clone().into(),
                    )
                    .with_sidebar(rss_sidebar.clone())
                    .optional(optional),
                ),
                ActivityEntry::Webhooks => routes.push(
                    ShellRoute::new(
                        crate::activity::WEBHOOKS_ROUTE,
                        button_id,
                        "activity-webhooks-tooltip",
                        entry.label_key(),
                        Icon::new(FluxIcon::Webhook),
                        push.clone().into(),
                    )
                    .optional(optional),
                ),
                ActivityEntry::Account => {
                    let host = Desktop::global(cx).account_host.clone();
                    let button = cx.new(|cx| {
                        AccountRailButton::new(
                            host,
                            ShellView::ACTION_BUTTON_SIZE,
                            |_, cx| activity::open_account(cx),
                            cx,
                        )
                    });
                    actions.push(
                        ShellAction::view(button_id, entry.label_key(), button.into())
                            .layout_id(entry.layout_id())
                            .optional(optional),
                    );
                }
                ActivityEntry::Settings => actions.push(
                    ShellAction::new(
                        button_id,
                        "activity-settings-tooltip",
                        entry.label_key(),
                        Icon::new(FluxIcon::Settings),
                        move |_, cx| crate::windows::settings::open(cx),
                    )
                    .layout_id(entry.layout_id())
                    .optional(optional),
                ),
            }
        }
        let chrome = ShellChrome {
            widgets: crate::chrome_layout::widgets(&downloads, cx),
            catalog: crate::chrome_layout::catalog(),
            on_layout_change: Rc::new(|value, cx| {
                Desktop::set_pref(cx, crate::chrome_layout::CHROME_LAYOUT_PREF, value);
            }),
        };
        let shell = cx.new(|cx| ShellView::new(translator.clone(), routes, actions, chrome, cx));
        let update_button = cx.new(|cx| {
            UpdateButton::new(
                translator.clone(),
                settings_store.clone(),
                Rc::new(crate::windows::update_prompt::open),
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.observe_widget_source(&downloads, cx);
            shell.set_status_accessory(Some(update_button.into()), cx);
            let preferences = Desktop::preferences(cx).clone();
            activity::apply_visibility(shell, &preferences, cx);
            crate::chrome_layout::apply(shell, &preferences, cx);
            crate::menus::apply_menu_bar_visibility(shell, &preferences, cx);
        });

        let settings_for_categories = settings_store.clone();
        let translator_for_categories = translator.clone();
        let shutdown_status = crate::power::status(cx);
        let shutdown_port = crate::power::shutdown_port(cx);
        downloads.update(cx, |downloads, _| {
            let host = DownloadHostActions {
                open_new_download: Some(Rc::new(|context, _, cx| {
                    crate::windows::new_download::open(cx, context);
                })),
                open_task_window: Some(Rc::new(|task_id, _, cx| {
                    crate::windows::task_detail::open(cx, task_id);
                })),
                open_group_window: Some(Rc::new(|group_id, _, cx| {
                    crate::windows::group_detail::open(cx, group_id);
                })),
                open_queue_manager: Some(Rc::new(|_, cx| {
                    crate::windows::queue_manager::open(cx);
                })),
                open_add_device: Some(Rc::new(crate::account_host::open_add_device_dialog)),
                rename_linked_device: Some(Rc::new(crate::account_host::open_link_rename_dialog)),
                unpair_linked_device: Some(Rc::new(crate::account_host::confirm_link_unpair)),
                open_file_conflicts: Some(Rc::new(|window, cx| {
                    crate::windows::file_conflict::open_from_window(window, cx);
                })),
                open_category_editor: Some(Rc::new(move |id, window, cx| {
                    let translator = translator_for_categories.read(cx).clone();
                    fluxdown_ui_settings::open_category_editor(
                        settings_for_categories.clone(),
                        &translator,
                        id,
                        window,
                        cx,
                    );
                })),
                shutdown_status,
                shutdown: shutdown_port,
                on_user_started: Some(Rc::new(crate::progress_windows::user_started)),
                run_plugin_command: Some(crate::plugin_contributions::command_runner()),
                plugin_tab_view: Some(crate::plugin_contributions::tab_view_factory()),
            };
            downloads.set_host_actions(host);
        });

        attach(&session, &downloads, cx);
        attach(&session, &rss, cx);
        {
            let desktop = Desktop::global_mut(cx);
            desktop.main_downloads = Some(downloads.downgrade());
            desktop.main_shell = Some(shell.downgrade());
        }
        crate::plugin_contributions::attach_main_window(cx);

        let root = cx.new(|cx| Root::new(shell, window, cx));
        WindowRegistry::persist_bounds(RememberedWindow::Main, client, &root, window, cx);
        install_close_policy(window, cx);
        root
    });
    if handle.is_some() {
        // 启动前就已存在的入站配对请求、窗口尚未就绪时跳过的插件熔断提示：窗口就绪后补弹。
        cx.defer(crate::account_host::replay_pending);
        cx.defer(crate::plugin_notices::replay_pending);
        cx.defer(crate::update_notices::replay_pending);
        cx.defer(crate::service_notices::replay_pending);
    }
    handle
}

/// 导入闸门挂起状态同步到下载页：挂起期间「全部开始」必须可点（它是唯一解除入口）。
fn watch_import_suspension(
    client: &crate::agent_client::AgentClient,
    downloads: &gpui::Entity<DownloadView>,
    cx: &mut App,
) {
    let mut state = client.submissions.subscribe();
    let suspended = state.borrow_and_update().suspended();
    downloads.update(cx, |view, cx| view.set_import_suspended(suspended, cx));
    let downloads = downloads.downgrade();
    cx.spawn(async move |cx| {
        while state.changed().await.is_ok() {
            let suspended = state.borrow_and_update().suspended();
            if downloads
                .update(cx, |view, cx| view.set_import_suspended(suspended, cx))
                .is_err()
            {
                // 主窗口已关闭：重建时会重新订阅。
                return;
            }
        }
        log::debug!("download submission state channel closed");
    })
    .detach();
}

/// 关闭策略：原生关闭按钮（`windowShouldClose:`）与 ⌘W 共用一份判定。
fn install_close_policy(window: &mut Window, cx: &mut App) {
    window.on_window_should_close(cx, should_close);
}

/// 主窗口关闭：agent 托盘驻留 → 只关窗（最后一个窗口关闭后界面进程退出，下载与队列在后台
/// 继续）；否则等同「退出」——有活跃任务时先确认，随后完全退出（后台随界面一起停止）。
/// 尚未收到首个快照时驻留策略未知，按只关窗处理，绝不误停后台。
pub fn should_close(window: &mut Window, cx: &mut App) -> bool {
    let desktop = Desktop::global(cx);
    if desktop.shell.resident || desktop.session.read(cx).latest().is_none() {
        return true;
    }
    if Desktop::active_task_count(cx) == 0 {
        crate::lifecycle::quit_everything(cx);
    } else {
        confirm_active_tasks(window, cx, |_window, cx| {
            crate::lifecycle::quit_everything(cx);
        });
    }
    false
}
