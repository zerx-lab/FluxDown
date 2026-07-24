//! WS 广播中枢 + 引擎事件/选择接口的服务器端实现。
//!
//! - [`EngineEventSink`]：`EngineEvent` → [`WsServerMsg`] → JSON →
//!   `broadcast::Sender` fan-out（非阻塞，满足 `EventSink` 的禁阻塞契约）。
//!   同时维护任务前态表（`task_id` → 最近一次已知 status），按统一规则
//!   （[`task_event_for_transition`] / [`reconcile_snapshot_states`]）把
//!   状态迁移映射为 aria2 兼容层 WS 通知源
//!   [`TaskEvent`](fluxdown_api::service::TaskEvent)（经
//!   `ApiHost::subscribe_task_events` 由 jsonrpc 层订阅并转译为
//!   `aria2.onDownloadXxx` 通知帧）。
//! - [`WsHostSelection`]：HLS/BT 选择请求经 WS 广播给全部客户端，用
//!   oneshot 等待表接收任一客户端的应答（镜像
//!   `hub/src/rinf_selection.rs` 的桌面实现）。
//!
//! ## 删除路径的 Stop 时序
//!
//! `DownloadManager::delete_task`/`delete_tasks_batch`（`download_manager.rs`）
//! 会先发一条 `status=4, error_message="deleted"` 的合成 `TaskProgress`
//! （仅用于让各 `EventSink` 清理自身内部状态表，见 [`is_delete_sentinel`]），
//! `ActorCmd::DeleteTask`（`actor.rs`）随后在**同一次命令处理内**同步调用
//! `load_and_send_all_tasks()` 重新广播 `TasksSnapshot`。也就是说任务消失到
//! `TasksSnapshot` 剪枝判定 Stop（[`reconcile_snapshot_states`]）之间没有
//! 额外延迟——两者发生在同一个 HTTP/命令请求周期内，因此不需要在
//! `actor.rs` 里再为 `DeleteTask` 单独广播一次 Stop。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use fluxdown_api::service::{LiveSpeed, TaskEvent, TaskEventKind, task_event_for_transition};
use fluxdown_engine::events::{EngineEvent, EventSink};
use fluxdown_engine::log_info;
use fluxdown_engine::model::{BtFileEntry, HlsQualityOption, ResolveVariantOption, TaskInfo};
use fluxdown_engine::selection::{HostSelection, SelectionOutcome};
use tokio::sync::{broadcast, oneshot};

use crate::wire::WsServerMsg;

/// 无客户端应答时 BT 文件选择的兜底超时（与桌面端常量一致）。
const BT_SELECTION_TIMEOUT: Duration = Duration::from_secs(60);

/// WS 广播中枢：事件出站通道 + HLS/BT 选择等待表 + 实时速率缓存 + 任务
/// 生命周期事件出站通道 + 前态表。
pub struct WsHub {
    /// 序列化后的 [`WsServerMsg`] JSON 广播通道；每个 WS 连接 subscribe 一份。
    pub events: broadcast::Sender<String>,
    pending_hls: Mutex<HashMap<String, oneshot::Sender<i32>>>,
    pending_bt: Mutex<HashMap<String, oneshot::Sender<Vec<i32>>>>,
    /// 插件 resolve 变体选择等待表（task_id → 应答通道）。
    pending_variant: Mutex<HashMap<String, oneshot::Sender<i32>>>,
    /// 任务实时速率缓存（task_id → 速率）。[`EngineEventSink`] 消费
    /// `TaskProgress`/`TasksSnapshot` 写入与清理，供 `ServerApiHost::live_speeds`
    /// （aria2 兼容层）经 `live_speeds_snapshot` 读取。
    live_speeds: Mutex<HashMap<String, LiveSpeed>>,
    /// 任务生命周期事件广播通道；aria2 `/jsonrpc` WS 通知源，经
    /// [`WsHub::subscribe_task_events`] 由 `ServerApiHost::subscribe_task_events`
    /// 转发订阅。
    task_events: broadcast::Sender<TaskEvent>,
    /// 任务前态表（task_id → 最近一次已知 status）。[`EngineEventSink`]
    /// 用它经 [`task_event_for_transition`] 判定生命周期事件、经
    /// [`reconcile_snapshot_states`] 判定快照剪枝 Stop。
    task_states: Mutex<HashMap<String, i32>>,
}

impl WsHub {
    pub fn new(capacity: usize) -> Self {
        let (events, _) = broadcast::channel(capacity);
        let (task_events, _) = broadcast::channel(capacity);
        Self {
            events,
            pending_hls: Mutex::new(HashMap::new()),
            pending_bt: Mutex::new(HashMap::new()),
            pending_variant: Mutex::new(HashMap::new()),
            live_speeds: Mutex::new(HashMap::new()),
            task_events,
            task_states: Mutex::new(HashMap::new()),
        }
    }

    /// 序列化并广播一条服务端消息。无订阅者时静默丢弃（正常情形）。
    pub fn broadcast(&self, msg: &WsServerMsg) {
        match serde_json::to_string(msg) {
            Ok(json) => {
                let _ = self.events.send(json);
            }
            Err(e) => log_info!("[ws-hub] serialize failed: {}", e),
        }
    }

    /// 全部任务的实时速率快照（单次 clone；供 `ServerApiHost::live_speeds`
    /// 读取，aria2 `tellStatus`/`tellActive` 的 downloadSpeed 字段来源）。
    pub fn live_speeds_snapshot(&self) -> HashMap<String, LiveSpeed> {
        lock_or_recover(&self.live_speeds).clone()
    }

    /// 订阅任务生命周期事件（aria2 `/jsonrpc` WS 通知源）。见
    /// [`fluxdown_api::service::ApiHost::subscribe_task_events`]。
    pub fn subscribe_task_events(&self) -> broadcast::Receiver<TaskEvent> {
        self.task_events.subscribe()
    }

    /// 广播一条任务生命周期事件。无订阅者时静默丢弃（正常情形——尚无
    /// `/jsonrpc` WS 客户端连接）。
    fn broadcast_task_event(&self, task_id: String, kind: TaskEventKind) {
        let _ = self.task_events.send(TaskEvent { task_id, kind });
    }
}

/// 取出锁内容，`Mutex` 中毒时回退到内部值（防御性处理，避免 panic）。
fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 任务是否处于终态（`completed`/`error`）。终态任务从快照消失时不触发
/// `Stop`——镜像 aria2 语义：`removeDownloadResult` 清理已完成/出错的历史
/// 记录不发通知，只有 `remove()` 主动结束一个仍活跃/等待中的任务才发
/// `onDownloadStop`。
fn is_terminal_status(status: i32) -> bool {
    matches!(status, 3 | 4)
}

/// engine 用 `status=4` 且 `error_message="deleted"` 的合成 `TaskProgress`
/// 触发各 `EventSink` 清理自身内部状态表（`DownloadManager::delete_task`/
/// `delete_tasks_batch`，见 `download_manager.rs` 中对 `progress_tx` 的
/// 直接 `send`），并非真实的下载错误。任务生命周期事件判定必须跳过
/// 它——否则每次删除任务都会误发一次 `aria2.onDownloadError`；前态表也
/// 不应被它覆盖，需保留删除前的真实状态，让随后同步广播的
/// `TasksSnapshot` 经 [`reconcile_snapshot_states`] 剪枝正确判定 Stop
/// （见模块顶部“删除路径的 Stop 时序”）。
fn is_delete_sentinel(status: i32, error_message: &str) -> bool {
    status == 4 && error_message == "deleted"
}

/// `TasksSnapshot` 到达时剪枝前态表：
/// - 前态表里存在、但不在新快照里的 task_id（任务已删除）：前态非终态
///   （见 [`is_terminal_status`]）则收进返回值用于广播 `Stop`；前态终态
///   则静默移除、不发，镜像 aria2 `removeDownloadResult` 不通知的语义。
/// - 新快照里前态表尚不存在的 task_id（首次观测）：只登记当前状态，不
///   产出任何事件——不仅限于终态，覆盖全部状态码。原因：进程启动时
///   `load_and_send_all_tasks` 会先把残留的 downloading/pending 任务批量
///   矫正为 paused 再广播首个快照，若不加区分地对这些“首次观测”状态套用
///   [`task_event_for_transition`] 的迁移规则，会在启动时对一整批历史
///   任务补发 `Pause`，造成与终态同样的通知风暴。
/// - 已经登记过的 task_id 保留原前态：状态迁移的权威来源是
///   `TaskProgress`（见 `EngineEventSink::emit`），快照只做“对账”，不
///   覆盖，防止过期/竞态快照把已知的最新前态往回冲。
///
/// 纯函数（不接触锁/广播），直接对调用方持锁的 map 做原地更新，返回需要
/// 广播 `Stop` 的 task_id 列表，便于单测独立覆盖两个剪枝分支。
fn reconcile_snapshot_states(states: &mut HashMap<String, i32>, tasks: &[TaskInfo]) -> Vec<String> {
    let live_ids: HashSet<&str> = tasks.iter().map(|t| t.task_id.as_str()).collect();
    let mut stopped = Vec::new();
    states.retain(|task_id, status| {
        if live_ids.contains(task_id.as_str()) {
            true
        } else {
            if !is_terminal_status(*status) {
                stopped.push(task_id.clone());
            }
            false
        }
    });
    for t in tasks {
        states.entry(t.task_id.clone()).or_insert(t.status);
    }
    stopped
}

/// `EngineEvent` → WS 广播的 [`EventSink`] 实现。
pub struct EngineEventSink(pub std::sync::Arc<WsHub>);

impl EventSink for EngineEventSink {
    fn emit(&self, event: EngineEvent) {
        let msg = match event {
            EngineEvent::TaskProgress {
                task_id,
                status,
                downloaded_bytes,
                total_bytes,
                speed,
                file_name,
                save_dir,
                url,
                error_message,
                upload_speed_bps,
                ..
            } => {
                // 实时速率缓存：仅 downloading(1)/preparing(5) 保留非零值，
                // 到达终态（paused/completed/error）立即清除，避免 aria2
                // tellStatus 的 downloadSpeed 字段返回陈旧速率。
                let mut speeds = lock_or_recover(&self.0.live_speeds);
                if matches!(status, 1 | 5) {
                    speeds.insert(
                        task_id.clone(),
                        LiveSpeed {
                            download_bps: speed,
                            upload_bps: upload_speed_bps,
                        },
                    );
                } else {
                    speeds.remove(&task_id);
                }
                drop(speeds);
                // 任务生命周期事件：跳过 delete 合成信号（引擎内部清理标记，
                // 见 `is_delete_sentinel` 文档），避免误发 `aria2.onDownloadError`；
                // 其余按前态表 + 纯函数判定，命中则广播 `TaskEvent`。
                if !is_delete_sentinel(status, &error_message) {
                    let prev = lock_or_recover(&self.0.task_states).insert(task_id.clone(), status);
                    if let Some(kind) = task_event_for_transition(prev, status) {
                        self.0.broadcast_task_event(task_id.clone(), kind);
                    }
                }
                WsServerMsg::TaskProgress {
                    task_id,
                    status,
                    downloaded_bytes,
                    total_bytes,
                    speed,
                    file_name,
                    save_dir,
                    url,
                    error_message,
                }
            }
            EngineEvent::TasksSnapshot(tasks) => {
                // 快照是权威任务列表：删除任务没有专属事件（只广播快照），
                // 借此机会清理其中已不存在的 task_id，防止速率缓存无界增长。
                let live_ids: HashSet<&str> = tasks.iter().map(|t| t.task_id.as_str()).collect();
                lock_or_recover(&self.0.live_speeds).retain(|k, _| live_ids.contains(k.as_str()));
                // 前态表剪枝 + Stop 判定 + 新任务静默登记（见
                // `reconcile_snapshot_states` 文档：消失且前态非终态 → 广播
                // Stop；首次观测一律只登记、不产出事件，避免历史任务在
                // 启动时重放成通知风暴）。
                let stopped = {
                    let mut states = lock_or_recover(&self.0.task_states);
                    reconcile_snapshot_states(&mut states, &tasks)
                };
                for task_id in stopped {
                    self.0.broadcast_task_event(task_id, TaskEventKind::Stop);
                }
                WsServerMsg::TasksSnapshot {
                    tasks: tasks.into_iter().map(Into::into).collect(),
                }
            }
            EngineEvent::SegmentProgress {
                task_id,
                total_bytes,
                segment_count,
                segments,
            } => WsServerMsg::SegmentProgress {
                task_id,
                total_bytes,
                segment_count,
                segments: segments.into_iter().map(Into::into).collect(),
            },
            EngineEvent::TaskMetaProbed {
                task_id,
                file_name,
                total_bytes,
            } => WsServerMsg::TaskMetaProbed {
                task_id,
                file_name,
                total_bytes,
            },
            EngineEvent::QueuePositionsChanged(positions) => WsServerMsg::QueuePositionsChanged {
                positions: positions.into_iter().map(Into::into).collect(),
            },
            EngineEvent::QueuesChanged(queues) => WsServerMsg::QueuesChanged {
                queues: queues.into_iter().map(Into::into).collect(),
            },
            EngineEvent::TaskQueueChanged { task_id, queue_id } => {
                WsServerMsg::TaskQueueChanged { task_id, queue_id }
            }
            EngineEvent::PriorityTaskChanged {
                priority_task_id,
                auto_paused_count,
            } => WsServerMsg::PriorityTaskChanged {
                priority_task_id,
                auto_paused_count,
            },
            EngineEvent::SegmentSplit {
                task_id,
                parent_index,
                parent_new_end,
                child_index,
                child_start,
                child_end,
                is_proactive,
                total_segments,
            } => WsServerMsg::SegmentSplit {
                task_id,
                parent_index,
                parent_new_end,
                child_index,
                child_start,
                child_end,
                is_proactive,
                total_segments,
            },
            // BT 数据下载完成(引擎每任务至多发一次):无对应 WS 快照消息,
            // 仅广播 aria2 `onBtDownloadComplete` 通知源事件。
            EngineEvent::BtDataFinished { task_id } => {
                self.0
                    .broadcast_task_event(task_id, TaskEventKind::BtComplete);
                return;
            }
            // 插件因熔断被自动禁用（reason 固定 "CircuitBreaker"）。
            EngineEvent::PluginAutoDisabled { identity, reason } => {
                WsServerMsg::PluginAutoDisabled { identity, reason }
            }
            // 插件 onDone 钩子活动状态（running=true/false），驱动“插件处理
            // 中…”指示器；可能并发/丢失，客户端自带看门狗兜底。
            EngineEvent::PluginHookActivity {
                task_id,
                plugin_id,
                running,
            } => WsServerMsg::PluginHookActivity {
                task_id,
                plugin_id,
                running,
            },
            // `#[non_exhaustive]`：未来新增变体默认丢弃并记录日志。
            other => {
                log_info!("[ws-hub] unhandled engine event: {:?}", other);
                return;
            }
        };
        self.0.broadcast(&msg);
    }
}

/// HLS/BT 选择的 WS 实现：广播选择请求，等待任一客户端经
/// `provide_*` 投递答案；超时按引擎语义兜底（HLS 选最高带宽，BT 全下）。
pub struct WsHostSelection(pub std::sync::Arc<WsHub>);

#[async_trait::async_trait]
impl HostSelection for WsHostSelection {
    async fn select_hls_quality(
        &self,
        task_id: &str,
        options: &[HlsQualityOption],
        timeout: Duration,
    ) -> SelectionOutcome<i32> {
        let best_default = options
            .iter()
            .enumerate()
            .max_by_key(|(_, o)| o.bandwidth)
            .map(|(i, _)| i as i32)
            .unwrap_or(0);

        let (tx, rx) = oneshot::channel();
        lock_or_recover(&self.0.pending_hls).insert(task_id.to_string(), tx);

        self.0.broadcast(&WsServerMsg::HlsSelectionRequest {
            task_id: task_id.to_string(),
            options: options.iter().cloned().map(Into::into).collect(),
        });

        let outcome = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(idx)) => SelectionOutcome::UserChose(idx),
            Ok(Err(_)) | Err(_) => {
                log_info!(
                    "[ws-selection] task {} HLS selection timed out/closed, defaulting",
                    task_id
                );
                SelectionOutcome::TimedOutDefaulted(best_default)
            }
        };
        // 必须移除等待表条目：防 map 无界增长 / 向已丢弃 Receiver 投递。
        lock_or_recover(&self.0.pending_hls).remove(task_id);
        outcome
    }

    async fn select_bt_files(
        &self,
        task_id: &str,
        files: &[BtFileEntry],
        timeout: Option<Duration>,
    ) -> SelectionOutcome<Vec<i32>> {
        let (tx, rx) = oneshot::channel();
        lock_or_recover(&self.0.pending_bt).insert(task_id.to_string(), tx);

        self.0.broadcast(&WsServerMsg::BtSelectionRequest {
            task_id: task_id.to_string(),
            files: files.iter().cloned().map(Into::into).collect(),
        });

        let effective_timeout = timeout.unwrap_or(BT_SELECTION_TIMEOUT);
        let outcome = match tokio::time::timeout(effective_timeout, rx).await {
            Ok(Ok(indices)) => SelectionOutcome::UserChose(indices),
            Ok(Err(_)) | Err(_) => {
                log_info!(
                    "[ws-selection] task {} BT selection timed out/closed, defaulting to all files",
                    task_id
                );
                // 空 = 下载全部文件（与桌面语义一致）。
                SelectionOutcome::TimedOutDefaulted(Vec::new())
            }
        };
        lock_or_recover(&self.0.pending_bt).remove(task_id);
        outcome
    }

    async fn select_resolve_variant(
        &self,
        task_id: &str,
        options: &[ResolveVariantOption],
        default_index: i32,
        timeout: Duration,
    ) -> SelectionOutcome<i32> {
        let (tx, rx) = oneshot::channel();
        lock_or_recover(&self.0.pending_variant).insert(task_id.to_string(), tx);

        self.0.broadcast(&WsServerMsg::ResolveVariantRequest {
            task_id: task_id.to_string(),
            default_index,
            options: options.iter().cloned().map(Into::into).collect(),
        });

        let outcome = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(idx)) => SelectionOutcome::UserChose(idx),
            Ok(Err(_)) | Err(_) => {
                log_info!(
                    "[ws-selection] task {} resolve variant selection timed out/closed, defaulting",
                    task_id
                );
                SelectionOutcome::TimedOutDefaulted(default_index)
            }
        };
        // 必须移除等待表条目：防 map 无界增长 / 向已丢弃 Receiver 投递。
        lock_or_recover(&self.0.pending_variant).remove(task_id);
        outcome
    }

    fn provide_hls_selection(&self, task_id: &str, selected_index: i32) {
        if let Some(tx) = lock_or_recover(&self.0.pending_hls).remove(task_id) {
            let _ = tx.send(selected_index);
        } else {
            log_info!(
                "[ws-selection] no pending HLS selection for task {}",
                task_id
            );
        }
    }

    fn provide_bt_selection(&self, task_id: &str, selected_indices: Vec<i32>) {
        if let Some(tx) = lock_or_recover(&self.0.pending_bt).remove(task_id) {
            let _ = tx.send(selected_indices);
        } else {
            log_info!(
                "[ws-selection] no pending BT selection for task {}",
                task_id
            );
        }
    }

    fn provide_variant_selection(&self, task_id: &str, selected_index: i32) {
        if let Some(tx) = lock_or_recover(&self.0.pending_variant).remove(task_id) {
            let _ = tx.send(selected_index);
        } else {
            log_info!(
                "[ws-selection] no pending resolve variant selection for task {}",
                task_id
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn engine_event_sink_maps_task_progress_to_camel_case_json() {
        let hub = Arc::new(WsHub::new(16));
        let mut rx = hub.events.subscribe();
        let sink = EngineEventSink(Arc::clone(&hub));

        sink.emit(EngineEvent::TaskProgress {
            task_id: "t1".into(),
            status: 1,
            downloaded_bytes: 50,
            total_bytes: 200,
            speed: 1024,
            file_name: "a.bin".into(),
            save_dir: "/tmp".into(),
            url: "http://x".into(),
            error_message: String::new(),
            upload_speed_bps: 0,
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
        });

        let json = rx.recv().await.expect("broadcast recv");
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["type"], "taskProgress");
        assert_eq!(v["taskId"], "t1");
        assert_eq!(v["downloadedBytes"], 50);
        assert_eq!(v["totalBytes"], 200);
        assert_eq!(v["speed"], 1024);
        assert_eq!(v["fileName"], "a.bin");
    }

    /// BT 上传速率透传(见 `RinfEventSink` 同名契约):
    /// `EngineEvent::TaskProgress::upload_speed_bps` 必须原样写入
    /// `live_speeds` 缓存的 `upload_bps`,供 `ServerApiHost::live_speeds`
    /// (aria2 `tellStatus`)读取。
    #[tokio::test]
    async fn engine_event_sink_forwards_upload_speed_bps_into_live_speeds() {
        let hub = Arc::new(WsHub::new(16));
        let sink = EngineEventSink(Arc::clone(&hub));

        sink.emit(EngineEvent::TaskProgress {
            task_id: "bt1".into(),
            status: 1,
            downloaded_bytes: 100,
            total_bytes: 1000,
            speed: 2048,
            file_name: "a.torrent".into(),
            save_dir: "/tmp".into(),
            url: "magnet:?xt=urn:btih:abc".into(),
            error_message: String::new(),
            upload_speed_bps: 777,
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
        });

        let speeds = hub.live_speeds_snapshot();
        assert_eq!(
            speeds.get("bt1"),
            Some(&LiveSpeed {
                download_bps: 2048,
                upload_bps: 777,
            })
        );
    }

    /// `EngineEvent::BtDataFinished` 经 `EngineEventSink::emit` 广播为
    /// `TaskEventKind::BtComplete`(aria2 `onBtDownloadComplete` 通知源),
    /// 且不产出任何 `WsServerMsg` 广播(无对应快照/进度消息)。
    #[tokio::test]
    async fn engine_event_sink_bt_data_finished_broadcasts_bt_complete_without_ws_message() {
        let hub = Arc::new(WsHub::new(16));
        let mut ws_rx = hub.events.subscribe();
        let mut task_rx = hub.subscribe_task_events();
        let sink = EngineEventSink(Arc::clone(&hub));

        sink.emit(EngineEvent::BtDataFinished {
            task_id: "bt1".into(),
        });

        let ev = task_rx.recv().await.expect("task event recv");
        assert_eq!(ev.task_id, "bt1");
        assert_eq!(ev.kind, TaskEventKind::BtComplete);

        assert!(
            ws_rx.try_recv().is_err(),
            "BtDataFinished must not broadcast a WsServerMsg"
        );
    }

    #[tokio::test]
    async fn engine_event_sink_maps_segment_split_to_camel_case_json() {
        let hub = Arc::new(WsHub::new(16));
        let mut rx = hub.events.subscribe();
        let sink = EngineEventSink(Arc::clone(&hub));

        sink.emit(EngineEvent::SegmentSplit {
            task_id: "t1".into(),
            parent_index: 0,
            parent_new_end: 400,
            child_index: 1,
            child_start: 400,
            child_end: 800,
            is_proactive: false,
            total_segments: 2,
        });

        let json = rx.recv().await.expect("broadcast recv");
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["type"], "segmentSplit");
        assert_eq!(v["parentIndex"], 0);
        assert_eq!(v["parentNewEnd"], 400);
        assert_eq!(v["childIndex"], 1);
        assert_eq!(v["childStart"], 400);
        assert_eq!(v["childEnd"], 800);
        assert_eq!(v["isProactive"], false);
        assert_eq!(v["totalSegments"], 2);
    }

    #[tokio::test]
    async fn engine_event_sink_maps_queues_changed_to_camel_case_json() {
        use fluxdown_engine::model::QueueInfo;

        let hub = Arc::new(WsHub::new(16));
        let mut rx = hub.events.subscribe();
        let sink = EngineEventSink(Arc::clone(&hub));

        sink.emit(EngineEvent::QueuesChanged(vec![QueueInfo {
            queue_id: "q1".into(),
            name: "work".into(),
            speed_limit_kbps: 256,
            max_concurrent: 2,
            default_save_dir: "/downloads/work".into(),
            position: 0,
            default_segments: 4,
            default_user_agent: String::new(),
            is_running: true,
            schedule_enabled: false,
            schedule_start: String::new(),
            schedule_stop: String::new(),
            schedule_days: 127,
        }]));

        let json = rx.recv().await.expect("broadcast recv");
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["type"], "queuesChanged");
        assert_eq!(v["queues"][0]["queueId"], "q1");
        assert_eq!(v["queues"][0]["speedLimitKbps"], 256);
        assert_eq!(v["queues"][0]["maxConcurrent"], 2);
    }

    #[tokio::test]
    async fn ws_host_selection_bt_files_answered_before_timeout_returns_user_chose() {
        let hub = Arc::new(WsHub::new(16));
        let selector = Arc::new(WsHostSelection(Arc::clone(&hub)));
        let responder = Arc::clone(&selector);

        let respond_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            responder.provide_bt_selection("task-a", vec![1, 2]);
        });

        let outcome = selector
            .select_bt_files("task-a", &[], Some(Duration::from_millis(500)))
            .await;

        respond_task.await.expect("responder task panicked");
        assert_eq!(outcome, SelectionOutcome::UserChose(vec![1, 2]));
    }

    #[tokio::test]
    async fn ws_host_selection_bt_files_times_out_with_no_answer_defaults_to_empty_vec() {
        let hub = Arc::new(WsHub::new(16));
        let selector = WsHostSelection(hub);

        let outcome = selector
            .select_bt_files("task-b", &[], Some(Duration::from_millis(50)))
            .await;

        assert_eq!(outcome, SelectionOutcome::TimedOutDefaulted(Vec::new()));
    }

    #[tokio::test]
    async fn ws_host_selection_hls_quality_times_out_defaults_to_highest_bandwidth_slot() {
        let hub = Arc::new(WsHub::new(16));
        let selector = WsHostSelection(hub);
        // Deliberately give the option at slice position 1 the highest
        // bandwidth while giving it an unrelated `index` field (9), to pin
        // down that the timeout default picks the *slice position* of the
        // best-bandwidth option, not its `index` field -- this mirrors
        // `RinfHostSelection::select_hls_quality`'s identical
        // `enumerate().max_by_key(...).map(|(i, _)| i as i32)` logic.
        let options = [
            HlsQualityOption {
                index: 7,
                bandwidth: 500_000,
                width: 640,
                height: 360,
            },
            HlsQualityOption {
                index: 9,
                bandwidth: 5_000_000,
                width: 1920,
                height: 1080,
            },
            HlsQualityOption {
                index: 3,
                bandwidth: 2_000_000,
                width: 1280,
                height: 720,
            },
        ];

        let outcome = selector
            .select_hls_quality("task-c", &options, Duration::from_millis(50))
            .await;

        assert_eq!(outcome, SelectionOutcome::TimedOutDefaulted(1));
    }

    #[tokio::test]
    async fn ws_host_selection_resolve_variant_answered_before_timeout_returns_user_chose() {
        let hub = Arc::new(WsHub::new(16));
        let selector = Arc::new(WsHostSelection(Arc::clone(&hub)));
        let responder = Arc::clone(&selector);

        let respond_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            responder.provide_variant_selection("task-d", 1);
        });

        let options = [ResolveVariantOption {
            index: 0,
            label: "1080p MP4".into(),
            container: "mp4".into(),
            bandwidth: 5_000_000,
            width: 1920,
            height: 1080,
            total_bytes: 123_456,
        }];

        let outcome = selector
            .select_resolve_variant("task-d", &options, 0, Duration::from_millis(500))
            .await;

        respond_task.await.expect("responder task panicked");
        assert_eq!(outcome, SelectionOutcome::UserChose(1));
    }

    #[tokio::test]
    async fn ws_host_selection_resolve_variant_times_out_defaults_to_default_index() {
        let hub = Arc::new(WsHub::new(16));
        let selector = WsHostSelection(hub);
        let options = [ResolveVariantOption {
            index: 0,
            label: "1080p MP4".into(),
            container: "mp4".into(),
            bandwidth: 5_000_000,
            width: 1920,
            height: 1080,
            total_bytes: 123_456,
        }];

        let outcome = selector
            .select_resolve_variant("task-e", &options, 0, Duration::from_millis(50))
            .await;

        assert_eq!(outcome, SelectionOutcome::TimedOutDefaulted(0));
    }

    #[tokio::test]
    async fn engine_event_sink_tracks_live_speed_while_active_and_clears_on_terminal_status() {
        let hub = Arc::new(WsHub::new(16));
        let sink = EngineEventSink(Arc::clone(&hub));

        sink.emit(EngineEvent::TaskProgress {
            task_id: "t1".into(),
            status: 1, // downloading
            downloaded_bytes: 50,
            total_bytes: 200,
            speed: 4096,
            file_name: "a.bin".into(),
            save_dir: "/tmp".into(),
            url: "http://x".into(),
            error_message: String::new(),
            upload_speed_bps: 0,
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
        });
        let snap = hub.live_speeds_snapshot();
        assert_eq!(snap.get("t1").map(|s| s.download_bps), Some(4096));

        sink.emit(EngineEvent::TaskProgress {
            task_id: "t1".into(),
            status: 3, // completed
            downloaded_bytes: 200,
            total_bytes: 200,
            speed: 0,
            file_name: "a.bin".into(),
            save_dir: "/tmp".into(),
            url: "http://x".into(),
            error_message: String::new(),
            upload_speed_bps: 0,
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
        });
        assert!(
            !hub.live_speeds_snapshot().contains_key("t1"),
            "terminal status must clear the live-speed entry"
        );
    }

    #[tokio::test]
    async fn engine_event_sink_prunes_live_speed_for_tasks_missing_from_snapshot() {
        let hub = Arc::new(WsHub::new(16));
        let sink = EngineEventSink(Arc::clone(&hub));

        for id in ["keep-me", "drop-me"] {
            sink.emit(EngineEvent::TaskProgress {
                task_id: id.to_string(),
                status: 1,
                downloaded_bytes: 0,
                total_bytes: 100,
                speed: 1000,
                file_name: "f".into(),
                save_dir: "/tmp".into(),
                url: "http://x".into(),
                error_message: String::new(),
                upload_speed_bps: 0,
                uploaded_bytes: 0,
                seeding_status: 0,
                seeding_message: String::new(),
            });
        }
        assert_eq!(hub.live_speeds_snapshot().len(), 2);

        // "drop-me" 已被删除：快照里只剩 "keep-me"，借此机会清理速率缓存
        // （删除任务没有专属事件，只广播 TasksSnapshot）。
        sink.emit(EngineEvent::TasksSnapshot(vec![TaskInfo {
            task_id: "keep-me".to_string(),
            url: "http://x".to_string(),
            file_name: "f".to_string(),
            save_dir: "/tmp".to_string(),
            status: 1,
            downloaded_bytes: 0,
            total_bytes: 100,
            error_message: String::new(),
            created_at: "0".to_string(),
            proxy_url: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            file_missing: false,
            completed_at: String::new(),
            segments: 0,
            queue_order: 0,
            uploaded_bytes: 0,
            uploaded_at_completion: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            referrer: String::new(),
        }]));

        let snap = hub.live_speeds_snapshot();
        assert!(snap.contains_key("keep-me"));
        assert!(!snap.contains_key("drop-me"));
    }

    fn mk_task(task_id: &str, status: i32) -> TaskInfo {
        TaskInfo {
            task_id: task_id.to_string(),
            url: "http://x".to_string(),
            file_name: "f".to_string(),
            save_dir: "/tmp".to_string(),
            status,
            downloaded_bytes: 0,
            total_bytes: 100,
            error_message: String::new(),
            created_at: "0".to_string(),
            proxy_url: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            file_missing: false,
            completed_at: String::new(),
            segments: 0,
            queue_order: 0,
            uploaded_bytes: 0,
            uploaded_at_completion: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            referrer: String::new(),
        }
    }

    // -- task_event_for_transition：覆盖统一规则的每条分支 ------------------

    #[test]
    fn task_event_for_transition_fires_start_from_none_pending_paused_or_preparing() {
        for prev in [None, Some(0), Some(2), Some(5)] {
            assert_eq!(
                task_event_for_transition(prev, 1),
                Some(TaskEventKind::Start),
                "prev={prev:?} -> downloading 必须触发 Start"
            );
        }
    }

    #[test]
    fn task_event_for_transition_does_not_fire_start_from_downloading_completed_or_error() {
        // prev==next(1) 是「无实际变化」的去重分支；prev=3/4 是明确排除在
        // Start 触发集合之外的两个前态（完成/出错后的自动重试不算「开始」）。
        for prev in [Some(1), Some(3), Some(4)] {
            assert_eq!(task_event_for_transition(prev, 1), None, "prev={prev:?}");
        }
    }

    #[test]
    fn task_event_for_transition_fires_pause_regardless_of_prev_except_noop() {
        // prev=None(首次观测即 paused)不再发 Pause——统一规则要求
        // `prev.is_some()`,与 Complete/Error 分支对齐,只登记不触发。
        assert_eq!(task_event_for_transition(None, 2), None);
        for prev in [Some(0), Some(1), Some(3), Some(4), Some(5)] {
            assert_eq!(
                task_event_for_transition(prev, 2),
                Some(TaskEventKind::Pause),
                "prev={prev:?}"
            );
        }
        // prev == next：状态未变化，不重复触发。
        assert_eq!(task_event_for_transition(Some(2), 2), None);
    }

    #[test]
    fn task_event_for_transition_fires_complete_only_when_previously_observed() {
        assert_eq!(
            task_event_for_transition(Some(1), 3),
            Some(TaskEventKind::Complete)
        );
        // 首次观测即终态（典型场景：进程重启后处理历史任务）→ 只登记不发。
        assert_eq!(task_event_for_transition(None, 3), None);
        // 状态未变化的重复上报不重复触发。
        assert_eq!(task_event_for_transition(Some(3), 3), None);
    }

    #[test]
    fn task_event_for_transition_fires_error_only_when_previously_observed() {
        assert_eq!(
            task_event_for_transition(Some(1), 4),
            Some(TaskEventKind::Error)
        );
        assert_eq!(task_event_for_transition(None, 4), None);
        assert_eq!(task_event_for_transition(Some(4), 4), None);
    }

    #[test]
    fn task_event_for_transition_never_fires_for_pending_or_preparing_targets() {
        for prev in [None, Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)] {
            assert_eq!(
                task_event_for_transition(prev, 0),
                None,
                "prev={prev:?} -> 0"
            );
            assert_eq!(
                task_event_for_transition(prev, 5),
                None,
                "prev={prev:?} -> 5"
            );
        }
    }

    // -- reconcile_snapshot_states：快照剪枝 Stop + 首次登记静默 ------------

    #[test]
    fn reconcile_snapshot_states_stops_missing_non_terminal_tasks() {
        let mut states = HashMap::from([
            ("downloading".to_string(), 1),
            ("paused".to_string(), 2),
            ("preparing".to_string(), 5),
        ]);
        // 三个任务全部从快照消失，前态均非终态。
        let stopped = reconcile_snapshot_states(&mut states, &[]);

        let mut sorted = stopped;
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                "downloading".to_string(),
                "paused".to_string(),
                "preparing".to_string(),
            ]
        );
        assert!(states.is_empty(), "消失的任务必须从前态表移除");
    }

    #[test]
    fn reconcile_snapshot_states_silently_drops_missing_terminal_tasks_without_stop() {
        let mut states = HashMap::from([("completed".to_string(), 3), ("errored".to_string(), 4)]);
        let stopped = reconcile_snapshot_states(&mut states, &[]);

        assert!(
            stopped.is_empty(),
            "终态任务消失不应广播 Stop（镜像 aria2 removeDownloadResult 不通知）"
        );
        assert!(states.is_empty(), "仍要从前态表移除，防止无界增长");
    }

    #[test]
    fn reconcile_snapshot_states_registers_first_seen_tasks_without_stop_candidates() {
        let mut states = HashMap::new();
        let tasks = [
            mk_task("t-pending", 0),
            mk_task("t-downloading", 1),
            mk_task("t-paused", 2),
            mk_task("t-completed", 3),
            mk_task("t-error", 4),
        ];
        let stopped = reconcile_snapshot_states(&mut states, &tasks);

        assert!(stopped.is_empty(), "首次观测不产出 Stop 候选");
        assert_eq!(states.get("t-pending"), Some(&0));
        assert_eq!(states.get("t-downloading"), Some(&1));
        assert_eq!(states.get("t-paused"), Some(&2));
        assert_eq!(states.get("t-completed"), Some(&3));
        assert_eq!(states.get("t-error"), Some(&4));
    }

    #[test]
    fn reconcile_snapshot_states_keeps_known_prev_state_instead_of_overwriting_from_snapshot() {
        // "t1" 已知前态是 1（来自某次 TaskProgress），快照却报告 status=2——
        // 快照不是权威迁移来源，不能覆盖已知前态。
        let mut states = HashMap::from([("t1".to_string(), 1)]);
        let tasks = [mk_task("t1", 2)];

        let stopped = reconcile_snapshot_states(&mut states, &tasks);

        assert!(stopped.is_empty());
        assert_eq!(states.get("t1"), Some(&1), "快照不得覆盖已登记的前态");
    }

    // -- EngineEventSink 集成：task_events 广播 ------------------------------

    #[tokio::test]
    async fn engine_event_sink_broadcasts_start_then_pause_then_start_again_on_status_flow() {
        let hub = Arc::new(WsHub::new(16));
        let mut rx = hub.subscribe_task_events();
        let sink = EngineEventSink(Arc::clone(&hub));

        fn progress(status: i32) -> EngineEvent {
            EngineEvent::TaskProgress {
                task_id: "t1".into(),
                status,
                downloaded_bytes: 0,
                total_bytes: 100,
                speed: 0,
                file_name: "f".into(),
                save_dir: "/tmp".into(),
                url: "http://x".into(),
                error_message: String::new(),
                upload_speed_bps: 0,
                uploaded_bytes: 0,
                seeding_status: 0,
                seeding_message: String::new(),
            }
        }

        sink.emit(progress(1)); // 首次观测 -> downloading：Start
        let ev = rx.recv().await.expect("start event");
        assert_eq!(ev.task_id, "t1");
        assert_eq!(ev.kind, TaskEventKind::Start);

        sink.emit(progress(1)); // 同状态重复上报：不应再次触发
        sink.emit(progress(2)); // downloading -> paused：Pause
        let ev = rx.recv().await.expect("pause event");
        assert_eq!(ev.kind, TaskEventKind::Pause);

        sink.emit(progress(1)); // unpause 恢复 -> downloading：再次 Start
        let ev = rx.recv().await.expect("restart event");
        assert_eq!(ev.kind, TaskEventKind::Start);

        // 上面按发送顺序 recv 三次都拿到了预期 kind：通道里不应再有积压，
        // 证明「同状态重复上报」那次调用确实没有多广播一条事件。
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn engine_event_sink_delete_sentinel_does_not_fire_error_but_enables_later_stop() {
        let hub = Arc::new(WsHub::new(16));
        let mut rx = hub.subscribe_task_events();
        let sink = EngineEventSink(Arc::clone(&hub));

        sink.emit(EngineEvent::TaskProgress {
            task_id: "t1".into(),
            status: 1, // downloading
            downloaded_bytes: 10,
            total_bytes: 100,
            speed: 100,
            file_name: "f".into(),
            save_dir: "/tmp".into(),
            url: "http://x".into(),
            error_message: String::new(),
            upload_speed_bps: 0,
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
        });
        assert_eq!(
            rx.recv().await.expect("start event").kind,
            TaskEventKind::Start
        );

        // DownloadManager::delete_task 内部合成的清理标记（见
        // `is_delete_sentinel`）：不得被判定为 Error。
        sink.emit(EngineEvent::TaskProgress {
            task_id: "t1".into(),
            status: 4,
            downloaded_bytes: 0,
            total_bytes: 0,
            speed: 0,
            file_name: String::new(),
            save_dir: String::new(),
            url: String::new(),
            error_message: "deleted".into(),
            upload_speed_bps: 0,
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
        });
        assert!(
            matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Empty)),
            "delete 合成信号不得广播 Error"
        );

        // actor.rs 在 DeleteTask 处理内同步重发的 TasksSnapshot：任务已从
        // 权威列表消失，且前态仍是 downloading(1)（未被合成信号污染）。
        sink.emit(EngineEvent::TasksSnapshot(vec![]));
        let ev = rx.recv().await.expect("stop event");
        assert_eq!(ev.task_id, "t1");
        assert_eq!(ev.kind, TaskEventKind::Stop);
    }

    #[tokio::test]
    async fn engine_event_sink_snapshot_pruning_suppresses_stop_for_terminal_prev_state() {
        let hub = Arc::new(WsHub::new(16));
        let mut rx = hub.subscribe_task_events();
        let sink = EngineEventSink(Arc::clone(&hub));

        // 先以 downloading(1) 观测一次：Complete 只在「前态已观测」时触发
        //（见 task_event_for_transition_fires_complete_only_when_previously_observed），
        // 首次观测即 completed 不发任何事件，直接 recv 会永久阻塞。
        sink.emit(EngineEvent::TaskProgress {
            task_id: "t1".into(),
            status: 1, // downloading
            downloaded_bytes: 50,
            total_bytes: 100,
            speed: 10,
            file_name: "f".into(),
            save_dir: "/tmp".into(),
            url: "http://x".into(),
            error_message: String::new(),
            upload_speed_bps: 0,
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
        });
        assert_eq!(
            rx.recv().await.expect("start event").kind,
            TaskEventKind::Start
        );

        sink.emit(EngineEvent::TaskProgress {
            task_id: "t1".into(),
            status: 3, // completed
            downloaded_bytes: 100,
            total_bytes: 100,
            speed: 0,
            file_name: "f".into(),
            save_dir: "/tmp".into(),
            url: "http://x".into(),
            error_message: String::new(),
            upload_speed_bps: 0,
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
        });
        assert_eq!(
            rx.recv().await.expect("complete event").kind,
            TaskEventKind::Complete
        );

        // 用户清除一个已完成的任务：镜像 aria2 removeDownloadResult，不发 Stop。
        sink.emit(EngineEvent::TasksSnapshot(vec![]));
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn engine_event_sink_first_snapshot_registers_historical_tasks_silently() {
        let hub = Arc::new(WsHub::new(16));
        let mut rx = hub.subscribe_task_events();
        let sink = EngineEventSink(Arc::clone(&hub));

        // 进程启动后处理的第一个快照：DB 里积压的历史任务，状态各异。
        sink.emit(EngineEvent::TasksSnapshot(vec![
            mk_task("old-paused", 2),
            mk_task("old-completed", 3),
            mk_task("old-error", 4),
        ]));
        assert!(
            matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Empty)),
            "首个快照绝不应重放历史任务的生命周期事件（防通知风暴）"
        );

        // 但确实登记了前态：随后 completed 任务消失时按「前态终态」静默处理。
        sink.emit(EngineEvent::TasksSnapshot(vec![mk_task("old-paused", 2)]));
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        // old-paused 也消失：前态是 2（非终态）→ 广播 Stop，证明它被正确登记。
        sink.emit(EngineEvent::TasksSnapshot(vec![]));
        let ev = rx.recv().await.expect("stop event for old-paused");
        assert_eq!(ev.task_id, "old-paused");
        assert_eq!(ev.kind, TaskEventKind::Stop);
    }
}
