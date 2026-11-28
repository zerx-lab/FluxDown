//! 「文件已存在」聚合窗口：所有待确认的 `fileExists` 请求共用一个 `Floating` 窗口
//! （[`WindowKey::FileConflicts`]），不像其它引擎选择那样每请求一窗。
//!
//! 开关规则：
//! - 启动时快照里已有待确认请求 / 重连后的替换快照带来请求：窗口没开就打开（已开不打扰）。
//! - 新请求到达（`SelectionPending`）：打开或置前。
//! - 请求清空：视图自己发 `Close`，窗口关闭；「稍后决定」同样只是关窗，不发 RPC。
//!
//! 窗口内容随会话快照 / 事件自行增删行（见 [`FileConflictView`]）。

use std::sync::Arc;

use fluxdown_protocol::{
    AgentEvent, DaemonEvent, SelectionKind, SelectionRequestDto, ServiceEvent,
};
use fluxdown_ui_downloads::{
    FILE_CONFLICT_WINDOW_HEIGHT, FILE_CONFLICT_WINDOW_WIDTH, FileConflictEvent, FileConflictView,
};
use fluxdown_ui_shell::{AuxiliaryWindowView, auxiliary_window_options};
use gpui::{App, AppContext as _, DisplayId, Window, WindowBounds, WindowKind, px, size};
use gpui_component::Root;

use crate::{
    app::Desktop,
    downloads_port::AgentDownloadsPort,
    session::{SessionSignal, agent_body, attach},
    windows::{WindowKey, WindowRegistry, bring_to_front},
};

const TITLE_KEY: &str = "fileConflictTitle";

/// 订阅会话：启动回放与后续请求都由这里决定开窗。
pub fn install(cx: &mut App) {
    let session = Desktop::global(cx).session.clone();
    let startup_pending = session
        .read(cx)
        .agent_snapshot()
        .is_some_and(|body| any_file_exists(&body.daemon.pending_selections));
    if startup_pending {
        open(cx);
    }
    cx.subscribe(&session, |_, signal, cx| match signal {
        SessionSignal::Snapshot(snapshot) => {
            if agent_body(snapshot)
                .is_some_and(|body| any_file_exists(&body.daemon.pending_selections))
            {
                open_if_closed(cx);
            }
        }
        SessionSignal::Event(frame) => match &frame.event {
            ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::SelectionPending(request)))
                if is_file_exists(request) =>
            {
                open(cx);
            }
            // daemon 晚于 agent 就绪 / 重连：挂起的选择随替换快照到达，而不是逐条事件。
            ServiceEvent::Agent(AgentEvent::DaemonSnapshotReplaced(snapshot))
            | ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(snapshot)))
                if any_file_exists(&snapshot.pending_selections) =>
            {
                open_if_closed(cx);
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

fn is_file_exists(request: &SelectionRequestDto) -> bool {
    matches!(request.kind, SelectionKind::FileExists { .. })
}

fn any_file_exists(requests: &[SelectionRequestDto]) -> bool {
    requests.iter().any(is_file_exists)
}

fn open_if_closed(cx: &mut App) {
    if !WindowRegistry::is_open(cx, &WindowKey::FileConflicts) {
        open(cx);
    }
}

/// 打开（或置前）聚合窗口，居中主窗口当前边界（无主窗口时为主显示器）。
pub fn open(cx: &mut App) {
    let display_id = WindowRegistry::main_display_id(cx);
    open_on(cx, display_id);
}

/// 任务表「待确认」角标点击入口：此刻主窗口正处于自己的 update 中，读不到它的显示器，
/// 直接取触发点击的窗口所在显示器。
pub fn open_from_window(window: &Window, cx: &mut App) {
    let display_id = window.display(cx).map(|display| display.id());
    open_on(cx, display_id);
}

fn open_on(cx: &mut App, display_id: Option<DisplayId>) {
    let desktop = Desktop::global(cx);
    let translator = desktop.translator.clone();
    let client = desktop.client.clone();
    let session = desktop.session.clone();
    let mut options = auxiliary_window_options(translator.read(cx).text(TITLE_KEY).to_owned());
    options.display_id = display_id;
    options.window_min_size = None;
    options.kind = WindowKind::Floating;
    options.is_resizable = false;
    let (display_id, bounds) = WindowRegistry::place_following_main(
        size(
            px(FILE_CONFLICT_WINDOW_WIDTH),
            px(FILE_CONFLICT_WINDOW_HEIGHT),
        ),
        display_id,
        cx,
    );
    options.display_id = display_id;
    options.window_bounds = Some(WindowBounds::Windowed(bounds));

    WindowRegistry::open_or_focus(cx, WindowKey::FileConflicts, options, move |window, cx| {
        let port = Arc::new(AgentDownloadsPort::new(client));
        let view = cx.new(|cx| FileConflictView::new(translator.clone(), port, window, cx));
        let window_view = cx.new(|cx| {
            AuxiliaryWindowView::new(translator.clone(), TITLE_KEY, view.clone().into(), cx)
                .resizable(false)
        });
        let root = cx.new(|cx| {
            let root = Root::new(window_view, window, cx);
            cx.subscribe_in(&view, window, |_, _, event, window, _| match event {
                FileConflictEvent::Close => window.remove_window(),
            })
            .detach();
            root
        });
        // 订阅就绪后再灌入当前快照：此时若已无请求，`Close` 不会丢。
        attach(&session, &view, cx);
        root
    });

    // 新请求要被看到：连同应用一起置前（已开则也置前）。
    if let Some(handle) = WindowRegistry::handle(cx, &WindowKey::FileConflicts)
        && let Err(error) = handle.update(cx, |_, window, cx| bring_to_front(window, cx))
    {
        log::debug!("view or window released before lifecycle update: {error:#}");
    }
}
