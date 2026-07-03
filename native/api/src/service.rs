//! API 宿主契约 —— [`ApiHost`] trait。
//!
//! HTTP 层（`server`/`jsonrpc` 模块）只依赖本 trait，不关心宿主形态：
//! - 桌面/手机 App（hub）：实现为「命令 + oneshot 回执」桥接到 download_actor
//! - 未来 headless server：实现为直接调用 `fluxdown_engine::Engine`
//!
//! 换宿主不换 HTTP 层，这是「一份 API 契约、多宿主复用」的核心边界。

use async_trait::async_trait;

use crate::types::{CreateTaskRequest, DownloadRequest, QueueDto, TaskDto};

/// API 操作错误。由 HTTP 层映射为响应状态码。
///
/// # Examples
///
/// ```
/// use fluxdown_api::service::ApiError;
///
/// let e = ApiError::BadRequest("url is required".to_string());
/// assert_eq!(e.to_string(), "url is required");
/// assert_eq!(ApiError::NotFound.to_string(), "not found");
/// ```
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 资源不存在 → 404。
    #[error("not found")]
    NotFound,
    /// 请求参数非法 → 400。
    #[error("{0}")]
    BadRequest(String),
    /// 宿主正在关闭（命令通道已断）→ 503。
    #[error("app shutting down")]
    Unavailable,
    /// 其它内部错误 → 500。
    #[error("{0}")]
    Internal(String),
}

/// API 宿主能力契约。所有方法与 `/api/v1` 端点一一对应，
/// 外加 [`submit_external`](ApiHost::submit_external)（接管/aria2 兼容入口）。
#[async_trait]
pub trait ApiHost: Send + Sync {
    /// 列出全部任务。
    async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError>;

    /// 按 ID 查询单个任务。
    async fn get_task(&self, task_id: &str) -> Result<Option<TaskDto>, ApiError>;

    /// 直接创建下载任务（不弹确认框），返回新任务 ID。
    async fn create_task(&self, req: CreateTaskRequest) -> Result<String, ApiError>;

    /// 删除任务。`delete_files = true` 时同时删除磁盘文件。
    async fn delete_task(&self, task_id: &str, delete_files: bool) -> Result<(), ApiError>;

    /// 暂停单个任务。
    async fn pause_task(&self, task_id: &str) -> Result<(), ApiError>;

    /// 恢复单个任务。
    async fn continue_task(&self, task_id: &str) -> Result<(), ApiError>;

    /// 暂停全部活跃任务。
    async fn pause_all(&self) -> Result<(), ApiError>;

    /// 恢复全部已暂停任务。
    async fn continue_all(&self) -> Result<(), ApiError>;

    /// 列出全部命名队列。
    async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError>;

    /// 提交一个外部下载请求（浏览器脚本接管 / aria2 兼容层）。
    ///
    /// 语义与浏览器扩展 NMH 一致：进入宿主的「外部下载」流程
    /// （桌面端会弹出快速下载确认框），**不**直接创建任务。
    async fn submit_external(&self, req: DownloadRequest) -> Result<(), ApiError>;
}
