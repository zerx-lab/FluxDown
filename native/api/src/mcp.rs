//! MCP（Model Context Protocol）兼容端点（`POST /mcp`）。
//!
//! 让 AI 客户端（Claude Desktop / Cursor / Cline 等）把 FluxDown 当作 MCP
//! server：以自然语言驱动「新建下载 / 查询任务 / 暂停恢复 / 删除 / 列队列」。
//!
//! # 传输层
//!
//! 采用 MCP 官方 **Streamable HTTP** 传输的无状态子集（规范 2025-06-18）：
//! 客户端把 JSON-RPC 2.0 请求 `POST` 到单一端点，服务端对**请求**返回
//! `application/json` 单条响应，对**通知**（无 `id`）返回 `202 Accepted` 空体。
//! 本 server 是纯请求/响应工具服务，不推送服务端消息，故不需要 SSE 流，
//! 也不维护会话（无 `Mcp-Session-Id`）——每个请求自包含，与规范
//! 2026-07-28 的无状态方向一致。
//!
//! # 鉴权
//!
//! 由 HTTP 层用 [`crate::auth::check_management_auth`] 强制 token
//! （`Authorization: Bearer <token>` 或 `X-FluxDown-Token`）。规范允许内部
//! 部署用静态 Bearer token 代替 OAuth 2.1，与管理 API 同一把 token。
//!
//! # 与 [`crate::jsonrpc`] 的关系
//!
//! 两者都是「JSON-RPC over HTTP，薄薄一层，全走 [`ApiHost`]」的同构实现：
//! `jsonrpc` 面向 aria2 客户端（方法名 `aria2.*`），`mcp` 面向 AI 客户端
//! （方法名 `tools/call` 等）。不共享方法集，各自独立派发。

use serde_json::{Value, json};

use crate::service::ApiHost;
use crate::types::CreateTaskRequest;

/// 服务端声明支持的 MCP 协议版本（初始化时若客户端未指定则回退到此值）。
const PROTOCOL_VERSION: &str = "2025-06-18";

/// 处理一个 `/mcp` 请求体。
///
/// 返回 `Some(response)` 表示应以 `200 application/json` 回该 JSON-RPC 响应；
/// 返回 `None` 表示这是一条**通知**（无 `id`），应回 `202 Accepted` 空体。
///
/// `app_version` 注入 `initialize` 的 `serverInfo.version`。
///
/// 鉴权由 HTTP 层在调用本函数前完成（token 校验失败直接 401/403，不进此函数）。
pub(crate) async fn handle_mcp(
    host: &dyn ApiHost,
    app_version: &str,
    body: &[u8],
) -> Option<Value> {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Some(rpc_err(&Value::Null, -32700, &format!("parse error: {e}"))),
    };

    let Value::Object(_) = parsed else {
        // MCP 2025-06-18 移除了 JSON-RPC 批量；只接受单个对象。
        return Some(rpc_err(
            &Value::Null,
            -32600,
            "invalid request: expected a single object",
        ));
    };

    // 无 `id` = 通知（如 notifications/initialized）：`?` 早退 None，不产生响应体。
    let id = parsed.get("id")?.clone();
    let method = parsed.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = parsed.get("params").cloned().unwrap_or(Value::Null);

    let resp = match method {
        "initialize" => rpc_ok(&id, initialize_result(&params, app_version)),
        "ping" => rpc_ok(&id, json!({})),
        "tools/list" => rpc_ok(&id, json!({ "tools": tool_definitions() })),
        "tools/call" => tools_call(&id, &params, host).await,
        other => rpc_err(&id, -32601, &format!("Method not found: {other}")),
    };
    Some(resp)
}

/// `initialize` 结果：回显客户端请求的协议版本（不识别则回退默认值），
/// 声明仅提供 `tools` 能力。
fn initialize_result(params: &Value, app_version: &str) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "FluxDown", "version": app_version },
    })
}

/// 全部工具的 JSON Schema 定义（`tools/list` 响应）。
fn tool_definitions() -> Value {
    // 复用的字符串参数 schema 片段。
    let str_prop = |desc: &str| json!({ "type": "string", "description": desc });
    json!([
        {
            "name": "download_add",
            "description": "新建一个下载任务。支持 HTTP/HTTPS/FTP/磁力链接/BitTorrent。返回新任务 ID。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": str_prop("要下载的 URL、磁力链接或 torrent 地址（必填）。"),
                    "fileName": str_prop("保存的文件名（空 = 从 URL/Content-Disposition 推断）。"),
                    "saveDir": str_prop("保存目录绝对路径（空 = 全局默认目录）。"),
                    "segments": { "type": "integer", "description": "分段线程数（0 = 按文件大小自动决定）。" },
                    "proxyUrl": str_prop("单任务代理 URL（空 = 全局代理）。"),
                    "cookies": str_prop("Cookie 字符串。"),
                    "referrer": str_prop("Referer 头。"),
                    "userAgent": str_prop("User-Agent（空 = 全局 UA）。"),
                    "queueId": str_prop("命名队列 ID（空 = 默认队列）。"),
                    "checksum": str_prop("校验和，格式 algo=hexhash（空 = 跳过校验）。")
                },
                "required": ["url"]
            }
        },
        {
            "name": "download_list",
            "description": "列出下载任务，可按状态过滤。返回任务数组（含进度、速度、状态等）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["all", "pending", "downloading", "paused", "completed", "error", "preparing"],
                        "description": "按状态过滤（省略或 all = 全部）。"
                    }
                }
            }
        },
        {
            "name": "download_get",
            "description": "按任务 ID 查询单个任务的详细信息。",
            "inputSchema": {
                "type": "object",
                "properties": { "taskId": str_prop("任务 ID（必填）。") },
                "required": ["taskId"]
            }
        },
        {
            "name": "download_pause",
            "description": "暂停指定任务。",
            "inputSchema": {
                "type": "object",
                "properties": { "taskId": str_prop("任务 ID（必填）。") },
                "required": ["taskId"]
            }
        },
        {
            "name": "download_resume",
            "description": "恢复指定的已暂停任务。",
            "inputSchema": {
                "type": "object",
                "properties": { "taskId": str_prop("任务 ID（必填）。") },
                "required": ["taskId"]
            }
        },
        {
            "name": "download_pause_all",
            "description": "暂停全部活跃任务（pending / downloading / preparing）。",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "download_resume_all",
            "description": "恢复全部已暂停任务。",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "download_remove",
            "description": "删除指定任务，可选同时删除已下载的磁盘文件。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "taskId": str_prop("任务 ID（必填）。"),
                    "deleteFiles": { "type": "boolean", "description": "是否同时删除磁盘文件（默认 false）。" }
                },
                "required": ["taskId"]
            }
        },
        {
            "name": "queue_list",
            "description": "列出全部命名队列及其配置（并发数、限速、默认目录等）。",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

/// 派发 `tools/call`：解析 `{ name, arguments }`，执行工具，包装为 MCP 工具结果。
async fn tools_call(id: &Value, params: &Value, host: &dyn ApiHost) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    match call_tool(name, &args, host).await {
        Ok(payload) => rpc_ok(id, tool_result(&payload, false)),
        Err(msg) => rpc_ok(id, tool_result(&Value::String(msg), true)),
    }
}

/// 执行单个工具，返回结构化载荷（`Ok`）或错误消息（`Err`）。
///
/// 载荷会被 [`tool_result`] 序列化为文本内容返回给客户端。
async fn call_tool(name: &str, args: &Value, host: &dyn ApiHost) -> Result<Value, String> {
    match name {
        "download_add" => {
            // args 与 CreateTaskRequest 同为 camelCase，直接反序列化。
            let req: CreateTaskRequest = serde_json::from_value(args.clone())
                .map_err(|e| format!("invalid arguments: {e}"))?;
            if req.url.trim().is_empty() {
                return Err("url is required".to_string());
            }
            let task_id = host.create_task(req).await.map_err(|e| e.to_string())?;
            Ok(json!({ "taskId": task_id }))
        }
        "download_list" => {
            let mut tasks = host.list_tasks().await.map_err(|e| e.to_string())?;
            if let Some(code) = args
                .get("status")
                .and_then(|v| v.as_str())
                .and_then(status_code)
            {
                tasks.retain(|t| t.status == code);
            }
            Ok(json!({ "tasks": tasks, "count": tasks.len() }))
        }
        "download_get" => {
            let task_id = require_task_id(args)?;
            match host.get_task(task_id).await.map_err(|e| e.to_string())? {
                Some(task) => Ok(json!({ "task": task })),
                None => Err(format!("task not found: {task_id}")),
            }
        }
        "download_pause" => {
            let task_id = require_task_id(args)?;
            host.pause_task(task_id).await.map_err(|e| e.to_string())?;
            Ok(json!({ "paused": task_id }))
        }
        "download_resume" => {
            let task_id = require_task_id(args)?;
            host.continue_task(task_id)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "resumed": task_id }))
        }
        "download_pause_all" => {
            host.pause_all().await.map_err(|e| e.to_string())?;
            Ok(json!({ "pausedAll": true }))
        }
        "download_resume_all" => {
            host.continue_all().await.map_err(|e| e.to_string())?;
            Ok(json!({ "resumedAll": true }))
        }
        "download_remove" => {
            let task_id = require_task_id(args)?;
            let delete_files = args
                .get("deleteFiles")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            host.delete_task(task_id, delete_files)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "removed": task_id, "deletedFiles": delete_files }))
        }
        "queue_list" => {
            let queues = host.list_queues().await.map_err(|e| e.to_string())?;
            Ok(json!({ "queues": queues }))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

/// 从 `arguments` 取出非空 `taskId`。
fn require_task_id(args: &Value) -> Result<&str, String> {
    match args.get("taskId").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => Ok(id),
        _ => Err("taskId is required".to_string()),
    }
}

/// 状态名 → 状态码（未知名返回 `None`，即不过滤）。
/// `all` 显式返回 `None`（不过滤）。
fn status_code(name: &str) -> Option<i32> {
    match name {
        "pending" => Some(0),
        "downloading" => Some(1),
        "paused" => Some(2),
        "completed" => Some(3),
        "error" => Some(4),
        "preparing" => Some(5),
        _ => None,
    }
}

/// 包装为 MCP 工具结果：结构化载荷序列化为 JSON 文本放入 `content`。
///
/// 用文本内容（而非仅 `structuredContent`）保证所有 MCP 客户端都能显示。
fn tool_result(payload: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error,
    })
}

fn rpc_ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::service::{ApiError, ApiHost};
    use crate::types::{DownloadRequest, QueueDto, TaskDto};
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// 记录调用的假宿主。
    #[derive(Default)]
    struct FakeHost {
        tasks: Vec<TaskDto>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeHost {
        fn record(&self, s: &str) {
            self.calls.lock().unwrap().push(s.to_string());
        }
    }

    fn sample_task(id: &str, status: i32) -> TaskDto {
        TaskDto {
            task_id: id.to_string(),
            url: "https://example.com/f.zip".to_string(),
            file_name: "f.zip".to_string(),
            save_dir: "/tmp".to_string(),
            status,
            downloaded_bytes: 0,
            total_bytes: 0,
            error_message: String::new(),
            created_at: "1700000000".to_string(),
            proxy_url: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            file_missing: false,
            completed_at: String::new(),
            referrer: String::new(),
        }
    }

    #[async_trait]
    impl ApiHost for FakeHost {
        async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError> {
            Ok(self.tasks.clone())
        }
        async fn get_task(&self, id: &str) -> Result<Option<TaskDto>, ApiError> {
            Ok(self.tasks.iter().find(|t| t.task_id == id).cloned())
        }
        async fn create_task(&self, req: CreateTaskRequest) -> Result<String, ApiError> {
            self.record(&format!("create:{}:{}", req.url, req.segments));
            Ok("new-task-id".to_string())
        }
        async fn delete_task(&self, id: &str, files: bool) -> Result<(), ApiError> {
            self.record(&format!("delete:{id}:{files}"));
            Ok(())
        }
        async fn pause_task(&self, id: &str) -> Result<(), ApiError> {
            self.record(&format!("pause:{id}"));
            Ok(())
        }
        async fn continue_task(&self, id: &str) -> Result<(), ApiError> {
            self.record(&format!("continue:{id}"));
            Ok(())
        }
        async fn pause_all(&self) -> Result<(), ApiError> {
            self.record("pause_all");
            Ok(())
        }
        async fn continue_all(&self) -> Result<(), ApiError> {
            self.record("continue_all");
            Ok(())
        }
        async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
            Ok(vec![])
        }
        async fn submit_external(&self, _req: DownloadRequest) -> Result<(), ApiError> {
            Ok(())
        }
    }

    async fn call(host: &dyn ApiHost, body: &str) -> Option<Value> {
        handle_mcp(host, "9.9.9", body.as_bytes()).await
    }

    #[tokio::test]
    async fn initialize_echoes_version_and_declares_tools() {
        let host = FakeHost::default();
        let resp = call(
            &host,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
        )
        .await
        .unwrap();
        let result = &resp["result"];
        assert_eq!(result["protocolVersion"], "2025-03-26");
        assert_eq!(result["serverInfo"]["name"], "FluxDown");
        assert_eq!(result["serverInfo"]["version"], "9.9.9");
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn initialize_falls_back_to_default_version() {
        let host = FakeHost::default();
        let resp = call(
            &host,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn notification_yields_no_response() {
        let host = FakeHost::default();
        let resp = call(
            &host,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn tools_list_returns_nine_tools() {
        let host = FakeHost::default();
        let resp = call(&host, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .await
            .unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 9);
        assert!(tools.iter().any(|t| t["name"] == "download_add"));
        // 每个工具都必须带 inputSchema。
        assert!(tools.iter().all(|t| t["inputSchema"].is_object()));
    }

    #[tokio::test]
    async fn tools_call_download_add_forwards_to_host() {
        let host = FakeHost::default();
        let resp = call(
            &host,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"download_add","arguments":{"url":"https://x.com/a.bin","segments":8}}}"#,
        )
        .await
        .unwrap();
        let result = &resp["result"];
        assert_eq!(result["isError"], false);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("new-task-id"));
        assert_eq!(
            host.calls.lock().unwrap()[0],
            "create:https://x.com/a.bin:8"
        );
    }

    #[tokio::test]
    async fn tools_call_download_add_missing_url_is_error() {
        let host = FakeHost::default();
        let resp = call(
            &host,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"download_add","arguments":{}}}"#,
        )
        .await
        .unwrap();
        // 协议层成功（result 存在），业务错误经 isError 表达。
        assert_eq!(resp["result"]["isError"], true);
        assert!(
            resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("url")
        );
    }

    #[tokio::test]
    async fn tools_call_list_filters_by_status() {
        let host = FakeHost {
            tasks: vec![
                sample_task("a", 1),
                sample_task("b", 2),
                sample_task("c", 1),
            ],
            calls: Mutex::new(vec![]),
        };
        let resp = call(
            &host,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"download_list","arguments":{"status":"downloading"}}}"#,
        )
        .await
        .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert_eq!(payload["count"], 2);
    }

    #[tokio::test]
    async fn tools_call_remove_passes_delete_flag() {
        let host = FakeHost::default();
        call(
            &host,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"download_remove","arguments":{"taskId":"t1","deleteFiles":true}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(host.calls.lock().unwrap()[0], "delete:t1:true");
    }

    #[tokio::test]
    async fn unknown_tool_is_error() {
        let host = FakeHost::default();
        let resp = call(
            &host,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn unknown_method_returns_error_object() {
        let host = FakeHost::default();
        let resp = call(
            &host,
            r#"{"jsonrpc":"2.0","id":7,"method":"resources/list"}"#,
        )
        .await
        .unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn parse_error_returns_minus_32700() {
        let host = FakeHost::default();
        let resp = call(&host, "not json").await.unwrap();
        assert_eq!(resp["error"]["code"], -32700);
    }
}
