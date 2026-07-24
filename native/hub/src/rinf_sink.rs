//! `EventSink` 实现 —— 把 `EngineEvent` 变体转发为具体 Dart 信号。
//!
//! 这是现状 hub 内 22 处 `.send_signal_to_dart()` 调用点的收敛,内容是搬移
//! 而非新写业务逻辑。
//!
//! 附带维护 aria2 兼容层所需的两块内存态,均由 `EngineEvent::TaskProgress`
//! 驱动:
//! - `ApiHost::live_speeds()` 的实时速率表:活跃状态(pending/downloading/
//!   preparing)写入当前速率,终态(paused/completed/error)移除条目。
//! - `ApiHost::subscribe_task_events()` 的任务生命周期事件广播:按
//!   「前态 → 新状态」的迁移判定(见 [`fluxdown_api::service::task_event_for_transition`])经
//!   `broadcast::Sender` 发出 `aria2.onDownloadXxx` 通知源事件,供
//!   `/jsonrpc` 的 WS 层转译转发。
//!
//! 任务删除也走 `TaskProgress` 这条路径的一部分——`DownloadManager::
//! delete_task`/`delete_tasks_batch` 会经 `progress_reporter` 补发一条
//! `status=4` 的终态 `TaskProgress`(用于清理其自身内部状态表),因此无需
//! 在各删除命令处理点单独清理 `live_speeds`。但 aria2 的 `Stop`(用户主动
//! 删除)与 `Error`(下载出错)语义不同,`status=4` 无法区分两者——
//! `download_actor` 在删除命令处理点直接调用
//! [`RinfEventSink::broadcast_task_stop`] 广播 `Stop` 并抹掉前态表条目,
//! 详见该方法文档。

use std::collections::HashMap;
use std::sync::Mutex;

use fluxdown_api::service::{LiveSpeed, TaskEvent, TaskEventKind, task_event_for_transition};
use fluxdown_engine::events::{EngineEvent, EventSink};
use rinf::RustSignal;
use tokio::sync::broadcast;

use crate::api_host::{LiveSpeedMap, lock_or_recover};
use crate::signals;

/// 桥接 `EngineEvent` 到 `hub::signals::*` 具体信号类型的 `EventSink` 实现。
pub struct RinfEventSink {
    /// `task_id → 实时速率`。与注入 `HubApiHost` 的是同一个 `Arc`
    /// (构造点见 `download_actor::run`),供 `ApiHost::live_speeds()` 读取。
    live_speeds: LiveSpeedMap,
    /// `task_id → 上一次已知状态码`,供
    /// [`fluxdown_api::service::task_event_for_transition`] 判定状态
    /// 迁移。不对外共享(`HubApiHost` 不需要历史状态),故不必像
    /// `live_speeds` 那样内含 `Arc`——多持有者场景由外层
    /// `Arc<RinfEventSink>` 本身覆盖(构造点见 `download_actor::run`;
    /// 删除命令处理点经 [`RinfEventSink::broadcast_task_stop`] 访问)。
    prev_status: Mutex<HashMap<String, i32>>,
    /// 任务生命周期事件广播源。`ApiHost::subscribe_task_events()` 经同一个
    /// `Sender`(构造点见 `download_actor::run`)开出新的 `Receiver`。
    task_events_tx: broadcast::Sender<TaskEvent>,
}

impl RinfEventSink {
    /// `live_speeds`:必须与传给 `HubApiHost::new` 的是同一个 `Arc`。
    /// `task_events_tx`:必须与传给 `HubApiHost::new` 的是同一个 `Sender`。
    pub fn new(live_speeds: LiveSpeedMap, task_events_tx: broadcast::Sender<TaskEvent>) -> Self {
        Self {
            live_speeds,
            prev_status: Mutex::new(HashMap::new()),
            task_events_tx,
        }
    }

    /// 任务删除时调用(`download_actor` 的 `DeleteTask`/批量删除/
    /// `ApiCommand::DeleteTask` 处理点):直接广播 `Stop` 并从前态表移除
    /// 条目,使随后引擎经 `progress_reporter` 补发的终态 `status=4`
    /// `TaskProgress` 因「前态缺失」被
    /// [`fluxdown_api::service::task_event_for_transition`] 判定为不发,
    /// 不会重复触发一次 `Error`。
    ///
    /// 调用时机要求:必须紧跟在对应的 `delete_task`/`delete_tasks_batch`
    /// `.await` 完成之后、且中间不能再插入其它 `.await` 点——
    /// `progress_reporter` 是独立 spawn 的任务,只有保证本方法先于那条
    /// 补发事件被 `emit()` 处理,上面的「前态缺失」判定才成立。
    pub fn broadcast_task_stop(&self, task_id: &str) {
        lock_or_recover(&self.prev_status).remove(task_id);
        // 无订阅者(尚无 WS 客户端连接 `/jsonrpc`)时 send 返回 Err,直接忽略。
        let _ = self.task_events_tx.send(TaskEvent {
            task_id: task_id.to_string(),
            kind: TaskEventKind::Stop,
        });
    }
}

/// 按任务进度状态更新/清除实时速率表条目:活跃状态(0=pending/1=downloading/
/// 5=preparing)写入当前速率;其余(2=paused/3=completed/4=error,含删除的
/// 终态补发)移除——这些状态之后引擎不会再为该任务发送 `TaskProgress`,残留
/// 的旧速率值若不清理会一直 stale。纯函数,便于单测覆盖每个状态码分支。
///
/// BT 任务的上传速率经 `EngineEvent::TaskProgress::upload_speed_bps` 透传
/// (librqbit 统计,其余协议恒 0),随下载速率一并写入 `upload_bps`。
fn apply_task_progress_speed(
    map: &mut HashMap<String, LiveSpeed>,
    task_id: &str,
    status: i32,
    speed: i64,
    upload_bps: i64,
) {
    match status {
        0 | 1 | 5 => {
            map.insert(
                task_id.to_string(),
                LiveSpeed {
                    download_bps: speed,
                    upload_bps,
                },
            );
        }
        _ => {
            map.remove(task_id);
        }
    }
}

impl EventSink for RinfEventSink {
    fn emit(&self, event: EngineEvent) {
        match event {
            EngineEvent::TaskProgress {
                task_id,
                status,
                downloaded_bytes,
                total_bytes,
                speed,
                upload_speed_bps,
                file_name,
                save_dir,
                url,
                error_message,
            } => {
                {
                    let mut speeds = lock_or_recover(&self.live_speeds);
                    apply_task_progress_speed(
                        &mut speeds,
                        &task_id,
                        status,
                        speed,
                        upload_speed_bps,
                    );
                }
                {
                    let event_kind = {
                        let mut prev_status = lock_or_recover(&self.prev_status);
                        let old = prev_status.insert(task_id.clone(), status);
                        task_event_for_transition(old, status)
                    };
                    if let Some(kind) = event_kind {
                        // 无订阅者(尚无 WS 客户端连接 `/jsonrpc`)时 send 返回
                        // Err,直接忽略。
                        let _ = self.task_events_tx.send(TaskEvent {
                            task_id: task_id.clone(),
                            kind,
                        });
                    }
                }
                signals::TaskProgress {
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
                .send_signal_to_dart();
            }
            EngineEvent::TasksSnapshot(tasks) => {
                signals::AllTasks {
                    tasks: tasks.into_iter().map(Into::into).collect(),
                }
                .send_signal_to_dart();
            }
            EngineEvent::SegmentProgress {
                task_id,
                total_bytes,
                segment_count,
                segments,
            } => {
                signals::SegmentProgress {
                    task_id,
                    total_bytes,
                    segment_count,
                    segments: segments.into_iter().map(Into::into).collect(),
                }
                .send_signal_to_dart();
            }
            EngineEvent::TaskMetaProbed {
                task_id,
                file_name,
                total_bytes,
            } => {
                signals::TaskMetaProbed {
                    task_id,
                    file_name,
                    total_bytes,
                }
                .send_signal_to_dart();
            }
            EngineEvent::QueuePositionsChanged(positions) => {
                signals::QueuePositionsUpdate {
                    positions: positions.into_iter().map(Into::into).collect(),
                }
                .send_signal_to_dart();
            }
            EngineEvent::QueuesChanged(queues) => {
                signals::AllQueues {
                    queues: queues.into_iter().map(Into::into).collect(),
                }
                .send_signal_to_dart();
            }
            EngineEvent::TaskQueueChanged { task_id, queue_id } => {
                signals::TaskQueueChanged { task_id, queue_id }.send_signal_to_dart();
            }
            EngineEvent::PriorityTaskChanged {
                priority_task_id,
                auto_paused_count,
            } => {
                signals::PriorityTaskChanged {
                    priority_task_id,
                    auto_paused_count,
                }
                .send_signal_to_dart();
            }
            EngineEvent::SegmentSplit {
                task_id,
                parent_index,
                parent_new_end,
                child_index,
                child_start,
                child_end,
                is_proactive,
                total_segments,
            } => {
                signals::SegmentSplitEvent {
                    task_id,
                    parent_index,
                    parent_new_end,
                    child_index,
                    child_start,
                    child_end,
                    is_proactive,
                    total_segments,
                }
                .send_signal_to_dart();
            }
            EngineEvent::FileMissingChanged(updates) => {
                signals::FileMissingChanged {
                    updates: updates
                        .into_iter()
                        .map(|(task_id, missing)| signals::FileMissingUpdate { task_id, missing })
                        .collect(),
                }
                .send_signal_to_dart();
            }
            // BT 数据下载完成(引擎每任务至多发一次,见
            // `EngineEvent::BtDataFinished` 文档):无对应 Dart 信号,仅广播
            // aria2 `onBtDownloadComplete` 通知源事件。
            EngineEvent::BtDataFinished { task_id } => {
                let _ = self.task_events_tx.send(TaskEvent {
                    task_id,
                    kind: TaskEventKind::BtComplete,
                });
            }
            EngineEvent::PluginAutoDisabled { identity, reason } => {
                signals::PluginAutoDisabledNotice { identity, reason }.send_signal_to_dart();
            }
            EngineEvent::PluginHookActivity {
                task_id,
                plugin_id,
                running,
            } => {
                signals::PluginHookActivityEvent {
                    task_id,
                    plugin_id,
                    running,
                }
                .send_signal_to_dart();
            }
            EngineEvent::GroupsChanged(groups) => {
                signals::AllGroups {
                    groups: groups.into_iter().map(Into::into).collect(),
                }
                .send_signal_to_dart();
            }
            EngineEvent::ResolvePreviewReady {
                preview_id,
                name,
                source_url,
                items,
                error,
            } => {
                signals::ResolvePreviewResult {
                    preview_id,
                    name,
                    source_url,
                    error,
                    items: items.into_iter().map(Into::into).collect(),
                }
                .send_signal_to_dart();
            }
            EngineEvent::TaskCdnEvent {
                task_id,
                kind,
                host,
                nodes,
                ip,
                reason,
                candidates,
                alive,
                cap,
                auto_cap,
            } => {
                signals::TaskCdnEvent {
                    task_id,
                    kind,
                    host,
                    nodes: nodes.into_iter().map(Into::into).collect(),
                    ip,
                    reason,
                    candidates,
                    alive,
                    cap,
                    auto_cap,
                }
                .send_signal_to_dart();
            }
            // `#[non_exhaustive]`：未来新增变体默认丢弃并记录日志，而非编译失败。
            _ => {
                crate::logger::log_info!(
                    "[rinf-sink] unhandled EngineEvent variant (added after this match was written)"
                );
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use fluxdown_api::service::{LiveSpeed, TaskEventKind};
    use fluxdown_engine::events::{EngineEvent, EventSink};
    use tokio::sync::broadcast;

    use super::{RinfEventSink, apply_task_progress_speed, task_event_for_transition};

    #[test]
    fn downloading_status_upserts_speed() {
        let mut map = HashMap::new();
        apply_task_progress_speed(&mut map, "t1", 1, 1024, 0);
        assert_eq!(
            map.get("t1"),
            Some(&LiveSpeed {
                download_bps: 1024,
                upload_bps: 0,
            })
        );
    }

    /// BT 任务上传速率透传:`apply_task_progress_speed` 把 `upload_bps`
    /// 与下载速率一并写入,活跃状态(status=1)必须原样透传,不能被下载速率
    /// 覆盖或归零。
    #[test]
    fn downloading_status_upserts_upload_speed_too() {
        let mut map = HashMap::new();
        apply_task_progress_speed(&mut map, "t1", 1, 2048, 512);
        assert_eq!(
            map.get("t1"),
            Some(&LiveSpeed {
                download_bps: 2048,
                upload_bps: 512,
            })
        );
    }

    #[test]
    fn pending_and_preparing_also_upsert() {
        let mut map = HashMap::new();
        apply_task_progress_speed(&mut map, "t1", 0, 10, 0);
        assert!(map.contains_key("t1"));
        apply_task_progress_speed(&mut map, "t2", 5, 20, 0);
        assert!(map.contains_key("t2"));
    }

    #[test]
    fn completed_status_removes_entry() {
        let mut map = HashMap::new();
        apply_task_progress_speed(&mut map, "t1", 1, 1024, 0);
        apply_task_progress_speed(&mut map, "t1", 3, 0, 0);
        assert!(!map.contains_key("t1"));
    }

    #[test]
    fn paused_status_removes_entry() {
        let mut map = HashMap::new();
        apply_task_progress_speed(&mut map, "t1", 1, 1024, 0);
        apply_task_progress_speed(&mut map, "t1", 2, 0, 0);
        assert!(!map.contains_key("t1"));
    }

    #[test]
    fn error_status_removes_entry_covers_delete_path() {
        // status=4 也是 delete_task/delete_tasks_batch 补发的终态标记,
        // 覆盖“删除后不留残余速率”这条路径。
        let mut map = HashMap::new();
        apply_task_progress_speed(&mut map, "t1", 1, 1024, 0);
        apply_task_progress_speed(&mut map, "t1", 4, 0, 0);
        assert!(!map.contains_key("t1"));
    }

    #[test]
    fn removing_absent_entry_is_noop() {
        let mut map: HashMap<String, LiveSpeed> = HashMap::new();
        apply_task_progress_speed(&mut map, "ghost", 3, 0, 0);
        assert!(map.is_empty());
    }

    #[test]
    fn first_sight_terminal_status_does_not_fire() {
        // 首见即终态(paused/completed/error)不发——避免启动时历史任务
        // 快照被误判为新事件,也覆盖删除广播 Stop 后前态表已移除、补发的
        // status=4 不再重复触发 Error 这条路径。
        assert_eq!(task_event_for_transition(None, 2), None);
        assert_eq!(task_event_for_transition(None, 3), None);
        assert_eq!(task_event_for_transition(None, 4), None);
    }

    #[test]
    fn first_sight_pending_or_preparing_does_not_fire() {
        // aria2 没有对应 pending/preparing 的 WS 通知语义,不发。
        assert_eq!(task_event_for_transition(None, 0), None);
        assert_eq!(task_event_for_transition(None, 5), None);
    }

    #[test]
    fn first_sight_downloading_fires_start() {
        // prev="无"也在 Start 触发集合内:本进程内第一次见到该任务就
        // 已在下载,等价于 aria2 对这个 GID 触发一次 onDownloadStart。
        assert_eq!(
            task_event_for_transition(None, 1),
            Some(TaskEventKind::Start)
        );
    }

    #[test]
    fn pending_to_downloading_fires_start() {
        assert_eq!(
            task_event_for_transition(Some(0), 1),
            Some(TaskEventKind::Start)
        );
    }

    #[test]
    fn paused_to_downloading_fires_start() {
        assert_eq!(
            task_event_for_transition(Some(2), 1),
            Some(TaskEventKind::Start)
        );
    }

    #[test]
    fn preparing_to_downloading_fires_start() {
        assert_eq!(
            task_event_for_transition(Some(5), 1),
            Some(TaskEventKind::Start)
        );
    }

    #[test]
    fn completed_or_error_to_downloading_does_not_fire_start() {
        // completed(3)/error(4) 是 aria2 模型里 GID 的终点,「重新下载」
        // 对应新 GID,不在 Start 触发集合 {无,0,2,5} 内,不发。
        assert_eq!(task_event_for_transition(Some(3), 1), None);
        assert_eq!(task_event_for_transition(Some(4), 1), None);
    }

    #[test]
    fn downloading_to_paused_fires_pause() {
        assert_eq!(
            task_event_for_transition(Some(1), 2),
            Some(TaskEventKind::Pause)
        );
    }

    #[test]
    fn downloading_to_completed_fires_complete() {
        assert_eq!(
            task_event_for_transition(Some(1), 3),
            Some(TaskEventKind::Complete)
        );
    }

    #[test]
    fn downloading_to_error_fires_error() {
        assert_eq!(
            task_event_for_transition(Some(1), 4),
            Some(TaskEventKind::Error)
        );
    }

    #[test]
    fn repeated_same_status_does_not_refire() {
        assert_eq!(task_event_for_transition(Some(1), 1), None);
        assert_eq!(task_event_for_transition(Some(2), 2), None);
        assert_eq!(task_event_for_transition(Some(3), 3), None);
        assert_eq!(task_event_for_transition(Some(4), 4), None);
    }

    /// `EngineEvent::BtDataFinished` 经 `RinfEventSink::emit` 广播为
    /// `TaskEventKind::BtComplete`(aria2 `onBtDownloadComplete` 通知源),
    /// 不经 `send_signal_to_dart`(无对应 Dart 信号),也不触碰 `live_speeds`。
    #[tokio::test]
    async fn bt_data_finished_emits_bt_complete_task_event() {
        let live_speeds: crate::api_host::LiveSpeedMap =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        let (task_events_tx, mut rx) = broadcast::channel(8);
        let sink = RinfEventSink::new(Arc::clone(&live_speeds), task_events_tx);

        sink.emit(EngineEvent::BtDataFinished {
            task_id: "bt1".to_string(),
        });

        let ev = rx.recv().await.expect("task event recv");
        assert_eq!(ev.task_id, "bt1");
        assert_eq!(ev.kind, TaskEventKind::BtComplete);
        assert!(
            live_speeds.lock().expect("mutex poisoned").is_empty(),
            "BtDataFinished must not write into the live_speeds cache"
        );
    }
}
