//! [`HubApiHost`] —— `fluxdown_api::service::ApiHost` 的桌面 App 实现。
//!
//! ## 读写分离
//!
//! - **读操作**（任务列表 / 单任务 / 队列列表 / 全局配置）直查 [`Db`]（`Clone`，
//!   Arc 背书），零 actor 往返。进度字段（`downloaded_bytes`）随下载引擎的
//!   5s 批量持久化刷新，对轮询式管理客户端足够。
//! - **写操作**（创建 / 暂停 / 恢复 / 删除 / 配置写入）打包为 [`ApiCommand`] +
//!   oneshot 回执，经 mpsc 送入 `download_actor` 的 `select!` 事件循环 ——
//!   engine 由 actor 独占（单线程串行），与 rinf 信号处理共享同一条路径，
//!   天然免数据竞争。
//! - **外部下载**（脚本接管 / aria2 兼容）复用既有 `native_msg_rx` 通道，
//!   走「确认弹框 → 创建」全链路，与浏览器扩展完全一致。
//! - **实时速率**（[`ApiHost::live_speeds`]）直查内存态 [`LiveSpeedMap`]，
//!   由 [`crate::rinf_sink::RinfEventSink`] 在 `EngineEvent::TaskProgress`
//!   时写入，两者共享同一个 `Arc`（构造点见 `download_actor::run`），
//!   不经 actor 往返。
//! - **任务事件订阅**([`ApiHost::subscribe_task_events`])返回内存态
//!   `broadcast::Sender<TaskEvent>` 的新 `Receiver`,同一个 `Sender` 由
//!   [`crate::rinf_sink::RinfEventSink`] 在状态迁移判定后发送(构造点同见
//!   `download_actor::run`),供 `/jsonrpc` 的 WS 层转译为
//!   `aria2.onDownloadXxx` 通知帧。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fluxdown_api::service::{ApiError, ApiHost, LiveSpeed, TaskEvent};
use fluxdown_api::types::{CreateTaskRequest, DownloadRequest, QueueDto, TaskDto};
use fluxdown_engine::db::Db;
use tokio::sync::{broadcast, mpsc, oneshot};

/// 任务实时速率表：`task_id → LiveSpeed`。写端见 [`crate::rinf_sink::RinfEventSink`]；
/// 这里只是共享 `Arc` 的类型别名，读写双方各自加锁做「单次操作 + 立即
/// 释放」，不跨 `.await` 持锁。
pub type LiveSpeedMap = Arc<Mutex<HashMap<String, LiveSpeed>>>;

/// 取出锁内容；`Mutex` 中毒（某持锁线程 panic）时回退到内部值而非扩散
/// panic——这是内存态缓存，恢复正确性由后续事件覆盖写入保证，值得用
/// 回退换稳定性（同一模式见 `rinf_selection.rs`/`ws_hub.rs` 各自的
/// `lock_or_recover`）。
pub(crate) fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 写操作命令。由 `download_actor` 的 `api_cmd_rx` 分支消费。
///
/// 每个变体携带 oneshot 回执：actor 完成操作后发送结果，HTTP 层同步等待。
/// actor 退出（应用关闭）时 channel 断开，映射为 503。
pub enum ApiCommand {
    /// 直接创建任务（不弹确认框），回执新任务 ID；`None` = DB 插入失败。
    /// `req` 装箱：`CreateTaskRequest` 远大于其余变体（clippy::large_enum_variant）。
    CreateTask {
        req: Box<CreateTaskRequest>,
        ack: oneshot::Sender<Option<String>>,
    },
    PauseTask {
        task_id: String,
        ack: oneshot::Sender<()>,
    },
    ContinueTask {
        task_id: String,
        ack: oneshot::Sender<()>,
    },
    DeleteTask {
        task_id: String,
        delete_files: bool,
        ack: oneshot::Sender<()>,
    },
    PauseAll {
        ack: oneshot::Sender<()>,
    },
    ContinueAll {
        ack: oneshot::Sender<()>,
    },
    /// 配置键已由 `HubApiHost::apply_config` 逐键写入 DB，按键名 live-apply
    /// 到引擎（镜像桌面 `SaveConfig` 信号分支的「键 → 引擎 setter」逻辑，
    /// 见 `download_actor::apply_config_key`）。
    ApplyConfig {
        keys: Vec<String>,
        ack: oneshot::Sender<()>,
    },
}

/// 桌面 App 的 API 宿主。构造后传给 `fluxdown_api::server::spawn_api_server`。
pub struct HubApiHost {
    db: Db,
    cmd_tx: mpsc::Sender<ApiCommand>,
    ext_tx: mpsc::Sender<DownloadRequest>,
    /// 实时速率表,与注入 `RinfEventSink` 的是同一个 `Arc`。
    live_speeds: LiveSpeedMap,
    /// 任务生命周期事件广播源,与注入 `RinfEventSink` 的是同一个 `Sender`;
    /// `subscribe_task_events()` 经它开出新的 `Receiver`。
    task_events_tx: broadcast::Sender<TaskEvent>,
}

impl HubApiHost {
    /// `cmd_tx` → actor 的 `api_cmd_rx`;`ext_tx` → actor 的 `native_msg_rx`
    /// (与 NMH / 脚本接管共用的外部下载通道);`live_speeds` → 与
    /// `RinfEventSink` 共享的同一个实时速率表 `Arc`;`task_events_tx` → 与
    /// `RinfEventSink` 共享的同一个任务事件广播 `Sender`。
    pub fn new(
        db: Db,
        cmd_tx: mpsc::Sender<ApiCommand>,
        ext_tx: mpsc::Sender<DownloadRequest>,
        live_speeds: LiveSpeedMap,
        task_events_tx: broadcast::Sender<TaskEvent>,
    ) -> Self {
        Self {
            db,
            cmd_tx,
            ext_tx,
            live_speeds,
            task_events_tx,
        }
    }

    /// 发送命令并等待回执。actor 侧断开 → 503。
    async fn send_cmd<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> ApiCommand,
    ) -> Result<T, ApiError> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(make(ack))
            .await
            .map_err(|_| ApiError::Unavailable)?;
        rx.await.map_err(|_| ApiError::Unavailable)
    }

    /// 任务存在性检查（写操作前置），不存在 → 404。
    async fn ensure_task_exists(&self, task_id: &str) -> Result<(), ApiError> {
        match self.db.load_task_by_id(task_id).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(ApiError::NotFound),
            Err(e) => Err(ApiError::Internal(e.to_string())),
        }
    }
}

#[async_trait]
impl ApiHost for HubApiHost {
    async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError> {
        self.db
            .load_all_tasks()
            .await
            .map(|tasks| tasks.into_iter().map(TaskDto::from).collect())
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn get_task(&self, task_id: &str) -> Result<Option<TaskDto>, ApiError> {
        self.db
            .load_task_by_id(task_id)
            .await
            .map(|t| t.map(TaskDto::from))
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn create_task(&self, req: CreateTaskRequest) -> Result<String, ApiError> {
        self.send_cmd(|ack| ApiCommand::CreateTask {
            req: Box::new(req),
            ack,
        })
        .await?
        .ok_or_else(|| ApiError::Internal("failed to persist task".to_string()))
    }

    async fn delete_task(&self, task_id: &str, delete_files: bool) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.send_cmd(|ack| ApiCommand::DeleteTask {
            task_id: task_id.to_string(),
            delete_files,
            ack,
        })
        .await
    }

    async fn pause_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.send_cmd(|ack| ApiCommand::PauseTask {
            task_id: task_id.to_string(),
            ack,
        })
        .await
    }

    async fn continue_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.send_cmd(|ack| ApiCommand::ContinueTask {
            task_id: task_id.to_string(),
            ack,
        })
        .await
    }

    async fn pause_all(&self) -> Result<(), ApiError> {
        self.send_cmd(|ack| ApiCommand::PauseAll { ack }).await
    }

    async fn continue_all(&self) -> Result<(), ApiError> {
        self.send_cmd(|ack| ApiCommand::ContinueAll { ack }).await
    }

    async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
        self.db
            .load_all_queues()
            .await
            .map(|qs| qs.into_iter().map(QueueDto::from).collect())
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn submit_external(&self, req: DownloadRequest) -> Result<(), ApiError> {
        self.ext_tx
            .send(req)
            .await
            .map_err(|_| ApiError::Unavailable)
    }

    async fn get_config(&self) -> Result<HashMap<String, String>, ApiError> {
        self.db
            .get_all_config()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn apply_config(&self, changes: HashMap<String, String>) -> Result<(), ApiError> {
        // 先逐键持久化到 DB，全部成功后才触发引擎 live-apply。命令只携带
        // keys（不带值）：与 server 侧 `ActorCmd::ApplyConfig` 语义一致——
        // 接收端重新从 DB 整表读取，避免命令 payload 与 DB 状态不一致。
        for (key, value) in &changes {
            self.db
                .set_config(key, value)
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        let keys: Vec<String> = changes.into_keys().collect();
        self.send_cmd(|ack| ApiCommand::ApplyConfig { keys, ack })
            .await
    }

    async fn live_speeds(&self) -> Result<HashMap<String, LiveSpeed>, ApiError> {
        Ok(lock_or_recover(&self.live_speeds).clone())
    }

    fn subscribe_task_events(&self) -> Option<broadcast::Receiver<TaskEvent>> {
        Some(self.task_events_tx.subscribe())
    }
}
