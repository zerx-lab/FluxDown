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
/// MCP（Model Context Protocol）兼容端点。
pub const MCP: &str = "/mcp";

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

/// 插件集合（GET 列表）。
pub const API_PLUGINS: &str = "/api/v1/plugins";
/// 安装插件（POST zip bytes，≤10MB）。
pub const API_PLUGINS_INSTALL: &str = "/api/v1/plugins/install";
/// 安装 dev 插件（POST {dirPath}）。
pub const API_PLUGINS_INSTALL_DEV: &str = "/api/v1/plugins/install-dev";
/// 单插件启用开关（PUT {enabled}）。
pub const API_PLUGIN_ENABLED: &str = "/api/v1/plugins/{identity}/enabled";
/// 单插件设置（PUT {key:value}）。
pub const API_PLUGIN_SETTINGS: &str = "/api/v1/plugins/{identity}/settings";
/// 卸载单插件（DELETE）。
pub const API_PLUGIN: &str = "/api/v1/plugins/{identity}";
/// 任务级逃生舱：忽略插件重试，按原始链接重跑（POST）。
pub const API_TASK_IGNORE_PLUGIN_RETRY: &str = "/api/v1/tasks/{id}/ignore-plugin-retry";

/// 去中心化插件市场：拉取索引（GET）。
pub const API_MARKET: &str = "/api/v1/market";
/// 从市场安装（POST {pluginId}）。
pub const API_MARKET_INSTALL: &str = "/api/v1/market/install";

/// 前置预解析清单（POST，只读、不建任务）。
pub const API_RESOLVE_PREVIEW: &str = "/api/v1/resolve/preview";
/// 任务组集合（GET 列表 / POST 建组+子任务）。
pub const API_GROUPS: &str = "/api/v1/groups";
/// 单任务组（DELETE，query `deleteFiles=true` 可选同时删文件）。
pub const API_GROUP: &str = "/api/v1/groups/{id}";
/// 暂停组内成员（PUT）。
pub const API_GROUP_PAUSE: &str = "/api/v1/groups/{id}/pause";
/// 恢复组内成员（PUT）。
pub const API_GROUP_CONTINUE: &str = "/api/v1/groups/{id}/continue";

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

/// 生成单任务组路径（客户端用）。
#[must_use]
pub fn group_path(group_id: &str) -> String {
    format!("{API_GROUPS}/{group_id}")
}

/// 生成暂停任务组路径（客户端用）。
#[must_use]
pub fn group_pause_path(group_id: &str) -> String {
    format!("{API_GROUPS}/{group_id}/pause")
}

/// 生成恢复任务组路径（客户端用）。
#[must_use]
pub fn group_continue_path(group_id: &str) -> String {
    format!("{API_GROUPS}/{group_id}/continue")
}
