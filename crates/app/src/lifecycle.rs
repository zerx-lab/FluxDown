//! 桌面进程的两种退出，与 agent 驻留策略（`AgentSnapshot.shell.resident`）配合：
//!
//! - **只退出界面**：托盘驻留时关闭主窗口 / 关闭最后一个窗口。agent + daemon 继续运行，
//!   托盘由 agent 承载，点击托盘再拉起本进程。
//! - **完全退出**：菜单「退出」、⌘Q，或非驻留模式下关闭主窗口。先请求 agent
//!   `system.shutdown`（agent 先关停 daemon 再退出），再退出本进程。
//!
//! agent 以 `service-quit` 关闭连接（例如托盘「退出」）时本进程同样立即退出且不再重拉 agent。

use std::time::Duration;

use fluxdown_protocol::method;
use gpui::{App, Global, Task};
use serde_json::Value;

use crate::{app::Desktop, windows::WindowRegistry};

/// 关窗后仍须送达 agent 的请求（新建下载提交、外部捕获确认 / 忽略）数量。未完成前
/// 「只退出界面」推迟到它们结束，避免最后一个窗口关闭后进程先退出、请求丢失。
#[derive(Default)]
struct InFlight {
    count: usize,
    quit_pending: bool,
}

impl Global for InFlight {}

impl InFlight {
    fn finish(&mut self) -> bool {
        self.count = self.count.saturating_sub(1);
        self.count == 0 && std::mem::take(&mut self.quit_pending)
    }

    fn request_quit(&mut self, open_windows: usize) -> bool {
        // defer 执行前可能收到新捕获、托盘唤起或提交结果打开的进度 / 错误窗口。
        self.quit_pending = open_windows == 0 && self.count > 0;
        open_windows == 0 && self.count == 0
    }
}

/// 登记一个关窗后仍须完成的请求；返回的任务完成时解除登记（期间推迟的退出随之执行）。
pub fn keep_alive<R: 'static>(cx: &mut App, task: Task<R>) -> Task<R> {
    cx.default_global::<InFlight>().count += 1;
    cx.spawn(async move |cx| {
        let output = task.await;
        cx.update(|cx| {
            if cx.default_global::<InFlight>().finish() {
                quit_ui(cx);
            }
        });
        output
    })
}

/// 等 agent 受理 `system.shutdown` 的上限；agent 未连接时不能让界面卡在退出中。
const SHUTDOWN_REQUEST_BUDGET: Duration = Duration::from_secs(3);

/// 完全退出：停止后台（agent + daemon）后退出界面；重复调用无副作用。
pub fn quit_everything(cx: &mut App) {
    let desktop = Desktop::global_mut(cx);
    if desktop.quitting {
        return;
    }
    desktop.quitting = true;
    let client = desktop.client.clone();
    client.stop_service_bootstrap();
    let request = client.call::<Value, Value>(method::SYSTEM_SHUTDOWN, None);
    let timer = cx.background_executor().timer(SHUTDOWN_REQUEST_BUDGET);
    cx.spawn(async move |cx| {
        match futures_util::future::select(request, timer).await {
            futures_util::future::Either::Left((result, _timer)) => {
                if let Err(error) = result {
                    log::warn!(
                        "shutdown request failed before desktop exit: {:?}",
                        error.code
                    );
                }
            }
            futures_util::future::Either::Right(((), _request)) => {
                log::warn!("shutdown request timed out before desktop exit");
            }
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// 只退出界面（后台驻留）。延后一轮执行：同一轮里随窗口释放的视图（例如关窗时忽略
/// 外部捕获）先经 [`keep_alive`] 登记在途请求，登记了就等它们结束再退出。
pub fn quit_ui(cx: &mut App) {
    cx.defer(|cx| {
        let open_windows = WindowRegistry::open_count(cx);
        if !cx.default_global::<InFlight>().request_quit(open_windows) {
            return;
        }
        let desktop = Desktop::global_mut(cx);
        if desktop.quitting {
            return;
        }
        desktop.quitting = true;
        cx.quit();
    });
}

/// agent 已完全退出：界面随之退出，且不再拉起 agent。
pub fn service_stopped(cx: &mut App) {
    let desktop = Desktop::global_mut(cx);
    desktop.client.stop_service_bootstrap();
    desktop.quitting = true;
    cx.quit();
}

/// 退出流程外的应用退出（macOS Dock「退出」、系统注销等）：非驻留模式下尽力通知 agent
/// 一起退出。GPUI 只给 `on_app_quit` 很短的收尾时间，这里只发请求不保证送达；送达失败时
/// 由 agent 的闲置检测兜底。
pub fn shutdown_request_on_app_quit(
    cx: &mut App,
) -> Option<crate::agent_client::AgentFuture<Value>> {
    let known = Desktop::global(cx).session.read(cx).latest().is_some();
    let desktop = Desktop::global_mut(cx);
    if desktop.quitting || desktop.shell.resident || !known {
        return None;
    }
    desktop.quitting = true;
    desktop.client.stop_service_bootstrap();
    Some(
        desktop
            .client
            .call::<Value, Value>(method::SYSTEM_SHUTDOWN, None),
    )
}

#[cfg(test)]
mod tests {
    use super::InFlight;

    #[test]
    fn last_window_waits_for_every_submission_then_exits() {
        let mut requests = InFlight {
            count: 2,
            quit_pending: false,
        };
        assert!(!requests.request_quit(0));
        assert!(!requests.finish());
        assert!(requests.finish());
        assert!(requests.request_quit(0));
    }

    #[test]
    fn reopened_window_cancels_deferred_exit_until_it_closes() {
        let mut requests = InFlight {
            count: 1,
            quit_pending: false,
        };
        assert!(!requests.request_quit(0));
        assert!(requests.finish());
        // 提交完成后、defer 执行前，进度窗 / 新捕获 / 主窗口打开。
        assert!(!requests.request_quit(1));
        assert!(!requests.quit_pending);
        assert!(requests.request_quit(0));
    }

    #[test]
    fn window_opened_while_submitting_stays_alive_after_completion() {
        let mut requests = InFlight {
            count: 1,
            quit_pending: false,
        };
        assert!(!requests.request_quit(0));
        assert!(!requests.request_quit(1));
        assert!(!requests.finish());
        assert!(requests.request_quit(0));
    }

    #[test]
    fn request_finished_before_close_does_not_leave_a_headless_process() {
        let mut requests = InFlight {
            count: 1,
            quit_pending: false,
        };
        assert!(!requests.finish());
        assert!(requests.request_quit(0));
    }
}
