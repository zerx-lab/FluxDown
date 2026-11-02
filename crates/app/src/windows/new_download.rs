//! 「新建下载」窗口：菜单 / 快捷键 / 拖放入口，以及外部捕获（浏览器扩展 / NMH）确认。
//!
//! 外部捕获不单独开确认弹窗：待确认队列里新出现的事务追加进同一个新建下载表单（没有就开
//! 一个，无主窗口时也可以）。提交时普通链接经主窗口下载页建任务（无主窗口时直接走 agent
//! 端口），捕获条目经 `agent.capture.resolve` 确认；表单释放时未确认的捕获一并忽略。
//! 成功后关窗并在主窗口 toast。

use std::{collections::HashSet, rc::Rc, sync::Arc};

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, CaptureResolveParams, CloudDevice, LinkDeviceInfo,
    PendingCaptureDto, ServiceEvent,
};
use fluxdown_ui_downloads::{
    DownloadsCommand, DownloadsPort, NewDownloadContext, NewDownloadSubmission, NewDownloadView,
    SubmissionHooks, SubmitNotice, new_download_context_from_snapshot, new_download_targets,
    run_submission,
};
use fluxdown_ui_i18n::keys;
use fluxdown_ui_shell::{AuxiliaryWindowView, auxiliary_window_options};
use gpui::{App, AppContext as _, Global, WeakEntity, px, size};
use gpui_component::{Root, WindowExt as _, notification::Notification};

use crate::{
    app::Desktop,
    downloads_port::AgentDownloadsPort,
    lifecycle,
    session::{SessionSignal, agent_body},
    windows::{RememberedWindow, WindowKey, WindowRegistry},
};

const NEW_DOWNLOAD_WINDOW_SIZE: gpui::Size<gpui::Pixels> = size(px(640.), px(530.));
const NEW_DOWNLOAD_WINDOW_MIN_SIZE: gpui::Size<gpui::Pixels> = size(px(560.), px(440.));

/// 外部捕获派发状态：当前表单 + 已交给表单的事务（agent 移除前不重复追加，
/// 关窗忽略后也不会因列表尚未刷新而重开）；另缓存设备名册，随事件推给已打开的表单。
#[derive(Default)]
struct CaptureDispatch {
    form: Option<WeakEntity<NewDownloadView>>,
    /// 当前表单按 daemon 未连上时的空环境打开（冷启动：agent 首个快照先于 daemon 就绪，
    /// 不带配置 / 队列 / 运行时目录），daemon 连上后需补投影一次。
    provisional: bool,
    handed: HashSet<String>,
    cloud_devices: Vec<CloudDevice>,
    cloud_user: Option<String>,
    cloud_connected: bool,
    agent_current: bool,
    linked_devices: Vec<LinkDeviceInfo>,
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
                        window.push_notification(Notification::error(message), cx);
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

/// 订阅会话：待确认捕获出现 → 追加进新建下载表单（必要时打开）；启动时回放快照。
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
                let notification = Notification::error(message).action(move |_, _, _| {
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
                sync_captures(cx, &body.pending_captures);
            }
        }
        SessionSignal::Event(frame) => match &frame.event {
            // agent 先替换 daemon 快照再发布已连接，此时会话快照已带 daemon 数据。
            ServiceEvent::Agent(AgentEvent::DaemonConnectionChanged(true)) => {
                let latest = Desktop::global(cx).session.read(cx).latest().cloned();
                if let Some(body) = latest.as_deref().and_then(agent_body) {
                    refresh_provisional_context(cx, body);
                }
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
            _ => {}
        },
        SessionSignal::Stale
        | SessionSignal::Fatal(_)
        | SessionSignal::ServiceStopped
        | SessionSignal::ServiceFailed(_) => {
            cx.global_mut::<CaptureDispatch>().agent_current = false;
            sync_targets(cx);
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

/// 把快照里最新的分类列表推给已打开的新建下载表单（快捷添加网址规则以它为写回基准）。
fn sync_categories(cx: &mut App, snapshot: &AgentSnapshot) {
    let form = cx
        .global::<CaptureDispatch>()
        .form
        .as_ref()
        .and_then(WeakEntity::upgrade);
    let (Some(form), Some(handle)) = (form, WindowRegistry::handle(cx, &WindowKey::NewDownload))
    else {
        return;
    };
    let categories = new_download_context_from_snapshot(snapshot).custom_categories;
    if let Err(error) = handle.update(cx, |_, window, cx| {
        form.update(cx, |form, cx| {
            form.set_custom_categories(categories, window, cx)
        });
    }) {
        log::debug!("view or window released before lifecycle update: {error:#}");
    }
}

/// 把最新设备名册（在线状态 / 默认目录 / 设备增减）推给已打开的新建下载表单。
fn sync_targets(cx: &mut App) {
    let (form, targets) = {
        let dispatch = cx.global::<CaptureDispatch>();
        (
            dispatch.form.as_ref().and_then(WeakEntity::upgrade),
            new_download_targets(
                &dispatch.cloud_devices,
                &dispatch.linked_devices,
                dispatch.agent_current && dispatch.cloud_connected,
                dispatch.agent_current,
            ),
        )
    };
    let (Some(form), Some(handle)) = (form, WindowRegistry::handle(cx, &WindowKey::NewDownload))
    else {
        return;
    };
    if let Err(error) = handle.update(cx, |_, window, cx| {
        form.update(cx, |form, cx| form.set_targets(targets, window, cx));
    }) {
        log::debug!("view or window released before lifecycle update: {error:#}");
    }
}

/// 表单按空环境打开后 daemon 已连上：以最新快照补投影保存目录 / 队列 / 线程数 / 代理等
/// 默认值（只覆盖用户未改动的初值），之后不再跟随。
fn refresh_provisional_context(cx: &mut App, snapshot: &AgentSnapshot) {
    if !snapshot.daemon_connected {
        return;
    }
    let form = {
        let dispatch = cx.global::<CaptureDispatch>();
        if !dispatch.provisional {
            return;
        }
        dispatch.form.as_ref().and_then(WeakEntity::upgrade)
    };
    let (Some(form), Some(handle)) = (form, WindowRegistry::handle(cx, &WindowKey::NewDownload))
    else {
        return;
    };
    let context = new_download_context_from_snapshot(snapshot);
    match handle.update(cx, |_, window, cx| {
        form.update(cx, |form, cx| form.refresh_context(context, window, cx));
    }) {
        Ok(()) => cx.global_mut::<CaptureDispatch>().provisional = false,
        Err(error) => log::debug!("view or window released before lifecycle update: {error:#}"),
    }
}

/// 对齐 agent 的待确认列表：已消失的事务从表单移除（链接行保留为普通链接），新事务追加
/// 进现有表单并置前；没有表单时以快照投影的环境开一个。
fn sync_captures(cx: &mut App, pending: &[PendingCaptureDto]) {
    let (form, fresh) = {
        let dispatch = cx.global_mut::<CaptureDispatch>();
        dispatch.handed.retain(|transaction_id| {
            pending
                .iter()
                .any(|capture| &capture.transaction_id == transaction_id)
        });
        let fresh = pending
            .iter()
            .filter(|capture| dispatch.handed.insert(capture.transaction_id.clone()))
            .cloned()
            .collect::<Vec<_>>();
        (dispatch.form.as_ref().and_then(WeakEntity::upgrade), fresh)
    };
    let mut fresh = Some(fresh);
    if let (Some(form), Some(handle)) = (form, WindowRegistry::handle(cx, &WindowKey::NewDownload))
    {
        let delivered = handle.update(cx, |_, window, cx| {
            let fresh = fresh.take().unwrap_or_default();
            let arrived = !fresh.is_empty();
            form.update(cx, |form, cx| {
                form.retain_captures(pending, window, cx);
                if arrived {
                    form.add_captures(fresh, window, cx);
                }
            });
            if arrived {
                crate::windows::bring_to_front(window, cx);
            }
        });
        if delivered.is_ok() {
            return;
        }
    }
    let fresh = fresh.unwrap_or_default();
    if fresh.is_empty() {
        return;
    }
    let context = Desktop::global(cx)
        .session
        .read(cx)
        .agent_snapshot()
        .map(new_download_context_from_snapshot)
        .unwrap_or_default();
    open_with(cx, context, fresh);
}

fn open_with(cx: &mut App, context: NewDownloadContext, captures: Vec<PendingCaptureDto>) {
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

    let handle =
        WindowRegistry::open_or_focus(cx, WindowKey::NewDownload, options, move |window, cx| {
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
                    window,
                    cx,
                )
            });
            if !captures.is_empty() {
                form.update(cx, |form, cx| form.add_captures(captures, window, cx));
            }
            cx.observe_release(&form, move |form, cx| {
                ignore_captures(form.take_captures(), &port, cx);
            })
            .detach();
            let dispatch = cx.default_global::<CaptureDispatch>();
            dispatch.form = Some(form.downgrade());
            dispatch.provisional = provisional;
            let window_view = cx.new(|cx| {
                AuxiliaryWindowView::new(translator, keys::NEW_DOWNLOAD, form.into(), cx)
            });
            let root = cx.new(|cx| Root::new(window_view, window, cx));
            WindowRegistry::persist_bounds(
                RememberedWindow::NewDownload,
                client,
                &root,
                window,
                cx,
            );
            root
        });
    // 菜单入口与外部捕获都是需要用户立即处理的操作：macOS 后台时也要将窗口及应用置前。
    if let Some(handle) = handle {
        if let Err(error) = handle.update(cx, |_, window, cx| {
            crate::windows::bring_to_front(window, cx)
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    } else if let Some(handle) = WindowRegistry::handle(cx, &WindowKey::NewDownload) {
        // 窗口已开：新建流程不会重建表单，拖入的链接追加进已有表单（去重、保留已输入内容）。
        let form = cx
            .global::<CaptureDispatch>()
            .form
            .as_ref()
            .and_then(WeakEntity::upgrade);
        if let Err(error) = handle.update(cx, |_, window, cx| {
            if let Some(form) = form
                && !initial_urls.is_empty()
            {
                form.update(cx, |form, cx| form.append_urls(initial_urls, window, cx));
            }
            crate::windows::bring_to_front(window, cx)
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
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
                cx.update(|cx| report.notice(translator.read(cx)))
            })
        }
    };
    let task = cx.spawn(async move |cx| {
        let SubmitNotice { ok, message } = notice.await;
        // 全部新建成功不提示：任务已在列表中，toast 只会遮挡列表顶部（#884）。
        if ok && message.is_empty() {
            return;
        }
        cx.update(|cx| {
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
                Notification::from(message)
            } else {
                Notification::error(message)
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
    use super::CaptureDispatch;

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
}
