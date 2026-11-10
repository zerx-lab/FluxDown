//! 连接所有权的交互选择订阅、终局与默认回退。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use fluxdown_protocol::{
    DaemonEvent, EventFrame, FileExistsAction, SelectionKind, SelectionOutcome,
    SelectionRequestDto, SelectionResolutionDto, ServiceEvent, TaskDto, WsServerMsg,
};
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::event_hub::DaemonEventHub;

const RESOLVED_HISTORY_LIMIT: usize = 1024;

/// 任务表里的「已暂停」状态。
const TASK_STATUS_PAUSED: i32 = 2;

#[derive(Clone, Copy, Eq, PartialEq)]
enum SelectionType {
    Hls,
    Bt,
    Variant,
    FileExists,
    Plugin,
}

struct PendingSelection {
    kind: SelectionType,
    task_id: String,
    default_choice: SelectionOutcome,
    /// 仅 FileExists：该请求允许的动作；其余类型为空。
    file_exists_actions: Vec<FileExistsAction>,
    /// 仅 Plugin：可选插件 identity；其余类型为空。
    plugin_candidates: Vec<String>,
    /// 请求是否带自动解析的截止时刻。无截止（用户关闭了 BT「自动选择默认项」）的请求只等用户：
    /// 最后一个订阅者离开时保持待处理，daemon 关闭时以取消终结，都不替用户采用默认值。
    has_deadline: bool,
    sender: oneshot::Sender<SelectionOutcome>,
}

#[derive(Default)]
struct SelectionState {
    subscribers: HashSet<String>,
    pending: HashMap<String, PendingSelection>,
    resolved: VecDeque<String>,
}

/// 新选择请求的等待方式。
pub enum SelectionWait {
    Immediate(SelectionOutcome),
    Pending(oneshot::Receiver<SelectionOutcome>),
}

/// 选择协议错误。
#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    #[error("selection request not found")]
    NotFound,
    #[error("selection request was already resolved")]
    Conflict,
    #[error("selection outcome does not match request kind")]
    InvalidOutcome,
    #[error("HLS selection cannot be cancelled")]
    HlsCancel,
}

/// 所有 WebSocket 连接共享的选择状态。
#[derive(Clone)]
pub struct DaemonSelection {
    state: Arc<Mutex<SelectionState>>,
    events: DaemonEventHub,
}

impl DaemonSelection {
    #[must_use]
    pub fn new(events: DaemonEventHub) -> Self {
        Self {
            state: Arc::new(Mutex::new(SelectionState::default())),
            events,
        }
    }

    /// 将一个已完成 hello 的连接登记为交互选择消费者。
    pub fn subscribe(&self, connection_id: String) {
        lock_or_recover(&self.state)
            .subscribers
            .insert(connection_id);
    }

    /// 移除连接；最后一个订阅者离开时，有截止的待选择立即采用各自默认值。
    ///
    /// 无截止的请求保持待处理：它仍在快照的 `pending_selections` 里，下一个连上的客户端
    /// （Web 刷新 / 重连、重新打开的桌面窗口）照常看到并作答。
    pub fn unsubscribe(&self, connection_id: &str) {
        let resolved_ids = {
            let mut state = lock_or_recover(&self.state);
            state.subscribers.remove(connection_id);
            if !state.subscribers.is_empty() {
                return;
            }
            let ids: Vec<String> = state
                .pending
                .iter()
                .filter(|(_, pending)| pending.has_deadline)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                let Some(selection) = state.pending.remove(id) else {
                    continue;
                };
                if selection.sender.send(selection.default_choice).is_err() {
                    tracing::debug!(request_id = %id, "selection waiter disconnected before default resolution");
                }
                remember_resolved(&mut state.resolved, id.clone());
            }
            ids
        };
        for request_id in resolved_ids {
            self.events
                .publish(DaemonEvent::SelectionResolved { request_id });
        }
    }

    /// 发布待选择请求；无订阅者时不发布并立即采用默认值。
    pub fn begin(&self, request: SelectionRequestDto) -> SelectionWait {
        let mut state = lock_or_recover(&self.state);
        if state.subscribers.is_empty() {
            return SelectionWait::Immediate(request.default_choice);
        }
        let (sender, receiver) = oneshot::channel();
        let kind = selection_type(&request.kind);
        let request_id = request.request_id.clone();
        state.pending.insert(
            request_id,
            PendingSelection {
                kind,
                task_id: request.task_id.clone(),
                default_choice: request.default_choice.clone(),
                file_exists_actions: match &request.kind {
                    SelectionKind::FileExists { actions, .. } => actions.clone(),
                    _ => Vec::new(),
                },
                plugin_candidates: match &request.kind {
                    SelectionKind::Plugin { candidates, .. } => candidates
                        .iter()
                        .map(|candidate| candidate.identity.clone())
                        .collect(),
                    _ => Vec::new(),
                },
                has_deadline: request.has_deadline(),
                sender,
            },
        );
        drop(state);
        self.events.publish(DaemonEvent::SelectionPending(request));
        SelectionWait::Pending(receiver)
    }

    /// 接受第一个合法终局；重复或迟到答复返回 conflict。
    pub fn resolve(&self, resolution: SelectionResolutionDto) -> Result<(), SelectionError> {
        let selection = {
            let mut state = lock_or_recover(&self.state);
            let Some(pending) = state.pending.get(&resolution.request_id) else {
                if state.resolved.contains(&resolution.request_id) {
                    return Err(SelectionError::Conflict);
                }
                return Err(SelectionError::NotFound);
            };
            validate_outcome(pending, &resolution.outcome)?;
            let Some(selection) = state.pending.remove(&resolution.request_id) else {
                return Err(SelectionError::Conflict);
            };
            remember_resolved(&mut state.resolved, resolution.request_id.clone());
            selection
        };
        // 超时或任务取消可以先撤销等待者；终局仍保持 first-wins 并广播。
        if selection.sender.send(resolution.outcome).is_err() {
            tracing::debug!(request_id = %resolution.request_id, "selection waiter disconnected before resolution");
        }
        self.events.publish(DaemonEvent::SelectionResolved {
            request_id: resolution.request_id,
        });
        Ok(())
    }

    /// 兼容引擎按 task ID 投递答案的 trait；仍由同一 first-wins 终局处理。
    fn resolve_for_task(
        &self,
        task_id: &str,
        kind: SelectionType,
        outcome: SelectionOutcome,
    ) -> Result<(), SelectionError> {
        let request_id = {
            let state = lock_or_recover(&self.state);
            state
                .pending
                .iter()
                .find(|(_, pending)| pending.task_id == task_id && pending.kind == kind)
                .map(|(request_id, _)| request_id.clone())
                .ok_or(SelectionError::NotFound)?
        };
        self.resolve(SelectionResolutionDto {
            request_id,
            outcome,
        })
    }

    /// 超时后移除 pending 并返回默认值；迟到答复随后得到 conflict。
    pub fn timeout(&self, request_id: &str) -> Option<SelectionOutcome> {
        let default_choice = {
            let mut state = lock_or_recover(&self.state);
            let selection = state.pending.remove(request_id)?;
            remember_resolved(&mut state.resolved, request_id.to_owned());
            selection.default_choice
        };
        self.events.publish(DaemonEvent::SelectionResolved {
            request_id: request_id.to_owned(),
        });
        Some(default_choice)
    }

    /// daemon 关闭路径：终结全部待处理选择。有截止的采用默认值；无截止的不替用户选择，
    /// BT / 变体等以「取消」终结（任务暂停，继续时重新询问），HLS 没有取消语义仍取默认。
    pub fn resolve_all_for_shutdown(&self) {
        let ids = {
            let mut state = lock_or_recover(&self.state);
            let pending = std::mem::take(&mut state.pending);
            let mut ids = Vec::with_capacity(pending.len());
            for (id, selection) in pending {
                let outcome = if selection.has_deadline || selection.kind == SelectionType::Hls {
                    selection.default_choice
                } else {
                    SelectionOutcome::Cancelled
                };
                if selection.sender.send(outcome).is_err() {
                    tracing::debug!(request_id = %id, "selection waiter disconnected during daemon shutdown");
                }
                remember_resolved(&mut state.resolved, id.clone());
                ids.push(id);
            }
            ids
        };
        for request_id in ids {
            self.events
                .publish(DaemonEvent::SelectionResolved { request_id });
        }
    }

    /// 撤销某任务所有待处理的选择（任务已暂停 / 删除，继续等待用户只会弹出无主的对话框）。
    ///
    /// Bt / Variant 以「取消」答复引擎；HLS 没有取消语义，采用默认值。随后广播
    /// `SelectionResolved`，迟到的客户端答复得到 conflict。返回撤销条数。
    pub fn cancel_task(&self, task_id: &str) -> usize {
        self.cancel_matching(|pending_task| pending_task == task_id)
    }

    /// 撤销所属任务在 `tasks` 中已不存在或处于暂停状态的待处理选择。
    fn cancel_for_tasks(&self, tasks: &[TaskDto]) {
        if lock_or_recover(&self.state).pending.is_empty() {
            return;
        }
        let statuses: HashMap<&str, i32> = tasks
            .iter()
            .map(|task| (task.task_id.as_str(), task.status))
            .collect();
        self.cancel_matching(|task_id| {
            statuses
                .get(task_id)
                .is_none_or(|status| *status == TASK_STATUS_PAUSED)
        });
    }

    /// 同 [`Self::cancel_for_tasks`]，任务状态取自 hub 当前投影：只查待处理选择所属的那几个
    /// 任务，不克隆任务表（整表事件在广播里已不带任务）。
    fn cancel_for_projection(&self) {
        let pending: Vec<String> = {
            let state = lock_or_recover(&self.state);
            state
                .pending
                .values()
                .map(|pending| pending.task_id.clone())
                .collect()
        };
        if pending.is_empty() {
            return;
        }
        let statuses: HashMap<String, i32> = self.events.inspect(|snapshot| {
            pending
                .iter()
                .filter_map(|id| snapshot.task(id).map(|task| (id.clone(), task.status)))
                .collect()
        });
        self.cancel_matching(|task_id| {
            statuses
                .get(task_id)
                .is_none_or(|status| *status == TASK_STATUS_PAUSED)
        });
    }

    fn cancel_matching(&self, mut matches: impl FnMut(&str) -> bool) -> usize {
        let cancelled = {
            let mut state = lock_or_recover(&self.state);
            let ids: Vec<String> = state
                .pending
                .iter()
                .filter(|(_, pending)| matches(&pending.task_id))
                .map(|(id, _)| id.clone())
                .collect();
            let mut cancelled = Vec::with_capacity(ids.len());
            for id in ids {
                let Some(selection) = state.pending.remove(&id) else {
                    continue;
                };
                let outcome = match selection.kind {
                    SelectionType::Hls => selection.default_choice,
                    SelectionType::Bt
                    | SelectionType::Variant
                    | SelectionType::FileExists
                    | SelectionType::Plugin => SelectionOutcome::Cancelled,
                };
                if selection.sender.send(outcome).is_err() {
                    tracing::debug!(request_id = %id, "selection waiter disconnected before task cancellation");
                }
                remember_resolved(&mut state.resolved, id.clone());
                cancelled.push(id);
            }
            cancelled
        };
        let count = cancelled.len();
        for request_id in cancelled {
            self.events
                .publish(DaemonEvent::SelectionResolved { request_id });
        }
        count
    }

    /// 事件帧里任务被暂停 / 删除时撤销该任务的待处理选择。
    fn observe_frame(&self, frame: &EventFrame) {
        let ServiceEvent::Daemon(event) = &frame.event else {
            return;
        };
        match event {
            DaemonEvent::TaskDeleted { task_id } => {
                self.cancel_task(task_id);
            }
            DaemonEvent::TasksDeleted { task_ids } => {
                for task_id in task_ids {
                    self.cancel_task(task_id);
                }
            }
            DaemonEvent::TaskChanged(task) if task.status == TASK_STATUS_PAUSED => {
                self.cancel_task(&task.task_id);
            }
            DaemonEvent::TasksChanged(tasks) => {
                for task in tasks
                    .iter()
                    .filter(|task| task.status == TASK_STATUS_PAUSED)
                {
                    self.cancel_task(&task.task_id);
                }
            }
            DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id,
                status,
                error_message,
                ..
            }) if *status == TASK_STATUS_PAUSED || (*status == 4 && error_message == "deleted") => {
                self.cancel_task(task_id);
            }
            DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks }) => {
                self.cancel_for_tasks(tasks);
            }
            // 广播里的整表替换只带头 / 重载宣告，任务表不在帧内：按 hub 当前投影复核。
            DaemonEvent::SnapshotReplaced(_)
            | DaemonEvent::TablesReloading { tasks: Some(_), .. } => {
                self.cancel_for_projection();
            }
            _ => {}
        }
    }

    /// 后台监听任务事件：任务暂停 / 删除即撤销其待处理选择。事件通道滞后时按当前
    /// 投影整体复核一次。`cancel` 触发后退出。
    pub fn spawn_task_watcher(&self, cancel: CancellationToken) {
        let selection = self.clone();
        let mut receiver = self.events.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    frame = receiver.recv() => match frame {
                        Ok(frame) => selection.observe_frame(&frame),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            selection.cancel_for_projection();
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                }
            }
        });
    }

    /// BT 文件选择的等待上限：调用方显式给的优先；否则按 `bt_selection_auto_default`
    /// 取 60 秒或不设上限（`None`）。
    fn bt_selection_timeout(
        &self,
        requested: Option<std::time::Duration>,
    ) -> Option<std::time::Duration> {
        if requested.is_some() {
            return requested;
        }
        // 写入经 `normalize_daemon_config_value` 规范化为 "true" / "false"；旧库或手改库的
        // "0" 同样视为关闭，其余值按默认（开启）。
        let auto_default = self.events.inspect(|snapshot| {
            let value = snapshot
                .config
                .values
                .get(BT_SELECTION_AUTO_DEFAULT_KEY)
                .map_or_else(
                    || fluxdown_protocol::daemon_config_default(BT_SELECTION_AUTO_DEFAULT_KEY),
                    String::as_str,
                );
            !matches!(value.trim(), "false" | "0")
        });
        auto_default.then_some(DEFAULT_BT_SELECTION_TIMEOUT)
    }
}

const DEFAULT_BT_SELECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const BT_SELECTION_AUTO_DEFAULT_KEY: &str = "bt_selection_auto_default";

#[async_trait::async_trait]
impl fluxdown_engine::selection::HostSelection for DaemonSelection {
    async fn select_hls_quality(
        &self,
        task_id: &str,
        options: &[fluxdown_engine::model::HlsQualityOption],
        timeout: std::time::Duration,
    ) -> fluxdown_engine::selection::SelectionOutcome<i32> {
        let default_index = options
            .iter()
            .enumerate()
            .max_by_key(|(_, option)| option.bandwidth)
            .map_or(0, |(index, _)| index as i32);
        let request_id = uuid::Uuid::new_v4().to_string();
        let request = SelectionRequestDto {
            request_id: request_id.clone(),
            task_id: task_id.to_owned(),
            kind: SelectionKind::Hls {
                options: options
                    .iter()
                    .cloned()
                    .map(fluxdown_engine_protocol::hls_quality_option_to_dto)
                    .collect(),
            },
            default_choice: SelectionOutcome::Hls {
                index: default_index,
            },
            deadline_unix_ms: deadline_unix_ms(timeout),
        };
        match self.begin(request) {
            SelectionWait::Immediate(_) => {
                fluxdown_engine::selection::SelectionOutcome::NoSelectorConfigured(default_index)
            }
            SelectionWait::Pending(receiver) => match tokio::time::timeout(timeout, receiver).await
            {
                Ok(Ok(SelectionOutcome::Hls { index })) => {
                    fluxdown_engine::selection::SelectionOutcome::UserChose(index)
                }
                _ => {
                    let _ = self.timeout(&request_id);
                    fluxdown_engine::selection::SelectionOutcome::TimedOutDefaulted(default_index)
                }
            },
        }
    }

    async fn select_bt_files(
        &self,
        task_id: &str,
        files: &[fluxdown_engine::model::BtFileEntry],
        save_dir: &str,
        timeout: Option<std::time::Duration>,
    ) -> fluxdown_engine::selection::SelectionOutcome<fluxdown_engine::selection::BtFileChoice>
    {
        let default_choice = fluxdown_engine::selection::BtFileChoice::default();
        let effective_timeout = self.bt_selection_timeout(timeout);
        let request_id = uuid::Uuid::new_v4().to_string();
        let request = SelectionRequestDto {
            request_id: request_id.clone(),
            task_id: task_id.to_owned(),
            kind: SelectionKind::Bt {
                files: files
                    .iter()
                    .cloned()
                    .map(fluxdown_engine_protocol::bt_file_entry_to_dto)
                    .collect(),
                save_dir: save_dir.to_owned(),
            },
            default_choice: SelectionOutcome::Bt {
                indices: Vec::new(),
                save_dir: None,
            },
            deadline_unix_ms: effective_timeout
                .map_or(fluxdown_protocol::SELECTION_NO_DEADLINE, deadline_unix_ms),
        };
        match self.begin(request) {
            SelectionWait::Immediate(_) => {
                fluxdown_engine::selection::SelectionOutcome::NoSelectorConfigured(default_choice)
            }
            SelectionWait::Pending(receiver) => {
                // 任务暂停 / 删除时引擎丢弃本 future（与取消信号竞争）；守卫保证待选项随之
                // 撤销并广播 SelectionResolved。无截止的请求终结只靠它、用户答复或 daemon 关闭。
                let _guard = PendingGuard {
                    selection: self,
                    request_id,
                };
                // 最后一个订阅者离开不会终结无截止的请求，下一个客户端连上后继续作答。
                let answer = match effective_timeout {
                    Some(limit) => tokio::time::timeout(limit, receiver).await.ok(),
                    None => Some(receiver.await),
                };
                match answer {
                    Some(Ok(SelectionOutcome::Bt { indices, save_dir })) => {
                        fluxdown_engine::selection::SelectionOutcome::UserChose(
                            fluxdown_engine::selection::BtFileChoice {
                                indices,
                                save_dir: nonempty_save_dir(save_dir),
                            },
                        )
                    }
                    Some(Ok(SelectionOutcome::Cancelled)) => {
                        fluxdown_engine::selection::SelectionOutcome::UserChose(
                            fluxdown_engine::selection::BtFileChoice {
                                indices: vec![-1],
                                save_dir: None,
                            },
                        )
                    }
                    // 超时：守卫在返回时撤销仍待处理的请求并广播。
                    _ => fluxdown_engine::selection::SelectionOutcome::TimedOutDefaulted(
                        default_choice,
                    ),
                }
            }
        }
    }

    async fn select_resolve_variant(
        &self,
        task_id: &str,
        options: &[fluxdown_engine::model::ResolveVariantOption],
        default_index: i32,
        timeout: std::time::Duration,
    ) -> fluxdown_engine::selection::SelectionOutcome<i32> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let request = SelectionRequestDto {
            request_id: request_id.clone(),
            task_id: task_id.to_owned(),
            kind: SelectionKind::Variant {
                options: options
                    .iter()
                    .cloned()
                    .map(fluxdown_engine_protocol::resolve_variant_option_to_dto)
                    .collect(),
            },
            default_choice: SelectionOutcome::Variant {
                index: default_index,
            },
            deadline_unix_ms: deadline_unix_ms(timeout),
        };
        match self.begin(request) {
            SelectionWait::Immediate(_) => {
                fluxdown_engine::selection::SelectionOutcome::NoSelectorConfigured(default_index)
            }
            SelectionWait::Pending(receiver) => match tokio::time::timeout(timeout, receiver).await
            {
                Ok(Ok(SelectionOutcome::Variant { index })) => {
                    fluxdown_engine::selection::SelectionOutcome::UserChose(index)
                }
                Ok(Ok(SelectionOutcome::Cancelled)) => {
                    fluxdown_engine::selection::SelectionOutcome::UserChose(-1)
                }
                _ => {
                    let _ = self.timeout(&request_id);
                    fluxdown_engine::selection::SelectionOutcome::TimedOutDefaulted(default_index)
                }
            },
        }
    }

    fn provide_hls_selection(&self, task_id: &str, selected_index: i32) {
        match self.resolve_for_task(
            task_id,
            SelectionType::Hls,
            SelectionOutcome::Hls {
                index: selected_index,
            },
        ) {
            Ok(()) => {}
            Err(SelectionError::NotFound | SelectionError::Conflict) => {
                tracing::debug!(task_id, "late HLS selection answer has no pending request");
            }
            Err(error) => tracing::warn!(%error, task_id, "HLS selection answer was rejected"),
        }
    }

    fn provide_bt_selection(&self, task_id: &str, selected_indices: Vec<i32>) {
        match self.resolve_for_task(
            task_id,
            SelectionType::Bt,
            SelectionOutcome::Bt {
                indices: selected_indices,
                save_dir: None,
            },
        ) {
            Ok(()) => {}
            Err(SelectionError::NotFound | SelectionError::Conflict) => {
                tracing::debug!(task_id, "late BT selection answer has no pending request");
            }
            Err(error) => tracing::warn!(%error, task_id, "BT selection answer was rejected"),
        }
    }

    fn provide_variant_selection(&self, task_id: &str, selected_index: i32) {
        match self.resolve_for_task(
            task_id,
            SelectionType::Variant,
            SelectionOutcome::Variant {
                index: selected_index,
            },
        ) {
            Ok(()) => {}
            Err(SelectionError::NotFound | SelectionError::Conflict) => {
                tracing::debug!(
                    task_id,
                    "late variant selection answer has no pending request"
                );
            }
            Err(error) => tracing::warn!(%error, task_id, "variant selection answer was rejected"),
        }
    }

    fn can_prompt(&self) -> bool {
        !lock_or_recover(&self.state).subscribers.is_empty()
    }

    async fn select_file_exists(
        &self,
        task_id: &str,
        conflict: &fluxdown_engine::selection::FileConflict,
        timeout: std::time::Duration,
    ) -> fluxdown_engine::selection::SelectionOutcome<fluxdown_engine::selection::FileExistsChoice>
    {
        use fluxdown_engine::selection::{FileExistsChoice, SelectionOutcome as EngineOutcome};

        let mut actions = vec![FileExistsAction::Rename, FileExistsAction::Overwrite];
        if conflict.allow_skip {
            actions.push(FileExistsAction::Skip);
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        let request = SelectionRequestDto {
            request_id: request_id.clone(),
            task_id: task_id.to_owned(),
            kind: SelectionKind::FileExists {
                file_name: conflict.file_name.clone(),
                save_dir: conflict.save_dir.clone(),
                existing_size: conflict.existing_size,
                existing_modified_unix_ms: conflict.existing_modified_unix_ms,
                incoming_size: conflict.incoming_size,
                rename_preview: conflict.rename_preview.clone(),
                actions,
            },
            default_choice: SelectionOutcome::FileExists {
                action: FileExistsAction::Rename,
            },
            deadline_unix_ms: deadline_unix_ms(timeout),
        };
        match self.begin(request) {
            SelectionWait::Immediate(_) => {
                EngineOutcome::NoSelectorConfigured(FileExistsChoice::Rename)
            }
            SelectionWait::Pending(receiver) => {
                // 引擎在任务暂停 / 删除时会直接丢弃本 future；守卫保证待选项随之撤销并广播。
                let _guard = PendingGuard {
                    selection: self,
                    request_id,
                };
                match tokio::time::timeout(timeout, receiver).await {
                    Ok(Ok(SelectionOutcome::FileExists { action })) => {
                        EngineOutcome::UserChose(match action {
                            FileExistsAction::Rename => FileExistsChoice::Rename,
                            FileExistsAction::Overwrite => FileExistsChoice::Overwrite,
                            FileExistsAction::Skip => FileExistsChoice::Skip,
                        })
                    }
                    Ok(Ok(SelectionOutcome::Cancelled)) => {
                        EngineOutcome::UserChose(FileExistsChoice::Cancel)
                    }
                    _ => EngineOutcome::TimedOutDefaulted(FileExistsChoice::Rename),
                }
            }
        }
    }

    async fn select_resolver_plugin(
        &self,
        task_id: &str,
        request: &fluxdown_engine::selection::PluginChoiceRequest,
        timeout: std::time::Duration,
    ) -> fluxdown_engine::selection::SelectionOutcome<fluxdown_engine::selection::PluginChoice>
    {
        use fluxdown_engine::selection::{PluginChoice, SelectionOutcome as EngineOutcome};

        let fallback = PluginChoice::Use {
            identity: request.default_identity.clone(),
            remember: false,
        };
        let request_id = uuid::Uuid::new_v4().to_string();
        let selection = SelectionRequestDto {
            request_id: request_id.clone(),
            task_id: task_id.to_owned(),
            kind: SelectionKind::Plugin {
                site: request.site.clone(),
                url: request.url.clone(),
                candidates: request
                    .candidates
                    .iter()
                    .cloned()
                    .map(fluxdown_engine_protocol::plugin_candidate_to_dto)
                    .collect(),
            },
            default_choice: SelectionOutcome::Plugin {
                identity: request.default_identity.clone(),
                remember: false,
            },
            deadline_unix_ms: deadline_unix_ms(timeout),
        };
        match self.begin(selection) {
            SelectionWait::Immediate(_) => EngineOutcome::NoSelectorConfigured(fallback),
            SelectionWait::Pending(receiver) => {
                // 任务暂停 / 删除时引擎丢弃本 future；守卫保证待选项随之撤销并广播。
                let _guard = PendingGuard {
                    selection: self,
                    request_id,
                };
                match tokio::time::timeout(timeout, receiver).await {
                    Ok(Ok(SelectionOutcome::Plugin { identity, remember })) => {
                        EngineOutcome::UserChose(PluginChoice::Use { identity, remember })
                    }
                    Ok(Ok(SelectionOutcome::Cancelled)) => {
                        EngineOutcome::UserChose(PluginChoice::Cancel)
                    }
                    _ => EngineOutcome::TimedOutDefaulted(fallback),
                }
            }
        }
    }
}

/// 等待方被丢弃或结束时撤销仍待处理的请求；已终局的请求无操作。
struct PendingGuard<'a> {
    selection: &'a DaemonSelection,
    request_id: String,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        // 已被终局时返回 None；仍待处理则移除并广播 SelectionResolved。
        let _ = self.selection.timeout(&self.request_id);
    }
}

fn deadline_unix_ms(timeout: std::time::Duration) -> i64 {
    let deadline = std::time::SystemTime::now()
        .checked_add(timeout)
        .unwrap_or(std::time::SystemTime::now());
    deadline
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn nonempty_save_dir(save_dir: Option<String>) -> Option<String> {
    save_dir.and_then(|dir| {
        let trimmed = dir.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

fn selection_type(kind: &SelectionKind) -> SelectionType {
    match kind {
        SelectionKind::Hls { .. } => SelectionType::Hls,
        SelectionKind::Bt { .. } => SelectionType::Bt,
        SelectionKind::Variant { .. } => SelectionType::Variant,
        SelectionKind::FileExists { .. } => SelectionType::FileExists,
        SelectionKind::Plugin { .. } => SelectionType::Plugin,
    }
}

fn validate_outcome(
    pending: &PendingSelection,
    outcome: &SelectionOutcome,
) -> Result<(), SelectionError> {
    match (pending.kind, outcome) {
        (SelectionType::Hls, SelectionOutcome::Hls { .. })
        | (SelectionType::Bt, SelectionOutcome::Bt { .. })
        | (SelectionType::Variant, SelectionOutcome::Variant { .. }) => Ok(()),
        (SelectionType::FileExists, SelectionOutcome::FileExists { action })
            if pending.file_exists_actions.contains(action) =>
        {
            Ok(())
        }
        (SelectionType::Plugin, SelectionOutcome::Plugin { identity, .. })
            if pending.plugin_candidates.contains(identity) =>
        {
            Ok(())
        }
        (SelectionType::Hls, SelectionOutcome::Cancelled) => Err(SelectionError::HlsCancel),
        (
            SelectionType::Bt
            | SelectionType::Variant
            | SelectionType::FileExists
            | SelectionType::Plugin,
            SelectionOutcome::Cancelled,
        ) => Ok(()),
        _ => Err(SelectionError::InvalidOutcome),
    }
}

fn remember_resolved(resolved: &mut VecDeque<String>, request_id: String) {
    resolved.push_back(request_id);
    while resolved.len() > RESOLVED_HISTORY_LIMIT {
        resolved.pop_front();
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{
        DaemonSnapshot, SelectionKind, SelectionOutcome, SelectionRequestDto,
        SelectionResolutionDto,
    };

    use super::{DaemonSelection, SelectionError, SelectionWait};
    use crate::event_hub::DaemonEventHub;

    fn manager() -> DaemonSelection {
        DaemonSelection::new(DaemonEventHub::new(DaemonSnapshot::default(), 16))
    }

    /// 有截止的请求（2100-01-01）；`SELECTION_NO_DEADLINE` 另有「不自动解析」语义，夹具不用它。
    const FIXTURE_DEADLINE_UNIX_MS: i64 = 4_102_444_800_000;

    fn hls_request(id: &str) -> SelectionRequestDto {
        SelectionRequestDto {
            request_id: id.to_owned(),
            task_id: "task-1".to_owned(),
            kind: SelectionKind::Hls {
                options: Vec::new(),
            },
            default_choice: SelectionOutcome::Hls { index: 0 },
            deadline_unix_ms: FIXTURE_DEADLINE_UNIX_MS,
        }
    }

    #[test]
    fn zero_subscribers_short_circuits_to_default() {
        let manager = manager();
        assert!(matches!(
            manager.begin(hls_request("r0")),
            SelectionWait::Immediate(SelectionOutcome::Hls { index: 0 })
        ));
    }

    #[tokio::test]
    async fn first_valid_resolution_wins_and_late_reply_conflicts() {
        let manager = manager();
        manager.subscribe("connection-1".to_owned());
        let SelectionWait::Pending(receiver) = manager.begin(hls_request("r1")) else {
            panic!("subscribed request did not wait");
        };
        assert!(matches!(
            manager.resolve(SelectionResolutionDto {
                request_id: "r1".to_owned(),
                outcome: SelectionOutcome::Cancelled,
            }),
            Err(SelectionError::HlsCancel)
        ));
        manager
            .resolve(SelectionResolutionDto {
                request_id: "r1".to_owned(),
                outcome: SelectionOutcome::Hls { index: 2 },
            })
            .expect("valid resolution");
        assert_eq!(
            receiver.await.expect("selection result"),
            SelectionOutcome::Hls { index: 2 }
        );
        assert!(matches!(
            manager.resolve(SelectionResolutionDto {
                request_id: "r1".to_owned(),
                outcome: SelectionOutcome::Hls { index: 1 },
            }),
            Err(SelectionError::Conflict)
        ));
    }

    #[tokio::test]
    async fn last_subscriber_leaving_resolves_defaults() {
        let manager = manager();
        manager.subscribe("connection-1".to_owned());
        let SelectionWait::Pending(receiver) = manager.begin(hls_request("r2")) else {
            panic!("subscribed request did not wait");
        };
        manager.unsubscribe("connection-1");
        assert_eq!(
            receiver.await.expect("default result"),
            SelectionOutcome::Hls { index: 0 }
        );
    }

    fn variant_request(id: &str, task_id: &str) -> SelectionRequestDto {
        SelectionRequestDto {
            request_id: id.to_owned(),
            task_id: task_id.to_owned(),
            kind: SelectionKind::Variant {
                options: Vec::new(),
            },
            default_choice: SelectionOutcome::Variant { index: 1 },
            deadline_unix_ms: FIXTURE_DEADLINE_UNIX_MS,
        }
    }

    #[tokio::test]
    async fn cancelling_a_task_replies_cancelled_and_ignores_other_tasks() {
        let manager = manager();
        manager.subscribe("connection-1".to_owned());
        let SelectionWait::Pending(doomed) = manager.begin(variant_request("v1", "gone")) else {
            panic!("subscribed request did not wait");
        };
        let SelectionWait::Pending(kept) = manager.begin(variant_request("v2", "alive")) else {
            panic!("subscribed request did not wait");
        };
        let SelectionWait::Pending(hls) = manager.begin(SelectionRequestDto {
            task_id: "gone".to_owned(),
            ..hls_request("h1")
        }) else {
            panic!("subscribed request did not wait");
        };
        assert_eq!(manager.cancel_task("gone"), 2);
        assert_eq!(
            doomed.await.expect("cancel reply"),
            SelectionOutcome::Cancelled
        );
        assert_eq!(
            hls.await.expect("hls falls back to default"),
            SelectionOutcome::Hls { index: 0 }
        );
        assert!(matches!(
            manager.resolve(SelectionResolutionDto {
                request_id: "v1".to_owned(),
                outcome: SelectionOutcome::Variant { index: 0 },
            }),
            Err(SelectionError::Conflict)
        ));
        manager
            .resolve(SelectionResolutionDto {
                request_id: "v2".to_owned(),
                outcome: SelectionOutcome::Variant { index: 0 },
            })
            .expect("unrelated task keeps its selection");
        assert_eq!(
            kept.await.expect("user reply"),
            SelectionOutcome::Variant { index: 0 }
        );
    }

    #[tokio::test]
    async fn task_events_revoke_selections_of_paused_or_deleted_tasks() {
        use fluxdown_protocol::{EventFrame, ServiceEvent, WsServerMsg};

        let manager = manager();
        manager.subscribe("connection-1".to_owned());
        let frame = |event| EventFrame {
            epoch: "e".to_owned(),
            sequence: 1,
            previous: None,
            event: ServiceEvent::Daemon(event),
        };
        let progress = |task_id: &str, status: i32, error_message: &str| {
            frame(fluxdown_protocol::DaemonEvent::Engine(
                WsServerMsg::TaskProgress {
                    task_id: task_id.to_owned(),
                    status,
                    downloaded_bytes: 0,
                    total_bytes: 0,
                    speed: 0,
                    upload_speed: 0,
                    file_name: String::new(),
                    save_dir: String::new(),
                    url: String::new(),
                    error_message: error_message.to_owned(),
                    uploaded_bytes: 0,
                    seeding_status: 0,
                    seeding_message: String::new(),
                    seeding_time_secs: 0,
                },
            ))
        };
        let SelectionWait::Pending(running) = manager.begin(variant_request("a", "running")) else {
            panic!("subscribed request did not wait");
        };
        let SelectionWait::Pending(paused) = manager.begin(variant_request("b", "paused")) else {
            panic!("subscribed request did not wait");
        };
        let SelectionWait::Pending(deleted) = manager.begin(variant_request("c", "deleted")) else {
            panic!("subscribed request did not wait");
        };
        manager.observe_frame(&progress("running", 5, ""));
        manager.observe_frame(&progress("paused", 2, ""));
        manager.observe_frame(&progress("deleted", 4, "deleted"));
        assert_eq!(paused.await.expect("paused"), SelectionOutcome::Cancelled);
        assert_eq!(deleted.await.expect("deleted"), SelectionOutcome::Cancelled);
        manager
            .resolve(SelectionResolutionDto {
                request_id: "a".to_owned(),
                outcome: SelectionOutcome::Variant { index: 0 },
            })
            .expect("running task keeps its selection");
        assert_eq!(
            running.await.expect("running"),
            SelectionOutcome::Variant { index: 0 }
        );
    }

    #[tokio::test]
    async fn snapshot_drops_selections_of_missing_tasks() {
        use fluxdown_protocol::{EventFrame, ServiceEvent, WsServerMsg};

        let manager = manager();
        manager.subscribe("connection-1".to_owned());
        let SelectionWait::Pending(orphan) = manager.begin(variant_request("o", "orphan")) else {
            panic!("subscribed request did not wait");
        };
        manager.observe_frame(&EventFrame {
            epoch: "e".to_owned(),
            sequence: 1,
            previous: None,
            event: ServiceEvent::Daemon(fluxdown_protocol::DaemonEvent::Engine(
                WsServerMsg::TasksSnapshot { tasks: Vec::new() },
            )),
        });
        assert_eq!(orphan.await.expect("orphan"), SelectionOutcome::Cancelled);
    }

    fn file_exists_request(id: &str, task_id: &str, skip: bool) -> SelectionRequestDto {
        use fluxdown_protocol::FileExistsAction;
        let mut actions = vec![FileExistsAction::Rename, FileExistsAction::Overwrite];
        if skip {
            actions.push(FileExistsAction::Skip);
        }
        SelectionRequestDto {
            request_id: id.to_owned(),
            task_id: task_id.to_owned(),
            kind: SelectionKind::FileExists {
                file_name: "a.bin".to_owned(),
                save_dir: "/tmp".to_owned(),
                existing_size: Some(1),
                existing_modified_unix_ms: None,
                incoming_size: None,
                rename_preview: "a (1).bin".to_owned(),
                actions,
            },
            default_choice: SelectionOutcome::FileExists {
                action: FileExistsAction::Rename,
            },
            deadline_unix_ms: FIXTURE_DEADLINE_UNIX_MS,
        }
    }

    #[tokio::test]
    async fn file_exists_validates_action_against_offered_actions() {
        use fluxdown_protocol::FileExistsAction;
        let manager = manager();
        manager.subscribe("c".to_owned());
        let SelectionWait::Pending(receiver) = manager.begin(file_exists_request("f1", "t", false))
        else {
            panic!("subscribed request did not wait");
        };
        let resolve = |outcome| {
            manager.resolve(SelectionResolutionDto {
                request_id: "f1".to_owned(),
                outcome,
            })
        };
        assert!(matches!(
            resolve(SelectionOutcome::FileExists {
                action: FileExistsAction::Skip
            }),
            Err(SelectionError::InvalidOutcome)
        ));
        assert!(matches!(
            resolve(SelectionOutcome::Hls { index: 0 }),
            Err(SelectionError::InvalidOutcome)
        ));
        resolve(SelectionOutcome::FileExists {
            action: FileExistsAction::Overwrite,
        })
        .expect("offered action");
        assert_eq!(
            receiver.await.expect("reply"),
            SelectionOutcome::FileExists {
                action: FileExistsAction::Overwrite
            }
        );
        assert!(matches!(
            resolve(SelectionOutcome::Cancelled),
            Err(SelectionError::Conflict)
        ));
    }

    #[tokio::test]
    async fn file_exists_accepts_cancel_and_task_cancel_maps_to_cancelled() {
        let manager = manager();
        manager.subscribe("c".to_owned());
        let SelectionWait::Pending(user) = manager.begin(file_exists_request("f2", "t", true))
        else {
            panic!("subscribed request did not wait");
        };
        manager
            .resolve(SelectionResolutionDto {
                request_id: "f2".to_owned(),
                outcome: SelectionOutcome::Cancelled,
            })
            .expect("cancel is a valid answer");
        assert_eq!(user.await.expect("reply"), SelectionOutcome::Cancelled);

        let SelectionWait::Pending(paused) = manager.begin(file_exists_request("f3", "p", true))
        else {
            panic!("subscribed request did not wait");
        };
        assert_eq!(manager.cancel_task("p"), 1);
        assert_eq!(paused.await.expect("reply"), SelectionOutcome::Cancelled);
    }

    #[tokio::test]
    async fn file_exists_defaults_to_rename_without_subscribers_or_when_last_leaves() {
        use fluxdown_engine::selection::{
            FileConflict, FileExistsChoice, HostSelection, SelectionOutcome as Engine,
        };
        let conflict = FileConflict {
            file_name: "a.bin".to_owned(),
            save_dir: "/tmp".to_owned(),
            existing_size: None,
            existing_modified_unix_ms: None,
            incoming_size: None,
            rename_preview: "a (1).bin".to_owned(),
            allow_skip: true,
        };
        let timeout = std::time::Duration::from_secs(30);
        let manager = manager();
        assert!(!manager.can_prompt());
        assert_eq!(
            manager.select_file_exists("t", &conflict, timeout).await,
            Engine::NoSelectorConfigured(FileExistsChoice::Rename)
        );

        manager.subscribe("c".to_owned());
        assert!(manager.can_prompt());
        let waiter = {
            let manager = manager.clone();
            let conflict = conflict.clone();
            tokio::spawn(async move { manager.select_file_exists("t", &conflict, timeout).await })
        };
        while lock_pending_len(&manager) == 0 {
            tokio::task::yield_now().await;
        }
        manager.unsubscribe("c");
        assert_eq!(
            waiter.await.expect("join"),
            Engine::UserChose(FileExistsChoice::Rename)
        );
    }

    #[tokio::test]
    async fn plugin_choice_only_accepts_offered_candidates_and_remember_round_trips() {
        use fluxdown_engine::selection::{
            HostSelection, PluginCandidate, PluginChoice, PluginChoiceRequest,
            SelectionOutcome as Engine,
        };
        let manager = manager();
        manager.subscribe("c".to_owned());
        let candidate = |identity: &str| PluginCandidate {
            identity: identity.to_owned(),
            name: identity.to_owned(),
            version: "1.0.0".to_owned(),
            description: String::new(),
        };
        let request = PluginChoiceRequest {
            site: "example.com".to_owned(),
            url: "https://example.com/v".to_owned(),
            candidates: vec![candidate("a@x"), candidate("b@x")],
            default_identity: "a@x".to_owned(),
        };
        let waiter = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .select_resolver_plugin("t", &request, std::time::Duration::from_secs(5))
                    .await
            })
        };
        let request_id = loop {
            if let Some(id) = super::lock_or_recover(&manager.state).pending.keys().next() {
                break id.clone();
            }
            tokio::task::yield_now().await;
        };
        let resolve = |outcome| {
            manager.resolve(SelectionResolutionDto {
                request_id: request_id.clone(),
                outcome,
            })
        };
        assert!(matches!(
            resolve(SelectionOutcome::Plugin {
                identity: "evil@x".to_owned(),
                remember: true,
            }),
            Err(SelectionError::InvalidOutcome)
        ));
        resolve(SelectionOutcome::Plugin {
            identity: "b@x".to_owned(),
            remember: true,
        })
        .expect("offered candidate");
        assert_eq!(
            waiter.await.expect("join"),
            Engine::UserChose(PluginChoice::Use {
                identity: "b@x".to_owned(),
                remember: true,
            })
        );
        assert_eq!(lock_pending_len(&manager), 0);
    }
    #[tokio::test]
    async fn bt_selection_forwards_save_dir_and_filters_blank() {
        use fluxdown_engine::model::BtFileEntry;
        use fluxdown_engine::selection::{BtFileChoice, HostSelection, SelectionOutcome as Engine};
        use fluxdown_protocol::{DaemonEvent, ServiceEvent};

        let files = vec![BtFileEntry {
            index: 0,
            path: "a.bin".to_owned(),
            size: 1,
        }];
        let manager = manager();
        assert_eq!(
            manager.select_bt_files("t", &files, "/data", None).await,
            Engine::NoSelectorConfigured(BtFileChoice::default())
        );

        manager.subscribe("c".to_owned());
        let mut frames = manager.events.subscribe();
        let waiter = {
            let manager = manager.clone();
            let files = files.clone();
            tokio::spawn(async move {
                manager
                    .select_bt_files(
                        "t",
                        &files,
                        "/data/bt",
                        Some(std::time::Duration::from_secs(5)),
                    )
                    .await
            })
        };
        let request = loop {
            let frame = frames.recv().await.expect("selection event");
            if let ServiceEvent::Daemon(DaemonEvent::SelectionPending(request)) = frame.event {
                break request;
            }
        };
        assert!(matches!(
            &request.kind,
            SelectionKind::Bt { save_dir, .. } if save_dir == "/data/bt"
        ));
        assert!(matches!(
            request.default_choice,
            SelectionOutcome::Bt { ref indices, save_dir: None } if indices.is_empty()
        ));
        manager
            .resolve(SelectionResolutionDto {
                request_id: request.request_id,
                outcome: SelectionOutcome::Bt {
                    indices: vec![0, 2],
                    save_dir: Some("  /movies  ".to_owned()),
                },
            })
            .expect("resolve moved dir");
        assert_eq!(
            waiter.await.expect("join"),
            Engine::UserChose(BtFileChoice {
                indices: vec![0, 2],
                save_dir: Some("/movies".to_owned()),
            })
        );

        let blank = {
            let manager = manager.clone();
            let files = files.clone();
            tokio::spawn(async move {
                manager
                    .select_bt_files(
                        "t",
                        &files,
                        "/data/bt",
                        Some(std::time::Duration::from_secs(5)),
                    )
                    .await
            })
        };
        let request_id = loop {
            let frame = frames.recv().await.expect("blank selection event");
            if let ServiceEvent::Daemon(DaemonEvent::SelectionPending(request)) = frame.event {
                break request.request_id;
            }
        };
        manager
            .resolve(SelectionResolutionDto {
                request_id,
                outcome: SelectionOutcome::Bt {
                    indices: vec![1],
                    save_dir: Some("   ".to_owned()),
                },
            })
            .expect("resolve blank dir");
        assert_eq!(
            blank.await.expect("join"),
            Engine::UserChose(BtFileChoice {
                indices: vec![1],
                save_dir: None,
            })
        );

        let cancelled = {
            let manager = manager.clone();
            let files = files.clone();
            tokio::spawn(async move {
                manager
                    .select_bt_files(
                        "t",
                        &files,
                        "/data/bt",
                        Some(std::time::Duration::from_secs(5)),
                    )
                    .await
            })
        };
        let request_id = loop {
            let frame = frames.recv().await.expect("cancel selection event");
            if let ServiceEvent::Daemon(DaemonEvent::SelectionPending(request)) = frame.event {
                break request.request_id;
            }
        };
        manager
            .resolve(SelectionResolutionDto {
                request_id,
                outcome: SelectionOutcome::Cancelled,
            })
            .expect("resolve cancel");
        assert_eq!(
            cancelled.await.expect("join"),
            Engine::UserChose(BtFileChoice {
                indices: vec![-1],
                save_dir: None,
            })
        );

        let provided = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .select_bt_files(
                        "late",
                        &files,
                        "/data/bt",
                        Some(std::time::Duration::from_secs(5)),
                    )
                    .await
            })
        };
        while lock_pending_len(&manager) == 0 {
            tokio::task::yield_now().await;
        }
        manager.provide_bt_selection("late", vec![3]);
        assert_eq!(
            provided.await.expect("join"),
            Engine::UserChose(BtFileChoice {
                indices: vec![3],
                save_dir: None,
            })
        );

        let timed_out = manager
            .select_bt_files(
                "slow",
                &[],
                "/data/bt",
                Some(std::time::Duration::from_millis(20)),
            )
            .await;
        assert_eq!(
            timed_out,
            Engine::TimedOutDefaulted(BtFileChoice::default())
        );
    }

    async fn next_pending(
        frames: &mut tokio::sync::broadcast::Receiver<fluxdown_protocol::EventFrame>,
    ) -> SelectionRequestDto {
        use fluxdown_protocol::{DaemonEvent, ServiceEvent};
        loop {
            let frame = frames.recv().await.expect("selection event");
            if let ServiceEvent::Daemon(DaemonEvent::SelectionPending(request)) = frame.event {
                break request;
            }
        }
    }

    #[tokio::test]
    async fn bt_selection_auto_default_switch_controls_deadline() {
        use fluxdown_engine::selection::{BtFileChoice, HostSelection, SelectionOutcome as Engine};
        use fluxdown_protocol::SELECTION_NO_DEADLINE;

        // 默认开启：60 秒截止。
        let manager = manager();
        manager.subscribe("c".to_owned());
        let mut frames = manager.events.subscribe();
        let waiter = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.select_bt_files("t", &[], "/data", None).await })
        };
        let before = super::deadline_unix_ms(std::time::Duration::ZERO);
        let request = next_pending(&mut frames).await;
        assert!(request.has_deadline());
        let remaining = request.deadline_unix_ms - before;
        assert!(
            (50_000..=61_000).contains(&remaining),
            "default deadline ≈ 60s, got {remaining}ms"
        );
        manager.cancel_task("t");
        waiter.await.expect("join");

        // 关闭：无截止，一直等到用户答复。
        let manager = no_deadline_manager();
        manager.subscribe("c".to_owned());
        let mut frames = manager.events.subscribe();
        let waiter = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.select_bt_files("t", &[], "/data", None).await })
        };
        let request = next_pending(&mut frames).await;
        assert_eq!(request.deadline_unix_ms, SELECTION_NO_DEADLINE);
        assert!(!request.has_deadline());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());
        assert_eq!(lock_pending_len(&manager), 1);
        manager
            .resolve(SelectionResolutionDto {
                request_id: request.request_id,
                outcome: SelectionOutcome::Bt {
                    indices: vec![0],
                    save_dir: None,
                },
            })
            .expect("resolve");
        assert_eq!(
            waiter.await.expect("join"),
            Engine::UserChose(BtFileChoice {
                indices: vec![0],
                save_dir: None,
            })
        );

        // 关闭时调用方显式给的上限仍然生效。
        let timed_out = manager
            .select_bt_files(
                "slow",
                &[],
                "/data",
                Some(std::time::Duration::from_millis(20)),
            )
            .await;
        assert_eq!(
            timed_out,
            Engine::TimedOutDefaulted(BtFileChoice::default())
        );
    }

    fn no_deadline_manager() -> DaemonSelection {
        let mut snapshot = DaemonSnapshot::default();
        snapshot
            .config
            .values
            .insert("bt_selection_auto_default".to_owned(), "false".to_owned());
        DaemonSelection::new(DaemonEventHub::new(snapshot, 16))
    }

    #[tokio::test]
    async fn no_deadline_bt_selection_survives_last_subscriber_leaving() {
        use fluxdown_engine::selection::{BtFileChoice, HostSelection, SelectionOutcome as Engine};

        let manager = no_deadline_manager();
        manager.subscribe("tab".to_owned());
        let mut frames = manager.events.subscribe();
        let bt = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.select_bt_files("bt", &[], "/data", None).await })
        };
        let request = next_pending(&mut frames).await;
        let variant = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .select_resolve_variant("v", &[], 0, std::time::Duration::from_secs(30))
                    .await
            })
        };
        while lock_pending_len(&manager) < 2 {
            tokio::task::yield_now().await;
        }

        // Web 刷新 / 断线：最后一个订阅者离开。有截止的请求照旧取默认值。
        manager.unsubscribe("tab");
        assert_eq!(variant.await.expect("join"), Engine::UserChose(0));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!bt.is_finished(), "no-deadline request must not default");
        assert_eq!(lock_pending_len(&manager), 1);
        assert!(manager.events.inspect(|snapshot| {
            snapshot
                .pending_selections
                .iter()
                .any(|pending| pending.request_id == request.request_id)
        }));

        // 下一个客户端从快照拿到请求并作答。
        manager.subscribe("tab-reloaded".to_owned());
        manager
            .resolve(SelectionResolutionDto {
                request_id: request.request_id,
                outcome: SelectionOutcome::Bt {
                    indices: vec![1],
                    save_dir: None,
                },
            })
            .expect("answer after reconnect");
        assert_eq!(
            bt.await.expect("join"),
            Engine::UserChose(BtFileChoice {
                indices: vec![1],
                save_dir: None,
            })
        );
    }

    #[tokio::test]
    async fn shutdown_cancels_no_deadline_bt_selection_instead_of_defaulting() {
        use fluxdown_engine::selection::{BtFileChoice, HostSelection, SelectionOutcome as Engine};

        let manager = no_deadline_manager();
        manager.subscribe("c".to_owned());
        let bt = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.select_bt_files("bt", &[], "/data", None).await })
        };
        let variant = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .select_resolve_variant("v", &[], 0, std::time::Duration::from_secs(30))
                    .await
            })
        };
        while lock_pending_len(&manager) < 2 {
            tokio::task::yield_now().await;
        }
        manager.resolve_all_for_shutdown();
        assert_eq!(
            bt.await.expect("join"),
            Engine::UserChose(BtFileChoice {
                indices: vec![-1],
                save_dir: None,
            })
        );
        assert_eq!(variant.await.expect("join"), Engine::UserChose(0));
        assert_eq!(lock_pending_len(&manager), 0);
    }

    // 关闭自动默认后请求不设截止：任务暂停 / 删除时引擎丢弃等待 future，这是它唯一的
    // 非答复终结路径。丢弃后待选项必须撤销、广播 SelectionResolved，快照里不再挂着。
    #[tokio::test]
    async fn dropping_no_deadline_bt_wait_withdraws_and_broadcasts() {
        use fluxdown_engine::selection::HostSelection;
        use fluxdown_protocol::{DaemonEvent, ServiceEvent};

        let manager = no_deadline_manager();
        manager.subscribe("c".to_owned());
        let mut frames = manager.events.subscribe();
        let waiter = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.select_bt_files("bt", &[], "/data", None).await })
        };
        let request = next_pending(&mut frames).await;
        assert!(!request.has_deadline());
        waiter.abort();
        assert!(waiter.await.expect_err("aborted").is_cancelled());

        assert_eq!(lock_pending_len(&manager), 0);
        let resolved = loop {
            let frame = frames.recv().await.expect("selection event");
            if let ServiceEvent::Daemon(DaemonEvent::SelectionResolved { request_id }) = frame.event
            {
                break request_id;
            }
        };
        assert_eq!(resolved, request.request_id);
        assert!(
            manager
                .events
                .inspect(|snapshot| snapshot.pending_selections.is_empty())
        );
        assert!(matches!(
            manager.resolve(SelectionResolutionDto {
                request_id: request.request_id,
                outcome: SelectionOutcome::Cancelled,
            }),
            Err(SelectionError::Conflict)
        ));
    }

    fn lock_pending_len(manager: &DaemonSelection) -> usize {
        super::lock_or_recover(&manager.state).pending.len()
    }
}

#[cfg(test)]
mod drop_tests {
    use fluxdown_engine::selection::{FileConflict, HostSelection};
    use fluxdown_protocol::{DaemonSnapshot, SelectionResolutionDto};

    use super::{DaemonSelection, SelectionError};
    use crate::event_hub::DaemonEventHub;

    #[tokio::test]
    async fn dropping_the_file_exists_wait_withdraws_the_pending_request() {
        let manager = DaemonSelection::new(DaemonEventHub::new(DaemonSnapshot::default(), 16));
        manager.subscribe("c".to_owned());
        let conflict = FileConflict {
            file_name: "a.bin".to_owned(),
            save_dir: "/tmp".to_owned(),
            existing_size: None,
            existing_modified_unix_ms: None,
            incoming_size: None,
            rename_preview: "a (1).bin".to_owned(),
            allow_skip: false,
        };
        let waiter = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .select_file_exists("t", &conflict, std::time::Duration::from_secs(30))
                    .await
            })
        };
        let request_id = loop {
            let id = super::lock_or_recover(&manager.state)
                .pending
                .keys()
                .next()
                .cloned();
            if let Some(id) = id {
                break id;
            }
            tokio::task::yield_now().await;
        };
        waiter.abort();
        assert!(waiter.await.expect_err("aborted").is_cancelled());
        assert!(matches!(
            manager.resolve(SelectionResolutionDto {
                request_id,
                outcome: fluxdown_protocol::SelectionOutcome::Cancelled,
            }),
            Err(SelectionError::Conflict)
        ));
    }
}
