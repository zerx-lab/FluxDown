//! WebSocket 事件转发：广播接收与套接字写入解耦，落后的订阅者靠合并而非断线追赶。
//!
//! 连接循环里直接 `recv → socket.send` 时，套接字一慢（对端不读 / 大帧 / 网络抖动），广播
//! 接收端就停止消费，积压超过广播容量即 `Lagged` → 4009 断线重拉整份快照。这里拆成两段：
//!
//! - [`EventPump`] 起一个任务始终及时 `recv`，把帧放进 [`FrameQueue`]（同一实体「后帧完整取代
//!   前帧」的事件就地合并，积压量被实体数而非事件数封顶）。
//! - [`run_writer`] 独占套接字写端：RPC 应答 / 控制帧走有界的有序通道，事件从队列里逐帧取出
//!   序列化后写出。所有 `await` 点都在 `select!` 的分支体之外，写入不会被取消在半途。
//!
//! 合并后待发帧数超过 [`MAX_PENDING_FRAMES`]（或广播仍然 `Lagged`）才以 4009 `event-gap`
//! 断开并 warn，这时订阅者确实已无法在合理内存内追上。

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use fluxdown_protocol::coalesce::FrameQueue;
use fluxdown_protocol::{EventFrame, JSONRPC_VERSION, method};
use futures_util::SinkExt;
use futures_util::stream::SplitSink;
use serde::Serialize;
use tokio::sync::{Notify, broadcast, mpsc};
use tokio::task::JoinHandle;

/// 合并后待发帧数上限。
pub(crate) const MAX_PENDING_FRAMES: usize = 16_384;
/// 连接内待写出的应答 / 控制帧上限；写端卡住时连接循环据此停止读取新请求。
pub(crate) const OUTBOX_CAPACITY: usize = 256;
/// 连接收尾时等待写端冲刷已入队消息（含关闭帧）的最长时间。
const WRITER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// 接收任务每次持锁最多连续并入的帧数，之后让出给写端。
const DRAIN_BATCH: usize = 512;

/// 事件流的终止原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PumpEnd {
    /// 广播接收端自己落后，丢了 `skipped` 帧。
    Lagged { skipped: u64 },
    /// 合并后待发帧数超限：对端读得太慢。
    Overflow { pending: usize },
    /// 事件源已关闭。
    Closed,
}

struct State {
    queue: FrameQueue,
    end: Option<PumpEnd>,
}

struct Shared {
    state: Mutex<State>,
    wake: Notify,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 广播接收任务的所有者；丢弃即中止接收。
pub(crate) struct EventPump {
    shared: Arc<Shared>,
    task: Option<JoinHandle<()>>,
}

/// 写端取帧的句柄。
#[derive(Clone)]
pub(crate) struct EventReader {
    shared: Arc<Shared>,
}

impl EventPump {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    queue: FrameQueue::new(),
                    end: None,
                }),
                wake: Notify::new(),
            }),
            task: None,
        }
    }

    #[must_use]
    pub(crate) fn reader(&self) -> EventReader {
        EventReader {
            shared: Arc::clone(&self.shared),
        }
    }

    /// 开始转发；已经在转发时忽略（一条连接只订阅一次）。
    pub(crate) fn attach(&mut self, receiver: broadcast::Receiver<Arc<EventFrame>>, limit: usize) {
        if self.task.is_none() {
            self.task = Some(tokio::spawn(pump(
                Arc::clone(&self.shared),
                receiver,
                limit,
            )));
        }
    }
}

impl Drop for EventPump {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl EventReader {
    /// 下一帧待发事件。取消安全：不持有任何跨 `await` 的状态。
    pub(crate) async fn next(&self) -> Result<EventFrame, PumpEnd> {
        loop {
            {
                let mut state = lock(&self.shared.state);
                if let Some(end) = state.end {
                    return Err(end);
                }
                if let Some(frame) = state.queue.pop() {
                    return Ok(frame);
                }
            }
            self.shared.wake.notified().await;
        }
    }
}

async fn pump(
    shared: Arc<Shared>,
    mut receiver: broadcast::Receiver<Arc<EventFrame>>,
    limit: usize,
) {
    use broadcast::error::{RecvError, TryRecvError};
    loop {
        let first = receiver.recv().await;
        let end = {
            let mut state = lock(&shared.state);
            let mut incoming = first;
            let mut batched = 0;
            let end = loop {
                match incoming {
                    Ok(frame) => {
                        // 合并会就地改写帧：广播环仍持有同一 Arc，这里取得自己的副本。
                        state.queue.push(Arc::unwrap_or_clone(frame));
                        if state.queue.len() > limit {
                            break Some(PumpEnd::Overflow {
                                pending: state.queue.len(),
                            });
                        }
                    }
                    Err(RecvError::Lagged(skipped)) => break Some(PumpEnd::Lagged { skipped }),
                    Err(RecvError::Closed) => break Some(PumpEnd::Closed),
                }
                batched += 1;
                if batched >= DRAIN_BATCH {
                    break None;
                }
                incoming = match receiver.try_recv() {
                    Ok(frame) => Ok(frame),
                    Err(TryRecvError::Empty) => break None,
                    Err(TryRecvError::Lagged(skipped)) => Err(RecvError::Lagged(skipped)),
                    Err(TryRecvError::Closed) => Err(RecvError::Closed),
                };
            };
            if let Some(end) = end {
                if matches!(end, PumpEnd::Overflow { .. }) {
                    // 连接即将以 4009 断开：积压帧不会再被使用。
                    state.queue = FrameQueue::new();
                }
                state.end = Some(end);
            }
            end
        };
        shared.wake.notify_one();
        if end.is_some() {
            return;
        }
    }
}

/// `service.event` 通知：借用帧直接序列化一遍（不经 `serde_json::Value` 中转）。
#[derive(Serialize)]
struct EventNotification<'a> {
    jsonrpc: &'static str,
    method: &'static str,
    params: &'a EventFrame,
}

fn event_text(frame: &EventFrame) -> Result<String, serde_json::Error> {
    serde_json::to_string(&EventNotification {
        jsonrpc: JSONRPC_VERSION,
        method: method::SERVICE_EVENT,
        params: frame,
    })
}

/// 套接字写端。`outbox` 的消息（应答 / Pong / 关闭帧）优先且保持入队顺序，事件按队列顺序
/// 夹在其间；`outbox` 关闭（连接循环结束）或写失败即返回。
pub(crate) async fn run_writer(
    mut sink: SplitSink<WebSocket, Message>,
    mut outbox: mpsc::Receiver<Message>,
    events: EventReader,
) {
    loop {
        tokio::select! {
            biased;
            message = outbox.recv() => {
                let Some(message) = message else { return };
                let closing = matches!(message, Message::Close(_));
                if sink.send(message).await.is_err() || closing {
                    return;
                }
            }
            event = events.next() => match event {
                Ok(frame) => {
                    let text = match event_text(&frame) {
                        Ok(text) => text,
                        Err(error) => {
                            tracing::warn!(%error, sequence = frame.sequence, "event frame is not serializable; closing connection");
                            return;
                        }
                    };
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                Err(PumpEnd::Closed) => return,
                Err(end) => {
                    match end {
                        PumpEnd::Lagged { skipped } => tracing::warn!(
                            skipped,
                            "event subscriber fell behind the broadcast; closing with 4009 event-gap"
                        ),
                        PumpEnd::Overflow { pending } => tracing::warn!(
                            pending,
                            limit = MAX_PENDING_FRAMES,
                            "event subscriber is too slow to drain coalesced frames; closing with 4009 event-gap"
                        ),
                        PumpEnd::Closed => {}
                    }
                    let close = Message::Close(Some(CloseFrame {
                        code: 4009,
                        reason: "event-gap".into(),
                    }));
                    if let Err(error) = sink.send(close).await {
                        // 对端可能已经断开；关闭帧 best effort。
                        tracing::debug!(%error, close_code = 4009, "event-gap close frame was not delivered");
                    }
                    return;
                }
            },
        }
    }
}

/// 结束写端：关闭 `outbox`，让已入队消息在限时内冲刷，超时则中止。
pub(crate) async fn finish_writer(outbox: mpsc::Sender<Message>, mut writer: JoinHandle<()>) {
    drop(outbox);
    if tokio::time::timeout(WRITER_DRAIN_TIMEOUT, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{DaemonEvent, ServiceEvent, WsServerMsg, judge_frame};
    use fluxdown_protocol::{DaemonRuntimeStatsDto, FrameVerdict};

    use super::*;

    fn frame(sequence: u64, event: DaemonEvent) -> Arc<EventFrame> {
        Arc::new(EventFrame {
            epoch: "epoch".to_owned(),
            sequence,
            previous: None,
            event: ServiceEvent::Daemon(event),
        })
    }

    fn progress(sequence: u64, task: &str, downloaded: i64) -> Arc<EventFrame> {
        frame(
            sequence,
            DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id: task.to_owned(),
                status: 1,
                downloaded_bytes: downloaded,
                total_bytes: 1 << 40,
                speed: 0,
                file_name: String::new(),
                save_dir: String::new(),
                upload_speed: 0,
                url: String::new(),
                error_message: String::new(),
                uploaded_bytes: 0,
                seeding_status: 0,
                seeding_message: String::new(),
                seeding_time_secs: 0,
            }),
        )
    }

    /// 接收任务不被读端拖住：对端不取帧时积压被合并，之后取出的帧链按 `judge_frame` 无缺口，
    /// 且每个任务的最后一帧都在。
    #[tokio::test]
    async fn stalled_reader_gets_a_gapless_coalesced_chain() {
        let (sender, receiver) = broadcast::channel(64);
        let mut pump = EventPump::new();
        pump.attach(receiver, MAX_PENDING_FRAMES);
        let reader = pump.reader();

        let tasks = 8_u64;
        let total = 20_000_u64;
        for sequence in 1..=total {
            let task = format!("task-{}", sequence % tasks);
            assert!(
                sender
                    .send(progress(sequence, &task, sequence as i64))
                    .is_ok()
            );
            // 广播容量只有 64：不让出就会在测试线程里把接收任务甩下。
            if sequence % 32 == 0 {
                tokio::task::yield_now().await;
            }
        }

        let mut cursor = 0_u64;
        let mut received = 0_usize;
        let mut last_by_task = std::collections::HashMap::new();
        while cursor < total {
            let next = tokio::time::timeout(Duration::from_secs(5), reader.next())
                .await
                .expect("frame arrives")
                .expect("pump stays alive");
            assert_eq!(
                judge_frame("epoch", cursor, &next),
                FrameVerdict::Accept,
                "gap before sequence {}",
                next.sequence
            );
            cursor = next.sequence;
            received += 1;
            if let ServiceEvent::Daemon(DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id,
                downloaded_bytes,
                ..
            })) = next.event
            {
                last_by_task.insert(task_id, downloaded_bytes);
            }
        }
        assert!(
            received < 1_000,
            "{received} frames: backlog must be coalesced per task"
        );
        assert_eq!(last_by_task.len(), tasks as usize);
        for (task, downloaded) in last_by_task {
            let index: u64 = task
                .trim_start_matches("task-")
                .parse()
                .expect("task index");
            // 每个任务取到的是它最后一次进度。
            assert_eq!(
                u64::try_from(downloaded).expect("positive"),
                total - (total - index) % tasks
            );
        }
    }

    /// 不可合并的事件超过上限：回退为 `Overflow`，并丢弃积压。
    #[tokio::test]
    async fn unmergeable_backlog_beyond_the_limit_ends_the_stream() {
        let (sender, receiver) = broadcast::channel(64);
        let mut pump = EventPump::new();
        pump.attach(receiver, 8);
        let reader = pump.reader();
        for sequence in 1..=32 {
            let event = DaemonEvent::TaskDeleted {
                task_id: format!("task-{sequence}"),
            };
            if sender.send(frame(sequence, event)).is_err() {
                // 接收任务已因超限结束。
                break;
            }
            tokio::task::yield_now().await;
        }
        let end = tokio::time::timeout(Duration::from_secs(5), reader.next())
            .await
            .expect("stream ends");
        assert!(
            matches!(end, Err(PumpEnd::Overflow { pending: 9 })),
            "{end:?}"
        );
    }

    #[tokio::test]
    async fn broadcast_lag_and_close_are_reported() {
        let (sender, receiver) = broadcast::channel(2);
        let mut pump = EventPump::new();
        // 不让出：接收任务没机会运行，广播环被写穿。
        pump.attach(receiver, MAX_PENDING_FRAMES);
        let reader = pump.reader();
        for sequence in 1..=8 {
            let stats = DaemonEvent::RuntimeStatsChanged(DaemonRuntimeStatsDto::default());
            assert!(sender.send(frame(sequence, stats)).is_ok());
        }
        let end = tokio::time::timeout(Duration::from_secs(5), reader.next())
            .await
            .expect("stream ends");
        assert!(matches!(end, Err(PumpEnd::Lagged { .. })), "{end:?}");
    }
}
