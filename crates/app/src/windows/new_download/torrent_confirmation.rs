//! 由宿主持有的种子确认请求：表单只是可选的观察者，从不持有请求。
use std::sync::Arc;

use fluxdown_protocol::{
    ApplicationErrorCode, RpcErrorData, torrent_preview::TorrentConfirmParams,
};
use fluxdown_ui_downloads::{
    DownloadsCommand, DownloadsPort, DownloadsResult, PortFuture, SubmitNotice,
    TorrentConfirmationDone,
};
use gpui::App;

use crate::{app::Desktop, downloads_port::AgentDownloadsPort, lifecycle};

pub(super) fn start(
    params: Box<TorrentConfirmParams>,
    external: bool,
    done: TorrentConfirmationDone,
    port: &Arc<AgentDownloadsPort>,
    cx: &mut App,
) {
    let paused = params.start_paused;
    let request = port.execute(DownloadsCommand::ConfirmTorrent(params));
    let task = cx.spawn(async move |cx| {
        run(request, |result| {
            cx.update(|cx| {
                if let Ok(task_id) = &result
                    && !paused
                {
                    if external {
                        crate::progress_windows::user_started_on_launch(task_id.clone(), cx);
                    } else {
                        crate::progress_windows::user_started(
                            fluxdown_ui_downloads::UserStartIntent::Created(task_id.clone()),
                            cx,
                        );
                    }
                }
                let failure = result.as_ref().err().cloned();
                let displayed = done(result, cx);
                if let Some(error) = failure
                    && !displayed
                {
                    log::warn!(
                        "torrent confirmation failed after form closed: {:?}",
                        error.code
                    );
                    let translator = Desktop::global(cx).translator.clone();
                    let message = translator
                        .read(cx)
                        .text("localServiceActionFailed")
                        .to_owned();
                    super::show_notice(SubmitNotice { ok: false, message }, cx);
                }
            });
        })
        .await;
    });
    lifecycle::keep_alive(cx, task).detach();
}

// 原来的视图已经不在时，完成处理照样执行。
pub(crate) async fn run(
    request: PortFuture<DownloadsResult>,
    completed: impl FnOnce(Result<String, RpcErrorData>),
) {
    let result = request.await.and_then(|result| match result {
        DownloadsResult::Value(value) => value
            .get("taskId")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| RpcErrorData::new(ApplicationErrorCode::Internal, false)),
        _ => Err(RpcErrorData::new(ApplicationErrorCode::Internal, false)),
    });
    completed(result);
}

#[cfg(test)]
mod tests {
    use super::run;
    use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData};
    use fluxdown_ui_downloads::DownloadsResult;

    /// `run` 只负责把端口应答归一成「任务 ID 或错误」交给宿主回调；表单关闭后请求
    /// 仍由宿主持有，这一点由 `start` 里的 `lifecycle::keep_alive` 保证，不在这里覆盖。
    #[tokio::test]
    async fn port_replies_are_normalized_into_a_task_id_or_an_error() {
        let unavailable = RpcErrorData::new(ApplicationErrorCode::Unavailable, true);
        for (reply, expected) in [
            (
                Ok(DownloadsResult::Value(
                    serde_json::json!({"taskId": "created"}),
                )),
                Ok("created".to_owned()),
            ),
            (Err(unavailable.clone()), Err(unavailable)),
            // 没有任务 ID 的「成功」不能当作已建任务。
            (
                Ok(DownloadsResult::Value(serde_json::json!({"taskId": ""}))),
                Err(RpcErrorData::new(ApplicationErrorCode::Internal, false)),
            ),
            (
                Ok(DownloadsResult::Unit),
                Err(RpcErrorData::new(ApplicationErrorCode::Internal, false)),
            ),
        ] {
            let mut completed = None;
            run(Box::pin(async move { reply }), |result| {
                completed = Some(result);
            })
            .await;
            assert_eq!(completed.expect("host callback always runs"), expected);
        }
    }
}
