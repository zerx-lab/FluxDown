//! [`HubApiHost`] —— `fluxdown_api::service::ApiHost` 的桌面 App 实现。
//!
//! ## 读写分离
//!
//! - **读操作**（任务列表 / 单任务 / 队列列表）直查 [`Db`]（`Clone`，Arc 背书），
//!   零 actor 往返。进度字段（`downloaded_bytes`）随下载引擎的 5s 批量持久化
//!   刷新，对轮询式管理客户端足够。
//! - **写操作**（创建 / 暂停 / 恢复 / 删除）打包为 [`ApiCommand`] + oneshot 回执，
//!   经 mpsc 送入 `download_actor` 的 `select!` 事件循环 —— engine 由 actor 独占
//!   （单线程串行），与 rinf 信号处理共享同一条路径，天然免数据竞争。
//! - **外部下载**（脚本接管 / aria2 兼容）复用既有 `native_msg_rx` 通道，
//!   走「确认弹框 → 创建」全链路，与浏览器扩展完全一致。

use async_trait::async_trait;
use fluxdown_api::service::{ApiError, ApiHost};
use fluxdown_api::types::{CreateTaskRequest, DownloadRequest, QueueDto, TaskDto};
use fluxdown_engine::db::Db;
use tokio::sync::{mpsc, oneshot};

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
}

/// 桌面 App 的 API 宿主。构造后传给 `fluxdown_api::server::spawn_api_server`。
pub struct HubApiHost {
    db: Db,
    cmd_tx: mpsc::Sender<ApiCommand>,
    ext_tx: mpsc::Sender<DownloadRequest>,
}

impl HubApiHost {
    /// `cmd_tx` → actor 的 `api_cmd_rx`；`ext_tx` → actor 的 `native_msg_rx`
    /// （与 NMH / 脚本接管共用的外部下载通道）。
    pub fn new(
        db: Db,
        cmd_tx: mpsc::Sender<ApiCommand>,
        ext_tx: mpsc::Sender<DownloadRequest>,
    ) -> Self {
        Self { db, cmd_tx, ext_tx }
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
}
