//! Boost（优先下载）：单个任务独占带宽，其余活跃 / 排队任务让位暂停，Boost 结束时按序恢复。
//!
//! 生命周期不变式：
//! - 只有未结束的任务（等待 / 准备中 / 下载中 / 暂停）可设为优先；已完成、失败的任务被拒绝
//!   （[`BoostError`]），绝不为它们暂停其他任务。
//! - 优先任务一旦不再在途（完成、失败、暂停、删除、插件解析失败或裂变、「文件已存在」
//!   取消……），Boost 立即结束并恢复让位任务：常规路径各自清理，引擎回流入口再经
//!   [`DownloadManager::reconcile_priority`] 统一兜底，不留「一直可取消」的残留状态。
//! - 状态随每次变化写入 config `boost_state`；daemon 重启后由
//!   [`DownloadManager::resume_on_start`] 按 `auto_resume_on_start` 恢复 Boost 或将让位任务
//!   视作被中断任务处理，避免它们永久停在暂停态。

use serde::{Deserialize, Serialize};

use super::{DownloadManager, QueuedTask};
use crate::db::DbError;
use crate::events::EngineEvent;
use crate::logger::log_info;

/// config 表 key：上次运行中的 Boost 状态（引擎私有，UI 不读写）。
const BOOST_STATE_KEY: &str = "boost_state";

/// 设为优先任务被拒绝的原因。
#[derive(Debug, thiserror::Error)]
pub enum BoostError {
    #[error("task not found")]
    NotFound,
    #[error("task is already completed")]
    Completed,
    #[error("task has ended with an error")]
    Ended,
    #[error("load task for boost: {0}")]
    Db(#[from] DbError),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedBoost {
    priority: String,
    #[serde(default)]
    auto_paused: Vec<String>,
}

impl DownloadManager {
    /// Set or toggle the priority (Boost) download task.
    ///
    /// - If `task_id` is empty, or equals the current priority task → cancel boost.
    /// - Completed / failed tasks are rejected without touching other tasks.
    /// - Otherwise: auto-pause all other active/queued tasks, ensure the target
    ///   task is downloading, and broadcast the new state to the host.
    pub async fn set_priority_task(&mut self, task_id: String) -> Result<(), BoostError> {
        if task_id.is_empty() || self.priority_task_id.as_deref() == Some(task_id.as_str()) {
            self.clear_priority().await;
            return Ok(());
        }
        match self.db.load_task_by_id(&task_id).await?.map(|t| t.status) {
            None => return Err(BoostError::NotFound),
            Some(3) => return Err(BoostError::Completed),
            Some(4) => return Err(BoostError::Ended),
            Some(_) => {}
        }

        // 切换 boost 目标时，保留上一轮 boost 自动暂停的任务 ID，
        // 使它们在新 boost 结束时也能一并被恢复，避免永久卡在暂停状态。
        // 将新目标从集合中移除（它将被启动，不需要在结束时当作"恢复对象"）。
        self.auto_paused_ids.retain(|id| id != &task_id);
        self.priority_task_id = None;

        // Step 1: If the target task is currently waiting in pending_queue, extract it
        // before we start pausing others.  This is critical: without this, two problems occur:
        //   a) resume_task() has an early-return guard for tasks already in pending_queue,
        //      so the target would never actually start.
        //   b) drain_queue() called inside each pause_task() call below could promote
        //      a different queued task to active, causing it to immediately get paused again.
        // By removing the target first we guarantee it won't be touched by drain_queue.
        let target_queued: Option<QueuedTask> = self
            .pending_queue
            .iter()
            .position(|q| q.task_id == task_id)
            .and_then(|pos| self.pending_queue.remove(pos));

        // Step 2: Auto-pause all currently active tasks (except the target itself,
        // which may already be downloading).
        // Note: each pause invokes drain_queue(), which could promote a
        // queued task to active.  We collect active IDs first, then pause them.
        // 重建连接 flush 窗口内的任务（`pending_resume_task_ids`）同样算活跃：不让位的话旧
        // writer 收尾后会自动恢复，与优先任务并行下载。
        let mut active_ids: Vec<String> = self
            .active_tasks
            .keys()
            .filter(|id| id.as_str() != task_id.as_str())
            .cloned()
            .collect();
        active_ids.extend(
            self.pending_resume_task_ids()
                .into_iter()
                .filter(|id| id.as_str() != task_id.as_str()),
        );
        // 先按恢复优先级登记暂停顺序：先前活跃的任务在前，排队任务按原 FIFO 在后。
        // （暂停活跃任务时 drain_queue 会提拔排队头部，必须在此之前固定顺序。）
        let queued_before: Vec<String> = self
            .pending_queue
            .iter()
            .filter(|t| t.task_id != task_id.as_str())
            .map(|t| t.task_id.clone())
            .collect();
        for id in active_ids.iter().chain(queued_before.iter()) {
            self.record_auto_paused(id);
        }
        for id in active_ids {
            self.pause_task_silent(&id).await;
        }

        // Step 3: Pause all remaining tasks in the pending queue (excluding the target).
        let queued_ids: Vec<String> = self
            .pending_queue
            .iter()
            .filter(|t| t.task_id != task_id.as_str())
            .map(|t| t.task_id.clone())
            .collect();
        for id in queued_ids {
            self.record_auto_paused(&id);
            self.pause_task_silent(&id).await;
        }

        // Step 4: Mop up — drain_queue() calls in step 2/3 may have promoted additional
        // tasks to active.  Pause anything that slipped through.
        let stray_active: Vec<String> = self
            .active_tasks
            .keys()
            .filter(|id| id.as_str() != task_id.as_str())
            .cloned()
            .collect();
        for id in stray_active {
            self.record_auto_paused(&id);
            self.pause_task_silent(&id).await;
        }

        self.priority_task_id = Some(task_id.clone());

        // Step 5: Ensure the target task is downloading.
        // For a previously-queued target: it was removed from pending_queue in step 1
        // so resume_task() will proceed normally (no early-return guard).
        // For an already-active target: nothing to do.
        if !self.active_tasks.contains_key(&task_id) {
            // Remove from auto_paused_ids so clear_priority won't try to resume
            // the task that's already running as priority.
            self.auto_paused_ids.retain(|id| id != &task_id);
            if let Some(queued) = target_queued {
                // Task was queued but never actually started (pending_queue slot) —
                // start it with its original request (method/body, webhook, plugin
                // hooks) since all other tasks were paused above.
                if queued.is_resume {
                    self.do_resume_task(&task_id).await;
                } else {
                    self.do_start_task(queued).await;
                }
            } else {
                // Task was paused — use the full resume path.
                self.resume_task(&task_id).await;
            }
        }

        // 验证目标任务是否真的启动成功（或处于重建连接 flush 窗口、旧 writer 收尾即恢复）。
        // 若 do_resume_task / resume_task 内部出错（DB 读取失败、BT 初始化失败等），
        // 任务不会出现在 active_tokens 中。此时必须取消 boost 并恢复已暂停的任务，
        // 否则客户端会显示 boost 激活但实际无任务下载，产生莫名其妙的结果。
        if !self.active_tasks.contains_key(&task_id) && !self.has_pending_resume(&task_id) {
            log_info!(
                "[manager] boost: target task {} failed to start — cancelling boost mode",
                task_id
            );
            self.clear_priority().await;
            return Ok(());
        }

        log_info!(
            "[manager] boost mode: priority={}, auto_paused={}",
            task_id,
            self.auto_paused_ids.len()
        );

        self.persist_boost().await;
        self.sink.emit(EngineEvent::PriorityTaskChanged {
            priority_task_id: task_id,
            auto_paused_count: self.auto_paused_ids.len() as i32,
        });
        Ok(())
    }

    /// 登记一个被 Boost 自动暂停的任务；已登记的保持原有顺序（去重）。
    pub(super) fn record_auto_paused(&mut self, task_id: &str) {
        if !self.auto_paused_ids.iter().any(|id| id == task_id) {
            self.auto_paused_ids.push(task_id.to_owned());
        }
    }

    /// Cancel boost mode and resume all auto-paused tasks.
    pub(super) async fn clear_priority(&mut self) {
        let was_active = self.priority_task_id.take().is_some();
        // 按暂停顺序恢复：先前活跃的任务优先拿回槽位，其后是原 FIFO 顺序的排队任务。
        let to_resume: Vec<String> = std::mem::take(&mut self.auto_paused_ids);
        if was_active {
            self.persist_boost().await;
        }
        log_info!(
            "[manager] boost cancelled, resuming {} tasks",
            to_resume.len()
        );
        for id in &to_resume {
            // 跳过已完成的任务，避免 clear_priority 误重启已完成下载。
            // 场景：boost 激活期间某任务恰好完成，clear_priority 时不应再 resume 它。
            let is_completed = match self.db.load_task_by_id(id).await {
                Ok(task) => task.is_some_and(|t| t.status == 3),
                Err(error) => {
                    crate::logger::report_error(
                        "download-manager",
                        "check boosted task completion",
                        &error,
                    );
                    continue;
                }
            };
            if is_completed {
                log_info!("[manager] clear_priority: skipping completed task {}", id);
                continue;
            }
            self.resume_task(id).await;
        }
        // 在发出 PriorityTaskChanged 之前广播最新队列位置。
        // resume_task 对于无空余槽的任务只是将其入队，不会主动广播。
        // 此次广播确保宿主在收到 PriorityTaskChanged 时已知道哪些任务在队列中
        // （queuePosition > 0），使 pauseAll 能正确识别并暂停它们。
        self.broadcast_queue_positions();
        self.sink.emit(EngineEvent::PriorityTaskChanged {
            priority_task_id: String::new(),
            auto_paused_count: 0,
        });
    }

    /// 兜底：优先任务已不在途（活跃 / 解析中 / 排队都不是）→ 结束 Boost 并恢复让位任务。
    ///
    /// 引擎回流入口（`on_task_done` / `on_resolve_ready`）处理完后调用，覆盖完成、失败、
    /// 插件解析失败或裂变、「文件已存在」取消等不经常规 Boost 守卫的离场路径。
    pub(super) async fn reconcile_priority(&mut self) {
        let Some(priority) = self.priority_task_id.as_deref() else {
            return;
        };
        // 内部重建连接（改线程数 / 换全局出口）暂停后挂起的恢复：旧 writer 收尾即重新起飞，
        // 仍算在途。
        let in_flight = self.active_tasks.contains_key(priority)
            || self.has_pending_resolve(priority)
            || self.pending_queue.iter().any(|q| q.task_id == priority)
            || self
                .pending_pauses
                .get(priority)
                .is_some_and(|pending| pending.resume_requested);
        if in_flight {
            return;
        }
        log_info!(
            "[manager] boost: priority task {} is no longer in flight — ending boost",
            priority
        );
        self.clear_priority().await;
    }

    /// 批量暂停摘掉 Boost 登记后同步持久化（不经 [`Self::clear_priority`]）。
    pub(super) async fn release_priority_for_batch(&mut self) -> Vec<String> {
        self.priority_task_id = None;
        let paused = std::mem::take(&mut self.auto_paused_ids);
        self.persist_boost().await;
        paused
    }

    /// 把当前 Boost 状态写入 config（无 Boost 时删除 key）。只在 Boost 变化时调用，
    /// 不引入周期写。
    async fn persist_boost(&self) {
        let result = match self.priority_task_id.as_deref() {
            None => self.db.delete_config(BOOST_STATE_KEY).await,
            Some(priority) => {
                let state = PersistedBoost {
                    priority: priority.to_owned(),
                    auto_paused: self.auto_paused_ids.clone(),
                };
                match serde_json::to_string(&state) {
                    Ok(json) => self.db.set_config(BOOST_STATE_KEY, &json).await,
                    Err(error) => {
                        crate::logger::report_error(
                            "download-manager",
                            "encode boost state",
                            &error,
                        );
                        return;
                    }
                }
            }
        };
        if let Err(error) = result {
            crate::logger::report_error("download-manager", "persist boost state", &error);
        }
    }

    /// 读取并删除上次运行遗留的 Boost 状态。
    async fn take_persisted_boost(&self) -> Option<PersistedBoost> {
        let raw = match self.db.get_config(BOOST_STATE_KEY).await {
            Ok(Some(raw)) => raw,
            Ok(None) => return None,
            Err(error) => {
                crate::logger::report_error("download-manager", "load boost state", &error);
                return None;
            }
        };
        if let Err(error) = self.db.delete_config(BOOST_STATE_KEY).await {
            crate::logger::report_error("download-manager", "clear boost state", &error);
        }
        match serde_json::from_str::<PersistedBoost>(&raw) {
            Ok(state) if !state.priority.is_empty() => Some(state),
            Ok(_) => None,
            Err(error) => {
                crate::logger::report_warning("download-manager", "decode boost state", &error);
                None
            }
        }
    }

    /// 启动恢复（进程启动首次加载时调用），在 `auto_resume_on_start` 之上处理上次的 Boost：
    /// - 开关开启且优先任务属于被中断的任务 → 恢复 Boost：其余被中断任务与原让位任务
    ///   一起登记为让位暂停，Boost 结束时按序恢复；
    /// - 否则 Boost 随重启结束，原让位任务与被中断任务同等对待（开关开则恢复，关则保持暂停）。
    pub(super) async fn resume_on_start(&mut self, reset_ids: Vec<String>) {
        let Some(state) = self.take_persisted_boost().await else {
            self.auto_resume_on_start(reset_ids).await;
            return;
        };
        let restore = reset_ids.contains(&state.priority) && self.auto_resume_enabled().await;
        if !restore {
            let mut ids = reset_ids;
            for id in state.auto_paused {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            self.auto_resume_on_start(ids).await;
            return;
        }
        let mut yielded = state.auto_paused;
        for id in reset_ids {
            if id != state.priority && !yielded.contains(&id) {
                yielded.push(id);
            }
        }
        for id in self.startup_resume_candidates(&yielded).await {
            self.record_auto_paused(&id);
        }
        log_info!(
            "[manager] restoring boost for {} after restart ({} yielded)",
            state.priority,
            self.auto_paused_ids.len()
        );
        if let Err(error) = self.set_priority_task(state.priority).await {
            log_info!("[manager] boost restore rejected: {:#}", error);
            self.clear_priority().await;
        }
    }
}
