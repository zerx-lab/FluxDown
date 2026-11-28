//! AI 助手服务测试：axum 搭的 mock FluxCloud + 替身工具执行器，覆盖 SSE 事件到 `AssistantRunDto`
//! 的映射、确认 / 执行 / 结果回传的顺序、拒绝不执行、取消与断线对工具的取消、账号切换清空。

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use fluxdown_protocol::{
    ApplicationErrorCode, AssistantAskParams, AssistantAskResult, AssistantConfirmParams,
    AssistantConfirmationState, AssistantRunDto, AssistantRunItemDto, AssistantRunStatus,
    AssistantSessionIdParams, AssistantStateDto, AssistantToolCallDto, AssistantToolEffect,
    AssistantToolStatus,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use super::cloud::{self, AiCloud, RunEvent, SseParser};
use super::tools::{ToolCall, ToolOutput};
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
        parser.pop().as_deref(),
        Some(b"{\"type\":\"text\",\n\"delta\":\"a\"}".as_slice())
    );
    assert_eq!(parser.pop().as_deref(), Some(b"b".as_slice()));
    assert!(parser.pop().is_none());
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
    assert!(cloud::parse_event(b"not json").is_err());
}

#[test]
fn cloud_errors_map_to_application_codes() {
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
        ),
        (
            error(403, "ai_disabled", false),
            ApplicationErrorCode::Unsupported,
        ),
        (
            error(403, "ai_quota_exceeded", false),
            ApplicationErrorCode::Conflict,
        ),
        (
            error(409, "conflict", false),
            ApplicationErrorCode::Conflict,
        ),
        (
            error(429, "rate_limited", true),
            ApplicationErrorCode::Unavailable,
        ),
    ];
    for (cloud_error, code) in cases {
        let status = cloud_error.status;
        let mapped = AssistantError::from(cloud_error);
        assert_eq!(mapped.data.code, code, "status {status:?}");
        assert_eq!(mapped.message, "cloud text");
    }
    assert!(
        AssistantError::from(error(429, "rate_limited", true))
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
    /// 云端与执行器共用的事件日志，用来断言先后顺序。
    log: Log,
    created: Mutex<Vec<Value>>,
    asks: Mutex<Vec<Value>>,
    results: Mutex<Vec<Value>>,
    stream: Mutex<Option<mpsc::UnboundedReceiver<Bytes>>>,
    ask_failure: Mutex<Option<(StatusCode, Value)>>,
    confirm_status: Mutex<Option<StatusCode>>,
    cancels: AtomicUsize,
    config: Mutex<Value>,
    status: Mutex<Value>,
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
    StatusCode::NO_CONTENT
}

// ------------------------------------------------------------------ 替身执行器

struct FakeTools {
    log: Log,
    /// `true`：执行器一直等到取消令牌触发才返回（模拟长时间运行的工具）。
    block: bool,
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

    async fn execute(&self, call: ToolCall, cancel: CancellationToken) -> ToolOutput {
        lock(&self.log).push(format!("exec {}", call.name));
        lock(&self.calls).push(call);
        self.started.notify_one();
        if self.block {
            cancel.cancelled().await;
            self.cancelled.store(true, Ordering::SeqCst);
            return ToolOutput {
                output: "cancelled".to_owned(),
                is_error: true,
            };
        }
        ToolOutput {
            output: "tool output".to_owned(),
            is_error: false,
        }
    }
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
        Self::build(label, false).await
    }

    /// 工具一直阻塞到被取消。
    async fn blocking(label: &str) -> Self {
        Self::build(label, true).await
    }

    async fn build(label: &str, block: bool) -> Self {
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
            .route("/api/v1/ai/sessions", post(mock_create_session))
            .route("/api/v1/ai/sessions/{id}/messages", post(mock_ask))
            .route("/api/v1/ai/sessions/{id}/cancel", post(mock_cancel))
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
            block,
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
    assert_eq!(published, harness.service.state());
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
    assert_eq!(error.message, "Daily limit reached");
    let state = harness.service.state();
    assert!(state.runs.is_empty(), "{state:?}");
    assert_eq!(
        state.last_error_message.as_deref(),
        Some("Daily limit reached")
    );
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
    assert!(harness.run().error.is_some());
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
