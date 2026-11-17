//! 本机服务不可用提示：agent 无法被拉起 / 反复启动即退出（[`SessionSignal::ServiceFailed`]）与不可
//! 恢复的连接错误（[`SessionSignal::Fatal`]）都是全局事件，不依赖哪个窗口打开。应用级订阅 agent
//! 会话，在承载浮层的窗口（主窗口优先）弹不自动消失的 error 通知，正文给出原因、细节与日志目录；
//! 暂时没有承载窗口时暂存，主窗口就绪后补弹。会话恢复（收到快照）后撤掉通知。
//!
//! 通知固定一个类型 id：新通知先移除旧的再显示，不会堆叠。

use fluxdown_protocol::ApplicationErrorCode;
use fluxdown_protocol::local_service::AgentStartupFailureKind;
use fluxdown_ui_components::toast;
use gpui::{AnyWindowHandle, App, Global};
use gpui_component::WindowExt as _;

use crate::{
    app::Desktop, service_failure::ServiceFailure, session::SessionSignal, windows::WindowRegistry,
};

/// 本模块通知的类型 id（`Notification::id1` / `remove_notification`）。
struct ServiceNoticeKind;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Notice {
    title: String,
    message: String,
}

#[derive(Default)]
struct ServiceNotices {
    /// 承载窗口尚不存在时暂存的提示（只保留最新一条）。
    pending: Option<Notice>,
    /// 当前显示通知的窗口：会话恢复时据此撤掉。
    shown_in: Option<AnyWindowHandle>,
    /// 通知序号：每条通知独立 id，避免与正在淡出的旧通知冲突。
    serial: usize,
}

impl Global for ServiceNotices {}

/// 订阅会话：失败信号弹通知，快照到达撤通知。
pub(crate) fn install(cx: &mut App) {
    cx.set_global(ServiceNotices::default());
    let session = Desktop::global(cx).session.clone();
    cx.subscribe(&session, |_, signal, cx| match signal {
        SessionSignal::ServiceFailed(failure) => {
            let notice = {
                let translator = Desktop::global(cx).translator.read(cx);
                failure_notice(failure, &logs_dir(), &|key, args| {
                    translator.text_with(key, args)
                })
            };
            show(notice, cx);
        }
        SessionSignal::Fatal(error) => {
            let notice = {
                let translator = Desktop::global(cx).translator.read(cx);
                fatal_notice(error.code, &logs_dir(), &|key, args| {
                    translator.text_with(key, args)
                })
            };
            show(notice, cx);
        }
        SessionSignal::Snapshot(_) => dismiss(cx),
        SessionSignal::Event(_) | SessionSignal::Stale | SessionSignal::ServiceStopped => {}
    })
    .detach();
}

/// 主窗口就绪后补弹窗口尚不存在时暂存的提示。
pub(crate) fn replay_pending(cx: &mut App) {
    if let Some(notice) = cx.global_mut::<ServiceNotices>().pending.take() {
        show(notice, cx);
    }
}

fn logs_dir() -> String {
    crate::app::agent_data_dir()
        .join("logs")
        .display()
        .to_string()
}

type Text<'a> = &'a dyn Fn(&str, &[(&str, &str)]) -> String;

fn failure_notice(failure: &ServiceFailure, logs: &str, text: Text<'_>) -> Notice {
    let (title, mut message) = match failure {
        ServiceFailure::Launch { program, detail } => (
            text("serviceNoticeLaunchTitle", &[]),
            text(
                "serviceNoticeLaunchBody",
                &[("program", program), ("detail", detail)],
            ),
        ),
        ServiceFailure::Exited { status, startup } => {
            let mut message = text("serviceNoticeExitedBody", &[("status", status)]);
            if let Some(record) = startup {
                let key = match record.kind {
                    AgentStartupFailureKind::GatewayBind => "serviceNoticeStartupGatewayBind",
                    AgentStartupFailureKind::DataDir => "serviceNoticeStartupDataDir",
                    AgentStartupFailureKind::Other => "serviceNoticeStartupOther",
                };
                message.push('\n');
                message.push_str(&text(key, &[("detail", &record.detail)]));
            }
            (text("serviceNoticeExitedTitle", &[]), message)
        }
    };
    message.push('\n');
    message.push_str(&text("serviceNoticeLogs", &[("logs", logs)]));
    Notice { title, message }
}

fn fatal_notice(code: ApplicationErrorCode, logs: &str, text: Text<'_>) -> Notice {
    let key = match code {
        ApplicationErrorCode::ProtocolIncompatible => "serviceNoticeFatalIncompatible",
        ApplicationErrorCode::Unauthorized => "serviceNoticeFatalUnauthorized",
        _ => "serviceNoticeFatalOther",
    };
    let mut message = text(key, &[]);
    message.push('\n');
    message.push_str(&text("serviceNoticeLogs", &[("logs", logs)]));
    Notice {
        title: text("serviceNoticeFatalTitle", &[]),
        message,
    }
}

fn show(notice: Notice, cx: &mut App) {
    let Some(handle) = WindowRegistry::overlay_window(cx) else {
        cx.global_mut::<ServiceNotices>().pending = Some(notice);
        return;
    };
    let serial = {
        let state = cx.global_mut::<ServiceNotices>();
        state.serial = state.serial.wrapping_add(1);
        state.serial
    };
    let Notice { title, message } = notice.clone();
    let shown = handle
        .update(cx, move |_, window, cx| {
            window.remove_notification::<ServiceNoticeKind>(cx);
            window.push_notification(
                toast::error(message)
                    .title(title)
                    .autohide(false)
                    .id1::<ServiceNoticeKind>(serial),
                cx,
            );
        })
        .is_ok();
    let state = cx.global_mut::<ServiceNotices>();
    if shown {
        state.pending = None;
        state.shown_in = Some(handle);
    } else {
        state.pending = Some(notice);
    }
}

/// 会话已恢复：撤掉通知并丢弃暂存。
fn dismiss(cx: &mut App) {
    let state = cx.global_mut::<ServiceNotices>();
    state.pending = None;
    let Some(handle) = state.shown_in.take() else {
        return;
    };
    if let Err(error) = handle.update(cx, |_, window, cx| {
        window.remove_notification::<ServiceNoticeKind>(cx);
    }) {
        log::debug!("window released before the service notice was removed: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::ApplicationErrorCode;
    use fluxdown_protocol::local_service::{AgentStartupFailure, AgentStartupFailureKind};

    use super::{failure_notice, fatal_notice};
    use crate::service_failure::ServiceFailure;

    /// 把键与参数原样回显，便于断言正文由哪些键和值拼成。
    fn echo(key: &str, args: &[(&str, &str)]) -> String {
        let args = args
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("<{key}:{args}>")
    }

    fn exited(kind: Option<AgentStartupFailureKind>) -> ServiceFailure {
        ServiceFailure::Exited {
            status: "exit code 101".to_owned(),
            startup: kind.map(|kind| AgentStartupFailure {
                kind,
                detail: "address in use".to_owned(),
                at_ms: 1,
                version: "0.0.0".to_owned(),
            }),
        }
    }

    #[test]
    fn launch_failure_names_program_detail_and_logs() {
        let notice = failure_notice(
            &ServiceFailure::Launch {
                program: "fluxdown-agent.exe".to_owned(),
                detail: "access denied".to_owned(),
            },
            "/logs",
            &echo,
        );
        assert_eq!(notice.title, "<serviceNoticeLaunchTitle:>");
        assert!(
            notice
                .message
                .contains("program=fluxdown-agent.exe,detail=access denied")
        );
        assert!(notice.message.ends_with("<serviceNoticeLogs:logs=/logs>"));
    }

    #[test]
    fn crash_loop_adds_the_agent_reported_reason_only_when_known() {
        let bare = failure_notice(&exited(None), "/logs", &echo);
        assert_eq!(bare.title, "<serviceNoticeExitedTitle:>");
        assert!(bare.message.contains("status=exit code 101"));
        assert!(!bare.message.contains("serviceNoticeStartup"));

        for (kind, key) in [
            (
                AgentStartupFailureKind::GatewayBind,
                "serviceNoticeStartupGatewayBind",
            ),
            (
                AgentStartupFailureKind::DataDir,
                "serviceNoticeStartupDataDir",
            ),
            (AgentStartupFailureKind::Other, "serviceNoticeStartupOther"),
        ] {
            let notice = failure_notice(&exited(Some(kind)), "/logs", &echo);
            assert!(
                notice
                    .message
                    .contains(&format!("<{key}:detail=address in use>")),
                "{kind:?} must use {key}"
            );
        }
    }

    #[test]
    fn fatal_errors_are_distinguished_by_code() {
        let message = |code| fatal_notice(code, "/logs", &echo).message;
        assert!(
            message(ApplicationErrorCode::ProtocolIncompatible)
                .starts_with("<serviceNoticeFatalIncompatible:>")
        );
        assert!(
            message(ApplicationErrorCode::Unauthorized)
                .starts_with("<serviceNoticeFatalUnauthorized:>")
        );
        assert!(message(ApplicationErrorCode::Internal).starts_with("<serviceNoticeFatalOther:>"));
    }
}
