//! 集成测试：真实 TCP 连接 + `serve_on` + [`MockHost`]，黑盒验证 axum 服务器的
//! HTTP + WS 契约（路由、鉴权、子开关、JSON-RPC 兼容层、aria2 WS 通知）。
//!
//! 覆盖语义迁移自旧 `native/hub/src/http_takeover.rs` 测试套件（见
//! `git show HEAD:native/hub/src/http_takeover.rs`），改为对新
//! `fluxdown_api`（axum 0.8 + [`ApiHost`] 抽象）的端到端验证：不再手写 HTTP 解析，
//! 而是用最小的原始 TCP 客户端发真实请求、按 `Content-Length` 精确读取响应体
//! （不依赖 `Connection: close`，与 keep-alive 无关，杜绝读取挂死）。WS 部分用
//! 真实 `tokio-tungstenite` 客户端握手 + 收发帧，同样是黑盒端到端验证。

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tokio_util::sync::CancellationToken;

use crate::aria2;
use crate::routes;
use crate::server::{self, ApiServerConfig};
use crate::service::{
    ApiError, ApiHost, LiveSpeed, TaskEvent, TaskEventKind, UNKNOWN_ENDPOINT_MESSAGE,
};
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
    config: HashMap<String, String>,
    applied_config: Vec<HashMap<String, String>>,
    speeds: HashMap<String, LiveSpeed>,
}

struct MockHost {
    inner: Mutex<MockHostInner>,
    /// `aria2.onDownloadXxx` 通知广播源；`None` 模拟宿主未接线事件源（见
    /// [`Self::without_task_events`]），`/jsonrpc` WS 会话据此退化为纯双向
    /// RPC，不推送任何通知。
    events: Option<broadcast::Sender<TaskEvent>>,
}

impl Default for MockHost {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            events: Some(broadcast::channel(16).0),
        }
    }
}

impl MockHost {
    fn new() -> Self {
        Self {
            inner: Mutex::new(MockHostInner {
                next_task_id: "mock-task-1".to_string(),
                ..Default::default()
            }),
            events: Some(broadcast::channel(16).0),
        }
    }

    /// 消费 `self` 是为了保证只在 `Arc` 包装前（独占所有权阶段）配置初始数据，
    /// 避免测试代码需要处理跨线程可变性。
    fn with_tasks(mut self, tasks: Vec<TaskDto>) -> Self {
        self.inner.get_mut().unwrap().tasks = tasks;
        self
    }

    fn with_config(mut self, config: HashMap<String, String>) -> Self {
        self.inner.get_mut().unwrap().config = config;
        self
    }

    fn with_speeds(mut self, speeds: HashMap<String, LiveSpeed>) -> Self {
        self.inner.get_mut().unwrap().speeds = speeds;
        self
    }

    /// 模拟宿主未接线任务事件源：`subscribe_task_events()` 恒返回 `None`。
    fn without_task_events(mut self) -> Self {
        self.events = None;
        self
    }

    fn submitted(&self) -> Vec<DownloadRequest> {
        self.inner.lock().unwrap().submitted.clone()
    }

    fn created(&self) -> Vec<CreateTaskRequest> {
        self.inner.lock().unwrap().created.clone()
    }

    fn applied_config(&self) -> Vec<HashMap<String, String>> {
        self.inner.lock().unwrap().applied_config.clone()
    }

    fn deleted(&self) -> Vec<(String, bool)> {
        self.inner.lock().unwrap().deleted.clone()
    }

    fn paused_ids(&self) -> Vec<String> {
        self.inner.lock().unwrap().paused_ids.clone()
    }

    fn continued_ids(&self) -> Vec<String> {
        self.inner.lock().unwrap().continued_ids.clone()
    }

    fn pause_all_calls(&self) -> u32 {
        self.inner.lock().unwrap().pause_all_calls
    }

    fn continue_all_calls(&self) -> u32 {
        self.inner.lock().unwrap().continue_all_calls
    }

    /// 模拟宿主检测到一次任务状态迁移，广播对应事件。若本实例未启用事件订阅
    /// （[`Self::without_task_events`]）或当前没有任何 WS 会话已订阅，静默
    /// 丢弃——调用方需自行保证先建立 WS 连接（使订阅生效）再调用本方法。
    fn emit_event(&self, task_id: &str, kind: TaskEventKind) {
        if let Some(tx) = &self.events {
            let _ = tx.send(TaskEvent {
                task_id: task_id.to_string(),
                kind,
            });
        }
    }
}

#[async_trait]
impl ApiHost for MockHost {
    async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError> {
        Ok(self.inner.lock().unwrap().tasks.clone())
    }

    async fn get_task(&self, task_id: &str) -> Result<Option<TaskDto>, ApiError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|t| t.task_id == task_id)
            .cloned())
    }

    async fn create_task(&self, req: CreateTaskRequest) -> Result<String, ApiError> {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_task_id.clone();
        inner.created.push(req);
        Ok(id)
    }

    async fn delete_task(&self, task_id: &str, delete_files: bool) -> Result<(), ApiError> {
        self.inner
            .lock()
            .unwrap()
            .deleted
            .push((task_id.to_string(), delete_files));
        Ok(())
    }

    async fn pause_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.inner
            .lock()
            .unwrap()
            .paused_ids
            .push(task_id.to_string());
        Ok(())
    }

    async fn continue_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.inner
            .lock()
            .unwrap()
            .continued_ids
            .push(task_id.to_string());
        Ok(())
    }

    async fn pause_all(&self) -> Result<(), ApiError> {
        self.inner.lock().unwrap().pause_all_calls += 1;
        Ok(())
    }

    async fn continue_all(&self) -> Result<(), ApiError> {
        self.inner.lock().unwrap().continue_all_calls += 1;
        Ok(())
    }

    async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
        Ok(self.inner.lock().unwrap().queues.clone())
    }

    async fn submit_external(&self, req: DownloadRequest) -> Result<(), ApiError> {
        self.inner.lock().unwrap().submitted.push(req);
        Ok(())
    }

    async fn get_config(&self) -> Result<HashMap<String, String>, ApiError> {
        Ok(self.inner.lock().unwrap().config.clone())
    }

    async fn web_language(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .config
            .get("web_language")
            .cloned()
    }

    async fn apply_config(&self, changes: HashMap<String, String>) -> Result<(), ApiError> {
        self.inner.lock().unwrap().applied_config.push(changes);
        Ok(())
    }

    async fn live_speeds(&self) -> Result<HashMap<String, LiveSpeed>, ApiError> {
        Ok(self.inner.lock().unwrap().speeds.clone())
    }

    fn subscribe_task_events(&self) -> Option<broadcast::Receiver<TaskEvent>> {
        self.events.as_ref().map(broadcast::Sender::subscribe)
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
        ignore_tls_errors: false,
        file_missing: false,
        completed_at: String::new(),
        referrer: String::new(),
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

    /// 连接 `/jsonrpc` 的 WS 端点：真实 HTTP Upgrade 握手，返回可收发帧的
    /// tokio-tungstenite 客户端流。握手失败（如路由未注册）时 panic，
    /// 调用方对该场景改用 [`TestServer::ws_connect_err`]。
    async fn ws_connect(&self) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
        let (stream, resp) = connect_async(format!("ws://{}{}", self.addr, routes::JSONRPC))
            .await
            .unwrap();
        assert_eq!(resp.status(), 101);
        stream
    }

    /// 同 [`TestServer::ws_connect`]，但返回握手 `Result` 而非 panic——
    /// 用于验证「路由未注册/未升级」场景下连接被正确拒绝。
    async fn ws_connect_err(&self) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        connect_async(format!("ws://{}{}", self.addr, routes::JSONRPC))
            .await
            .map(|_| ())
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

/// 读取下一个 WS 文本帧，按 `String` 返回。非文本帧/读错误/流已结束均 panic
/// ——测试期望值明确时用它，断言失败即测试失败，不需要额外包装。
async fn ws_recv_text(ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> String {
    let msg = ws
        .next()
        .await
        .expect("ws stream ended before expected message")
        .expect("ws read error");
    msg.into_text()
        .expect("expected a text frame")
        .as_str()
        .to_string()
}

/// [`ws_recv_text`] + JSON 解析。
async fn ws_recv_json(ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Value {
    let text = ws_recv_text(ws).await;
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("invalid json frame: {e}\ntext={text}"))
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
    // 宿主未提供 Web 语言时省略该字段
    assert!(json.get("language").is_none());
}

#[tokio::test]
async fn ping_includes_web_language_when_host_provides_it() {
    let host = MockHost::new().with_config(HashMap::from([(
        "web_language".to_string(),
        "zh".to_string(),
    )]));
    let server = TestServer::start(host, |_| {}).await;
    let resp = server.send(&request("GET", routes::PING, &[], "")).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json()["language"], "zh");
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
async fn jsonrpc_add_uri_creates_task_and_returns_gid() {
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
    assert_eq!(resp.json()["result"], aria2::task_id_to_gid("mock-task-1"));
    let created = server.host.created();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].url, "https://a.com/v.mp4");
    assert_eq!(created[0].file_name, "v.mp4");
    assert_eq!(created[0].cookies, "s=1");
    assert_eq!(created[0].referrer, "https://a.com/");
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
    assert_eq!(server.host.created().len(), 1);
}

#[tokio::test]
async fn jsonrpc_wrong_token_param_returns_unauthorized() {
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
    assert_eq!(resp.json()["error"]["message"], "Unauthorized");
    assert!(server.host.created().is_empty());
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
    let mut created_urls: Vec<String> = server.host.created().into_iter().map(|d| d.url).collect();
    created_urls.sort();
    assert_eq!(created_urls, ["https://a/1.zip", "https://b/2.zip"]);
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
    assert_eq!(server.host.created()[0].url, "https://a/1.zip");
}

#[tokio::test]
async fn jsonrpc_unknown_method_returns_code_1_no_such_method() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body =
        json!({"jsonrpc": "2.0", "id": 1, "method": "aria2.removeXyz", "params": []}).to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert_eq!(resp.json()["error"]["code"], 1);
    assert_eq!(
        resp.json()["error"]["message"],
        "No such method: aria2.removeXyz"
    );
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

#[tokio::test]
async fn jsonrpc_add_torrent_forwards_torrent_b64_and_returns_gid() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "aria2.addTorrent",
        "params": ["dGVzdHRvcnJlbnQ=", [], {"out": "movie.mkv"}]
    })
    .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert_eq!(resp.json()["result"], aria2::task_id_to_gid("mock-task-1"));
    let created = server.host.created();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].url, "");
    assert_eq!(created[0].torrent_b64.as_deref(), Some("dGVzdHRvcnJlbnQ="));
    assert_eq!(created[0].file_name, "movie.mkv");
}

#[tokio::test]
async fn jsonrpc_tell_status_returns_seeded_task_fields_and_live_speed() {
    let seeded = sample_task("11112222-3333-4444-5555-666677778888", 1);
    let gid = aria2::task_id_to_gid(&seeded.task_id);
    let task_id = seeded.task_id.clone();
    let mut speeds = HashMap::new();
    speeds.insert(
        task_id,
        LiveSpeed {
            download_bps: 4096,
            upload_bps: 0,
        },
    );
    let server = TestServer::start(
        MockHost::new().with_tasks(vec![seeded]).with_speeds(speeds),
        |_| {},
    )
    .await;
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "aria2.tellStatus", "params": [gid]
    })
    .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    let json = resp.json();
    let result = &json["result"];
    assert_eq!(result["gid"], gid);
    assert_eq!(result["status"], "active");
    assert_eq!(result["totalLength"], "100");
    assert_eq!(result["completedLength"], "10");
    assert_eq!(result["dir"], "/tmp");
    assert_eq!(result["downloadSpeed"], "4096");
}

#[tokio::test]
async fn jsonrpc_change_global_option_calls_apply_config_with_mapped_keys() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "aria2.changeGlobalOption",
        "params": [{"dir": "/data", "max-overall-download-limit": "5M"}]
    })
    .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert_eq!(resp.json()["result"], "OK");
    let applied = server.host.applied_config();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].get("default_save_dir").unwrap(), "/data");
    assert_eq!(
        applied[0].get("speed_limit_bytes").unwrap(),
        &(5 * 1024 * 1024).to_string()
    );
}

#[tokio::test]
async fn jsonrpc_get_global_option_returns_mapped_and_static_defaults() {
    let mut config = HashMap::new();
    config.insert("default_save_dir".to_string(), "/dl".to_string());
    let server = TestServer::start(MockHost::new().with_config(config), |_| {}).await;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "aria2.getGlobalOption", "params": []})
        .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    let result = &resp.json()["result"];
    assert_eq!(result["dir"], "/dl");
    assert_eq!(result["max-connection-per-server"], "1");
}

#[tokio::test]
async fn jsonrpc_pause_and_remove_use_resolved_task_id_not_gid_prefix() {
    let seeded = sample_task("aaaabbbb-cccc-dddd-eeee-ffffffffffff", 1);
    let full_task_id = seeded.task_id.clone();
    let server = TestServer::start(MockHost::new().with_tasks(vec![seeded]), |_| {}).await;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "aria2.pause", "params": ["aaaabbbb"]})
        .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert!(resp.json()["result"].is_string());
    assert_eq!(server.host.paused_ids(), vec![full_task_id.clone()]);

    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "aria2.remove", "params": ["aaaabbbb"]})
        .to_string();
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert!(resp.json()["result"].is_string());
    assert_eq!(server.host.deleted(), vec![(full_task_id, false)]);
}

#[tokio::test]
async fn jsonrpc_list_methods_returns_all_36_without_token() {
    let server = TestServer::start(MockHost::new(), |c| c.token = "S".to_string()).await;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "system.listMethods", "params": []})
        .to_string();
    // 不带任何 token —— 换成需要鉴权的方法本应 401/code:1，但 listMethods 例外。
    let resp = server
        .send(&request("POST", routes::JSONRPC, &[], &body))
        .await;
    assert_eq!(resp.json()["result"].as_array().unwrap().len(), 36);
}

// ---------------------------------------------------------------------------
// aria2 WS 通知 + 双向 JSON-RPC（`GET /jsonrpc` + upgrade）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ws_get_without_upgrade_headers_returns_400() {
    // 普通 GET（没有 Connection: Upgrade / Sec-WebSocket-Key 等头）命中同一个
    // `/jsonrpc` 路由，应由 `WebSocketUpgrade` 提取器自身拒绝为 400，而不是
    // 落入 `jsonrpc_ws` handler 或被当成别的方法处理。
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let resp = server.send(&request("GET", routes::JSONRPC, &[], "")).await;
    assert_eq!(resp.status, 400);
}

#[tokio::test]
async fn ws_upgrade_rejected_when_jsonrpc_disabled() {
    // WS 与 POST 共用 `jsonrpc_enabled` 开关：关闭时路由整体不注册，
    // 握手应在 HTTP 层就被拒绝（404 unknown_endpoint），而不是先 101 再断开。
    let server = TestServer::start(MockHost::new(), |c| c.jsonrpc_enabled = false).await;
    let err = server.ws_connect_err().await.unwrap_err();
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), 404);
        }
        other => panic!("expected an HTTP rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn ws_upgrade_succeeds_when_jsonrpc_enabled() {
    // 默认开关（jsonrpc_enabled=true）下握手必须成功——`ws_connect` 内部已经
    // 断言了 101 状态码，这里额外确认后续还能正常收发（连接真的可用）。
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let mut ws = server.ws_connect().await;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "aria2.getVersion", "params": []});
    ws.send(WsMessage::text(body.to_string())).await.unwrap();
    let resp = ws_recv_json(&mut ws).await;
    assert_eq!(resp["result"]["version"], aria2::ARIA2_VERSION);
}

#[tokio::test]
async fn ws_bidirectional_rpc_matches_http_behavior() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let mut ws = server.ws_connect().await;

    let req = json!({
        "jsonrpc": "2.0", "id": "1", "method": "aria2.addUri",
        "params": [
            ["https://a.com/v.mp4"],
            {"out": "v.mp4", "header": ["Cookie: s=1", "Referer: https://a.com/"]}
        ]
    });
    ws.send(WsMessage::text(req.to_string())).await.unwrap();
    let resp = ws_recv_json(&mut ws).await;

    assert_eq!(resp["result"], aria2::task_id_to_gid("mock-task-1"));
    let created = server.host.created();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].url, "https://a.com/v.mp4");
    assert_eq!(created[0].file_name, "v.mp4");
    assert_eq!(created[0].cookies, "s=1");
}

#[tokio::test]
async fn ws_text_frame_auth_only_trusts_params_token_not_any_header() {
    let server = TestServer::start(MockHost::new(), |c| c.token = "S3CRET".to_string()).await;
    let mut ws = server.ws_connect().await;

    // 无 token：WS 没有逐帧自定义头可依赖，header_token_ok 恒 false。
    let no_token = json!({
        "jsonrpc": "2.0", "id": 1, "method": "aria2.addUri",
        "params": [["https://a.com/f.zip"]]
    });
    ws.send(WsMessage::text(no_token.to_string()))
        .await
        .unwrap();
    let resp = ws_recv_json(&mut ws).await;
    assert_eq!(resp["error"]["code"], 1);
    assert_eq!(resp["error"]["message"], "Unauthorized");

    // 正确的 params[0]="token:xxx" 才通过，同一条连接上继续复用。
    let with_token = json!({
        "jsonrpc": "2.0", "id": 2, "method": "aria2.addUri",
        "params": ["token:S3CRET", ["https://a.com/f.zip"]]
    });
    ws.send(WsMessage::text(with_token.to_string()))
        .await
        .unwrap();
    let resp2 = ws_recv_json(&mut ws).await;
    assert_eq!(resp2["result"], aria2::task_id_to_gid("mock-task-1"));
    assert_eq!(server.host.created().len(), 1);
}

#[tokio::test]
async fn ws_notification_frame_matches_aria2_wire_format_exactly() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let mut ws = server.ws_connect().await;

    // 先做一次 RPC 往返，确保服务端会话循环已经跑到 select! 里（订阅必定已
    // 生效），再触发广播，避免「emit 早于 subscribe」的时序竞争。
    let warmup = json!({"jsonrpc": "2.0", "id": 1, "method": "aria2.getVersion", "params": []});
    ws.send(WsMessage::text(warmup.to_string())).await.unwrap();
    ws_recv_json(&mut ws).await;

    let task_id = "550e8400-e29b-41d4-a716-446655440000";
    server.host.emit_event(task_id, TaskEventKind::Start);

    let frame = ws_recv_json(&mut ws).await;
    assert_eq!(
        frame,
        json!({
            "jsonrpc": "2.0",
            "method": "aria2.onDownloadStart",
            "params": [{ "gid": aria2::task_id_to_gid(task_id) }],
        })
    );
    assert!(
        frame.as_object().unwrap().get("id").is_none(),
        "aria2 通知帧不带 id 字段"
    );
}

#[tokio::test]
async fn ws_notification_covers_multiple_kinds_with_correct_method_names() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let mut ws = server.ws_connect().await;
    let warmup = json!({"jsonrpc": "2.0", "id": 1, "method": "aria2.getVersion", "params": []});
    ws.send(WsMessage::text(warmup.to_string())).await.unwrap();
    ws_recv_json(&mut ws).await;

    // 逐个 await 收完再发下一条，避免 broadcast 容量导致的丢帧/顺序问题——
    // 这里要验证的是「多种 kind 都能端到端正确路由」，不是 lag 语义本身
    // （lag/continue 分支已由 `jsonrpc_ws::tests::recv_event_*` 精确单测覆盖）。
    for (kind, method) in [
        (TaskEventKind::Complete, "aria2.onDownloadComplete"),
        (TaskEventKind::Error, "aria2.onDownloadError"),
        (TaskEventKind::BtComplete, "aria2.onBtDownloadComplete"),
    ] {
        server.host.emit_event("task-x", kind);
        let frame = ws_recv_json(&mut ws).await;
        assert_eq!(frame["method"], method);
        assert_eq!(frame["params"][0]["gid"], aria2::task_id_to_gid("task-x"));
    }
}

#[tokio::test]
async fn ws_notification_broadcasts_to_every_connected_session() {
    // 真实 aria2 对所有已连接 WS 会话广播同一条消息；两个客户端都应收到。
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let mut ws1 = server.ws_connect().await;
    let mut ws2 = server.ws_connect().await;
    for ws in [&mut ws1, &mut ws2] {
        let warmup = json!({"jsonrpc": "2.0", "id": 1, "method": "aria2.getVersion", "params": []});
        ws.send(WsMessage::text(warmup.to_string())).await.unwrap();
        ws_recv_json(ws).await;
    }

    server.host.emit_event("task-y", TaskEventKind::Pause);

    let frame1 = ws_recv_json(&mut ws1).await;
    let frame2 = ws_recv_json(&mut ws2).await;
    assert_eq!(frame1, frame2);
    assert_eq!(frame1["method"], "aria2.onDownloadPause");
}

#[tokio::test]
async fn ws_without_task_events_still_allows_bidirectional_rpc() {
    // `subscribe_task_events()` 返回 None（宿主未接线通知源）时，握手仍要成功，
    // 只是退化为纯双向 RPC——不应因为没有事件源就拒绝或断开连接。
    let server = TestServer::start(MockHost::new().without_task_events(), |_| {}).await;
    let mut ws = server.ws_connect().await;
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "aria2.addUri",
        "params": [["https://a.com/f.zip"]]
    });
    ws.send(WsMessage::text(body.to_string())).await.unwrap();
    let resp = ws_recv_json(&mut ws).await;
    assert_eq!(resp["result"], aria2::task_id_to_gid("mock-task-1"));

    // emit_event 在 events=None 时静默丢弃：不会有多余帧混进来干扰下一次收发。
    server.host.emit_event("ignored", TaskEventKind::Start);
    let body2 = json!({"jsonrpc": "2.0", "id": 2, "method": "aria2.getVersion", "params": []});
    ws.send(WsMessage::text(body2.to_string())).await.unwrap();
    let resp2 = ws_recv_json(&mut ws).await;
    assert_eq!(resp2["id"], 2);
    assert_eq!(resp2["result"]["version"], aria2::ARIA2_VERSION);
}

#[tokio::test]
async fn ws_close_frame_from_client_does_not_hang_server() {
    let server = TestServer::start(MockHost::new(), |_| {}).await;
    let mut ws = server.ws_connect().await;
    ws.send(WsMessage::Close(None)).await.unwrap();

    // 服务端收到 Close 帧后退出会话循环；后续读取不应无限期挂起。
    let next = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;
    assert!(
        next.is_ok(),
        "server did not react to the Close frame within the timeout"
    );
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
    // fallback message 是 CLI 区分「管理 API 未启用」与「资源不存在」的依据
    // （CLI 据此给出可操作提示）。锁定该契约，改动 fallback message 会跑挂此测试。
    assert_eq!(resp.json()["message"], UNKNOWN_ENDPOINT_MESSAGE);
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
