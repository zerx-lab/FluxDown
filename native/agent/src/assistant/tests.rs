//! AI 助手服务测试：axum 搭的 mock FluxCloud + 替身工具执行器，覆盖 SSE 事件到 `AssistantRunDto`
//! 的映射、确认 / 执行 / 结果回传的顺序、拒绝不执行、取消与断线对工具的取消、账号切换清空。

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use fluxdown_protocol::{
    ASSISTANT_MAX_TITLE_CHARS, ApplicationErrorCode, AssistantAskParams, AssistantAskResult,
    AssistantAttachmentParams, AssistantConfirmParams, AssistantConfirmationState,
    AssistantExportFormat, AssistantExportParams, AssistantRenameParams, AssistantRunDto,
    AssistantRunItemDto, AssistantRunStatus, AssistantSessionIdParams, AssistantSessionsParams,
    AssistantStateDto, AssistantToolCallDto, AssistantToolEffect, AssistantToolPreviewDto,
    AssistantToolStatus, ErrorReason,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use super::cloud::{self, AiCloud, RunEvent, SseParser};
use super::tools::{ToolCall, ToolOutput, ToolPreview};
use super::{AssistantError, AssistantService, ToolExecutor, effect_floor, lock, stricter};
use crate::cloud::{CloudApi, CloudClient, CloudError};
use crate::event_hub::AgentEventHub;
use crate::state::{AgentState, CloudCredentials, StateStore};

// ------------------------------------------------------------------ 纯函数

#[test]
fn sse_parser_handles_split_chunks_comments_and_multiline_data() {
    let mut parser = SseParser::default();
    parser.feed(b": ka\r\n\r\nda").expect("feed");
    assert!(
        parser.pop().is_none(),
        "comment frames and half lines yield nothing"
    );
    parser
        .feed(b"ta: {\"type\":\"text\",\r\ndata: \"delta\":\"a\"}\r\n\r\nevent: x\ndata:b\n\n")
        .expect("feed");
    assert_eq!(
        parser.pop().map(|event| event.data).as_deref(),
        Some(b"{\"type\":\"text\",\n\"delta\":\"a\"}".as_slice())
    );
    assert_eq!(
        parser.pop().map(|event| event.data).as_deref(),
        Some(b"b".as_slice())
    );
    assert!(parser.pop().is_none());
}

#[test]
fn sse_parser_reads_event_ids() {
    let mut parser = SseParser::default();
    parser
        .feed(b"id: 7\ndata: a\n\ndata: b\n\nid: x\ndata: c\n\nid: 1")
        .expect("feed");
    parser.feed(b"2\r\ndata: d\r\n\r\n").expect("feed");
    let events: Vec<(Option<u64>, Vec<u8>)> = std::iter::from_fn(|| parser.pop())
        .map(|event| (event.id, event.data))
        .collect();
    assert_eq!(
        events,
        [
            (Some(7), b"a".to_vec()),
            (None, b"b".to_vec()),
            (None, b"c".to_vec()),
            (Some(12), b"d".to_vec()),
        ]
    );
}

#[test]
fn sse_parser_rejects_oversized_lines() {
    let mut parser = SseParser::default();
    let long = vec![b'a'; 1024 * 1024 + 1];
    assert!(parser.feed(&long).is_err());
}

#[test]
fn unknown_events_are_ignored_and_known_ones_parse() {
    assert!(matches!(
        cloud::parse_event(br#"{"type":"triage","intent":"x"}"#),
        Ok(RunEvent::Other)
    ));
    assert!(matches!(
        cloud::parse_event(br#"{"type":"reasoning","delta":"x"}"#),
        Ok(RunEvent::Other)
    ));
    let call = cloud::parse_event(
        br#"{"type":"clientToolCall","requestId":"r","name":"fd_create_tasks","version":1,
            "arguments":{"a":1},"effect":"write","confirmTimeoutSecs":30,"timeoutSecs":60,"extra":1}"#,
    );
    assert!(
        matches!(
            &call,
            Ok(RunEvent::ClientToolCall {
                effect: AssistantToolEffect::Write,
                confirm_timeout_secs: Some(30),
                timeout_secs: 60,
                ..
            })
        ),
        "{call:?}"
    );
    let error = cloud::parse_event(
        br#"{"type":"error","status":"failed","message":"m","reason":"contextOverflow","messageId":"m1"}"#,
    );
    assert!(
        matches!(
            &error,
            Ok(RunEvent::Error { reason: Some(reason), message_id: Some(message_id), .. })
                if reason == "contextOverflow" && message_id == "m1"
        ),
        "{error:?}"
    );
    assert!(matches!(
        cloud::parse_event(br#"{"type":"error","status":"failed","message":"m"}"#),
        Ok(RunEvent::Error {
            reason: None,
            message_id: None,
            ..
        })
    ));
    assert!(cloud::parse_event(b"not json").is_err());
}

#[test]
fn cloud_errors_map_to_application_codes_and_reasons() {
    fn error(status: u16, code: &str, retryable: bool) -> CloudError {
        CloudError {
            status: Some(status),
            code: Some(code.to_owned()),
            message: "cloud text".to_owned(),
            retryable,
            unreachable: false,
        }
    }
    let cases = [
        (
            error(503, "ai_unavailable", false),
            ApplicationErrorCode::Unavailable,
            None,
        ),
        (
            error(403, "ai_disabled", false),
            ApplicationErrorCode::Unsupported,
            None,
        ),
        // 当日额度用完与每分钟限频是两件事:UI 靠 reason 分开。
        (
            error(403, "ai_quota_exceeded", false),
            ApplicationErrorCode::Conflict,
            Some(ErrorReason::AssistantQuotaExceeded),
        ),
        (
            error(403, "ai_session_limit", false),
            ApplicationErrorCode::Conflict,
            Some(ErrorReason::AssistantSessionLimit),
        ),
        (
            error(409, "conflict", false),
            ApplicationErrorCode::Conflict,
            None,
        ),
        (
            error(429, "rate_limited", true),
            ApplicationErrorCode::Unavailable,
            Some(ErrorReason::RateLimited),
        ),
    ];
    for (cloud_error, code, reason) in cases {
        let status = cloud_error.status;
        let mapped = AssistantError::from(cloud_error);
        assert_eq!(mapped.data.code, code, "status {status:?}");
        assert_eq!(mapped.data.reason, reason, "status {status:?}");
        assert_eq!(mapped.message, "cloud text");
    }
    assert!(
        AssistantError::from(error(429, "rate_limited", true))
            .data
            .retryable
    );
    assert!(
        !AssistantError::from(error(403, "ai_quota_exceeded", false))
            .data
            .retryable
    );
}

#[test]
fn local_effect_floor_cannot_be_downgraded_by_the_cloud() {
    assert_eq!(effect_floor("fd_create_tasks"), AssistantToolEffect::Write);
    assert_eq!(effect_floor("fd_manage_tasks"), AssistantToolEffect::Write);
    assert_eq!(
        effect_floor("fd_update_settings"),
        AssistantToolEffect::Write
    );
    assert_eq!(
        effect_floor("fd_measure_routes"),
        AssistantToolEffect::Network
    );
    assert_eq!(effect_floor("fd_get_app_info"), AssistantToolEffect::Read);
    assert_eq!(
        stricter(AssistantToolEffect::Read, effect_floor("fd_create_tasks")),
        AssistantToolEffect::Write
    );
    assert_eq!(
        stricter(AssistantToolEffect::Write, AssistantToolEffect::Network),
        AssistantToolEffect::Write
    );
}

// ------------------------------------------------------------------ mock FluxCloud

type Log = Arc<Mutex<Vec<String>>>;

#[derive(Default)]
struct MockCloud {
    /// 云端与执行器共用的事件日志,用来断言先后顺序。
    log: Log,
    created: Mutex<Vec<Value>>,
    asks: Mutex<Vec<Value>>,
    results: Mutex<Vec<Value>>,
    stream: Mutex<Option<mpsc::UnboundedReceiver<Bytes>>>,
    ask_failure: Mutex<Option<(StatusCode, Value)>>,
    confirm_status: Mutex<Option<StatusCode>>,
    cancels: AtomicUsize,
    cancel_status: Mutex<Option<StatusCode>>,
    config: Mutex<Value>,
    status: Mutex<Value>,
    /// `DELETE /sessions/{id}` 收到的会话 ID 与应答状态(默认 204)。
    deleted: Mutex<Vec<String>>,
    delete_status: Mutex<Option<StatusCode>>,
    /// `GET /runs/{id}/events`(续读)的应答队列:状态 + 正文(2xx 是 SSE 文本,其余是错误 JSON)。
    resume: Mutex<VecDeque<(StatusCode, String)>>,
    /// `PATCH /sessions/{id}` 的请求体。
    renames: Mutex<Vec<Value>>,
    /// `GET /sessions` 的应答(默认空列表)与 `GET /sessions/{id}` 的应答(默认 404)。
    sessions: Mutex<Option<Value>>,
    detail: Mutex<Option<Value>>,
    /// 会话列表 / 导出请求的「名字 + 查询串」。
    queries: Mutex<Vec<String>>,
    /// `GET /attachments/{id}` 的应答:`Content-Type` + 正文。
    attachment: Mutex<Option<(String, Vec<u8>)>>,
    /// 置位后提问一直挂起,直到 `ask_release`。
    ask_hold: AtomicBool,
    ask_release: Notify,
}

type Mock = Arc<MockCloud>;

fn record(mock: &MockCloud, entry: String) {
    lock(&mock.log).push(entry);
}

async fn mock_config(State(mock): State<Mock>) -> axum::Json<Value> {
    axum::Json(lock(&mock.config).clone())
}

async fn mock_status(State(mock): State<Mock>) -> axum::Json<Value> {
    axum::Json(lock(&mock.status).clone())
}

async fn mock_create_session(
    State(mock): State<Mock>,
    axum::Json(body): axum::Json<Value>,
) -> axum::Json<Value> {
    lock(&mock.created).push(body);
    axum::Json(json!({
        "id": "sess-1", "channel": "desktop", "kind": "ask", "title": "", "messageCount": 0,
        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"
    }))
}

async fn mock_ask(
    State(mock): State<Mock>,
    Path(_session): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    lock(&mock.asks).push(body);
    if mock.ask_hold.load(Ordering::SeqCst) {
        mock.ask_release.notified().await;
    }
    let failure = lock(&mock.ask_failure).clone();
    if let Some((status, body)) = failure {
        return (status, axum::Json(body)).into_response();
    }
    let Some(receiver) = lock(&mock.stream).take() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|chunk| (Ok::<_, Infallible>(chunk), receiver))
    });
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::HeaderName::from_static("x-ai-run-id"), "run-1"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn mock_confirmation(
    State(mock): State<Mock>,
    Path(run): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> StatusCode {
    record(
        &mock,
        format!(
            "confirm {run} {} {}",
            body["requestId"].as_str().unwrap_or_default(),
            body["approved"]
        ),
    );
    lock(&mock.confirm_status).unwrap_or(StatusCode::NO_CONTENT)
}

async fn mock_result(
    State(mock): State<Mock>,
    Path(run): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> StatusCode {
    record(
        &mock,
        format!(
            "result {run} {}",
            body["requestId"].as_str().unwrap_or_default()
        ),
    );
    lock(&mock.results).push(body);
    StatusCode::NO_CONTENT
}

async fn mock_cancel(State(mock): State<Mock>, Path(_session): Path<String>) -> StatusCode {
    mock.cancels.fetch_add(1, Ordering::SeqCst);
    record(&mock, "cancel".to_owned());
    lock(&mock.cancel_status).unwrap_or(StatusCode::NO_CONTENT)
}

async fn mock_sessions(State(mock): State<Mock>, RawQuery(query): RawQuery) -> axum::Json<Value> {
    lock(&mock.queries).push(format!("sessions?{}", query.unwrap_or_default()));
    let page = lock(&mock.sessions).clone();
    axum::Json(page.unwrap_or_else(|| json!({"total": 0, "items": []})))
}

async fn mock_session_detail(State(mock): State<Mock>, Path(session): Path<String>) -> Response {
    record(&mock, format!("detail {session}"));
    match lock(&mock.detail).clone() {
        Some(body) => axum::Json(body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn session_json(active_run: Option<&str>) -> Value {
    let mut session = json!({
        "id": "sess-1", "channel": "desktop", "kind": "ask", "title": "", "messageCount": 0,
        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"
    });
    if let Some(run_id) = active_run {
        session["active"] = json!(true);
        session["activeRunId"] = json!(run_id);
    }
    session
}

fn detail_json(messages: Value) -> Value {
    json!({"session": session_json(None), "messages": messages, "issues": [], "questionsUsed": 1})
}

async fn mock_rename(
    State(mock): State<Mock>,
    Path(session): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> axum::Json<Value> {
    record(&mock, format!("rename {session}"));
    let title = body["title"].clone();
    lock(&mock.renames).push(body);
    axum::Json(json!({
        "id": session, "channel": "desktop", "kind": "ask", "title": title, "messageCount": 0,
        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"
    }))
}

async fn mock_delete_session(State(mock): State<Mock>, Path(session): Path<String>) -> StatusCode {
    lock(&mock.deleted).push(session);
    lock(&mock.delete_status).unwrap_or(StatusCode::NO_CONTENT)
}

async fn mock_export(
    State(mock): State<Mock>,
    Path(session): Path<String>,
    RawQuery(query): RawQuery,
) -> axum::Json<Value> {
    lock(&mock.queries).push(format!("export {session}?{}", query.unwrap_or_default()));
    axum::Json(json!({"fileName": "chat.md", "mime": "text/markdown", "content": "# chat"}))
}

async fn mock_attachment(State(mock): State<Mock>, Path(_id): Path<String>) -> Response {
    match lock(&mock.attachment).clone() {
        Some((mime, body)) => ([(header::CONTENT_TYPE, mime)], body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn mock_events(
    State(mock): State<Mock>,
    Path(run): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    record(
        &mock,
        format!(
            "events {run} after={}",
            query.get("after").map_or("", String::as_str)
        ),
    );
    let Some((status, body)) = lock(&mock.resume).pop_front() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = if status.is_success() {
        "text/event-stream"
    } else {
        "application/json"
    };
    (status, [(header::CONTENT_TYPE, content_type)], body).into_response()
}

// ------------------------------------------------------------------ 替身执行器

struct FakeTools {
    log: Log,
    setup: Setup,
    started: Notify,
    cancelled: AtomicBool,
    calls: Mutex<Vec<ToolCall>>,
}

#[async_trait::async_trait]
impl ToolExecutor for FakeTools {
    fn declared(&self) -> Vec<(&'static str, u32)> {
        vec![
            ("fd_get_app_info", 1),
            ("fd_create_tasks", 1),
            ("fd_update_settings", 1),
        ]
    }

    async fn preview(&self, _call: &ToolCall) -> ToolPreview {
        match self.setup.preview {
            Some((action, destructive)) => ToolPreview {
                preview: Some(AssistantToolPreviewDto {
                    action: action.to_owned(),
                    tasks: vec!["a.iso".to_owned()],
                    more_tasks: 3,
                    delete_files: destructive,
                    ..AssistantToolPreviewDto::default()
                }),
                destructive,
            },
            None => ToolPreview {
                preview: None,
                destructive: false,
            },
        }
    }

    async fn execute(&self, call: ToolCall, cancel: CancellationToken) -> ToolOutput {
        lock(&self.log).push(format!("exec {}", call.name));
        lock(&self.calls).push(call);
        self.started.notify_one();
        if self.setup.block {
            cancel.cancelled().await;
            self.cancelled.store(true, Ordering::SeqCst);
            return ToolOutput {
                output: "cancelled".to_owned(),
                is_error: true,
                interrupted: self.setup.interrupt,
            };
        }
        ToolOutput {
            output: "tool output".to_owned(),
            is_error: false,
            interrupted: false,
        }
    }
}

/// 替身执行器的可调项。
#[derive(Clone, Copy, Default)]
struct Setup {
    /// 执行器一直等到取消令牌触发才返回(模拟长时间运行的工具)。
    block: bool,
    /// 阻塞的工具被取消时按「写操作已发出、结果未知」返回。
    interrupt: bool,
    /// 待确认调用的预览(动作名,是否破坏性);`None` = 生成不了。
    preview: Option<(&'static str, bool)>,
}

/// 去掉主机时钟字段后的状态:时钟每次投影都不同,比较状态时要忽略。
fn without_clock(mut state: AssistantStateDto) -> AssistantStateDto {
    state.now_unix_ms = 0;
    state
}

/// 把带序号的事件拼成云端的 SSE 文本(续读应答)。
fn sse_body(frames: &[(u64, Value)]) -> String {
    frames
        .iter()
        .map(|(seq, event)| format!("id: {seq}\ndata: {event}\n\n"))
        .collect()
}

// ------------------------------------------------------------------ harness

struct Harness {
    service: Arc<AssistantService>,
    cloud: CloudApi,
    mock: Mock,
    events: AgentEventHub,
    frames: Mutex<Option<mpsc::UnboundedSender<Bytes>>>,
    tools: Arc<FakeTools>,
    cancel: CancellationToken,
    dir: PathBuf,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Harness {
    async fn new(label: &str) -> Self {
        Self::build(label, Setup::default()).await
    }

    /// 工具一直阻塞到被取消。
    async fn blocking(label: &str) -> Self {
        Self::build(
            label,
            Setup {
                block: true,
                ..Setup::default()
            },
        )
        .await
    }

    async fn build(label: &str, setup: Setup) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind assistant mock");
        let address = listener.local_addr().expect("assistant mock address");
        let (frames, receiver) = mpsc::unbounded_channel();
        let mock: Mock = Arc::new(MockCloud {
            stream: Mutex::new(Some(receiver)),
            config: Mutex::new(json!({"available": true, "visible": true})),
            status: Mutex::new(json!({"visible": false})),
            ..MockCloud::default()
        });
        let router = Router::new()
            .route("/api/v1/ai/config", get(mock_config))
            .route("/api/v1/ai/status", get(mock_status))
            .route(
                "/api/v1/ai/sessions",
                post(mock_create_session).get(mock_sessions),
            )
            .route(
                "/api/v1/ai/sessions/{id}",
                patch(mock_rename)
                    .delete(mock_delete_session)
                    .get(mock_session_detail),
            )
            .route("/api/v1/ai/sessions/{id}/export", get(mock_export))
            .route("/api/v1/ai/sessions/{id}/messages", post(mock_ask))
            .route("/api/v1/ai/sessions/{id}/cancel", post(mock_cancel))
            .route("/api/v1/ai/attachments/{id}", get(mock_attachment))
            .route("/api/v1/ai/runs/{id}/events", get(mock_events))
            .route(
                "/api/v1/ai/runs/{id}/tool-confirmations",
                post(mock_confirmation),
            )
            .route("/api/v1/ai/runs/{id}/tool-results", post(mock_result))
            .with_state(mock.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_assistant_{label}_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(
            StateStore::open(dir.clone())
                .await
                .expect("assistant state store"),
        );
        let initial = AgentState {
            device_id: "device-1".to_owned(),
            credentials: Some(CloudCredentials {
                access_token: "access".to_owned(),
                refresh_token: "refresh".to_owned(),
                expires_at_unix: i64::MAX,
                session: None,
                issuer: String::new(),
            }),
            ..Default::default()
        };
        store.save(&initial).await.expect("save assistant state");
        let state = Arc::new(tokio::sync::Mutex::new(initial));
        let events = AgentEventHub::new(fluxdown_protocol::AgentSnapshot::default());
        let cloud = CloudApi::new(
            CloudClient::new(format!("http://{address}"), state, store)
                .expect("assistant cloud client")
                .with_events(events.clone()),
        );
        let cancel = CancellationToken::new();
        let service = Arc::new(AssistantService::new(
            cloud.clone(),
            events.clone(),
            cancel.clone(),
        ));
        let tools = Arc::new(FakeTools {
            log: mock.log.clone(),
            setup,
            started: Notify::new(),
            cancelled: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
        });
        service.set_executor(tools.clone());
        Self {
            service,
            cloud,
            mock,
            events,
            frames: Mutex::new(Some(frames)),
            tools,
            cancel,
            dir,
            server,
        }
    }

    fn send(&self, event: Value) {
        let frames = lock(&self.frames);
        frames
            .as_ref()
            .expect("mock event stream is open")
            .send(Bytes::from(format!("data: {event}\n\n")))
            .expect("mock event stream is open");
    }

    /// 带序号的事件(云端每个事件都带 `id:`,续读靠它)。
    fn send_id(&self, seq: u64, event: Value) {
        let frames = lock(&self.frames);
        frames
            .as_ref()
            .expect("mock event stream is open")
            .send(Bytes::from(format!("id: {seq}\ndata: {event}\n\n")))
            .expect("mock event stream is open");
    }

    /// 为下一次提问备好新的事件流(每次提问消费一个)。
    fn next_stream(&self) {
        let (frames, receiver) = mpsc::unbounded_channel();
        *lock(&self.mock.stream) = Some(receiver);
        *lock(&self.frames) = Some(frames);
    }

    /// 关闭事件流（云端正常 EOF）。
    fn close_stream(&self) {
        lock(&self.frames).take();
    }

    async fn ask(&self) -> AssistantAskResult {
        self.service
            .ask(AssistantAskParams {
                text: "download something".to_owned(),
                ..AssistantAskParams::default()
            })
            .await
            .expect("ask accepted")
    }

    fn run(&self) -> AssistantRunDto {
        self.service
            .state()
            .runs
            .into_iter()
            .last()
            .expect("a run exists")
    }

    fn tool(&self, request_id: &str) -> AssistantToolCallDto {
        self.run()
            .items
            .into_iter()
            .find_map(|item| match item {
                AssistantRunItemDto::Tool { call }
                    if call.request_id.as_deref() == Some(request_id) =>
                {
                    Some(call)
                }
                _ => None,
            })
            .expect("tool call exists")
    }

    fn log(&self) -> Vec<String> {
        lock(&self.mock.log).clone()
    }

    fn has_log(&self, prefix: &str) -> bool {
        self.log().iter().any(|entry| entry.starts_with(prefix))
    }

    async fn eventually(&self, what: &str, mut check: impl FnMut(&Self) -> bool) {
        let waited = tokio::time::timeout(Duration::from_secs(8), async {
            while !check(self) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "timed out waiting for {what}; log = {:?}",
            self.log()
        );
    }

    /// 发出一次需要确认的写工具调用，并等到它挂起待确认。
    async fn pending_write_call(&self, name: &str, effect: &str, confirm_secs: u64) {
        self.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
        self.send(json!({"type": "toolStarted", "callId": "c1", "name": name, "kind": "client"}));
        self.send(json!({
            "type": "clientToolCall", "requestId": "r1", "name": name, "version": 1,
            "arguments": {"tasks": []}, "effect": effect,
            "confirmTimeoutSecs": confirm_secs, "timeoutSecs": 60
        }));
        self.ask().await;
        self.eventually("pending confirmation", |h| {
            h.service.state().runs.last().is_some_and(|run| {
                run.items.iter().any(|item| {
                    matches!(item, AssistantRunItemDto::Tool { call }
                        if call.confirmation == Some(AssistantConfirmationState::Pending))
                })
            })
        })
        .await;
    }

    async fn finish(self) {
        self.cancel.cancel();
        self.server.abort();
        match self.server.await {
            Ok(result) => result.expect("assistant mock server completes"),
            Err(error) if error.is_cancelled() => {}
            Err(error) => panic!("assistant mock server panicked: {error}"),
        }
        if let Err(error) = tokio::fs::remove_dir_all(&self.dir).await {
            tracing::warn!(path = %self.dir.display(), %error, "assistant test cleanup failed");
        }
    }
}

fn confirm(request_id: &str, approved: bool) -> AssistantConfirmParams {
    AssistantConfirmParams {
        request_id: request_id.to_owned(),
        approved,
    }
}

// ------------------------------------------------------------------ 配置

#[tokio::test]
async fn config_enables_tools_only_for_a_matching_protocol_and_catalog() {
    let harness = Harness::new("config").await;
    *lock(&harness.mock.config) = json!({
        "available": true, "clientToolProtocol": 3, "dailyLimit": 20, "usedToday": 2,
        "maxMessageChars": 2000,
        "clientTools": [{"name": "fd_get_app_info", "version": 1}]
    });
    let state = harness.service.refresh().await.expect("refresh");
    let config = state.config.expect("config cached");
    assert!(config.available && config.tools_enabled);
    assert_eq!((config.daily_limit, config.used_today), (20, 2));

    *lock(&harness.mock.config) = json!({
        "available": true, "clientToolProtocol": 2,
        "clientTools": [{"name": "fd_get_app_info", "version": 1}]
    });
    let state = harness.service.refresh().await.expect("refresh again");
    assert!(!state.config.expect("config cached").tools_enabled);
    harness.finish().await;
}

// ------------------------------------------------------------------ 映射

#[tokio::test]
async fn events_map_into_the_run_and_publish_the_whole_state() {
    let harness = Harness::new("mapping").await;
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.send(json!({"type": "text", "delta": "Hel"}));
    harness.send(json!({"type": "text", "delta": "lo"}));
    harness.send(
        json!({"type": "toolStarted", "callId": "s1", "name": "source_search", "kind": "server"}),
    );
    harness.send(
        json!({"type": "toolFinished", "callId": "s1", "name": "source_search",
        "kind": "server", "isError": false, "durationMs": 42}),
    );
    harness.send(json!({"type": "text", "delta": "partial"}));
    harness.send(json!({"type": "turnRetried", "turn": 2}));
    harness.send(json!({"type": "text", "delta": "final"}));
    harness.send(json!({"type": "triage", "intent": "x"}));
    harness.send(json!({"type": "issueDraft", "issue": {
        "id": "i1", "kind": "bug", "title": "t", "body": "b", "status": "draft",
        "createdAt": "x", "updatedAt": "y"}}));
    harness.send(
        json!({"type": "done", "status": "completed", "messageId": "m1",
        "text": "Hellofinal", "inputTokens": 10, "outputTokens": 5}),
    );
    let result = harness.ask().await;
    assert_eq!(result.session_id, "sess-1");
    harness
        .eventually("completed run", |h| {
            h.service
                .state()
                .runs
                .last()
                .is_some_and(|run| run.status == AssistantRunStatus::Completed)
        })
        .await;
    let run = harness.run();
    assert_eq!(run.run_id.as_deref(), Some("run-1"));
    assert_eq!(run.question, "download something");
    assert_eq!(
        (
            run.message_id.as_deref(),
            run.input_tokens,
            run.output_tokens
        ),
        (Some("m1"), 10, 5)
    );
    assert_eq!(run.items.len(), 4, "{:?}", run.items);
    assert!(matches!(&run.items[0], AssistantRunItemDto::Text { text } if text == "Hello"));
    assert!(matches!(&run.items[1], AssistantRunItemDto::Tool { call }
        if call.call_id == "s1" && call.kind == "server"
            && call.status == AssistantToolStatus::Succeeded && call.duration_ms == Some(42)));
    assert!(
        matches!(&run.items[2], AssistantRunItemDto::Text { text } if text == "final"),
        "a retried turn drops its partial text"
    );
    assert!(matches!(&run.items[3], AssistantRunItemDto::IssueDraft { issue } if issue.id == "i1"));
    // 整体替换：快照里的助手状态与服务状态一致。
    let published = harness
        .events
        .inspect(|snapshot| snapshot.assistant.clone());
    assert_eq!(
        without_clock(published),
        without_clock(harness.service.state())
    );
    assert!(
        harness.service.state().now_unix_ms > 0,
        "the projection carries the host clock"
    );
    // 提问体：服务端只拿到文本。
    assert_eq!(lock(&harness.mock.asks)[0]["text"], "download something");
    harness.finish().await;
}

#[tokio::test]
async fn first_ask_creates_a_session_that_declares_the_local_tools() {
    let harness = Harness::new("create").await;
    harness.send(json!({"type": "done", "status": "completed", "text": "hi"}));
    harness.ask().await;
    let created = lock(&harness.mock.created).clone();
    assert_eq!(created.len(), 1);
    let body = &created[0];
    assert_eq!(body["channel"], "desktop");
    assert_eq!(body["kind"], "ask");
    assert_eq!(body["deviceId"], "device-1");
    assert_eq!(body["client"]["clientToolProtocol"], 3);
    let tools = body["client"]["clientTools"].as_array().expect("tool refs");
    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "fd_create_tasks" && tool["version"] == 1)
    );
    harness.finish().await;
}

#[tokio::test]
async fn a_session_with_a_running_question_rejects_the_next_one() {
    let harness = Harness::new("busy").await;
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.ask().await;
    let error = harness
        .service
        .ask(AssistantAskParams {
            session_id: Some("sess-1".to_owned()),
            text: "again".to_owned(),
            ..AssistantAskParams::default()
        })
        .await
        .expect_err("a second question in a busy session must be refused");
    assert_eq!(error.data.code, ApplicationErrorCode::Conflict);
    assert_eq!(harness.service.state().runs.len(), 1);
    harness.finish().await;
}

#[tokio::test]
async fn cloud_refusal_is_mapped_recorded_and_leaves_no_run() {
    let harness = Harness::new("refusal").await;
    *lock(&harness.mock.ask_failure) = Some((
        StatusCode::FORBIDDEN,
        json!({"code": "ai_quota_exceeded", "message": "Daily limit reached"}),
    ));
    let error = harness
        .service
        .ask(AssistantAskParams {
            text: "hi".to_owned(),
            ..AssistantAskParams::default()
        })
        .await
        .expect_err("quota refusal");
    assert_eq!(error.data.code, ApplicationErrorCode::Conflict);
    assert_eq!(error.data.reason, Some(ErrorReason::AssistantQuotaExceeded));
    assert_eq!(error.message, "Daily limit reached");
    let state = harness.service.state();
    assert!(state.runs.is_empty(), "{state:?}");
    assert_eq!(
        state.last_error_message.as_deref(),
        Some("Daily limit reached")
    );
    assert_eq!(
        state.last_error_reason,
        Some(ErrorReason::AssistantQuotaExceeded)
    );
    // 这次提问自己建的会话在云端是空的：被删掉，不在历史里留下「新对话」。
    assert_eq!(*lock(&harness.mock.deleted), ["sess-1"]);
    harness.finish().await;
}

// ------------------------------------------------------------------ 确认与执行

#[tokio::test]
async fn read_tool_runs_without_confirmation_and_reports_its_result() {
    let harness = Harness::new("read").await;
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.send(
        json!({"type": "toolStarted", "callId": "c1", "name": "fd_get_app_info", "kind": "client"}),
    );
    harness.send(json!({
        "type": "clientToolCall", "requestId": "r1", "name": "fd_get_app_info", "version": 1,
        "arguments": {}, "effect": "read", "timeoutSecs": 20
    }));
    harness.ask().await;
    harness
        .eventually("result report", |h| h.has_log("result"))
        .await;
    assert_eq!(harness.log(), ["exec fd_get_app_info", "result run-1 r1"]);
    let result = lock(&harness.mock.results)[0].clone();
    assert_eq!(result["output"], "tool output");
    assert_eq!(result["isError"], false);
    harness
        .eventually("succeeded tool", |h| {
            h.tool("r1").status == AssistantToolStatus::Succeeded
        })
        .await;
    let call = harness.tool("r1");
    assert_eq!(call.call_id, "c1", "the tool card keeps the cloud call id");
    assert_eq!(call.confirmation, None);
    assert_eq!(call.output_preview.as_deref(), Some("tool output"));
    harness.finish().await;
}

#[tokio::test]
async fn write_tool_runs_only_after_the_confirmation_reaches_the_cloud() {
    let harness = Harness::new("confirm").await;
    harness
        .pending_write_call("fd_create_tasks", "write", 30)
        .await;
    let call = harness.tool("r1");
    assert_eq!(call.effect, AssistantToolEffect::Write);
    assert!(call.confirm_deadline_unix_ms.is_some());
    assert_eq!(call.arguments, Some(json!({"tasks": []})));
    // 执行器给不出预览：卡片回退到原始参数。
    assert_eq!(call.preview, None);
    assert!(!call.destructive);
    assert!(!call.interrupted);
    assert!(
        harness.log().is_empty(),
        "nothing may run before the user decides"
    );

    harness
        .service
        .confirm(confirm("r1", true))
        .await
        .expect("confirm");
    harness
        .eventually("result report", |h| h.has_log("result"))
        .await;
    assert_eq!(
        harness.log(),
        [
            "confirm run-1 r1 true",
            "exec fd_create_tasks",
            "result run-1 r1"
        ]
    );
    assert_eq!(
        harness.tool("r1").confirmation,
        Some(AssistantConfirmationState::Approved)
    );
    harness.finish().await;
}

#[tokio::test]
async fn declined_confirmation_never_runs_the_tool() {
    let harness = Harness::new("decline").await;
    harness
        .pending_write_call("fd_create_tasks", "write", 30)
        .await;
    harness
        .service
        .confirm(confirm("r1", false))
        .await
        .expect("decline");
    let call = harness.tool("r1");
    assert_eq!(
        call.confirmation,
        Some(AssistantConfirmationState::Declined)
    );
    assert_eq!(call.status, AssistantToolStatus::Failed);
    assert_eq!(
        call.output_preview, None,
        "the UI localises a declined call itself"
    );
    // 云端自己把「已拒绝」交还模型；本机既不执行也不回传结果。
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(harness.log(), ["confirm run-1 r1 false"]);
    assert!(lock(&harness.tools.calls).is_empty());
    // 同一个请求不能再被确认。
    let again = harness.service.confirm(confirm("r1", true)).await;
    assert!(matches!(again, Err(error) if error.data.code == ApplicationErrorCode::NotFound));
    harness.finish().await;
}

#[tokio::test]
async fn cloud_declared_read_effect_cannot_skip_confirmation_for_write_tools() {
    let harness = Harness::new("floor").await;
    // 云端把设置写入声明成 `read` 且不给确认窗口：本机仍按写工具先确认。
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.send(json!({
        "type": "clientToolCall", "requestId": "r1", "name": "fd_update_settings", "version": 1,
        "arguments": {}, "effect": "read", "timeoutSecs": 20
    }));
    harness.ask().await;
    harness
        .eventually("pending confirmation", |h| {
            h.service.state().runs.last().is_some_and(|run| {
                run.items.iter().any(|item| {
                    matches!(item, AssistantRunItemDto::Tool { call }
                        if call.confirmation == Some(AssistantConfirmationState::Pending)
                            && call.effect == AssistantToolEffect::Write)
                })
            })
        })
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(lock(&harness.tools.calls).is_empty());
    harness.finish().await;
}

#[tokio::test]
async fn confirmation_the_cloud_no_longer_waits_for_expires_without_running() {
    let harness = Harness::new("stale").await;
    harness
        .pending_write_call("fd_create_tasks", "write", 30)
        .await;
    *lock(&harness.mock.confirm_status) = Some(StatusCode::NOT_FOUND);
    let error = harness
        .service
        .confirm(confirm("r1", true))
        .await
        .expect_err("stale confirmation");
    assert_eq!(error.data.code, ApplicationErrorCode::Conflict);
    assert_eq!(
        error.data.reason,
        Some(ErrorReason::AssistantConfirmExpired)
    );
    assert_eq!(
        harness.tool("r1").confirmation,
        Some(AssistantConfirmationState::Expired)
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(lock(&harness.tools.calls).is_empty());
    assert!(!harness.has_log("result"));
    harness.finish().await;
}

#[tokio::test]
async fn unanswered_confirmation_expires_locally_and_is_then_refused() {
    let harness = Harness::new("expiry").await;
    harness
        .pending_write_call("fd_create_tasks", "write", 1)
        .await;
    harness
        .eventually("local expiry", |h| {
            h.tool("r1").confirmation == Some(AssistantConfirmationState::Expired)
        })
        .await;
    assert_eq!(harness.tool("r1").status, AssistantToolStatus::Failed);
    let late = harness.service.confirm(confirm("r1", true)).await;
    assert!(matches!(late, Err(error) if error.data.code == ApplicationErrorCode::NotFound));
    assert!(lock(&harness.tools.calls).is_empty());
    assert!(
        !harness.has_log("confirm"),
        "an expired confirmation is never sent"
    );
    harness.finish().await;
}

#[tokio::test]
async fn cloud_finishing_the_call_closes_the_pending_confirmation() {
    let harness = Harness::new("timeout").await;
    harness
        .pending_write_call("fd_create_tasks", "write", 30)
        .await;
    // 云端判定确认超时：toolFinished(isError) 到达。
    harness.send(
        json!({"type": "toolFinished", "callId": "c1", "name": "fd_create_tasks",
        "kind": "client", "isError": true, "durationMs": 30000}),
    );
    harness
        .eventually("closed confirmation", |h| {
            h.tool("r1").confirmation == Some(AssistantConfirmationState::Expired)
        })
        .await;
    let late = harness.service.confirm(confirm("r1", true)).await;
    assert!(matches!(late, Err(error) if error.data.code == ApplicationErrorCode::NotFound));
    harness.finish().await;
}

// ------------------------------------------------------------------ 取消 / 断线 / 登出

#[tokio::test]
async fn cancel_stops_the_running_tool_and_notifies_the_cloud() {
    let harness = Harness::blocking("cancel").await;
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.send(json!({
        "type": "clientToolCall", "requestId": "r1", "name": "fd_get_app_info", "version": 1,
        "arguments": {}, "effect": "read", "timeoutSecs": 60
    }));
    harness.ask().await;
    harness.tools.started.notified().await;
    harness
        .service
        .cancel(AssistantSessionIdParams {
            session_id: "sess-1".to_owned(),
        })
        .await
        .expect("cancel");
    harness
        .eventually("tool cancellation", |h| {
            h.tools.cancelled.load(Ordering::SeqCst)
        })
        .await;
    assert_eq!(harness.mock.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(harness.run().status, AssistantRunStatus::Canceled);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !harness.has_log("result"),
        "a cancelled run reports no tool result"
    );
    harness.finish().await;
}

#[tokio::test]
async fn stream_disconnect_fails_the_run_and_cancels_its_tools() {
    let harness = Harness::blocking("disconnect").await;
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.send(json!({
        "type": "clientToolCall", "requestId": "r1", "name": "fd_get_app_info", "version": 1,
        "arguments": {}, "effect": "read", "timeoutSecs": 60
    }));
    harness.ask().await;
    harness.tools.started.notified().await;
    // 云端断开（正常 EOF，没有 done 帧）。
    harness.close_stream();
    harness
        .eventually("failed run", |h| {
            h.run().status == AssistantRunStatus::Failed
        })
        .await;
    harness
        .eventually("tool cancellation", |h| {
            h.tools.cancelled.load(Ordering::SeqCst)
        })
        .await;
    let run = harness.run();
    assert!(run.error.is_some());
    assert_eq!(run.error_reason.as_deref(), Some("streamLost"));
    harness.finish().await;
}

#[tokio::test]
async fn account_change_cancels_runs_and_clears_the_state() {
    let harness = Harness::blocking("logout").await;
    let background = tokio::spawn(harness.service.clone().run());
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.send(json!({
        "type": "clientToolCall", "requestId": "r1", "name": "fd_get_app_info", "version": 1,
        "arguments": {}, "effect": "read", "timeoutSecs": 60
    }));
    harness.ask().await;
    harness.tools.started.notified().await;
    harness.cloud.clear_session().await.expect("clear session");
    harness
        .eventually("cleared state", |h| {
            h.service.state() == AssistantStateDto::default()
        })
        .await;
    harness
        .eventually("tool cancellation", |h| {
            h.tools.cancelled.load(Ordering::SeqCst)
        })
        .await;
    assert!(!harness.has_log("result"));
    harness.cancel.cancel();
    background.await.expect("assistant loop stops");
    harness.finish().await;
}

#[tokio::test]
async fn visibility_follows_config_when_signed_in_and_status_after_logout() {
    let harness = Harness::new("visibility").await;
    // 套餐不含助手时入口照常可见：可见性只看云端给的 `visible`。
    *lock(&harness.mock.config) =
        json!({"available": false, "reason": "plan_disabled", "visible": true});
    let background = tokio::spawn(harness.service.clone().run());
    harness
        .eventually("visible from config", |h| h.service.state().visible)
        .await;
    harness.cloud.clear_session().await.expect("clear session");
    harness
        .eventually("hidden from anonymous status", |h| {
            h.service.state() == AssistantStateDto::default()
        })
        .await;
    harness.cancel.cancel();
    background.await.expect("assistant loop stops");
    harness.finish().await;
}

// ------------------------------------------------------------------ 裁剪

#[tokio::test]
async fn run_history_is_trimmed_but_active_runs_survive() {
    let harness = Harness::new("trim").await;
    let service = &harness.service;
    let slot = |id: u64, active: bool| super::RunSlot {
        id,
        dto: AssistantRunDto {
            session_id: format!("s{id}"),
            ..AssistantRunDto::default()
        },
        ctl: active.then(|| super::RunCtl {
            cancel: CancellationToken::new(),
            cloud: AiCloud::new(harness.cloud.clone()),
            pending: std::collections::HashMap::new(),
        }),
    };
    {
        let mut inner = service.lock();
        for id in 0..8 {
            // 最旧的一条仍在进行中。
            inner.slots.push(slot(id, id == 0));
        }
        inner.make_room();
        inner.slots.push(slot(8, true));
        assert_eq!(inner.slots.len(), 8);
        assert!(
            inner.slots.iter().any(|slot| slot.id == 0),
            "the active run is never trimmed"
        );
        assert!(
            !inner.slots.iter().any(|slot| slot.id == 1),
            "the oldest finished run goes first"
        );
    }
    {
        let mut inner = service.lock();
        inner.slots.clear();
        for id in 0..8 {
            inner.slots.push(slot(id, true));
        }
        inner.make_room();
        assert_eq!(inner.slots.len(), 8, "all-active history is not trimmed");
    }
    harness.finish().await;
}

// ------------------------------------------------------------------ 提问准入

#[tokio::test]
async fn rate_limit_keeps_its_own_reason_and_an_existing_session() {
    let harness = Harness::new("rate_limit").await;
    *lock(&harness.mock.ask_failure) = Some((
        StatusCode::TOO_MANY_REQUESTS,
        json!({"code": "rate_limited", "message": "Slow down"}),
    ));
    let error = harness
        .service
        .ask(AssistantAskParams {
            session_id: Some("sess-9".to_owned()),
            text: "hi".to_owned(),
            ..AssistantAskParams::default()
        })
        .await
        .expect_err("rate limited");
    // 每分钟限频不是「额度用完」:两者的 reason 不同,UI 才能分开显示。
    assert_eq!(error.data.reason, Some(ErrorReason::RateLimited));
    assert!(error.data.retryable);
    assert!(lock(&harness.mock.created).is_empty());
    assert!(
        lock(&harness.mock.deleted).is_empty(),
        "only a session this ask created is removed"
    );
    harness.finish().await;
}

#[tokio::test]
async fn failed_empty_session_cleanup_does_not_mask_the_refusal() {
    let harness = Harness::new("cleanup_fails").await;
    *lock(&harness.mock.ask_failure) = Some((
        StatusCode::FORBIDDEN,
        json!({"code": "ai_quota_exceeded", "message": "Daily limit reached"}),
    ));
    *lock(&harness.mock.delete_status) = Some(StatusCode::INTERNAL_SERVER_ERROR);
    let error = harness
        .service
        .ask(AssistantAskParams {
            text: "hi".to_owned(),
            ..AssistantAskParams::default()
        })
        .await
        .expect_err("quota refusal");
    assert_eq!(error.data.reason, Some(ErrorReason::AssistantQuotaExceeded));
    assert_eq!(*lock(&harness.mock.deleted), ["sess-1"]);
    harness.finish().await;
}

#[tokio::test]
async fn ask_validates_replace_from_and_keep_images_before_touching_the_cloud() {
    let harness = Harness::new("validate_ask").await;
    let cases = [
        // (sessionId, replaceFrom, keepImages, 新图数, 出错字段)
        (None, Some("m1"), false, 0, "replaceFrom"),
        (Some("sess-1"), None, true, 0, "keepImages"),
        (Some("sess-1"), Some("m1"), true, 1, "keepImages"),
    ];
    for (session_id, replace_from, keep_images, images, field) in cases {
        let error = harness
            .service
            .ask(AssistantAskParams {
                session_id: session_id.map(str::to_owned),
                replace_from: replace_from.map(str::to_owned),
                keep_images,
                images: vec!["data:image/png;base64,AAAA".to_owned(); images],
                text: "hi".to_owned(),
                ..AssistantAskParams::default()
            })
            .await
            .expect_err(field);
        assert_eq!(
            error.data.code,
            ApplicationErrorCode::InvalidArgument,
            "{field}"
        );
        assert_eq!(error.data.field.as_deref(), Some(field));
    }
    assert!(lock(&harness.mock.created).is_empty());
    assert!(lock(&harness.mock.asks).is_empty());
    assert!(harness.service.state().runs.is_empty());
    harness.finish().await;
}

#[tokio::test]
async fn regenerate_passes_replace_from_and_keep_images_and_drops_the_old_answer() {
    let harness = Harness::new("replace").await;
    harness.send(json!({"type": "done", "status": "completed", "messageId": "m1", "text": "one"}));
    harness.ask().await;
    harness
        .eventually("first answer", |h| {
            h.service
                .state()
                .runs
                .last()
                .is_some_and(|run| run.status == AssistantRunStatus::Completed)
        })
        .await;
    harness.next_stream();
    harness.send(json!({"type": "done", "status": "completed", "messageId": "m2", "text": "two"}));
    harness
        .service
        .ask(AssistantAskParams {
            session_id: Some("sess-1".to_owned()),
            replace_from: Some("u1".to_owned()),
            keep_images: true,
            text: "again".to_owned(),
            ..AssistantAskParams::default()
        })
        .await
        .expect("regenerate accepted");
    harness
        .eventually("second answer", |h| {
            h.service
                .state()
                .runs
                .last()
                .is_some_and(|run| run.message_id.as_deref() == Some("m2"))
        })
        .await;
    let asks = lock(&harness.mock.asks).clone();
    assert_eq!(asks.len(), 2);
    assert!(asks[0].get("replaceFrom").is_none() && asks[0].get("keepImages").is_none());
    assert_eq!(asks[1]["replaceFrom"], "u1");
    assert_eq!(asks[1]["keepImages"], true);
    // 被替换的旧回答不再留着实时覆盖层。
    assert_eq!(harness.service.state().runs.len(), 1);
    harness.finish().await;
}

#[tokio::test]
async fn run_records_the_image_count_of_the_question() {
    let harness = Harness::new("images").await;
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness
        .service
        .ask(AssistantAskParams {
            text: "look".to_owned(),
            images: vec![
                "data:image/png;base64,AAAA".to_owned(),
                "data:image/png;base64,BBBB".to_owned(),
            ],
            ..AssistantAskParams::default()
        })
        .await
        .expect("ask accepted");
    assert_eq!(harness.run().image_count, 2);
    assert_eq!(
        lock(&harness.mock.asks)[0]["images"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    harness.finish().await;
}

// ------------------------------------------------------------------ 失败原因

#[tokio::test]
async fn cloud_error_reason_and_message_id_reach_the_run() {
    let harness = Harness::new("error_reason").await;
    harness.send(json!({
        "type": "error", "status": "failed", "message": "too long",
        "reason": "contextOverflow", "messageId": "m9"
    }));
    harness.ask().await;
    harness
        .eventually("failed run", |h| {
            h.service
                .state()
                .runs
                .last()
                .is_some_and(|run| run.status == AssistantRunStatus::Failed)
        })
        .await;
    let run = harness.run();
    assert_eq!(run.error.as_deref(), Some("too long"));
    assert_eq!(run.error_reason.as_deref(), Some("contextOverflow"));
    assert_eq!(run.message_id.as_deref(), Some("m9"));
    harness.finish().await;
}

#[tokio::test]
async fn persisted_runs_drop_tool_arguments_but_unpersisted_ones_keep_them() {
    /// 跑完一次只读工具调用(参数不小),返回等到工具成功后的 harness。
    async fn with_finished_tool(label: &str) -> Harness {
        let harness = Harness::new(label).await;
        harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
        harness.send(
            json!({"type": "toolStarted", "callId": "c1", "name": "fd_get_app_info", "kind": "client"}),
        );
        harness.send(json!({
            "type": "clientToolCall", "requestId": "r1", "name": "fd_get_app_info", "version": 1,
            "arguments": {"big": [1, 2, 3]}, "effect": "read", "timeoutSecs": 20
        }));
        harness.ask().await;
        harness
            .eventually("result report", |h| h.has_log("result"))
            .await;
        harness
            .eventually("succeeded tool", |h| {
                h.tool("r1").status == AssistantToolStatus::Succeeded
            })
            .await;
        harness
    }

    let harness = with_finished_tool("arguments").await;
    assert_eq!(
        harness.tool("r1").arguments,
        Some(json!({"big": [1, 2, 3]})),
        "a running run keeps the arguments for the live card"
    );
    // 没落库的失败(无 messageId):保留参数。
    harness.send(json!({"type": "error", "status": "failed", "message": "boom"}));
    harness
        .eventually("failed run", |h| {
            h.run().status == AssistantRunStatus::Failed
        })
        .await;
    assert!(harness.tool("r1").arguments.is_some());
    harness.finish().await;

    let harness = with_finished_tool("arguments_persisted").await;
    harness.send(json!({"type": "done", "status": "completed", "messageId": "m1", "text": "ok"}));
    harness
        .eventually("completed run", |h| {
            h.run().status == AssistantRunStatus::Completed
        })
        .await;
    // 回答已落库:会话详情里有完整参数,实时投影不再每次深拷贝它。
    assert_eq!(harness.tool("r1").arguments, None);
    harness.finish().await;
}

// ------------------------------------------------------------------ 事件流续读

#[tokio::test]
async fn interrupted_stream_resumes_after_the_last_event_id() {
    let harness = Harness::new("resume").await;
    harness.send_id(
        1,
        json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}),
    );
    harness.send_id(2, json!({"type": "text", "delta": "Hel"}));
    // 先一次 503(退避后重试),再成功续读:云端从 `after` 之后补发,含一帧已处理过的重放。
    *lock(&harness.mock.resume) = VecDeque::from([
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"code": "unavailable", "message": "down"}).to_string(),
        ),
        (
            StatusCode::OK,
            sse_body(&[
                (2, json!({"type": "text", "delta": "Hel"})),
                (3, json!({"type": "text", "delta": "lo"})),
                (
                    4,
                    json!({"type": "done", "status": "completed", "messageId": "m1", "text": "Hello"}),
                ),
            ]),
        ),
    ]);
    harness.ask().await;
    harness.close_stream();
    harness
        .eventually("completed run", |h| {
            h.service
                .state()
                .runs
                .last()
                .is_some_and(|run| run.status == AssistantRunStatus::Completed)
        })
        .await;
    let run = harness.run();
    assert_eq!(run.items.len(), 1, "{:?}", run.items);
    assert!(
        matches!(&run.items[0], AssistantRunItemDto::Text { text } if text == "Hello"),
        "the replayed event must not duplicate text: {:?}",
        run.items
    );
    assert_eq!(run.message_id.as_deref(), Some("m1"));
    let resumes: Vec<String> = harness
        .log()
        .into_iter()
        .filter(|entry| entry.starts_with("events"))
        .collect();
    assert_eq!(resumes, ["events run-1 after=2", "events run-1 after=2"]);
    harness.finish().await;
}

#[tokio::test]
async fn stream_closed_before_the_first_event_resumes_from_the_start() {
    let harness = Harness::new("resume_start").await;
    *lock(&harness.mock.resume) = VecDeque::from([(
        StatusCode::OK,
        sse_body(&[
            (
                1,
                json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}),
            ),
            (
                2,
                json!({"type": "done", "status": "completed", "messageId": "m1", "text": "hi"}),
            ),
        ]),
    )]);
    harness.close_stream();
    harness.ask().await;
    harness
        .eventually("completed run", |h| {
            h.service
                .state()
                .runs
                .last()
                .is_some_and(|run| run.status == AssistantRunStatus::Completed)
        })
        .await;
    assert!(
        harness.has_log("events run-1 after=0"),
        "{:?}",
        harness.log()
    );
    harness.finish().await;
}

#[tokio::test]
async fn resume_refused_with_gone_fails_the_run_as_stream_lost() {
    let harness = Harness::new("resume_gone").await;
    harness.send_id(
        1,
        json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}),
    );
    harness.send_id(2, json!({"type": "text", "delta": "partial"}));
    *lock(&harness.mock.resume) = VecDeque::from([(
        StatusCode::GONE,
        json!({"code": "ai_run_gone", "message": "The run is no longer available."}).to_string(),
    )]);
    harness.ask().await;
    harness.close_stream();
    harness
        .eventually("failed run", |h| {
            h.service
                .state()
                .runs
                .last()
                .is_some_and(|run| run.status == AssistantRunStatus::Failed)
        })
        .await;
    let run = harness.run();
    assert_eq!(run.error_reason.as_deref(), Some("streamLost"));
    assert_eq!(
        run.error.as_deref(),
        Some("The run is no longer available.")
    );
    assert_eq!(
        harness.log(),
        ["events run-1 after=2", "detail sess-1"],
        "410 is final: no retry, only one check of the persisted answer"
    );
    harness.finish().await;
}

#[tokio::test]
async fn stream_without_event_ids_is_never_resumed() {
    let harness = Harness::new("resume_no_ids").await;
    // 旧云端不带 `id:`:已处理过事件却不知道序号,续读会重放并重复文本,所以直接失败。
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.send(json!({"type": "text", "delta": "partial"}));
    harness.ask().await;
    harness.close_stream();
    harness
        .eventually("failed run", |h| {
            h.service
                .state()
                .runs
                .last()
                .is_some_and(|run| run.status == AssistantRunStatus::Failed)
        })
        .await;
    assert_eq!(harness.run().error_reason.as_deref(), Some("streamLost"));
    assert!(!harness.has_log("events"));
    harness.finish().await;
}

// ------------------------------------------------------------------ 取消竞态

#[tokio::test]
async fn cancel_while_the_question_is_in_flight_wins_the_race() {
    let harness = Harness::new("ask_race").await;
    harness.mock.ask_hold.store(true, Ordering::SeqCst);
    let service = harness.service.clone();
    let asking = tokio::spawn(async move {
        service
            .ask(AssistantAskParams {
                session_id: Some("sess-1".to_owned()),
                text: "hi".to_owned(),
                ..AssistantAskParams::default()
            })
            .await
    });
    harness
        .eventually("question in flight", |h| !lock(&h.mock.asks).is_empty())
        .await;
    harness
        .service
        .cancel(AssistantSessionIdParams {
            session_id: "sess-1".to_owned(),
        })
        .await
        .expect("cancel");
    let result = asking
        .await
        .expect("ask task")
        .expect("a cancelled question is not an error");
    assert_eq!(result.session_id, "sess-1");
    assert_eq!(harness.run().status, AssistantRunStatus::Canceled);
    // 取消方一次,被取消的提问自己再通知一次(云端可能已受理)。
    harness
        .eventually("both cloud cancels", |h| {
            h.mock.cancels.load(Ordering::SeqCst) == 2
        })
        .await;
    harness.finish().await;
}

#[tokio::test]
async fn cloud_cancel_failure_after_a_local_cancel_is_only_logged() {
    let harness = Harness::new("cancel_cloud_down").await;
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.ask().await;
    *lock(&harness.mock.cancel_status) = Some(StatusCode::INTERNAL_SERVER_ERROR);
    let params = || AssistantSessionIdParams {
        session_id: "sess-1".to_owned(),
    };
    harness
        .service
        .cancel(params())
        .await
        .expect("the local cancel already succeeded");
    assert_eq!(harness.run().status, AssistantRunStatus::Canceled);
    // 本机没有进行中的执行可取消:云端的失败就是答案。
    harness
        .service
        .cancel(params())
        .await
        .expect_err("nothing was cancelled locally");
    harness.finish().await;
}

#[tokio::test]
async fn every_run_end_refreshes_the_daily_usage() {
    let harness = Harness::new("usage").await;
    let used = |h: &Harness| h.service.state().config.map(|config| config.used_today);
    *lock(&harness.mock.config) = json!({"available": true, "usedToday": 1});
    harness.service.refresh().await.expect("refresh");
    assert_eq!(used(&harness), Some(1));

    *lock(&harness.mock.config) = json!({"available": true, "usedToday": 2});
    harness.send(json!({"type": "done", "status": "completed", "text": "hi"}));
    harness.ask().await;
    harness
        .eventually("usage after a finished run", |h| used(h) == Some(2))
        .await;

    // 被取消的执行同样计入当日额度。
    *lock(&harness.mock.config) = json!({"available": true, "usedToday": 3});
    harness.next_stream();
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.ask().await;
    harness
        .service
        .cancel(AssistantSessionIdParams {
            session_id: "sess-1".to_owned(),
        })
        .await
        .expect("cancel");
    harness
        .eventually("usage after a cancelled run", |h| used(h) == Some(3))
        .await;
    harness.finish().await;
}

// ------------------------------------------------------------------ 确认卡片预览 / 中断

#[tokio::test]
async fn confirmation_card_carries_the_executor_preview() {
    let harness = Harness::build(
        "preview",
        Setup {
            preview: Some(("delete", true)),
            ..Setup::default()
        },
    )
    .await;
    harness
        .pending_write_call("fd_manage_tasks", "write", 30)
        .await;
    let call = harness.tool("r1");
    let preview = call.preview.expect("the card shows what will change");
    assert_eq!(preview.action, "delete");
    assert_eq!(preview.tasks, ["a.iso"]);
    assert_eq!(preview.more_tasks, 3);
    assert!(call.destructive);
    harness.finish().await;
}

/// 批准一次会阻塞的写调用,等它开始执行后取消整次执行,等工具收到取消。
async fn cancelled_write_call(label: &str, interrupt: bool) -> Harness {
    let harness = Harness::build(
        label,
        Setup {
            block: true,
            interrupt,
            ..Setup::default()
        },
    )
    .await;
    harness
        .pending_write_call("fd_create_tasks", "write", 30)
        .await;
    harness
        .service
        .confirm(confirm("r1", true))
        .await
        .expect("confirm");
    harness.tools.started.notified().await;
    harness
        .service
        .cancel(AssistantSessionIdParams {
            session_id: "sess-1".to_owned(),
        })
        .await
        .expect("cancel");
    harness
        .eventually("tool cancellation", |h| {
            h.tools.cancelled.load(Ordering::SeqCst)
        })
        .await;
    harness
}

#[tokio::test]
async fn write_tool_stopped_mid_flight_is_marked_interrupted() {
    let harness = cancelled_write_call("interrupted", true).await;
    harness
        .eventually("interrupted mark", |h| h.tool("r1").interrupted)
        .await;
    assert_eq!(harness.tool("r1").status, AssistantToolStatus::Failed);
    harness.finish().await;
}

#[tokio::test]
async fn tool_cancelled_without_an_outcome_unknown_flag_is_not_marked_interrupted() {
    let harness = cancelled_write_call("not_interrupted", false).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!harness.tool("r1").interrupted);
    harness.finish().await;
}

// ------------------------------------------------------------------ 会话管理 RPC

#[tokio::test]
async fn rename_validates_the_title_and_routes_to_the_cloud() {
    let harness = Harness::new("rename").await;
    let rename = |title: String| AssistantRenameParams {
        session_id: "sess-7".to_owned(),
        title,
    };
    let renamed = harness
        .service
        .rename_session(rename("  New title ".to_owned()))
        .await
        .expect("rename");
    assert_eq!(
        (renamed.id.as_str(), renamed.title.as_str()),
        ("sess-7", "New title")
    );
    assert_eq!(
        *lock(&harness.mock.renames),
        [json!({"title": "New title"})]
    );

    // 长度按字符数算,不是字节数:80 个汉字合法。
    let long = "题".repeat(ASSISTANT_MAX_TITLE_CHARS);
    harness
        .service
        .rename_session(rename(long))
        .await
        .expect("a title of exactly the maximum length is accepted");

    for title in ["   ".to_owned(), "x".repeat(ASSISTANT_MAX_TITLE_CHARS + 1)] {
        let error = harness
            .service
            .rename_session(rename(title))
            .await
            .expect_err("invalid title");
        assert_eq!(error.data.code, ApplicationErrorCode::InvalidArgument);
        assert_eq!(error.data.field.as_deref(), Some("title"));
    }
    let error = harness
        .service
        .rename_session(AssistantRenameParams {
            session_id: " ".to_owned(),
            title: "ok".to_owned(),
        })
        .await
        .expect_err("empty session id");
    assert_eq!(error.data.field.as_deref(), Some("sessionId"));
    assert_eq!(
        lock(&harness.mock.renames).len(),
        2,
        "invalid titles never reach the cloud"
    );
    harness.finish().await;
}

#[tokio::test]
async fn export_and_search_route_to_the_cloud() {
    let harness = Harness::new("export").await;
    let exported = harness
        .service
        .export_session(AssistantExportParams {
            session_id: "sess-7".to_owned(),
            format: AssistantExportFormat::Json,
        })
        .await
        .expect("export");
    assert_eq!(
        (
            exported.file_name.as_str(),
            exported.mime.as_str(),
            exported.content.as_str()
        ),
        ("chat.md", "text/markdown", "# chat")
    );
    harness
        .service
        .export_session(AssistantExportParams {
            session_id: "sess-7".to_owned(),
            format: AssistantExportFormat::default(),
        })
        .await
        .expect("default export");

    // `query` 去空白后以 `q` 传给云端;空白查询等于不搜;超长查询截到上限。
    for query in ["  hello world ", "   "] {
        harness
            .service
            .sessions(AssistantSessionsParams {
                limit: Some(5),
                offset: Some(10),
                query: Some(query.to_owned()),
            })
            .await
            .expect("sessions");
    }
    harness
        .service
        .sessions(AssistantSessionsParams {
            query: Some("a".repeat(300)),
            ..AssistantSessionsParams::default()
        })
        .await
        .expect("sessions");
    assert_eq!(
        *lock(&harness.mock.queries),
        [
            "export sess-7?format=json".to_owned(),
            "export sess-7?format=markdown".to_owned(),
            "sessions?limit=5&offset=10&q=hello%20world".to_owned(),
            "sessions?limit=5&offset=10".to_owned(),
            format!("sessions?q={}", "a".repeat(200)),
        ]
    );
    harness.finish().await;
}

#[tokio::test]
async fn attachment_returns_a_data_url_with_the_response_mime_and_respects_the_cap() {
    let harness = Harness::new("attachment").await;
    let fetch = |id: &str| {
        harness.service.attachment(AssistantAttachmentParams {
            attachment_id: id.to_owned(),
        })
    };
    *lock(&harness.mock.attachment) = Some(("image/png".to_owned(), vec![1, 2, 3]));
    assert_eq!(
        fetch("att-1").await.expect("attachment").data_url,
        "data:image/png;base64,AQID"
    );
    // MIME 取自响应头(去掉参数),不是写死 png。
    *lock(&harness.mock.attachment) =
        Some(("image/jpeg; charset=binary".to_owned(), vec![1, 2, 3]));
    assert_eq!(
        fetch("att-1").await.expect("attachment").data_url,
        "data:image/jpeg;base64,AQID"
    );
    // 只放行云端接受的图片类型。
    *lock(&harness.mock.attachment) = Some(("text/html".to_owned(), vec![1, 2, 3]));
    fetch("att-1").await.expect_err("unsupported type");
    // 体积上限。
    *lock(&harness.mock.attachment) = Some(("image/png".to_owned(), vec![0; 6 * 1024 * 1024 + 1]));
    fetch("att-1").await.expect_err("oversized attachment");
    // 云端不认的 ID。
    *lock(&harness.mock.attachment) = None;
    fetch("missing").await.expect_err("unknown attachment");
    let error = fetch(" ").await.expect_err("empty id");
    assert_eq!(error.data.field.as_deref(), Some("attachmentId"));
    harness.finish().await;
}

// ------------------------------------------------------------------ 本机独自要求确认(云端声明成只读)

/// 云端把 `fd_create_tasks`(本机判定为写入)声明成只读且不给确认窗口:只有本机要求确认。
async fn pending_local_only_call(harness: &Harness, timeout_secs: u64) {
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.send(
        json!({"type": "toolStarted", "callId": "c1", "name": "fd_create_tasks", "kind": "client"}),
    );
    harness.send(json!({
        "type": "clientToolCall", "requestId": "r1", "name": "fd_create_tasks", "version": 1,
        "arguments": {"tasks": []}, "effect": "read", "timeoutSecs": timeout_secs
    }));
    harness.ask().await;
    harness
        .eventually("pending local confirmation", |h| {
            h.service.state().runs.last().is_some_and(|run| {
                run.items.iter().any(|item| {
                    matches!(item, AssistantRunItemDto::Tool { call }
                        if call.confirmation == Some(AssistantConfirmationState::Pending))
                })
            })
        })
        .await;
}

#[tokio::test]
async fn floor_upgraded_read_call_runs_after_a_local_approval_without_the_cloud_confirmation() {
    let harness = Harness::new("floor_approve").await;
    pending_local_only_call(&harness, 60).await;
    assert!(lock(&harness.tools.calls).is_empty());
    harness
        .service
        .confirm(confirm("r1", true))
        .await
        .expect("approve");
    harness
        .eventually("result report", |h| h.has_log("result run-1 r1"))
        .await;
    let log = harness.log();
    assert!(
        !log.iter().any(|entry| entry.starts_with("confirm ")),
        "no cloud confirmation may be sent: {log:?}"
    );
    assert_eq!(
        log,
        vec![
            "exec fd_create_tasks".to_owned(),
            "result run-1 r1".to_owned()
        ]
    );
    assert_eq!(lock(&harness.mock.results)[0]["isError"], json!(false));
    harness.finish().await;
}

#[tokio::test]
async fn floor_upgraded_read_call_declined_locally_reports_a_failure_result() {
    let harness = Harness::new("floor_decline").await;
    pending_local_only_call(&harness, 60).await;
    harness
        .service
        .confirm(confirm("r1", false))
        .await
        .expect("decline");
    harness
        .eventually("failure result", |h| h.has_log("result run-1 r1"))
        .await;
    let result = lock(&harness.mock.results)[0].clone();
    assert_eq!(result["isError"], json!(true));
    assert!(
        result["output"]
            .as_str()
            .is_some_and(|text| text.contains("declined")),
        "{result}"
    );
    assert!(lock(&harness.tools.calls).is_empty());
    assert!(!harness.has_log("confirm "));
    assert_eq!(
        harness.tool("r1").confirmation,
        Some(AssistantConfirmationState::Declined)
    );
    harness.finish().await;
}

#[tokio::test]
async fn floor_upgraded_read_call_expiring_locally_reports_a_failure_result() {
    let harness = Harness::new("floor_expire").await;
    // 确认窗口不会长过云端等结果的时限(1 秒)。
    pending_local_only_call(&harness, 1).await;
    harness
        .eventually("expiry result", |h| h.has_log("result run-1 r1"))
        .await;
    let result = lock(&harness.mock.results)[0].clone();
    assert_eq!(result["isError"], json!(true));
    assert!(
        result["output"]
            .as_str()
            .is_some_and(|text| text.contains("did not confirm")),
        "{result}"
    );
    assert!(lock(&harness.tools.calls).is_empty());
    assert_eq!(
        harness.tool("r1").confirmation,
        Some(AssistantConfirmationState::Expired)
    );
    harness
        .service
        .confirm(confirm("r1", true))
        .await
        .expect_err("an expired call cannot be confirmed");
    harness.finish().await;
}

// ------------------------------------------------------------------ 未知 effect / 读不懂的调用

#[tokio::test]
async fn unknown_effect_is_treated_as_write_and_needs_confirmation() {
    let harness = Harness::new("unknown_effect").await;
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.send(json!({
        "type": "clientToolCall", "requestId": "r1", "name": "fd_get_app_info", "version": 1,
        "arguments": {}, "effect": "teleport", "timeoutSecs": 60
    }));
    harness.ask().await;
    harness
        .eventually("pending confirmation", |h| {
            h.service.state().runs.last().is_some_and(|run| {
                run.items.iter().any(|item| {
                    matches!(item, AssistantRunItemDto::Tool { call }
                        if call.confirmation == Some(AssistantConfirmationState::Pending))
                })
            })
        })
        .await;
    assert_eq!(harness.tool("r1").effect, AssistantToolEffect::Write);
    assert!(lock(&harness.tools.calls).is_empty());
    assert!(!harness.has_log("result "));
    harness.finish().await;
}

#[tokio::test]
async fn unreadable_tool_call_with_a_request_id_reports_a_failure_result() {
    let harness = Harness::new("unreadable_call").await;
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    // 缺 `arguments`:整帧解析失败,但拿得到 requestId。
    harness.send(json!({
        "type": "clientToolCall", "requestId": "r9", "name": "fd_get_app_info", "version": 1,
        "effect": "read", "timeoutSecs": 20
    }));
    harness.ask().await;
    harness
        .eventually("failure result", |h| h.has_log("result run-1 r9"))
        .await;
    let result = lock(&harness.mock.results)[0].clone();
    assert_eq!(result["isError"], json!(true));
    assert!(
        result["output"]
            .as_str()
            .is_some_and(|text| text.contains("could not read")),
        "{result}"
    );
    assert!(lock(&harness.tools.calls).is_empty());
    harness.finish().await;
}

// ------------------------------------------------------------------ 事件流丢失后核实

#[tokio::test]
async fn lost_stream_is_settled_from_the_persisted_answer() {
    let harness = Harness::new("settle").await;
    *lock(&harness.mock.detail) = Some(detail_json(json!([
        {"id": "m7", "runId": "run-1", "seq": 2, "role": "assistant",
         "content": "Hello there", "createdAt": "2026-01-01T00:00:00Z"}
    ])));
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.ask().await;
    harness.close_stream();
    harness
        .eventually("settled run", |h| {
            h.run().status == AssistantRunStatus::Completed
        })
        .await;
    let run = harness.run();
    assert_eq!(run.message_id.as_deref(), Some("m7"));
    assert!(run.error.is_none());
    assert!(
        run.items.iter().any(
            |item| matches!(item, AssistantRunItemDto::Text { text } if text == "Hello there")
        )
    );
    harness.finish().await;
}

#[tokio::test]
async fn lost_stream_stays_failed_when_the_cloud_has_no_answer_for_the_run() {
    let harness = Harness::new("settle_none").await;
    *lock(&harness.mock.detail) = Some(detail_json(json!([
        {"id": "m3", "runId": "run-0", "seq": 2, "role": "assistant",
         "content": "older", "createdAt": "2026-01-01T00:00:00Z"}
    ])));
    harness.send(json!({"type": "run", "runId": "run-1", "sessionId": "sess-1"}));
    harness.ask().await;
    harness.close_stream();
    harness
        .eventually("failed run", |h| {
            h.run().status == AssistantRunStatus::Failed
        })
        .await;
    let run = harness.run();
    assert_eq!(run.error_reason.as_deref(), Some("streamLost"));
    assert!(run.message_id.is_none());
    harness.finish().await;
}

// ------------------------------------------------------------------ 旁观别的设备发起的执行

#[tokio::test]
async fn a_run_started_elsewhere_is_joined_as_an_observer_exactly_once() {
    let harness = Harness::new("observe").await;
    // 旧云端 / 没有进行中的执行:会话没有 activeRunId,什么都不接回。
    *lock(&harness.mock.sessions) = Some(json!({"total": 1, "items": [session_json(None)]}));
    harness
        .service
        .sessions(AssistantSessionsParams::default())
        .await
        .expect("sessions");
    assert!(harness.service.state().runs.is_empty());

    *lock(&harness.mock.sessions) =
        Some(json!({"total": 1, "items": [session_json(Some("run-9"))]}));
    *lock(&harness.mock.resume) = VecDeque::from([(
        StatusCode::OK,
        sse_body(&[
            (
                1,
                json!({"type": "run", "runId": "run-9", "sessionId": "sess-1"}),
            ),
            (2, json!({"type": "text", "delta": "Hi"})),
            (
                3,
                json!({"type": "toolStarted", "callId": "c1", "name": "fd_create_tasks", "kind": "client"}),
            ),
            (
                4,
                json!({
                    "type": "clientToolCall", "requestId": "ro", "name": "fd_create_tasks",
                    "version": 1, "arguments": {"tasks": []}, "effect": "write",
                    "confirmTimeoutSecs": 120, "timeoutSecs": 60
                }),
            ),
            (
                5,
                json!({"type": "done", "status": "completed", "messageId": "m1", "text": "Hi"}),
            ),
        ]),
    )]);
    harness
        .service
        .sessions(AssistantSessionsParams::default())
        .await
        .expect("sessions");
    harness
        .eventually("observed run finished", |h| {
            h.service
                .state()
                .runs
                .last()
                .is_some_and(|run| run.status == AssistantRunStatus::Completed)
        })
        .await;
    let run = harness.run();
    assert!(run.observing);
    assert_eq!(run.run_id.as_deref(), Some("run-9"));
    // 旁观:不挂起确认(不计待确认角标),本机不执行它的工具。
    assert_eq!(harness.tool("ro").confirmation, None);
    assert!(lock(&harness.tools.calls).is_empty());
    let error = harness
        .service
        .confirm(confirm("ro", true))
        .await
        .expect_err("an observed run cannot be confirmed here");
    assert!(
        error.message.contains("another device"),
        "{}",
        error.message
    );

    // 同一执行不重复接回。
    harness
        .service
        .sessions(AssistantSessionsParams::default())
        .await
        .expect("sessions again");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let joined = harness
        .log()
        .iter()
        .filter(|entry| entry.starts_with("events run-9"))
        .count();
    assert_eq!(joined, 1);
    assert_eq!(harness.service.state().runs.len(), 1);
    assert!(!harness.has_log("confirm ") && !harness.has_log("result "));
    harness.finish().await;
}

#[tokio::test]
async fn an_observed_run_adopted_from_the_list_gets_its_question_when_the_session_is_opened() {
    let harness = Harness::new("observe_fill").await;
    *lock(&harness.mock.sessions) =
        Some(json!({"total": 1, "items": [session_json(Some("run-9"))]}));
    // 事件流只推一帧 `run`:读完后进入续读退避,执行在测试期间保持进行中。
    *lock(&harness.mock.resume) = VecDeque::from([(
        StatusCode::OK,
        sse_body(&[(
            1,
            json!({"type": "run", "runId": "run-9", "sessionId": "sess-1"}),
        )]),
    )]);
    harness
        .service
        .sessions(AssistantSessionsParams::default())
        .await
        .expect("sessions");
    harness
        .eventually("observer joined", |h| h.has_log("events run-9"))
        .await;
    let run = harness.run();
    assert!(run.observing);
    assert_eq!(run.question, "");
    assert!(run.image_ids.is_empty());

    // 打开这个会话:详情里有进行中执行的提问与附图。
    let mut detail = detail_json(json!([
        {"id": "u0", "runId": "run-8", "seq": 1, "role": "user",
         "content": "an older question", "createdAt": "2026-01-01T00:00:00Z"},
        {"id": "u1", "runId": "run-9", "seq": 3, "role": "user", "content": "what is up",
         "images": ["att-1", "att-2"], "createdAt": "2026-01-01T00:00:00Z"}
    ]));
    detail["session"] = session_json(Some("run-9"));
    *lock(&harness.mock.detail) = Some(detail);
    harness
        .service
        .session(AssistantSessionIdParams {
            session_id: "sess-1".to_owned(),
        })
        .await
        .expect("session detail");
    let run = harness.run();
    assert_eq!(run.question, "what is up");
    assert_eq!(run.image_ids, ["att-1", "att-2"]);
    assert!(run.observing);
    // 仍是同一个旁观执行:没有重复接回。
    assert_eq!(harness.service.state().runs.len(), 1);
    let joined = harness
        .log()
        .iter()
        .filter(|entry| entry.as_str() == "events run-9 after=0")
        .count();
    assert_eq!(joined, 1);
    harness.finish().await;
}
