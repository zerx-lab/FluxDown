//! 应用内更新提示：菜单「检查更新…」的结果与「已更新」的一次性通知。
//!
//! 新版本由 agent 在后台下载，只经主窗口顶栏的更新按钮（`fluxdown_ui_settings::UpdateTitleButton`）
//! 呈现，界面从不为新版本自动弹窗或抢焦点；更新窗口只在用户主动时打开。

use fluxdown_protocol::{UpdatePhase, UpdateStatusDto, method};
use fluxdown_ui_settings::update_view;
use gpui::{App, Global};
use gpui_component::{WindowExt as _, notification::Notification};

use crate::{app::Desktop, windows::WindowRegistry};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Notice {
    message: String,
    error: bool,
}

/// 承载窗口尚不存在时暂存的提示。
#[derive(Default)]
struct PendingNotices(Vec<Notice>);

impl Global for PendingNotices {}

/// 菜单「检查更新…」：有新版本即打开更新窗口（不受跳过限制）；否则通知结果。
pub(crate) fn check_now(cx: &mut App) {
    let future = Desktop::global(cx)
        .client
        .call::<serde_json::Value, UpdateStatusDto>(
            method::AGENT_UPDATE_CHECK,
            Some(serde_json::json!({})),
        );
    cx.spawn(async move |cx| {
        let result = future.await;
        cx.update(|cx| {
            let translator = Desktop::global(cx).translator.read(cx).clone();
            let notice = match result {
                Ok(status) if status.has_update => {
                    crate::windows::update_prompt::open(cx);
                    return;
                }
                Ok(status) if status.phase == UpdatePhase::Failed => Notice {
                    message: translator
                        .text(update_view::failure_key(status.failure))
                        .to_owned(),
                    error: true,
                },
                Ok(_) => Notice {
                    message: translator.text("upToDate").to_owned(),
                    error: false,
                },
                Err(_) => Notice {
                    message: translator.text("localServiceActionFailed").to_owned(),
                    error: true,
                },
            };
            show(notice, cx);
        });
    })
    .detach();
}

/// `--after-update` 启动完成：提示已更新到当前版本。
pub(crate) fn show_installed(cx: &mut App) {
    let message = Desktop::global(cx).translator.read(cx).text_with(
        "updateInstalledToast",
        &[("v", fluxdown_protocol::APP_VERSION)],
    );
    show(
        Notice {
            message,
            error: false,
        },
        cx,
    );
}

/// 主窗口就绪后补弹窗口尚不存在时暂存的提示。
pub(crate) fn replay_pending(cx: &mut App) {
    let pending = std::mem::take(&mut cx.default_global::<PendingNotices>().0);
    for notice in pending {
        show(notice, cx);
    }
}

fn show(notice: Notice, cx: &mut App) {
    let Some(handle) = WindowRegistry::overlay_window(cx) else {
        cx.default_global::<PendingNotices>().0.push(notice);
        return;
    };
    let built = notice.clone();
    let shown = handle
        .update(cx, |_, window, cx| {
            let note = if built.error {
                Notification::error(built.message)
            } else {
                Notification::info(built.message)
            };
            window.push_notification(note, cx);
        })
        .is_ok();
    if !shown {
        cx.default_global::<PendingNotices>().0.push(notice);
    }
}
