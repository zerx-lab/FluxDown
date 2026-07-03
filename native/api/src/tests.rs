//! 集成测试：真实 TCP 连接 + `serve_on` + [`MockHost`]，黑盒验证 axum 服务器的
//! HTTP 契约（路由、鉴权、子开关、JSON-RPC 兼容层）。
//!
//! 覆盖语义迁移自旧 `native/hub/src/http_takeover.rs` 测试套件（见
//! `git show HEAD:native/hub/src/http_takeover.rs`），改为对新
//! `fluxdown_api`（axum 0.8 + [`ApiHost`] 抽象）的端到端验证：不再手写 HTTP 解析，
//! 而是用最小的原始 TCP 客户端发真实请求、按 `Content-Length` 精确读取响应体
//! （不依赖 `Connection: close`，与 keep-alive 无关，杜绝读取挂死）。

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::routes;
use crate::server::{self, ApiServerConfig};
use crate::service::{ApiError, ApiHost};
use crate::types::{CreateTaskRequest, DownloadRequest, QueueDto, TaskDto};

// ---------------------------------------------------------------------------
// MockHost：记录调用、返回可配置结果
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockHostInner {
    tasks: Vec<TaskDto>,
    queues: Vec<QueueDto>,
    submitted: Vec<DownloadRequest>,
    created: Vec<CreateTaskRequest>,
    next_task_id: String,
    deleted: Vec<(String, bool)>,
    paused_ids: Vec<String>,
    continued_ids: Vec<String>,
    pause_all_calls: u32,
    continue_all_calls: u32,
}

#[derive(Default)]
struct MockHost(Mutex<MockHostInner>);

impl MockHost {
    fn new() -> Self {
        Self(Mutex::new(MockHostInner {
            next_task_id: "mock-task-1".to_string(),
            ..Default::default()
        }))
    }

    /// 消费 `self` 是为了保证只在 `Arc` 包装前（独占所有权阶段）配置初始数据，
    /// 避免测试代码需要处理跨线程可变性。
    fn with_tasks(mut self, tasks: Vec<TaskDto>) -> Self {
        self.0.get_mut().unwrap().tasks = tasks;
        self
    }

    fn submitted(&self) -> Vec<DownloadRequest> {
        self.0.lock().unwrap().submitted.clone()
    }

    fn created(&self) -> Vec<CreateTaskRequest> {
        self.0.lock().unwrap().created.clone()
    }

    fn deleted(&self) -> Vec<(String, bool)> {
        self.0.lock().unwrap().deleted.clone()
    }

    fn paused_ids(&self) -> Vec<String> {
        self.0.lock().unwrap().paused_ids.clone()
    }

    fn continued_ids(&self) -> Vec<String> {
        self.0.lock().unwrap().continued_ids.clone()
    }

    fn pause_all_calls(&self) -> u32 {
        self.0.lock().unwrap().pause_all_calls
    }

    fn continue_all_calls(&self) -> u32 {
        self.0.lock().unwrap().continue_all_calls
    }
}

#[async_trait]
impl ApiHost for MockHost {
    async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError> {
        Ok(self.0.lock().unwrap().tasks.clone())
    }

    async fn get_task(&self, task_id: &str) -> Result<Option<TaskDto>, ApiError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|t| t.task_id == task_id)
            .cloned())
    }

    async fn create_task(&self, req: CreateTaskRequest) -> Result<String, ApiError> {
        let mut inner = self.0.lock().unwrap();
        let id = inner.next_task_id.clone();
        inner.created.push(req);
        Ok(id)
    }

    async fn delete_task(&self, task_id: &str, delete_files: bool) -> Result<(), ApiError> {
        self.0
            .lock()
            .unwrap()
            .deleted
            .push((task_id.to_string(), delete_files));
        Ok(())
    }

    async fn pause_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.0.lock().unwrap().paused_ids.push(task_id.to_string());
        Ok(())
    }

    async fn continue_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.0
            .lock()
            .unwrap()
            .continued_ids
            .push(task_id.to_string());
        Ok(())
    }

    async fn pause_all(&self) -> Result<(), ApiError> {
        self.0.lock().unwrap().pause_all_calls += 1;
        Ok(())
    }

    async fn continue_all(&self) -> Result<(), ApiError> {
        self.0.lock().unwrap().continue_all_calls += 1;
        Ok(())
    }

    async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
        Ok(self.0.lock().unwrap().queues.clone())
    }

    async fn submit_external(&self, req: DownloadRequest) -> Result<(), ApiError> {
        self.0.lock().unwrap().submitted.push(req);
        Ok(())
    }
}

fn sample_task(id: &str, status: i32) -> TaskDto {
    TaskDto {
        task_id: id.to_string(),
        url: format!("https://example.com/{id}.zip"),
        file_name: format!("{id}.zip"),
        save_dir: "/tmp".to_string(),
        status,
        downloaded_bytes: 10,
        total_bytes: 100,
        error_message: String::new(),
        created_at: "1700000000".to_string(),
        proxy_url: String::new(),
        queue_id: String::new(),
        checksum: String::new(),
    }
}

// ---------------------------------------------------------------------------
// 测试服务器 + 原始 HTTP 客户端
// ---------------------------------------------------------------------------

struct TestServer {
    addr: SocketAddr,
    host: Arc<MockHost>,
    cancel: CancellationToken,
}

impl TestServer {
    /// 绑定临时端口、跑 `serve_on`，返回可发请求 + 检查 `MockHost` 调用记录的句柄。
    async fn start(host: MockHost, mutate: impl FnOnce(&mut ApiServerConfig)) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut config = ApiServerConfig::from_config_map(&HashMap::new(), "9.9.9-test");
        config.port = addr.port();
        mutate(&mut config);
        let host = Arc::new(host);
        let cancel = CancellationToken::new();
        tokio::spawn(server::serve_on(
            listener,
            host.clone(),
            config,
            cancel.clone(),
        ));
        Self { addr, host, cancel }
    }

    async fn send(&self, raw_request: &str) -> RawResponse {
        send_raw(self.addr, raw_request).await
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct RawResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

impl RawResponse {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("invalid json body: {e}\nbody={}", self.body))
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// 发一个完整原始 HTTP/1.1 请求，按响应头里的 `Content-Length` 精确读取响应体。
///
/// 不依赖服务端是否关闭连接（`axum::serve` 默认 keep-alive），因此不会因为
/// 「服务器没主动关连接」而挂死在 `read_to_end` 上。
async fn send_raw(addr: SocketAddr, raw_request: &str) -> RawResponse {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(raw_request.as_bytes()).await.unwrap();

    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0, "connection closed before headers were complete");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end - 4]).into_owned();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk).await.unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body_end = (header_end + content_length).min(buf.len());
    let body = String::from_utf8_lossy(&buf[header_end..body_end]).into_owned();

    RawResponse {
        status,
        headers,
        body,
    }
}

fn request(method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> String {
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    req
}

// ---------------------------------------------------------------------------
// 探活
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ping_returns_200_without_auth() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let resp = server.send(&request("GET", routes::PING, &[], "")).await;
    assert_eq!(resp.status, 200);
    let json = resp.json();
    assert_eq!(json["app"], "FluxDown");
    assert_eq!(json["version"], "9.9.9-test");
    assert_eq!(json["message"], "pong");
}

// ---------------------------------------------------------------------------
// 脚本接管端点
// ---------------------------------------------------------------------------

#[tokio::test]
async fn download_without_client_header_returns_403() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body = json!({"url": "https://evil.example/x.zip"}).to_string();
    let resp = server
        .send(&request(
            "POST",
            routes::DOWNLOAD,
            &[("Content-Type", "application/json")],
            &body,
        ))
        .await;
    assert_eq!(resp.status, 403);
    assert!(server.host.submitted().is_empty());
}

#[tokio::test]
async fn download_with_client_header_submits_without_token() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body = json!({
        "url": "https://example.com/f.zip",
        "filename": "f.zip",
        "cookies": "a=b",
    })
    .to_string();
    let resp = server
        .send(&request(
            "POST",
            routes::DOWNLOAD,
            &[
                ("X-FluxDown-Client", "userscript"),
                ("Content-Type", "application/json"),
            ],
            &body,
        ))
        .await;
    assert_eq!(resp.status, 200);
    let submitted = server.host.submitted();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].url, "https://example.com/f.zip");
    assert_eq!(submitted[0].filename, "f.zip");
    assert_eq!(submitted[0].cookies, "a=b");
}

#[tokio::test]
async fn download_wrong_token_returns_401() {
    let server = TestServer::start(MockHost::new(), |c| c.token = "S3CRET".to_string()).await;
    let body = json!({"url": "https://example.com/x.zip"}).to_string();
    let resp = server
        .send(&request(
            "POST",
            routes::DOWNLOAD,
            &[
                ("X-FluxDown-Client", "userscript"),
                ("X-FluxDown-Token", "wrong"),
            ],
            &body,
        ))
        .await;
    assert_eq!(resp.status, 401);
    assert!(server.host.submitted().is_empty());
}

#[tokio::test]
async fn download_batch_joins_urls_and_submits() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body = json!({
        "urls": ["https://a/1.zip", "https://b/2.zip"],
        "referrer": "https://p/",
    })
    .to_string();
    let resp = server
        .send(&request(
            "POST",
            routes::DOWNLOAD_BATCH,
            &[("X-FluxDown-Client", "userscript")],
            &body,
        ))
        .await;
    assert_eq!(resp.status, 200);
    let submitted = server.host.submitted();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].url, "https://a/1.zip\nhttps://b/2.zip");
    assert_eq!(submitted[0].referrer, "https://p/");
}

// ---------------------------------------------------------------------------
// aria2 JSON-RPC 兼容端点
// ---------------------------------------------------------------------------

#[tokio::test]
async fn jsonrpc_add_uri_returns_gid_and_submits() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body = json!({
        "jsonrpc": "2.0", "id": "1", "method": "aria2.addUri",
        "params": [
            ["https://a.com/v.mp4"],
            {"out": "v.mp4", "header": ["Cookie: s=1", "Referer: https://a.com/"]}
        ]
    })
    .to_string();
    let resp = server
        .send(&request(
            "POST",
            routes::JSONRPC,
            &[("Content-Type", "application/json")],
            &body,
        ))
        .await;
    assert_eq!(resp.status, 200);
    assert!(resp.json()["result"].is_string());
    let submitted = server.host.submitted();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].url, "https://a.com/v.mp4");
    assert_eq!(submitted[0].filename, "v.mp4");
    assert_eq!(submitted[0].cookies, "s=1");
    assert_eq!(submitted[0].referrer, "https://a.com/");
}

#[tokio::test]
async fn jsonrpc_token_param_prefix_authenticates() {
    let server = TestServer::start(MockHost::new(), |c| c.token = "S".to_string()).await;
    let body = json!({
        "jsonrpc": "2.0", "id": "1", "method": "aria2.addUri",
        "params": ["token:S", ["https://a.com/f.zip"]]
    })
    .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert!(resp.json()["result"].is_string());
    assert_eq!(server.host.submitted().len(), 1);
}

#[tokio::test]
async fn jsonrpc_wrong_token_param_returns_error_code_1() {
    let server = TestServer::start(MockHost::new(), |c| c.token = "S".to_string()).await;
    let body = json!({
        "jsonrpc": "2.0", "id": "1", "method": "aria2.addUri",
        "params": ["token:WRONG", ["https://a.com/f.zip"]]
    })
    .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert_eq!(resp.json()["error"]["code"], 1);
    assert!(server.host.submitted().is_empty());
}

#[tokio::test]
async fn jsonrpc_batch_array_returns_equal_length_results() {
    // gofile-enhanced 等脚本一次 POST 多个 JSON-RPC 对象的实际行为。
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body = json!([
        {"jsonrpc": "2.0", "id": "a", "method": "aria2.addUri", "params": [["https://a/1.zip"]]},
        {"jsonrpc": "2.0", "id": "b", "method": "aria2.addUri", "params": [["https://b/2.zip"]]}
    ])
    .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert_eq!(resp.status, 200);
    let arr = resp.json();
    assert_eq!(arr.as_array().unwrap().len(), 2);
    let mut submitted_urls: Vec<String> =
        server.host.submitted().into_iter().map(|d| d.url).collect();
    submitted_urls.sort();
    assert_eq!(submitted_urls, ["https://a/1.zip", "https://b/2.zip"]);
}

#[tokio::test]
async fn jsonrpc_system_multicall_wraps_success_in_single_element_array() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "system.multicall",
        "params": [[
            {"methodName": "aria2.addUri", "params": [["https://a/1.zip"]]},
            {"methodName": "aria2.getVersion", "params": []}
        ]]
    })
    .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    let json = resp.json();
    let results = json["result"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    // addUri 成功 -> 单元素数组包 gid 字符串。
    assert!(results[0].is_array());
    assert!(results[0][0].is_string());
    // getVersion 成功 -> 单元素数组包 version 对象。
    assert!(results[1][0]["version"].is_string());
    assert_eq!(server.host.submitted()[0].url, "https://a/1.zip");
}

#[tokio::test]
async fn jsonrpc_unknown_method_returns_dash32601() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body =
        json!({"jsonrpc": "2.0", "id": 1, "method": "aria2.removeXyz", "params": []}).to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert_eq!(resp.json()["error"]["code"], -32601);
}

#[tokio::test]
async fn jsonrpc_non_json_body_returns_dash32700() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], "not json at all"))
        .await;
    assert_eq!(resp.json()["error"]["code"], -32700);
}

#[tokio::test]
async fn jsonrpc_processes_request_without_content_type_header() {
    // 回归：/jsonrpc 用 `Bytes` 提取器（不是 `Json`），完全没有 Content-Type 头
    // 的请求也必须被正常处理 —— 兼容不带 application/json 头的 aria2 风格脚本。
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "aria2.addUri",
        "params": [["https://a.com/v.mp4"]]
    })
    .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert_eq!(resp.status, 200);
    assert!(resp.json()["result"].is_string());
}

// ---------------------------------------------------------------------------
// 管理 API（/api/v1）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn management_api_returns_403_when_token_unset() {
    let server = TestServer::start(MockHost::new(), |c| c.management_enabled = true).await;
    let resp = server
        .send(&request("GET", routes::API_TASKS, &[], ""))
        .await;
    assert_eq!(resp.status, 403);
}

#[tokio::test]
async fn management_api_accepts_bearer_or_x_fluxdown_token_header() {
    let server = TestServer::start(MockHost::new(), |c| {
        c.token = "M-TOKEN".to_string();
        c.management_enabled = true;
    })
    .await;

    let bearer = server
        .send(&request(
            "GET",
            routes::API_TASKS,
            &[("Authorization", "Bearer M-TOKEN")],
            "",
        ))
        .await;
    assert_eq!(bearer.status, 200);

    let x_header = server
        .send(&request(
            "GET",
            routes::API_TASKS,
            &[("X-FluxDown-Token", "M-TOKEN")],
            "",
        ))
        .await;
    assert_eq!(x_header.status, 200);

    let wrong = server
        .send(&request(
            "GET",
            routes::API_TASKS,
            &[("X-FluxDown-Token", "wrong")],
            "",
        ))
        .await;
    assert_eq!(wrong.status, 401);
}

#[tokio::test]
async fn list_tasks_returns_camel_case_json_from_host() {
    let host = MockHost::new().with_tasks(vec![sample_task("t1", 1), sample_task("t2", 3)]);
    let server = TestServer::start(host, |c| {
        c.token = "T".to_string();
        c.management_enabled = true;
    })
    .await;
    let resp = server
        .send(&request(
            "GET",
            routes::API_TASKS,
            &[("X-FluxDown-Token", "T")],
            "",
        ))
        .await;
    assert_eq!(resp.status, 200);
    let arr = resp.json();
    let arr = arr.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["taskId"], "t1");
    assert_eq!(arr[0]["fileName"], "t1.zip");
    assert_eq!(arr[0]["downloadedBytes"], 10);
}

#[tokio::test]
async fn list_tasks_filters_by_status_query() {
    let host = MockHost::new().with_tasks(vec![
        sample_task("t1", 1),
        sample_task("t2", 3),
        sample_task("t3", 1),
    ]);
    let server = TestServer::start(host, |c| {
        c.token = "T".to_string();
        c.management_enabled = true;
    })
    .await;
    let resp = server
        .send(&request(
            "GET",
            &format!("{}?status=1", routes::API_TASKS),
            &[("X-FluxDown-Token", "T")],
            "",
        ))
        .await;
    let arr = resp.json();
    let ids: Vec<&str> = arr
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["taskId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["t1", "t3"]);
}

#[tokio::test]
async fn create_task_returns_task_id_from_host() {
    let server = TestServer::start(MockHost::new(), |c| {
        c.token = "T".to_string();
        c.management_enabled = true;
    })
    .await;
    let body = json!({"url": "https://example.com/f.zip", "segments": 4}).to_string();
    let resp = server
        .send(&request(
            "POST",
            routes::API_TASKS,
            &[("X-FluxDown-Token", "T")],
            &body,
        ))
        .await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json()["taskId"], "mock-task-1");
    let created = server.host.created();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].url, "https://example.com/f.zip");
    assert_eq!(created[0].segments, 4);
}

#[tokio::test]
async fn create_task_empty_url_returns_400() {
    let server = TestServer::start(MockHost::new(), |c| {
        c.token = "T".to_string();
        c.management_enabled = true;
    })
    .await;
    let body = json!({"url": "   "}).to_string();
    let resp = server
        .send(&request(
            "POST",
            routes::API_TASKS,
            &[("X-FluxDown-Token", "T")],
            &body,
        ))
        .await;
    assert_eq!(resp.status, 400);
    assert!(server.host.created().is_empty());
}

#[tokio::test]
async fn get_task_not_found_returns_404() {
    let server = TestServer::start(MockHost::new(), |c| {
        c.token = "T".to_string();
        c.management_enabled = true;
    })
    .await;
    let resp = server
        .send(&request(
            "GET",
            &routes::task_path("missing"),
            &[("X-FluxDown-Token", "T")],
            "",
        ))
        .await;
    assert_eq!(resp.status, 404);
}

#[tokio::test]
async fn delete_task_passes_delete_files_flag_to_host() {
    let host = MockHost::new().with_tasks(vec![sample_task("t1", 1)]);
    let server = TestServer::start(host, |c| {
        c.token = "T".to_string();
        c.management_enabled = true;
    })
    .await;
    let resp = server
        .send(&request(
            "DELETE",
            &format!("{}?deleteFiles=true", routes::task_path("t1")),
            &[("X-FluxDown-Token", "T")],
            "",
        ))
        .await;
    assert_eq!(resp.status, 200);
    assert_eq!(server.host.deleted(), vec![("t1".to_string(), true)]);
}

#[tokio::test]
async fn pause_continue_single_task_by_id() {
    let server = TestServer::start(MockHost::new(), |c| {
        c.token = "T".to_string();
        c.management_enabled = true;
    })
    .await;
    let pause_resp = server
        .send(&request(
            "PUT",
            &routes::task_pause_path("t1"),
            &[("X-FluxDown-Token", "T")],
            "",
        ))
        .await;
    assert_eq!(pause_resp.status, 200);
    let continue_resp = server
        .send(&request(
            "PUT",
            &routes::task_continue_path("t1"),
            &[("X-FluxDown-Token", "T")],
            "",
        ))
        .await;
    assert_eq!(continue_resp.status, 200);
    assert_eq!(server.host.paused_ids(), vec!["t1".to_string()]);
    assert_eq!(server.host.continued_ids(), vec!["t1".to_string()]);
}

#[tokio::test]
async fn pause_continue_all_static_route_not_swallowed_by_id_route() {
    let server = TestServer::start(MockHost::new(), |c| {
        c.token = "T".to_string();
        c.management_enabled = true;
    })
    .await;
    let pause_resp = server
        .send(&request(
            "PUT",
            routes::API_TASKS_PAUSE,
            &[("X-FluxDown-Token", "T")],
            "",
        ))
        .await;
    assert_eq!(pause_resp.status, 200);
    let continue_resp = server
        .send(&request(
            "PUT",
            routes::API_TASKS_CONTINUE,
            &[("X-FluxDown-Token", "T")],
            "",
        ))
        .await;
    assert_eq!(continue_resp.status, 200);
    assert_eq!(server.host.pause_all_calls(), 1);
    assert_eq!(server.host.continue_all_calls(), 1);
    // 关键回归点：静态段 `/tasks/pause`、`/tasks/continue` 不能被参数路由
    // `/tasks/{id}` 误吞成 pause_task("pause")/continue_task("continue")。
    assert!(server.host.paused_ids().is_empty());
    assert!(server.host.continued_ids().is_empty());
}

// ---------------------------------------------------------------------------
// 子开关
// ---------------------------------------------------------------------------

#[tokio::test]
async fn takeover_disabled_returns_404_but_ping_still_ok() {
    let server = TestServer::start(MockHost::new(), |c| c.takeover_enabled = false).await;
    let download_resp = server
        .send(&request(
            "POST",
            routes::DOWNLOAD,
            &[("X-FluxDown-Client", "userscript")],
            "{}",
        ))
        .await;
    assert_eq!(download_resp.status, 404);
    let ping_resp = server.send(&request("GET", routes::PING, &[], "")).await;
    assert_eq!(ping_resp.status, 200);
}

#[tokio::test]
async fn management_disabled_returns_404_for_tasks() {
    // `management_enabled` 默认已是 false；这里显式声明以固定契约，
    // 不依赖 `ApiServerConfig` 的默认值本身。
    let server = TestServer::start(MockHost::new(), |c| c.management_enabled = false).await;
    let resp = server
        .send(&request("GET", routes::API_TASKS, &[], ""))
        .await;
    assert_eq!(resp.status, 404);
}

// ---------------------------------------------------------------------------
// OPTIONS 预检
// ---------------------------------------------------------------------------

#[tokio::test]
async fn options_preflight_returns_204_without_cors_header() {
    let server = TestServer::start(MockHost::new(), |c| c.management_enabled = true).await;
    for path in [routes::DOWNLOAD, routes::API_TASKS, "/nonexistent"] {
        let resp = server.send(&request("OPTIONS", path, &[], "")).await;
        assert_eq!(resp.status, 204, "path={path}");
        assert!(
            !resp.headers.contains_key("access-control-allow-origin"),
            "path={path} must not carry an Access-Control-Allow-Origin header"
        );
    }
}

// ---------------------------------------------------------------------------
// OpenAPI 规范端点
// ---------------------------------------------------------------------------

/// 管理 API 开启时，/api/v1/openapi.json 无 token 也可读（纯接口描述，不含数据）。
#[tokio::test]
async fn openapi_spec_served_without_token_when_management_enabled() {
    let server = TestServer::start(MockHost::default(), |c| {
        c.management_enabled = true;
        c.token = "secret".to_string();
    })
    .await;
    let resp = server
        .send(&request("GET", routes::API_OPENAPI, &[], ""))
        .await;
    assert_eq!(resp.status, 200);
    let spec = resp.json();
    assert_eq!(spec["openapi"], "3.1.0");
    assert!(spec["paths"].get(routes::API_TASKS).is_some());
}

/// 管理 API 关闭时，规范端点随组下线。
#[tokio::test]
async fn openapi_spec_404_when_management_disabled() {
    let server = TestServer::start(MockHost::default(), |_| {}).await;
    let resp = server
        .send(&request("GET", routes::API_OPENAPI, &[], ""))
        .await;
    assert_eq!(resp.status, 404);
}
