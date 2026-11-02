//! 独立任务详情窗口：FluxDown 辅助窗口标题栏（与设置 / 新建下载等窗口一致），
//! 标题随任务名解析和变化实时刷新。

use std::{rc::Rc, sync::Arc};

use fluxdown_ui_downloads::{DownloadHostActions, TaskDetailView};
use fluxdown_ui_shell::{AuxiliaryWindowView, auxiliary_window_options};
use gpui::{App, AppContext as _, SharedString, px, size};
use gpui_component::Root;

use crate::{
    app::Desktop,
    downloads_port::AgentDownloadsPort,
    session::attach,
    windows::{RememberedWindow, WindowKey, WindowRegistry},
};

const TASK_WINDOW_SIZE: gpui::Size<gpui::Pixels> = size(px(560.), px(420.));
const TASK_WINDOW_MIN_SIZE: gpui::Size<gpui::Pixels> = size(px(420.), px(220.));

/// 打开或聚焦任务详情窗口。
pub fn open(cx: &mut App, task_id: String) {
    let desktop = Desktop::global(cx);
    let translator = desktop.translator.clone();
    let session = desktop.session.clone();
    let client = desktop.client.clone();
    let placeholder_title = translator.read(cx).text("detail").to_owned();

    let mut options = auxiliary_window_options(placeholder_title);
    options.window_min_size = Some(TASK_WINDOW_MIN_SIZE);
    WindowRegistry::restore_bounds(
        RememberedWindow::TaskDetail,
        &mut options,
        TASK_WINDOW_SIZE,
        cx,
    );

    let key = WindowKey::TaskDetail(task_id.clone());
    WindowRegistry::open_or_focus(cx, key, options, move |window, cx| {
        let downloads_port = Arc::new(AgentDownloadsPort::new(client.clone()));
        let host = DownloadHostActions {
            open_group_window: Some(Rc::new(|group_id, _window, cx| {
                crate::windows::group_detail::open(cx, group_id);
            })),
            on_user_started: Some(Rc::new(crate::progress_windows::user_started)),
            plugin_tab_view: Some(crate::plugin_contributions::tab_view_factory()),
            ..DownloadHostActions::default()
        };
        let detail = cx.new(|cx| {
            TaskDetailView::new(
                translator.clone(),
                task_id.clone(),
                downloads_port,
                host,
                window,
                cx,
            )
        });
        attach(&session, &detail, cx);
        crate::plugin_contributions::attach_detail_window(&detail, cx);
        let window_view = cx.new(|cx| {
            AuxiliaryWindowView::new(translator.clone(), "detail", detail.clone().into(), cx)
        });

        let root = cx.new(|cx| {
            let root = Root::new(window_view.clone(), window, cx);
            cx.observe_in(&detail, window, move |_, detail, window, cx| {
                let title = detail
                    .read(cx)
                    .file_name()
                    .unwrap_or_else(|| detail.read(cx).task_id().to_owned());
                window.set_window_title(&title);
                window_view.update(cx, |view, cx| {
                    view.set_title(Some(SharedString::from(title)), cx);
                });
            })
            .detach();
            cx.subscribe_in(&detail, window, |_, _, event, window, _cx| {
                if matches!(event, fluxdown_ui_downloads::TaskDetailEvent::Closed) {
                    window.remove_window();
                }
            })
            .detach();
            root
        });
        WindowRegistry::persist_bounds(RememberedWindow::TaskDetail, client, &root, window, cx);
        root
    });
}
