//! 插件自动禁用提示：插件被熔断器或插件市场撤回而自动禁用是全局事件（提示引导用户到「设置 → 扩展」处理），
//! 不依赖设置窗口是否打开。应用级订阅 agent 会话，在承载浮层的窗口（主窗口优先）弹 warning
//! 通知；暂时没有承载窗口时暂存，主窗口就绪后补弹。

use std::collections::HashMap;

use fluxdown_protocol::{AgentEvent, DaemonEvent, PluginDto, ServiceEvent, WsServerMsg};
use fluxdown_ui_components::toast;
use gpui::{App, Global};
use gpui_component::WindowExt as _;

use crate::{
    app::Desktop,
    session::{SessionSignal, agent_body},
    windows::WindowRegistry,
};

#[derive(Default)]
struct PluginNotices {
    /// 插件 identity → 展示名（来自快照与插件列表事件）。
    names: HashMap<String, String>,
    /// 承载窗口尚不存在时暂存的提示文案。
    pending: Vec<String>,
}

impl Global for PluginNotices {}

/// 订阅会话：缓存插件展示名，熔断事件到达时弹通知。
pub(crate) fn install(cx: &mut App) {
    cx.set_global(PluginNotices::default());
    let session = Desktop::global(cx).session.clone();
    if let Some(body) = session.read(cx).agent_snapshot() {
        let plugins = body.daemon.plugins.clone();
        remember_names(cx, &plugins);
    }
    cx.subscribe(&session, |_, signal, cx| match signal {
        SessionSignal::Snapshot(snapshot) => {
            if let Some(body) = agent_body(snapshot) {
                remember_names(cx, &body.daemon.plugins);
            }
        }
        SessionSignal::Event(frame) => match &frame.event {
            ServiceEvent::Agent(AgentEvent::DaemonSnapshotReplaced(snapshot))
            | ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(snapshot)))
            | ServiceEvent::Daemon(DaemonEvent::SnapshotReplaced(snapshot)) => {
                remember_names(cx, &snapshot.plugins);
            }
            ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::PluginsChanged(plugins)))
            | ServiceEvent::Daemon(DaemonEvent::PluginsChanged(plugins)) => {
                remember_names(cx, plugins);
            }
            ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::Engine(
                WsServerMsg::PluginAutoDisabled { identity, reason },
            )))
            | ServiceEvent::Daemon(DaemonEvent::Engine(WsServerMsg::PluginAutoDisabled {
                identity,
                reason,
            })) => notify_auto_disabled(identity, reason, cx),
            _ => {}
        },
        SessionSignal::Stale
        | SessionSignal::Fatal(_)
        | SessionSignal::ServiceStopped
        | SessionSignal::ServiceFailed(_) => {}
    })
    .detach();
}

/// 主窗口就绪后补弹窗口尚不存在时暂存的提示。
pub(crate) fn replay_pending(cx: &mut App) {
    let pending = std::mem::take(&mut cx.global_mut::<PluginNotices>().pending);
    for message in pending {
        show(message, cx);
    }
}

fn remember_names(cx: &mut App, plugins: &[PluginDto]) {
    let names = &mut cx.global_mut::<PluginNotices>().names;
    names.clear();
    names.extend(
        plugins
            .iter()
            .map(|plugin| (plugin.identity.clone(), plugin.name.clone())),
    );
}

/// 提示里的插件名：有展示名用展示名，取不到退回 identity。
fn display_name<'a>(names: &'a HashMap<String, String>, identity: &'a str) -> &'a str {
    names
        .get(identity)
        .map(String::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(identity)
}

fn notify_auto_disabled(identity: &str, reason: &str, cx: &mut App) {
    let name = display_name(&cx.global::<PluginNotices>().names, identity).to_owned();
    let key = if reason == "Revoked" {
        "pluginRevokedToast"
    } else {
        "pluginAutoDisabledToast"
    };
    let message = Desktop::global(cx)
        .translator
        .read(cx)
        .text_with(key, &[("name", &name)]);
    show(message, cx);
}

fn show(message: String, cx: &mut App) {
    let Some(handle) = WindowRegistry::overlay_window(cx) else {
        cx.global_mut::<PluginNotices>().pending.push(message);
        return;
    };
    let notice = message.clone();
    let shown = handle
        .update(cx, |_, window, cx| {
            window.push_notification(toast::warning(notice), cx);
        })
        .is_ok();
    if !shown {
        cx.global_mut::<PluginNotices>().pending.push(message);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::display_name;

    #[test]
    fn notice_uses_display_name_and_falls_back_to_identity() {
        let names = HashMap::from([
            ("a@1".to_owned(), "Alpha".to_owned()),
            ("b@1".to_owned(), String::new()),
        ]);
        assert_eq!(display_name(&names, "a@1"), "Alpha");
        assert_eq!(display_name(&names, "b@1"), "b@1");
        assert_eq!(display_name(&names, "missing"), "missing");
    }
}
