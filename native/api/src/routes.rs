//! 路由路径常量 —— server 与 Rust 客户端（如未来 MCP server）共用同一份，
//! 保证请求地址永不漂移。
//!
//! 参数占位符使用 axum 0.8 语法 `{id}`；客户端侧用
//! [`task_path`] 等辅助函数生成实际路径。
//!
//! # Examples
//!
//! ```
//! use fluxdown_api::routes;
//!
//! assert_eq!(routes::API_TASKS, "/api/v1/tasks");
//! assert_eq!(routes::task_path("abc"), "/api/v1/tasks/abc");
//! ```

/// 探活（无鉴权）。
pub const PING: &str = "/ping";
/// 油猴脚本接管：单任务。
pub const DOWNLOAD: &str = "/download";
/// 油猴脚本接管：批量。
pub const DOWNLOAD_BATCH: &str = "/download/batch";
/// aria2 JSON-RPC 兼容端点。
pub const JSONRPC: &str = "/jsonrpc";

/// 管理 API 版本前缀。
pub const API_PREFIX: &str = "/api/v1";
/// 应用信息。
pub const API_INFO: &str = "/api/v1/info";
/// 任务集合（GET 列表 / POST 创建）。
pub const API_TASKS: &str = "/api/v1/tasks";
/// 单任务（GET / DELETE）。
pub const API_TASK: &str = "/api/v1/tasks/{id}";
/// 暂停单任务（PUT）。
pub const API_TASK_PAUSE: &str = "/api/v1/tasks/{id}/pause";
/// 恢复单任务（PUT）。
pub const API_TASK_CONTINUE: &str = "/api/v1/tasks/{id}/continue";
/// 暂停全部（PUT）。
pub const API_TASKS_PAUSE: &str = "/api/v1/tasks/pause";
/// 恢复全部（PUT）。
pub const API_TASKS_CONTINUE: &str = "/api/v1/tasks/continue";
/// 队列列表（GET）。
pub const API_QUEUES: &str = "/api/v1/queues";
/// OpenAPI 3.1 规范文档（GET，无鉴权）。
pub const API_OPENAPI: &str = "/api/v1/openapi.json";

/// 生成单任务路径（客户端用）。
#[must_use]
pub fn task_path(task_id: &str) -> String {
    format!("{API_TASKS}/{task_id}")
}

/// 生成暂停单任务路径（客户端用）。
#[must_use]
pub fn task_pause_path(task_id: &str) -> String {
    format!("{API_TASKS}/{task_id}/pause")
}

/// 生成恢复单任务路径（客户端用）。
#[must_use]
pub fn task_continue_path(task_id: &str) -> String {
    format!("{API_TASKS}/{task_id}/continue")
}
