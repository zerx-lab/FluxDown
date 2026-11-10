//! 定时关机 / 睡眠的界面侧：状态机与执行归 agent（托盘驻留、界面全部关闭后仍要按时执行）。
//!
//! 这里只做三件事：把 agent 快照中的 `power` 投影成状态栏读取的 [`SharedShutdownStatus`]；
//! 把状态栏请求转成 `agent.power.arm/disarm`；倒计时期间在主窗口挂常驻通知（无主窗口时由
//! agent 托盘 tooltip 展示）。

use std::rc::Rc;
use std::time::Duration;

use fluxdown_protocol::{
    AgentEvent, PowerAction, PowerArmParams, PowerStatusDto, ServiceEvent, method,
};
use fluxdown_ui_downloads::{SharedShutdownStatus, ShutdownPort, ShutdownRequest, ShutdownStatus};
use gpui::{App, Global};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use serde_json::Value;

use crate::app::Desktop;
use crate::session::{SessionSignal, agent_body};
use crate::windows::{WindowKey, WindowRegistry};

struct ShutdownGlobal {
    status: SharedShutdownStatus,
    port: ShutdownPort,
}

impl Global for ShutdownGlobal {}

/// 安装完成后关机投影：跟随会话快照 / `PowerChanged` 更新状态并刷新主窗口。
pub fn install(cx: &mut App) {
    if cx.has_global::<ShutdownGlobal>() {
        return;
    }
    let status: SharedShutdownStatus = Rc::new(std::cell::Cell::new(ShutdownStatus::default()));
    let client = Desktop::global(cx).client.clone();
    let port: ShutdownPort = Rc::new(move |request, cx| {
        let call = match request {
            ShutdownRequest::Disarm => {
                client.call::<Value, Value>(method::AGENT_POWER_DISARM, None)
            }
            ShutdownRequest::Arm { delay, action } => client.call::<PowerArmParams, Value>(
                method::AGENT_POWER_ARM,
                Some(PowerArmParams {
                    delay_secs: delay.as_secs(),
                    action,
                }),
            ),
        };
        cx.spawn(async move |cx| {
            if let Err(error) = call.await {
                log::warn!("scheduled power action request failed: {:?}", error.code);
                cx.update(|cx| {
                    let Some(handle) = WindowRegistry::handle(cx, &WindowKey::Main) else {
                        return;
                    };
                    let message = Desktop::global(cx)
                        .translator
                        .read(cx)
                        .text("localServiceActionFailed")
                        .to_owned();
                    if let Err(error) = handle.update(cx, |_, window, cx| {
                        window.push_notification(Notification::error(message), cx);
                    }) {
                        log::debug!("power failure notification window released: {error:#}");
                    }
                });
            }
        })
        .detach();
    });
    cx.set_global(ShutdownGlobal {
        status: status.clone(),
        port,
    });

    let session = Desktop::global(cx).session.clone();
    if let Some(power) = session.read(cx).agent_snapshot().map(|body| body.power) {
        apply(&status, power, cx);
    }
    cx.subscribe(&session, move |_, signal, cx| match signal {
        SessionSignal::Snapshot(snapshot) => {
            if let Some(body) = agent_body(snapshot) {
                apply(&status, body.power, cx);
            }
        }
        SessionSignal::Event(frame) => {
            if let ServiceEvent::Agent(AgentEvent::PowerChanged(power)) = &frame.event {
                apply(&status, *power, cx);
            }
        }
        SessionSignal::Stale
        | SessionSignal::Fatal(_)
        | SessionSignal::ServiceStopped
        | SessionSignal::ServiceFailed(_) => {}
    })
    .detach();
}

/// 状态栏用只读句柄；`install` 尚未调用时返回 `None`。
pub fn status(cx: &App) -> Option<SharedShutdownStatus> {
    cx.try_global::<ShutdownGlobal>()
        .map(|global| global.status.clone())
}

/// 状态栏 / 通知取消按钮用的请求端口；`install` 尚未调用时返回 `None`。
pub fn shutdown_port(cx: &App) -> Option<ShutdownPort> {
    cx.try_global::<ShutdownGlobal>()
        .map(|global| global.port.clone())
}

fn apply(status: &SharedShutdownStatus, power: PowerStatusDto, cx: &mut App) {
    let next = shutdown_status(power);
    if status.get() == next {
        return;
    }
    status.set(next);
    if let Some(downloads) = Desktop::global(cx)
        .main_downloads
        .as_ref()
        .and_then(gpui::WeakEntity::upgrade)
    {
        downloads.update(cx, |_, cx| cx.notify());
    }
    match next.countdown_remaining {
        Some(remaining) => show_countdown(cx, remaining, next.action),
        None => clear_countdown(cx),
    }
}

fn shutdown_status(power: PowerStatusDto) -> ShutdownStatus {
    ShutdownStatus {
        armed_delay: power.armed_delay_secs.map(Duration::from_secs),
        countdown_remaining: power.countdown_remaining_secs.map(Duration::from_secs),
        action: power.action,
    }
}

struct ShutdownNote;

fn show_countdown(cx: &mut App, remaining: Duration, action: PowerAction) {
    let Some(handle) = WindowRegistry::handle(cx, &WindowKey::Main) else {
        return;
    };
    let (countdown_key, cancel_key) = match action {
        PowerAction::Shutdown => ("shutdownCountdown", "shutdownCancelButton"),
        PowerAction::Sleep => ("sleepCountdown", "sleepCancelButton"),
    };
    let translator = Desktop::global(cx).translator.read(cx).clone();
    let message = translator.text_with(countdown_key, &[("time", &format_remaining(remaining))]);
    let cancel_label = translator.text(cancel_key).to_owned();
    if let Err(error) = handle.update(cx, |_, window, cx| {
        let note = Notification::warning(message)
            .id::<ShutdownNote>()
            .autohide(false)
            .action(move |_, _, _| {
                let cancel_label = cancel_label.clone();
                gpui_component::button::Button::new("shutdown-cancel-button")
                    .label(cancel_label)
                    .on_click(|_, _, cx| {
                        if let Some(port) = shutdown_port(cx) {
                            port(ShutdownRequest::Disarm, cx);
                        }
                    })
            });
        window.push_notification(note, cx);
    }) {
        log::debug!("view or window released before lifecycle update: {error:#}");
    }
}

fn clear_countdown(cx: &mut App) {
    if let Some(handle) = WindowRegistry::handle(cx, &WindowKey::Main)
        && let Err(error) = handle.update(cx, |_, window, cx| {
            window.remove_notification::<ShutdownNote>(cx);
        })
    {
        log::debug!("view or window released before lifecycle update: {error:#}");
    }
}

/// `mm:ss`，与 agent 托盘 tooltip 一致。
fn format_remaining(remaining: Duration) -> String {
    let total = remaining.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_remaining_pads_minutes_and_seconds() {
        assert_eq!(format_remaining(Duration::from_secs(65)), "01:05");
        assert_eq!(format_remaining(Duration::from_secs(5)), "00:05");
        assert_eq!(format_remaining(Duration::from_secs(600)), "10:00");
    }
}
