//! `/jsonrpc` WebSocket 会话（`GET /jsonrpc` + upgrade）—— aria2 风格生命周期
//! 通知广播 + 双向 JSON-RPC，同一条连接复用。
//!
//! # 通知
//!
//! 连接建立后立即尝试 [`ApiHost::subscribe_task_events`]：
//! - `Some(rx)` —— 宿主已接线事件源，`rx` 上每条 [`TaskEvent`] 经
//!   [`crate::aria2::build_notification_frame`] 翻译为 aria2 通知帧
//!   （`{"jsonrpc":"2.0","method":"aria2.onDownloadXxx","params":[{"gid":...}]}`，
//!   无 `id`）后推给这个 WS 会话——与真实 aria2
//!   `WebSocketSessionMan::onEvent()` 对所有已连接会话广播同一条消息的行为一致
//!   （见 `local://aria2_rpc_methods.md` §5）。
//! - `None` —— 宿主未接线（或不支持）事件源，会话退化为纯双向 RPC，不推送
//!   任何通知（真实 aria2 在没有下载活动时同样不会有通知产生，语义等价）。
//!
//! # 双向 RPC
//!
//! 收到的每个文本帧当作一个 `/jsonrpc` 请求体，经 [`handle_jsonrpc`] 处理后把
//! 响应体原样文本回发——鉴权、方法分发、错误模型与 `POST /jsonrpc` 完全一致，
//! 唯一差异是 WS 没有逐帧自定义头可依赖，`header_token_ok` 恒 `false`（token
//! 只能经 aria2 约定的 `params[0] = "token:xxx"` 携带）。
//!
//! # 连接生命周期
//!
//! - ping/pong 帧由 axum（底层 tungstenite）在读取时自动应答，本模块不处理。
//! - 收到 `Close` 帧、socket 读错误、或广播端 `RecvError::Closed`（宿主全部
//!   `Sender` 已丢弃）→ 退出会话循环，连接关闭。
//! - 广播 `RecvError::Lagged`（本会话消费跟不上事件产生速率）→ 错过的事件无法
//!   补发，继续循环处理下一条，**不断开连接**——WS 通知本就是尽力而为的补充
//!   信息，客户端仍可用 `aria2.tellActive`/`tellStatus` 兜底刷新完整状态。

use axum::extract::ws::{Message, WebSocket};
use tokio::sync::broadcast;

use crate::aria2;
use crate::jsonrpc::handle_jsonrpc;
use crate::service::{ApiHost, TaskEvent};

/// 跑一个 WS 会话直到断开（`Close` 帧 / 读错误 / 广播端全部关闭）。
///
/// `events` 为 `None` 时等价于纯双向 RPC：[`recv_event`] 永久挂起，
/// [`tokio::select!`] 恒选 socket 分支。
pub(crate) async fn run_session(
    mut socket: WebSocket,
    host: &dyn ApiHost,
    config_token: &str,
    mut events: Option<broadcast::Receiver<TaskEvent>>,
) {
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let resp = respond_to_text(host, config_token, text.as_str()).await;
                        if socket.send(Message::Text(resp.into())).await.is_err() {
                            break;
                        }
                    }
                    // 二进制/ping/pong 帧：aria2 客户端不会用它们发 RPC 请求；
                    // ping/pong 已由 axum/tungstenite 在读取时自动应答，忽略后继续。
                    Some(Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_))) => {}
                    // Close 帧、连接已断（`None`）、读错误：退出会话循环。
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                }
            }
            ev = recv_event(&mut events) => {
                match ev {
                    Ok(event) => {
                        let frame = aria2::build_notification_frame(&event.task_id, event.kind);
                        if socket.send(Message::Text(frame.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// 从任务事件订阅拉取下一条事件；未订阅（`None`）时永久挂起，让
/// [`tokio::select!`] 恒选 socket 分支——等价于「宿主未接线通知源」时的纯
/// 双向 RPC 退化路径。
async fn recv_event(
    events: &mut Option<broadcast::Receiver<TaskEvent>>,
) -> Result<TaskEvent, broadcast::error::RecvError> {
    match events {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// WS 入站文本帧 → JSON-RPC 响应文本。不依赖 socket，可直接单测。
///
/// `header_token_ok` 恒 `false`：WS 会话没有逐帧的自定义头可依赖，鉴权只能靠
/// aria2 约定的 `params[0] = "token:xxx"`（复用 [`handle_jsonrpc`] 现有逻辑，
/// 与 `POST /jsonrpc` 完全一致）。
pub(crate) async fn respond_to_text(host: &dyn ApiHost, config_token: &str, text: &str) -> String {
    handle_jsonrpc(host, config_token, false, text.as_bytes())
        .await
        .to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::{Value, json};

    use super::*;
    use crate::service::{ApiError, TaskEventKind};
    use crate::types::{CreateTaskRequest, DownloadRequest, QueueDto, TaskDto};

    /// `respond_to_text` 专用的最小 `ApiHost`：只关心 `create_task` 是否被
    /// 调用。与 `jsonrpc.rs`/`tests.rs` 各自的 mock 相互独立——每个模块只需要
    /// 自己关心的最小子集，刻意不共享。
    #[derive(Default)]
    struct TestHost {
        next_task_id: String,
        created: Mutex<Vec<CreateTaskRequest>>,
    }

    impl TestHost {
        fn new(next_task_id: &str) -> Self {
            Self {
                next_task_id: next_task_id.to_string(),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl ApiHost for TestHost {
        async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError> {
            Ok(vec![])
        }
        async fn get_task(&self, _task_id: &str) -> Result<Option<TaskDto>, ApiError> {
            Ok(None)
        }
        async fn create_task(&self, req: CreateTaskRequest) -> Result<String, ApiError> {
            self.created.lock().unwrap().push(req);
            Ok(self.next_task_id.clone())
        }
        async fn delete_task(&self, _task_id: &str, _delete_files: bool) -> Result<(), ApiError> {
            Ok(())
        }
        async fn pause_task(&self, _task_id: &str) -> Result<(), ApiError> {
            Ok(())
        }
        async fn continue_task(&self, _task_id: &str) -> Result<(), ApiError> {
            Ok(())
        }
        async fn pause_all(&self) -> Result<(), ApiError> {
            Ok(())
        }
        async fn continue_all(&self) -> Result<(), ApiError> {
            Ok(())
        }
        async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
            Ok(vec![])
        }
        async fn submit_external(&self, _req: DownloadRequest) -> Result<(), ApiError> {
            Ok(())
        }
    }

    // -- respond_to_text -----------------------------------------------------

    #[tokio::test]
    async fn respond_to_text_forwards_to_handle_jsonrpc_and_serializes_result() {
        let host = TestHost::new("ws-task-1");
        let text = json!({
            "jsonrpc": "2.0", "id": "1", "method": "aria2.addUri",
            "params": [["https://a.com/f.zip"]]
        })
        .to_string();

        let resp_text = respond_to_text(&host, "", &text).await;
        let resp: Value = serde_json::from_str(&resp_text).unwrap();

        assert_eq!(resp["result"], aria2::task_id_to_gid("ws-task-1"));
        assert_eq!(host.created.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn respond_to_text_ignores_header_token_and_only_trusts_params_token() {
        // WS 无自定义头：即使服务端配置了 token，respond_to_text 也只认
        // params[0]="token:xxx"（header_token_ok 恒 false）。
        let host = TestHost::default();
        let no_token = json!({
            "jsonrpc": "2.0", "id": 1, "method": "aria2.getVersion", "params": []
        })
        .to_string();
        let resp: Value =
            serde_json::from_str(&respond_to_text(&host, "S3CRET", &no_token).await).unwrap();
        assert_eq!(resp["error"]["code"], 1);
        assert_eq!(resp["error"]["message"], "Unauthorized");

        let with_token = json!({
            "jsonrpc": "2.0", "id": 1, "method": "aria2.getVersion",
            "params": ["token:S3CRET"]
        })
        .to_string();
        let resp2: Value =
            serde_json::from_str(&respond_to_text(&host, "S3CRET", &with_token).await).unwrap();
        assert!(resp2["result"]["version"].is_string());
    }

    #[tokio::test]
    async fn respond_to_text_malformed_json_returns_parse_error() {
        let host = TestHost::default();
        let resp: Value =
            serde_json::from_str(&respond_to_text(&host, "", "not json").await).unwrap();
        assert_eq!(resp["error"]["code"], -32700);
    }

    // -- recv_event ------------------------------------------------------------

    #[tokio::test]
    async fn recv_event_surfaces_lagged_then_recovers_the_latest_event() {
        // capacity=1，无中间 `.await`：3 次 send 在 receiver 有机会被轮询前
        // 全部入队，前两条（a/b）被挤出缓冲区，下一次 recv() 必先报
        // `Lagged(2)`，之后才追上缓冲区里唯一剩下的最新事件 "c"。
        let (tx, rx) = broadcast::channel(1);
        let mut events = Some(rx);
        tx.send(TaskEvent {
            task_id: "a".to_string(),
            kind: TaskEventKind::Start,
        })
        .unwrap();
        tx.send(TaskEvent {
            task_id: "b".to_string(),
            kind: TaskEventKind::Pause,
        })
        .unwrap();
        tx.send(TaskEvent {
            task_id: "c".to_string(),
            kind: TaskEventKind::Stop,
        })
        .unwrap();

        let first = recv_event(&mut events).await;
        assert!(matches!(first, Err(broadcast::error::RecvError::Lagged(2))));

        let second = recv_event(&mut events).await.unwrap();
        assert_eq!(second.task_id, "c");
    }

    #[tokio::test]
    async fn recv_event_returns_closed_after_all_senders_dropped() {
        let (tx, rx) = broadcast::channel::<TaskEvent>(1);
        let mut events = Some(rx);
        drop(tx);
        assert!(matches!(
            recv_event(&mut events).await,
            Err(broadcast::error::RecvError::Closed)
        ));
    }
}
