//! 「新建下载」窗口：菜单 / 快捷键 / 拖放入口，以及外部捕获（浏览器扩展 / NMH）确认。
//!
//! External captures with the same destination and routing status use one confirmation form.
//! Distinct groups and manual drafts have separate windows and never share form options.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, CaptureResolveParams, CloudDevice, DaemonEvent, LinkDeviceInfo,
    PendingCaptureDto, ServiceEvent, TaskDto,
};
use fluxdown_ui_components::toast;
use fluxdown_ui_downloads::{
    DownloadsCommand, DownloadsPort, FailedRemoteCapture, NewDownloadContext,
    NewDownloadSubmission, NewDownloadView, SubmissionHooks, SubmitNotice,
    new_download_context_from_snapshot, new_download_targets, run_submission,
};
use fluxdown_ui_i18n::keys;
use fluxdown_ui_shell::{AuxiliaryWindowView, auxiliary_window_options};
use gpui::{AnyWindowHandle, App, AppContext as _, Entity, Global, WeakEntity, px, size};
use gpui_component::{Root, WindowExt as _};

use crate::{
    app::Desktop,
    downloads_port::AgentDownloadsPort,
    lifecycle,
    session::{SessionSignal, agent_body},
    windows::{RememberedWindow, WindowKey, WindowRegistry},
};

const NEW_DOWNLOAD_WINDOW_SIZE: gpui::Size<gpui::Pixels> = size(px(640.), px(530.));
const NEW_DOWNLOAD_WINDOW_MIN_SIZE: gpui::Size<gpui::Pixels> = size(px(560.), px(440.));

/// Tracks captures already shown and device state shared by all open download forms.
#[derive(Default)]
struct CaptureDispatch {
    forms: HashMap<WindowKey, OpenForm>,
    handed: HashSet<String>,
    cloud_devices: Vec<CloudDevice>,
    cloud_user: Option<String>,
    cloud_connected: bool,
    agent_current: bool,
    linked_devices: Vec<LinkDeviceInfo>,
}

struct OpenForm {
    view: WeakEntity<NewDownloadView>,
    // A cold-start snapshot can arrive before the daemon configuration is available.
    provisional: bool,
}

impl Global for CaptureDispatch {}

impl CaptureDispatch {
    fn set_cloud_user(&mut self, user: Option<&str>) {
        if self.cloud_user.as_deref() != user {
            self.cloud_devices.clear();
            self.cloud_connected = false;
            self.cloud_user = user.map(str::to_owned);
        }
    }

    fn set_cloud_connected(&mut self, connected: bool) {
        self.cloud_connected = self.cloud_user.is_some() && connected;
    }
}

/// 菜单 / 快捷键入口：上下文取自主窗口下载页；主窗口不存在时先重建主窗口。
pub fn open_default(cx: &mut App) {
    let downloads = main_downloads(cx);
    let Some(downloads) = downloads else {
        return;
    };
    let context = downloads.read(cx).new_download_context();
    open(cx, context);
}

fn main_downloads(cx: &mut App) -> Option<gpui::Entity<fluxdown_ui_downloads::DownloadView>> {
    if let Some(downloads) = Desktop::global(cx)
        .main_downloads
        .as_ref()
        .and_then(|weak| weak.upgrade())
    {
        return Some(downloads);
    }
    crate::windows::main::open(cx);
    Desktop::global(cx)
        .main_downloads
        .as_ref()
        .and_then(|weak| weak.upgrade())
}

pub fn open(cx: &mut App, context: NewDownloadContext) {
    open_with(cx, context, Vec::new());
}

/// 系统打开的链接被 agent 拒绝时在主窗口 toast（含启动时已发生的失败）；文案按错误原因取，
/// 分享链接的原因（本机分享、分享方离线、已过期……）与接收窗口一致。
fn install_capture_failure_notices(cx: &mut App) {
    let mut failures = Desktop::global(cx)
        .client
        .submissions
        .capture_failure
        .subscribe();
    cx.spawn(async move |cx| {
        let mut pending = failures.borrow_and_update().clone();
        loop {
            if pending.take().is_some() {
                cx.update(|cx| {
                    crate::windows::main::reveal(cx);
                    let Some(main) = WindowRegistry::handle(cx, &WindowKey::Main) else {
                        return;
                    };
                    let message = Desktop::global(cx)
                        .translator
                        .read(cx)
                        .text("localServiceActionFailed")
                        .to_owned();
                    if let Err(error) = main.update(cx, |_, window, cx| {
                        window.push_notification(toast::error(message), cx);
                    }) {
                        log::error!("could not display capture failure: {error:#}");
                    }
                });
            }
            if failures.changed().await.is_err() {
                log::debug!("capture failure notification channel closed");
                break;
            }
            pending = failures.borrow_and_update().clone();
        }
    })
    .detach();
}

/// Subscribe to agent capture and device updates, including pending captures from startup.
pub fn install_captures(cx: &mut App) {
    install_capture_failure_notices(cx);
    cx.set_global(CaptureDispatch::default());
    let mut interruptions = Desktop::global(cx)
        .client
        .submissions
        .interruption
        .subscribe();
    cx.spawn(async move |cx| {
        loop {
            if interruptions.changed().await.is_err() {
                log::debug!("download import notification channel closed");
                break;
            }
            let Some(source) = interruptions.borrow_and_update().clone() else {
                continue;
            };
            cx.update(|cx| {
                crate::windows::main::reveal(cx);
                let Some(main) = WindowRegistry::handle(cx, &WindowKey::Main) else {
                    return;
                };
                let translator = Desktop::global(cx).translator.read(cx);
                let message = format!("{}\n{source}", translator.text("downloadImportInterrupted"));
                let copy_label = translator.text("copyUrl").to_owned();
                let notification = toast::error(message).action(move |_, _, _| {
                    let source = source.clone();
                    gpui_component::button::Button::new("copy-interrupted-download")
                        .label(copy_label.clone())
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(source.clone()));
                        })
                });
                if let Err(error) = main.update(cx, |_, window, cx| {
                    window.push_notification(notification, cx);
                }) {
                    log::error!("could not display download import interruption: {error:#}");
                }
            });
        }
    })
    .detach();
    let session = Desktop::global(cx).session.clone();
    let initial = session.read(cx).agent_snapshot().map(|body| {
        (
            body.cloud_devices.clone(),
            body.linked_devices.clone(),
            body.pending_captures.clone(),
            body.cloud_connection.state == fluxdown_protocol::CloudConnectionState::Connected,
            body.session.as_ref().map(|session| session.user.id.clone()),
        )
    });
    if let Some((cloud, linked, pending, connected, user)) = initial {
        cache_devices(cx, &cloud, &linked, connected, user.as_deref());
        sync_captures(cx, &pending);
    }
    cx.subscribe(&session, |_, signal, cx| match signal {
        SessionSignal::Snapshot(snapshot) => {
            if let Some(body) = agent_body(snapshot) {
                cache_devices(
                    cx,
                    &body.cloud_devices,
                    &body.linked_devices,
                    body.cloud_connection.state
                        == fluxdown_protocol::CloudConnectionState::Connected,
                    body.session
                        .as_ref()
                        .map(|session| session.user.id.as_str()),
                );
                sync_targets(cx);
                sync_categories(cx, body);
                refresh_provisional_context(cx, body);
                sync_duplicate_hint(cx, None);
                sync_captures(cx, &body.pending_captures);
            }
        }
        SessionSignal::Event(frame) => match &frame.event {
            // agent 先替换 daemon 快照再发布已连接，此时会话快照已带 daemon 数据。
            ServiceEvent::Agent(AgentEvent::DaemonConnectionChanged(connected)) => {
                if *connected {
                    let latest = Desktop::global(cx).session.read(cx).latest().cloned();
                    if let Some(body) = latest.as_deref().and_then(agent_body) {
                        refresh_provisional_context(cx, body);
                    }
                }
                sync_duplicate_hint(cx, None);
            }
            ServiceEvent::Agent(AgentEvent::CloudConnectionChanged(connection)) => {
                cx.global_mut::<CaptureDispatch>().set_cloud_connected(
                    connection.state == fluxdown_protocol::CloudConnectionState::Connected,
                );
                sync_targets(cx);
            }
            ServiceEvent::Agent(AgentEvent::PendingCapturesChanged(pending)) => {
                sync_captures(cx, pending);
            }
            ServiceEvent::Agent(AgentEvent::CloudDevicesChanged(devices)) => {
                let dispatch = cx.global_mut::<CaptureDispatch>();
                if dispatch.cloud_user.is_some() {
                    dispatch.cloud_devices.clone_from(devices);
                }
                sync_targets(cx);
            }
            ServiceEvent::Agent(AgentEvent::LinkedDevicesChanged(devices)) => {
                cx.global_mut::<CaptureDispatch>()
                    .linked_devices
                    .clone_from(devices);
                sync_targets(cx);
            }
            // 会话结束：账号设备名册随之失效（已配对设备与账号无关，保留）。
            ServiceEvent::Agent(AgentEvent::SessionChanged(session)) => {
                cx.global_mut::<CaptureDispatch>().set_cloud_user(
                    session
                        .as_ref()
                        .as_ref()
                        .map(|session| session.user.id.as_str()),
                );
                sync_targets(cx);
            }
            ServiceEvent::Agent(AgentEvent::DaemonSnapshotReplaced(_)) => {
                sync_duplicate_hint(cx, None);
            }
            ServiceEvent::Agent(AgentEvent::Daemon(event)) => {
                sync_duplicate_hint(cx, Some(event));
            }
            _ => {}
        },
        SessionSignal::Stale
        | SessionSignal::Fatal(_)
        | SessionSignal::ServiceStopped
        | SessionSignal::ServiceFailed(_) => {
            cx.global_mut::<CaptureDispatch>().agent_current = false;
            sync_targets(cx);
            sync_duplicate_hint(cx, None);
        }
    })
    .detach();
}

fn cache_devices(
    cx: &mut App,
    cloud: &[CloudDevice],
    linked: &[LinkDeviceInfo],
    connected: bool,
    user: Option<&str>,
) {
    let dispatch = cx.global_mut::<CaptureDispatch>();
    dispatch.set_cloud_user(user);
    dispatch.set_cloud_connected(connected);
    dispatch.agent_current = true;
    dispatch.cloud_devices = if user.is_some() {
        cloud.to_vec()
    } else {
        Vec::new()
    };
    dispatch.linked_devices = linked.to_vec();
}

fn open_forms(
    cx: &App,
) -> impl Iterator<Item = (&WindowKey, Entity<NewDownloadView>, AnyWindowHandle)> + '_ {
    cx.global::<CaptureDispatch>()
        .forms
        .iter()
        .filter_map(|(key, form)| {
            Some((key, form.view.upgrade()?, WindowRegistry::handle(cx, key)?))
        })
}

fn sync_categories(cx: &mut App, snapshot: &AgentSnapshot) {
    if cx.global::<CaptureDispatch>().forms.is_empty() {
        return;
    }
    let categories = new_download_context_from_snapshot(snapshot).custom_categories;
    let forms: Vec<_> = open_forms(cx)
        .map(|(_, form, handle)| (form, handle))
        .collect();
    for (form, handle) in forms {
        if let Err(error) = handle.update(cx, |_, window, cx| {
            form.update(cx, |form, cx| {
                form.set_custom_categories(categories.clone(), window, cx);
            });
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    }
}

fn duplicate_scope(cx: &App) -> Option<(&[TaskDto], &str)> {
    duplicate_scope_from_snapshot(
        Desktop::global(cx)
            .session
            .read(cx)
            .current_agent_snapshot()?,
    )
}

fn duplicate_scope_from_snapshot(snapshot: &AgentSnapshot) -> Option<(&[TaskDto], &str)> {
    let daemon = &snapshot.daemon;
    if !snapshot.daemon_connected || daemon.task_total.is_some() {
        return None;
    }
    let default_dir = daemon
        .config
        .values
        .get("default_save_dir")
        .map(String::as_str)
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_default();
    Some((&daemon.tasks, default_dir))
}

fn sync_duplicate_hint(cx: &mut App, event: Option<&DaemonEvent>) {
    let forms: Vec<_> = open_forms(cx)
        .filter(|(_, form, _)| {
            event.is_none_or(|event| form.read(cx).duplicate_hint_needs_refresh(event))
        })
        .map(|(_, form, handle)| (form, handle))
        .collect();
    for (form, handle) in forms {
        if let Err(error) = handle.update(cx, |_, window, cx| {
            form.update(cx, |form, cx| match event {
                None
                | Some(
                    DaemonEvent::SnapshotReplaced(_)
                    | DaemonEvent::TablesReloading { tasks: Some(_), .. }
                    | DaemonEvent::Engine(fluxdown_protocol::WsServerMsg::TasksSnapshot { .. }),
                ) => form.reset_duplicate_hint(window, cx),
                Some(_) => form.refresh_duplicate_hint(window, cx),
            });
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    }
}

fn current_targets(cx: &App) -> Vec<fluxdown_ui_downloads::DeviceEntry> {
    let dispatch = cx.global::<CaptureDispatch>();
    new_download_targets(
        &dispatch.cloud_devices,
        &dispatch.linked_devices,
        dispatch.agent_current && dispatch.cloud_connected,
        dispatch.agent_current,
    )
}

/// Refresh the target roster in each live form.
fn sync_targets(cx: &mut App) {
    if cx.global::<CaptureDispatch>().forms.is_empty() {
        return;
    }
    let targets = current_targets(cx);
    let forms: Vec<_> = open_forms(cx)
        .map(|(_, form, handle)| (form, handle))
        .collect();
    for (form, handle) in forms {
        if let Err(error) = handle.update(cx, |_, window, cx| {
            form.update(cx, |form, cx| form.set_targets(targets.clone(), window, cx));
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    }
}

/// 表单按空环境打开后 daemon 已连上：以最新快照补投影保存目录 / 队列 / 线程数 / 代理等
/// 默认值（只覆盖用户未改动的初值），之后不再跟随。
fn refresh_provisional_context(cx: &mut App, snapshot: &AgentSnapshot) {
    if !snapshot.daemon_connected {
        return;
    }
    let forms: Vec<_> = open_forms(cx)
        .filter(|(key, _, _)| {
            cx.global::<CaptureDispatch>()
                .forms
                .get(*key)
                .is_some_and(|form| form.provisional)
        })
        .map(|(key, form, handle)| (key.clone(), form, handle))
        .collect();
    let mut context = None;
    for (key, form, handle) in forms {
        let context = context.get_or_insert_with(|| new_download_context_from_snapshot(snapshot));
        match handle.update(cx, |_, window, cx| {
            form.update(cx, |form, cx| {
                form.refresh_context(context.clone(), window, cx)
            });
        }) {
            Ok(()) => {
                if let Some(form) = cx.global_mut::<CaptureDispatch>().forms.get_mut(&key) {
                    form.provisional = false;
                }
            }
            Err(error) => log::debug!("view or window released before lifecycle update: {error:#}"),
        }
    }
}

fn group_captures(
    captures: impl IntoIterator<Item = PendingCaptureDto>,
) -> Vec<Vec<PendingCaptureDto>> {
    let mut groups: Vec<Vec<PendingCaptureDto>> = Vec::new();
    for capture in captures {
        let group = groups.iter_mut().find(|group| {
            group.first().is_some_and(|first| {
                first.suggested_target == capture.suggested_target
                    && first.routing_error == capture.routing_error
            }) && group.iter().all(|member| member.url != capture.url)
        });
        if let Some(group) = group {
            group.push(capture);
        } else {
            groups.push(vec![capture]);
        }
    }
    groups
}

fn mark_captures_handed(cx: &mut App, captures: &[PendingCaptureDto]) {
    cx.global_mut::<CaptureDispatch>().handed.extend(
        captures
            .iter()
            .map(|capture| capture.transaction_id.clone()),
    );
}

fn sync_captures(cx: &mut App, pending: &[PendingCaptureDto]) {
    let dispatch = cx.global_mut::<CaptureDispatch>();
    dispatch.handed.retain(|transaction_id| {
        pending
            .iter()
            .any(|capture| &capture.transaction_id == transaction_id)
    });
    let groups = group_captures(
        pending
            .iter()
            .filter(|capture| !dispatch.handed.contains(&capture.transaction_id))
            .cloned(),
    );
    let forms: Vec<_> = open_forms(cx)
        .filter(|(key, _, _)| matches!(key, WindowKey::CaptureDownload(_)))
        .map(|(_, form, handle)| (form, handle))
        .collect();
    for (form, handle) in &forms {
        if let Err(error) = handle.update(cx, |_, window, cx| {
            form.update(cx, |form, cx| form.retain_captures(pending, window, cx));
        }) {
            log::debug!("capture window released before queue update: {error:#}");
        }
    }
    let mut context = None;
    let mut first_handle = None;
    for group in groups {
        let Some(first) = group.first() else {
            continue;
        };
        let key = WindowKey::CaptureDownload(first.transaction_id.clone());
        let mut group = Some(group);
        for (form, handle) in &forms {
            if !group
                .as_deref()
                .is_some_and(|group| form.read(cx).can_append_captures(group))
            {
                continue;
            }
            let delivered = handle.update(cx, |_, window, cx| {
                if let Some(group) = group.take() {
                    mark_captures_handed(cx, &group);
                    form.update(cx, |form, cx| form.add_captures(group, window, cx));
                }
            });
            if delivered.is_ok() {
                first_handle.get_or_insert(*handle);
                break;
            }
        }
        if let Some(group) = group {
            let context = context.get_or_insert_with(|| {
                let mut context = Desktop::global(cx)
                    .session
                    .read(cx)
                    .agent_snapshot()
                    .map(new_download_context_from_snapshot)
                    .unwrap_or_default();
                context.targets = current_targets(cx);
                context
            });
            open_with(cx, context.clone(), group);
            if let Some(handle) = WindowRegistry::handle(cx, &key) {
                first_handle.get_or_insert(handle);
            }
        }
    }
    if let Some(handle) = first_handle
        && let Err(error) = handle.update(cx, |_, window, cx| {
            crate::windows::bring_to_front(window, cx);
        })
    {
        log::debug!("capture window released before activation: {error:#}");
    }
}

fn open_with(cx: &mut App, context: NewDownloadContext, captures: Vec<PendingCaptureDto>) {
    let key = captures.first().map_or(WindowKey::NewDownload, |capture| {
        WindowKey::CaptureDownload(capture.transaction_id.clone())
    });
    let manual = key == WindowKey::NewDownload;
    let form_key = key.clone();
    let initial_urls = context.initial_urls.clone();
    let desktop = Desktop::global(cx);
    let translator = desktop.translator.clone();
    let client = desktop.client.clone();
    let provisional = !desktop
        .session
        .read(cx)
        .agent_snapshot()
        .is_some_and(|snapshot| snapshot.daemon_connected);
    let title = translator.read(cx).text(keys::NEW_DOWNLOAD).to_owned();
    let mut options = auxiliary_window_options(title);
    options.display_id = WindowRegistry::main_display_id(cx);
    options.window_min_size = Some(NEW_DOWNLOAD_WINDOW_MIN_SIZE);
    options.is_resizable = true;
    WindowRegistry::restore_bounds(
        RememberedWindow::NewDownload,
        &mut options,
        NEW_DOWNLOAD_WINDOW_SIZE,
        cx,
    );

    let handle = WindowRegistry::open_or_focus(cx, key.clone(), options, move |window, cx| {
        let port = Arc::new(AgentDownloadsPort::new(client.clone()));
        let submit_port = Arc::clone(&port);
        let on_submit = Rc::new(
            move |submission, _window: &mut gpui::Window, cx: &mut App| {
                submit(submission, &submit_port, cx);
            },
        );
        let form_port: Arc<dyn DownloadsPort> = port.clone();
        let form = cx.new(|cx| {
            NewDownloadView::new(
                translator.clone(),
                context,
                form_port,
                on_submit,
                duplicate_scope,
                window,
                cx,
            )
        });
        if !captures.is_empty() {
            mark_captures_handed(cx, &captures);
            form.update(cx, |form, cx| form.add_captures(captures, window, cx));
        }
        let released_key = form_key.clone();
        cx.observe_release(&form, move |form, cx| {
            cx.global_mut::<CaptureDispatch>()
                .forms
                .remove(&released_key);
            ignore_captures(form.take_captures(), &port, cx);
        })
        .detach();
        cx.default_global::<CaptureDispatch>().forms.insert(
            form_key,
            OpenForm {
                view: form.downgrade(),
                provisional,
            },
        );
        let window_view =
            cx.new(|cx| AuxiliaryWindowView::new(translator, keys::NEW_DOWNLOAD, form.into(), cx));
        let root = cx.new(|cx| Root::new(window_view, window, cx));
        WindowRegistry::persist_bounds(RememberedWindow::NewDownload, client, &root, window, cx);
        root
    });
    if !manual {
        return;
    }
    if let Some(handle) = handle {
        if let Err(error) = handle.update(cx, |_, window, cx| {
            crate::windows::bring_to_front(window, cx)
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    } else if let Some(handle) = WindowRegistry::handle(cx, &key) {
        // The active window can still be on its own shortcut update stack.
        cx.defer(move |cx| {
            let form = cx
                .global::<CaptureDispatch>()
                .forms
                .get(&key)
                .and_then(|form| form.view.upgrade());
            if let Err(error) = handle.update(cx, |_, window, cx| {
                if let Some(form) = form
                    && !initial_urls.is_empty()
                {
                    form.update(cx, |form, cx| form.append_urls(initial_urls, window, cx));
                }
                crate::windows::bring_to_front(window, cx);
            }) {
                log::debug!("view or window released before lifecycle update: {error:#}");
            }
        });
    }
}

/// 表单关闭时仍未确认的捕获：逐条忽略，界面退出前等它们送达 agent。
fn ignore_captures(captures: Vec<PendingCaptureDto>, port: &AgentDownloadsPort, cx: &mut App) {
    for capture in captures {
        let ignore = port.execute(DownloadsCommand::CaptureResolve(Box::new(
            CaptureResolveParams {
                transaction_id: capture.transaction_id,
                accepted: false,
                disable_category_route: true,
                remote_target: None,
                request: None,
            },
        )));
        let task = cx.spawn(async move |_| match ignore.await {
            Ok(_) => log::trace!("unconfirmed capture dismissed"),
            Err(error) if error.code == fluxdown_protocol::ApplicationErrorCode::NotFound => {
                log::debug!("unconfirmed capture already resolved");
            }
            Err(error) => log::warn!("failed to dismiss unconfirmed capture: {:?}", error.code),
        });
        lifecycle::keep_alive(cx, task).detach();
    }
}

fn reoffer_failed_captures(cx: &mut App, failed: Vec<FailedRemoteCapture>) {
    let Some(mut pending) = Desktop::global(cx)
        .session
        .read(cx)
        .agent_snapshot()
        .map(|snapshot| snapshot.pending_captures.clone())
    else {
        return;
    };
    let dispatch = cx.global_mut::<CaptureDispatch>();
    for failure in failed {
        if let Some(capture) = pending
            .iter_mut()
            .find(|capture| capture.transaction_id == failure.transaction_id)
        {
            dispatch.handed.remove(&failure.transaction_id);
            // The RPC response can reach this client before the updated snapshot event.
            if capture.routing_error.is_none()
                || capture.suggested_target.as_deref() != Some(failure.target.as_str())
            {
                capture.suggested_target = Some(failure.target);
                capture.routing_error = Some("captureRoutingDispatchFailed".to_owned());
            }
        }
    }
    sync_captures(cx, &pending);
}

/// 执行提交：有主窗口时经下载页（失败进横幅），否则直接走 agent 端口；完成后在主窗口
/// toast（成功 / 失败 / 下发汇总文案由下载能力按 `reason` 生成）。界面退出前等提交送达
///（外部捕获拉起的界面没有主窗口，关表单即最后一个窗口）。
fn submit(submission: NewDownloadSubmission, port: &Arc<AgentDownloadsPort>, cx: &mut App) {
    let mut main = Desktop::global(cx)
        .main_downloads
        .as_ref()
        .and_then(WeakEntity::upgrade);
    // 超过一块的导入有进度条与取消入口，只在主窗口下载页显示：没有主窗口时先打开它。
    if main.is_none() && submission.exceeds_one_chunk() {
        crate::windows::main::reveal(cx);
        main = Desktop::global(cx)
            .main_downloads
            .as_ref()
            .and_then(WeakEntity::upgrade);
    }
    let translator = Desktop::global(cx).translator.clone();
    let notice = match main {
        Some(downloads) => downloads.update(cx, |downloads, cx| {
            downloads.create_download(submission, cx)
        }),
        None => {
            let starts_immediately = submission.starts_immediately();
            let port = Arc::clone(port);
            let translator = translator.clone();
            let timer = cx.background_executor().clone();
            cx.spawn(async move |cx| {
                let report = run_submission(
                    submission,
                    move |command| port.execute(command),
                    SubmissionHooks::new(move |delay| timer.timer(delay)),
                )
                .await;
                // 与主窗口下载页同一规则：恰好一个任务立即开始才弹进度窗口。
                if let ([task_id], true) = (report.created_task_ids(), starts_immediately) {
                    let task_id = task_id.clone();
                    cx.update(|cx| {
                        crate::progress_windows::user_started(
                            fluxdown_ui_downloads::UserStartIntent::Created(task_id),
                            cx,
                        );
                    });
                }
                let notice = cx.update(|cx| report.notice(translator.read(cx)));
                (notice, report.into_failed_remote_captures())
            })
        }
    };
    let task = cx.spawn(async move |cx| {
        let (SubmitNotice { ok, message }, failed) = notice.await;
        // 全部新建成功不提示：任务已在列表中，toast 只会遮挡列表顶部（#884）。
        if ok && message.is_empty() {
            return;
        }
        cx.update(|cx| {
            if !failed.is_empty() {
                cx.defer(move |cx| reoffer_failed_captures(cx, failed));
            }
            let main = match WindowRegistry::handle(cx, &WindowKey::Main) {
                Some(main) => main,
                // 成功时静默结束；失败则打开主窗口承载错误提示，否则用户无从得知。
                None if ok => return,
                None => {
                    crate::windows::main::reveal(cx);
                    let Some(main) = WindowRegistry::handle(cx, &WindowKey::Main) else {
                        return;
                    };
                    main
                }
            };
            let message = if message.is_empty() {
                translator
                    .read(cx)
                    .text("localServiceActionFailed")
                    .to_owned()
            } else {
                message
            };
            let notification = if ok {
                toast::plain(message)
            } else {
                toast::error(message)
            };
            if let Err(error) = main.update(cx, |_, window, cx| {
                window.push_notification(notification, cx)
            }) {
                log::debug!("view or window released before lifecycle update: {error:#}");
            }
        });
    });
    lifecycle::keep_alive(cx, task).detach();
}

#[cfg(test)]
mod presence_tests {
    use fluxdown_protocol::{AgentSnapshot, PendingCaptureDto};

    use super::{CaptureDispatch, duplicate_scope_from_snapshot, group_captures};

    #[test]
    fn duplicate_scope_requires_a_connected_host_and_complete_task_table() {
        let mut snapshot = AgentSnapshot::default();
        assert!(duplicate_scope_from_snapshot(&snapshot).is_none());
        snapshot.daemon_connected = true;
        snapshot.daemon.task_total = Some(33);
        assert!(duplicate_scope_from_snapshot(&snapshot).is_none());
        snapshot.daemon.task_total = None;
        snapshot.daemon.queue_total = Some(7);
        assert!(duplicate_scope_from_snapshot(&snapshot).is_some());
    }

    #[test]
    fn duplicate_scope_preserves_the_daemon_default_directory_setting() {
        let mut snapshot = AgentSnapshot {
            daemon_connected: true,
            ..AgentSnapshot::default()
        };
        snapshot.daemon.runtime_stats.save_dir = "old runtime sample".to_owned();
        snapshot.daemon.config.values.insert(
            "default_save_dir".to_owned(),
            r"D:\下载  2026\📦".to_owned(),
        );
        assert_eq!(
            duplicate_scope_from_snapshot(&snapshot).map(|(_, dir)| dir),
            Some(r"D:\下载  2026\📦")
        );
    }

    #[test]
    fn logout_and_account_switch_invalidate_presence_before_next_roster() {
        let mut dispatch = CaptureDispatch::default();
        dispatch.set_cloud_user(Some("account-a"));
        dispatch.set_cloud_connected(true);
        assert!(dispatch.cloud_connected);
        dispatch.set_cloud_user(None);
        assert!(!dispatch.cloud_connected);
        dispatch.set_cloud_connected(true);
        assert!(!dispatch.cloud_connected);
        dispatch.set_cloud_user(Some("account-b"));
        assert!(!dispatch.cloud_connected);
        dispatch.set_cloud_connected(true);
        dispatch.set_cloud_user(Some("account-c"));
        assert!(!dispatch.cloud_connected);
    }

    fn capture(
        id: &str,
        url: &str,
        target: Option<&str>,
        error: Option<&str>,
    ) -> PendingCaptureDto {
        PendingCaptureDto {
            transaction_id: id.into(),
            url: url.into(),
            file_name: String::new(),
            created_at_unix_ms: 0,
            file_size: 0,
            referrer: String::new(),
            save_dir: String::new(),
            has_cookies: false,
            header_names: Vec::new(),
            suggested_target: target.map(str::to_owned),
            routing_error: error.map(str::to_owned),
        }
    }

    #[test]
    fn capture_groups_keep_local_links_targets_and_errors_separate() {
        let groups = group_captures([
            capture("1", "https://a.test/app.apk", Some("cloud:a"), None),
            capture("2", "https://a.test/setup.exe", Some("link:b"), None),
            capture("3", "https://a.test/other.apk", Some("cloud:a"), None),
            capture("4", "https://a.test/task.zip", None, None),
            capture(
                "5",
                "https://a.test/failure.apk",
                Some("cloud:a"),
                Some("captureRoutingDispatchFailed"),
            ),
            capture("6", "https://a.test/app.apk", Some("cloud:a"), None),
        ]);
        let ids: Vec<Vec<_>> = groups
            .iter()
            .map(|group| {
                group
                    .iter()
                    .map(|capture| capture.transaction_id.as_str())
                    .collect()
            })
            .collect();
        assert_eq!(
            ids,
            [vec!["1", "3"], vec!["2"], vec!["4"], vec!["5"], vec!["6"]]
        );
    }
}
