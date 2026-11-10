//! 引擎发起的交互选择请求（HLS 画质 / BT 文件 / 插件变体）：每个 `request_id` 一个独立
//! `Floating` 窗口，互不阻塞、可并存；到期由 daemon 自动按默认值解析，`SelectionResolved`
//! 事件负责关窗。`FileExists` 不在此开窗，由聚合的 [`crate::windows::file_conflict`] 承载。

use std::sync::Arc;

use fluxdown_protocol::{
    AgentEvent, DaemonEvent, SelectionKind, SelectionRequestDto, ServiceEvent,
};
use fluxdown_ui_downloads::{SelectionPlan, SelectionView};
use fluxdown_ui_shell::{AuxiliaryWindowView, auxiliary_window_options};
use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowKind, px, size};
use gpui_component::Root;

use crate::{
    app::Desktop,
    downloads_port::AgentDownloadsPort,
    session::{SessionSignal, agent_body},
    windows::{RememberedWindow, WindowKey, WindowRegistry},
};

const HLS_VARIANT_SIZE: gpui::Size<gpui::Pixels> = size(px(520.), px(440.));
// 标题下多一行保存目录，默认高度从 580 提到 640，避免文件列表被挤没。
const BT_SIZE: gpui::Size<gpui::Pixels> = size(px(760.), px(640.));
const PLUGIN_SIZE: gpui::Size<gpui::Pixels> = size(px(560.), px(560.));

/// 订阅会话：回放启动时已挂起的选择，跟随新到达/已解决的请求开关窗口。
pub fn install(cx: &mut App) {
    let session = Desktop::global(cx).session.clone();
    for request in pending_selections(&session, cx) {
        open(cx, request);
    }
    cx.subscribe(&session, |_, signal, cx| match signal {
        SessionSignal::Snapshot(snapshot) => {
            if let Some(body) = agent_body(snapshot) {
                for request in body.daemon.pending_selections.clone() {
                    open(cx, request);
                }
            }
        }
        SessionSignal::Event(frame) => match &frame.event {
            ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::SelectionPending(request)))
            | ServiceEvent::Daemon(DaemonEvent::SelectionPending(request)) => {
                open(cx, request.clone());
            }
            // daemon 晚于 agent 就绪 / 重连：挂起的选择随替换快照到达，而不是逐条事件。
            ServiceEvent::Agent(AgentEvent::DaemonSnapshotReplaced(snapshot))
            | ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(snapshot)))
            | ServiceEvent::Daemon(DaemonEvent::SnapshotReplaced(snapshot)) => {
                for request in snapshot.pending_selections.clone() {
                    open(cx, request);
                }
            }
            ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::SelectionResolved {
                request_id,
            }))
            | ServiceEvent::Daemon(DaemonEvent::SelectionResolved { request_id }) => {
                WindowRegistry::close(cx, &WindowKey::Selection(request_id.clone()));
            }
            _ => {}
        },
        SessionSignal::Stale
        | SessionSignal::Fatal(_)
        | SessionSignal::ServiceStopped
        | SessionSignal::ServiceFailed(_) => {}
    })
    .detach();
}

fn pending_selections(
    session: &gpui::Entity<crate::session::AgentSession>,
    cx: &App,
) -> Vec<SelectionRequestDto> {
    session
        .read(cx)
        .agent_snapshot()
        .map(|body| body.daemon.pending_selections.clone())
        .unwrap_or_default()
}

/// 已开则跳过（去重）；否则按 `kind` 定尺寸、居中主窗口所在显示器打开。
fn open(cx: &mut App, request: SelectionRequestDto) {
    let Some(plan) = SelectionPlan::for_request(&request) else {
        // 「文件已存在」走聚合窗口。
        return;
    };
    let key = WindowKey::Selection(request.request_id.clone());
    if WindowRegistry::is_open(cx, &key) {
        return;
    }
    let desktop = Desktop::global(cx);
    let translator = desktop.translator.clone();
    let client = desktop.client.clone();
    let session = desktop.session.clone();
    let task_name = task_name_for(&session, cx, &request.task_id);
    let title_key: &'static str = match &request.kind {
        SelectionKind::Hls { .. } => "hlsQualityTitle",
        SelectionKind::Bt { .. } => "btFileSelectTitle",
        SelectionKind::Variant { .. } => "resolveVariantTitle",
        SelectionKind::Plugin { .. } => "pluginChoiceTitle",
        SelectionKind::FileExists { .. } => return,
    };
    let window_size = if matches!(request.kind, SelectionKind::Plugin { .. }) {
        PLUGIN_SIZE
    } else {
        HLS_VARIANT_SIZE
    };
    let is_bt = matches!(request.kind, SelectionKind::Bt { .. });
    let display_id = WindowRegistry::handle(cx, &WindowKey::Main)
        .and_then(|handle| {
            handle
                .update(cx, |_, window, cx| window.display(cx))
                .ok()
                .flatten()
        })
        .map(|display| display.id());
    let mut options = auxiliary_window_options(translator.read(cx).text(title_key).to_owned());
    options.display_id = display_id;
    options.window_min_size = None;
    // BT 窗口有「浏览」保存目录：macOS 的 Floating 是 NSFloatingWindowLevel，系统目录选择面板
    // 处于普通层级，会被压在窗口下面（#994）。BT 用普通窗口（与新建下载一致），其余选择保持置顶；
    // 普通窗口不会自己浮到其他应用之上，所以开窗后另行请求置前（见下方）。
    options.kind = if is_bt {
        WindowKind::Normal
    } else {
        WindowKind::Floating
    };
    // 开窗前取：开窗后新窗口本身会成为活动窗口。Linux / FreeBSD 不请求注意（见下方），不取。
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    let app_was_active = cx.active_window().is_some();
    options.is_resizable = is_bt;
    // 只有 BT 文件列表可调尺寸，记住用户调整后的尺寸；画质 / 变体选择固定尺寸。
    if is_bt {
        WindowRegistry::restore_bounds(RememberedWindow::BtSelection, &mut options, BT_SIZE, cx);
    } else {
        options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(
            display_id,
            window_size,
            cx,
        )));
    }

    let opened = WindowRegistry::open_or_focus(cx, key, options, move |window, cx| {
        if !task_name.is_empty() {
            window.set_window_title(&task_name);
        }
        let port = Arc::new(AgentDownloadsPort::new(client.clone()));
        let content = cx.new(|cx| {
            SelectionView::new(
                translator.clone(),
                request.clone(),
                plan,
                task_name.clone(),
                port,
                window,
                cx,
            )
        });
        let window_view = cx.new(|cx| {
            AuxiliaryWindowView::new(translator.clone(), title_key, content.into(), cx)
                .resizable(is_bt)
        });
        let root = cx.new(|cx| Root::new(window_view, window, cx));
        if is_bt {
            WindowRegistry::persist_bounds(
                RememberedWindow::BtSelection,
                client,
                &root,
                window,
                cx,
            );
        }
        root
    });
    if is_bt && let Some(handle) = opened {
        // 用户建完任务常已切去别的应用（磁力解析也可能要几十秒）：选择窗带倒计时、到期按默认
        // 开始下载，必须让用户看见。请求激活应用并置前；系统可能拒绝抢前台（macOS 协同激活、
        // Windows 前台锁），此时应用原本不在前台就再请求注意（程序坞弹跳 / 任务栏闪烁）。
        // 窗口内没有默认获焦的输入框或回车确认，抢到焦点也不会吞掉用户在别处的按键。
        if let Err(error) = handle.update(cx, |_, window, cx| {
            crate::windows::bring_to_front(window, cx);
            #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
            if !app_was_active {
                window.request_attention();
            }
        }) {
            log::debug!("BT selection window released before activation: {error:#}");
        }
    }
}

/// 从最近快照按 `task_id` 找文件名；找不到（元数据未落地/已删除）返回空串，
/// 由调用方用标题文案兜底，不把 UUID 暴露给用户。
fn task_name_for(
    session: &gpui::Entity<crate::session::AgentSession>,
    cx: &App,
    task_id: &str,
) -> String {
    session
        .read(cx)
        .agent_snapshot()
        .and_then(|body| {
            body.daemon
                .tasks
                .iter()
                .find(|task| task.task_id == task_id)
                .map(|task| task.file_name.clone())
        })
        .filter(|name| !name.is_empty())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_default()
}
