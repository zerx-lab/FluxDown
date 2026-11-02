//! 行为测试：mock FluxCloud（axum）+ 真实 `CloudApi` / 事件 hub / 状态存储 + 假 daemon。
//!
//! 云名单信封一律由测试自生成的 ed25519 密钥签发（`list::fixtures`），不涉及生产私钥。
//! 「什么都没发生」的断言靠 [`Signals`] 的轮数计数器确定性地等到事件处理完，不靠睡眠猜测。

use std::collections::{BTreeMap, VecDeque};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, BtGuardCloudApplyDto, BtGuardCloudApplyParams,
    BtGuardCloudStatusDto, BtGuardEvidenceItemDto, BtGuardStateDto, DaemonConfigSnapshot,
    DaemonEvent, DaemonSnapshot,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::daemon::{DaemonError, GuardDaemon};
use super::evidence::{BATCH_SIZE, RETAIN_CAP, UploadTiming};
use super::list::{MAX_ENVELOPE_BYTES, fixtures};
use super::sync::{CloudListSync, LIST_PATH, RetryTiming, SyncOutcome};
use super::{BtGuardWorker, Signals, WorkerParts};
use crate::cloud::{CloudApi, CloudClient};
use crate::egress::Egress;
use crate::event_hub::AgentEventHub;
use crate::signed_envelope::sign_envelope;
use crate::state::{AgentState, CloudCredentials, StateStore};

type Timeline = Arc<Mutex<Vec<String>>>;

// ---------------------------------------------------------------------------
// mock FluxCloud
// ---------------------------------------------------------------------------

#[derive(Default)]
enum ListBody {
    #[default]
    Document,
    /// 带 `Content-Length` 的超限响应体。
    Oversize,
    /// 无 `Content-Length`、分块流式的超限响应体。
    OversizeChunked,
}

#[derive(Default)]
struct ListState {
    doc: Option<(Vec<u8>, String)>,
    status: Option<u16>,
    body: ListBody,
    requests: u32,
    conditional_hits: u32,
}

#[derive(Default)]
struct EvidenceState {
    /// 按请求顺序消费的状态码（0 = 成功）。
    scripted: VecDeque<u16>,
    /// 脚本耗尽后的状态码（`None` = 成功）。
    always: Option<u16>,
    requests: u32,
    delivered: Vec<Value>,
    auth: Vec<String>,
}

#[derive(Default)]
struct MockCloud {
    list: Mutex<ListState>,
    evidence: Mutex<EvidenceState>,
    timeline: Timeline,
}

impl MockCloud {
    fn serve(&self, sequence: u64) {
        self.serve_raw(fixtures::envelope(sequence), format!("\"btg-{sequence}\""));
    }

    fn serve_raw(&self, bytes: Vec<u8>, etag: String) {
        self.list.lock().expect("list lock").doc = Some((bytes, etag));
    }

    fn set_list_status(&self, status: Option<u16>) {
        self.list.lock().expect("list lock").status = status;
    }

    fn set_list_body(&self, body: ListBody) {
        self.list.lock().expect("list lock").body = body;
    }

    fn list_requests(&self) -> u32 {
        self.list.lock().expect("list lock").requests
    }

    fn conditional_hits(&self) -> u32 {
        self.list.lock().expect("list lock").conditional_hits
    }

    fn script_evidence(&self, statuses: &[u16]) {
        self.evidence
            .lock()
            .expect("evidence lock")
            .scripted
            .extend(statuses.iter().copied());
    }

    fn evidence_always(&self, status: Option<u16>) {
        self.evidence.lock().expect("evidence lock").always = status;
    }

    fn evidence_requests(&self) -> u32 {
        self.evidence.lock().expect("evidence lock").requests
    }

    /// 已成功接收的请求体。
    fn delivered(&self) -> Vec<Value> {
        self.evidence
            .lock()
            .expect("evidence lock")
            .delivered
            .clone()
    }

    fn delivered_batch_sizes(&self) -> Vec<usize> {
        self.delivered()
            .iter()
            .map(|body| body["items"].as_array().map_or(0, Vec::len))
            .collect()
    }

    /// 已成功接收的全部证据条目的 `deliveredBytes`（测试用它当序号）。
    fn delivered_ids(&self) -> Vec<u64> {
        self.delivered()
            .iter()
            .flat_map(|body| body["items"].as_array().cloned().unwrap_or_default())
            .map(|item| item["deliveredBytes"].as_u64().expect("deliveredBytes"))
            .collect()
    }
}

async fn list_route(State(cloud): State<Arc<MockCloud>>, headers: HeaderMap) -> Response {
    let mut list = cloud.list.lock().expect("list lock");
    list.requests += 1;
    cloud
        .timeline
        .lock()
        .expect("timeline lock")
        .push("list".to_owned());
    if let Some(status) = list.status {
        return StatusCode::from_u16(status)
            .expect("valid status")
            .into_response();
    }
    match list.body {
        ListBody::Oversize => return vec![b' '; MAX_ENVELOPE_BYTES + 1].into_response(),
        ListBody::OversizeChunked => {
            let chunks = (0..17).map(|_| Ok::<_, std::convert::Infallible>(vec![b' '; 1 << 20]));
            return Body::from_stream(futures_util::stream::iter(chunks)).into_response();
        }
        ListBody::Document => {}
    }
    let Some((bytes, etag)) = list.doc.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let presented = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    if presented == Some(etag.as_str()) {
        list.conditional_hits += 1;
        return (StatusCode::NOT_MODIFIED, [(ETAG, etag)]).into_response();
    }
    ([(ETAG, etag)], bytes).into_response()
}

async fn evidence_route(
    State(cloud): State<Arc<MockCloud>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let mut state = cloud.evidence.lock().expect("evidence lock");
    state.requests += 1;
    let auth = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    state.auth.push(auth);
    let status = state
        .scripted
        .pop_front()
        .or(state.always)
        .unwrap_or_default();
    if status != 0 {
        return (
            StatusCode::from_u16(status).expect("valid status"),
            Json(json!({"code": "scripted", "message": "scripted failure"})),
        )
            .into_response();
    }
    let count = body["items"].as_array().map_or(0, Vec::len);
    state.delivered.push(body);
    Json(json!({"accepted": count, "rejected": 0})).into_response()
}

// ---------------------------------------------------------------------------
// 假 daemon
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum ApplyMode {
    Accept,
    Reject(String),
    Fail,
}

struct DaemonState {
    mode: ApplyMode,
    applied: Vec<BtGuardCloudApplyParams>,
    /// daemon 当前持有的云名单序号（接受一份文档后更新）。
    held: u64,
    drains: u32,
    drain_results: VecDeque<Result<Vec<BtGuardEvidenceItemDto>, DaemonError>>,
    state_calls: u32,
    session_active: bool,
}

struct FakeDaemon {
    state: Mutex<DaemonState>,
    timeline: Timeline,
}

impl FakeDaemon {
    fn new(timeline: Timeline, session_active: bool) -> Self {
        Self {
            state: Mutex::new(DaemonState {
                mode: ApplyMode::Accept,
                applied: Vec::new(),
                held: 0,
                drains: 0,
                drain_results: VecDeque::new(),
                state_calls: 0,
                session_active,
            }),
            timeline,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DaemonState> {
        self.state.lock().expect("daemon lock")
    }

    fn set_mode(&self, mode: ApplyMode) {
        self.lock().mode = mode;
    }

    fn applied(&self) -> Vec<BtGuardCloudApplyParams> {
        self.lock().applied.clone()
    }

    fn applied_sequences(&self) -> Vec<u64> {
        self.lock().applied.iter().map(|p| p.sequence).collect()
    }

    fn push_drain(&self, items: Vec<BtGuardEvidenceItemDto>) {
        self.lock().drain_results.push_back(Ok(items));
    }

    fn push_drain_error(&self, message: &str) {
        self.lock().drain_results.push_back(Err(message.to_owned()));
    }

    fn drains(&self) -> u32 {
        self.lock().drains
    }

    fn state_calls(&self) -> u32 {
        self.lock().state_calls
    }
}

#[async_trait]
impl GuardDaemon for FakeDaemon {
    async fn apply_cloud(
        &self,
        params: &BtGuardCloudApplyParams,
    ) -> Result<BtGuardCloudApplyDto, DaemonError> {
        let mut state = self.lock();
        self.timeline
            .lock()
            .expect("timeline lock")
            .push(format!("apply:{}", params.sequence));
        state.applied.push(params.clone());
        match state.mode.clone() {
            ApplyMode::Accept => {
                state.held = params.sequence;
                Ok(BtGuardCloudApplyDto {
                    applied: true,
                    rejected_reason: None,
                    status: BtGuardCloudStatusDto {
                        sequence: params.sequence,
                        ..BtGuardCloudStatusDto::default()
                    },
                })
            }
            ApplyMode::Reject(reason) => Ok(BtGuardCloudApplyDto {
                applied: false,
                rejected_reason: Some(reason),
                status: BtGuardCloudStatusDto::default(),
            }),
            ApplyMode::Fail => Err("daemon unavailable".to_owned()),
        }
    }

    async fn drain_evidence(&self) -> Result<Vec<BtGuardEvidenceItemDto>, DaemonError> {
        let mut state = self.lock();
        state.drains += 1;
        state
            .drain_results
            .pop_front()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    async fn state(&self) -> Result<BtGuardStateDto, DaemonError> {
        let mut state = self.lock();
        state.state_calls += 1;
        Ok(BtGuardStateDto {
            enabled: true,
            session_active: state.session_active,
            cloud: BtGuardCloudStatusDto {
                sequence: state.held,
                ..BtGuardCloudStatusDto::default()
            },
            ..BtGuardStateDto::default()
        })
    }
}

fn items(range: Range<u64>) -> Vec<BtGuardEvidenceItemDto> {
    range
        .map(|id| BtGuardEvidenceItemDto {
            ip_group: "1.2.3.4".to_owned(),
            rule: "excessive".to_owned(),
            strength: "strong".to_owned(),
            info_hash: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            torrent_size: 1 << 30,
            delivered_bytes: id,
            ratio_milli: 3700,
            first_seen: 1_761_900_000,
            last_seen: 1_761_903_600,
            ..BtGuardEvidenceItemDto::default()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 测试台
// ---------------------------------------------------------------------------

fn slow_list_retry() -> RetryTiming {
    // 测试期间不会触发的退避（生产值 30 min）。
    RetryTiming::PRODUCTION
}

fn fast_list_retry() -> RetryTiming {
    RetryTiming {
        first: Duration::from_millis(250),
        cap: Duration::from_millis(500),
    }
}

fn fast_upload_retry() -> UploadTiming {
    UploadTiming {
        first: Duration::from_millis(80),
        cap: Duration::from_millis(160),
    }
}

struct Options {
    logged_in: bool,
    config: Vec<(&'static str, &'static str)>,
    list_retry: RetryTiming,
    upload_retry: UploadTiming,
    cache_seed: Option<Vec<u8>>,
    /// 启动时 hub 里 daemon 已连上。
    connected: bool,
    session_active: bool,
    /// 工作循环启动前云端就已发布的文档序号。
    serve: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            logged_in: true,
            config: Vec::new(),
            list_retry: slow_list_retry(),
            upload_retry: UploadTiming::PRODUCTION,
            cache_seed: None,
            connected: false,
            session_active: true,
            serve: None,
        }
    }
}

struct Harness {
    events: AgentEventHub,
    cloud_api: CloudApi,
    mock: Arc<MockCloud>,
    daemon: Arc<FakeDaemon>,
    signals: Arc<Signals>,
    config: Mutex<BTreeMap<String, String>>,
    revision: std::sync::atomic::AtomicU64,
    dir: PathBuf,
    cancel: CancellationToken,
    worker: tokio::task::JoinHandle<()>,
    server: tokio::task::JoinHandle<()>,
}

async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// 给「不该再发生」的断言留出若干个重试周期的观察窗口。
async fn observe() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

impl Harness {
    async fn start(options: Options) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock cloud");
        let address = listener.local_addr().expect("mock cloud address");
        let timeline = Timeline::default();
        let mock = Arc::new(MockCloud {
            timeline: timeline.clone(),
            ..MockCloud::default()
        });
        if let Some(sequence) = options.serve {
            mock.serve(sequence);
        }
        let router = Router::new()
            .route(LIST_PATH, get(list_route))
            .route("/api/v1/bt-guard/evidence", post(evidence_route))
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::debug!(%error, "mock cloud stopped");
            }
        });

        let dir = std::env::temp_dir().join(format!(
            "fluxdown_bt_guard_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let cache_dir = dir.join("bt_guard");
        if let Some(seed) = &options.cache_seed {
            tokio::fs::create_dir_all(&cache_dir)
                .await
                .expect("create cache dir");
            tokio::fs::write(cache_dir.join("cloud.json"), seed)
                .await
                .expect("seed cache");
        }
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let initial = AgentState {
            device_id: "device-1".to_owned(),
            credentials: options.logged_in.then(|| CloudCredentials {
                access_token: "access".to_owned(),
                refresh_token: "refresh".to_owned(),
                expires_at_unix: i64::MAX,
                session: None,
            }),
            ..AgentState::default()
        };
        store.save(&initial).await.expect("save state");
        let state = Arc::new(tokio::sync::Mutex::new(initial));
        let events = AgentEventHub::new(AgentSnapshot {
            daemon_connected: options.connected,
            daemon: DaemonSnapshot {
                config: DaemonConfigSnapshot {
                    revision: 1,
                    values: options
                        .config
                        .iter()
                        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                        .collect(),
                },
                ..DaemonSnapshot::default()
            },
            ..AgentSnapshot::default()
        });
        let cloud_api = CloudApi::new(
            CloudClient::new(format!("http://{address}"), state, store)
                .expect("cloud client")
                .with_events(events.clone()),
        );
        let daemon = Arc::new(FakeDaemon::new(timeline, options.session_active));
        let worker = BtGuardWorker::build(WorkerParts {
            daemon: daemon.clone(),
            cloud: cloud_api.clone(),
            events: events.clone(),
            egress: Egress::system_default(),
            cache_dir,
            keys: fixtures::keys(),
            list_retry: options.list_retry,
            upload_retry: options.upload_retry,
        });
        let signals = worker.signals.clone();
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(worker.run(cancel.clone()));
        let config = options
            .config
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        let harness = Self {
            events,
            cloud_api,
            mock,
            daemon,
            signals,
            config: Mutex::new(config),
            revision: std::sync::atomic::AtomicU64::new(1),
            dir,
            cancel,
            worker,
            server,
        };
        // 两个工作循环都已进入第一轮等待，之后读到的轮数基线才可靠。
        eventually("the worker loops to start", || {
            harness.cloud_rounds() >= 1 && harness.evidence_rounds() >= 1
        })
        .await;
        harness
    }

    fn cache_path(&self) -> PathBuf {
        self.dir.join("bt_guard").join("cloud.json")
    }

    async fn cached(&self) -> Option<Vec<u8>> {
        tokio::fs::read(self.cache_path()).await.ok()
    }

    fn cloud_rounds(&self) -> u64 {
        self.signals.cloud_rounds.load(Ordering::SeqCst)
    }

    fn evidence_rounds(&self) -> u64 {
        self.signals.evidence_rounds.load(Ordering::SeqCst)
    }

    /// daemon 请求刷新云名单，并等到这一轮处理完。
    async fn refresh(&self, have_sequence: u64) {
        let before = self.cloud_rounds();
        self.events
            .apply_daemon_event(DaemonEvent::BtGuardCloudRefreshWanted { have_sequence });
        eventually("the refresh round", || self.cloud_rounds() > before).await;
    }

    /// daemon 通知有待上报证据，并等到这一轮处理完。
    async fn evidence_ready(&self) {
        let before = self.evidence_rounds();
        self.events
            .apply_daemon_event(DaemonEvent::BtGuardEvidenceReady { count: 1 });
        eventually("the evidence round", || self.evidence_rounds() > before).await;
    }

    fn set_config(&self, key: &str, value: &str) {
        let values = {
            let mut config = self.config.lock().expect("config lock");
            config.insert(key.to_owned(), value.to_owned());
            config.clone()
        };
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        self.events
            .publish(AgentEvent::Daemon(DaemonEvent::ConfigChanged(
                DaemonConfigSnapshot { revision, values },
            )));
    }

    async fn finish(self) {
        self.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.worker)
            .await
            .expect("worker stops on cancel")
            .expect("worker did not panic");
        self.server.abort();
        if let Err(error) = tokio::fs::remove_dir_all(&self.dir).await {
            tracing::debug!(%error, "bt guard test cleanup");
        }
    }
}

// ---------------------------------------------------------------------------
// 云名单
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_request_fetches_verifies_applies_and_caches_the_raw_envelope() {
    let harness = Harness::start(Options::default()).await;
    harness.mock.serve(5);

    harness.refresh(0).await;

    let applied = harness.daemon.applied();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].sequence, 5);
    assert_eq!(applied[0].envelope.as_bytes(), fixtures::envelope(5));
    assert_eq!(applied[0].verified_payload["sequence"], 5);
    assert_eq!(
        applied[0].verified_payload["entries"][0]["cidr"],
        "1.2.3.4/32"
    );
    assert_eq!(harness.cached().await, Some(fixtures::envelope(5)));
    assert_eq!(harness.mock.list_requests(), 1);
    assert_eq!(harness.mock.conditional_hits(), 0);
    harness.finish().await;
}

#[tokio::test]
async fn unchanged_document_is_a_304_and_is_not_applied_again() {
    let harness = Harness::start(Options::default()).await;
    harness.mock.serve(5);
    harness.refresh(0).await;

    harness.refresh(5).await;
    harness.refresh(5).await;

    assert_eq!(harness.mock.list_requests(), 3);
    assert_eq!(harness.mock.conditional_hits(), 2);
    assert_eq!(harness.daemon.applied_sequences(), [5]);
    harness.finish().await;
}

#[tokio::test]
async fn newer_sequence_is_applied_and_replaces_the_cache() {
    let harness = Harness::start(Options::default()).await;
    harness.mock.serve(5);
    harness.refresh(0).await;
    harness.mock.serve(6);

    harness.refresh(5).await;

    assert_eq!(harness.daemon.applied_sequences(), [5, 6]);
    assert_eq!(harness.cached().await, Some(fixtures::envelope(6)));
    // 新文档带新验证器：再次刷新是 304。
    harness.refresh(6).await;
    assert_eq!(harness.mock.conditional_hits(), 1);
    assert_eq!(harness.daemon.applied_sequences(), [5, 6]);
    harness.finish().await;
}

#[tokio::test]
async fn rolled_back_sequence_is_rejected_and_the_previous_cache_stays() {
    let harness = Harness::start(Options::default()).await;
    harness.mock.serve(10);
    harness.refresh(0).await;
    harness.mock.serve(9);

    harness.refresh(10).await;

    assert_eq!(harness.daemon.applied_sequences(), [10]);
    assert_eq!(harness.cached().await, Some(fixtures::envelope(10)));
    assert_eq!(harness.mock.list_requests(), 2);
    harness.finish().await;
}

#[tokio::test]
async fn sequence_below_the_daemons_own_document_is_a_rollback_even_without_a_cache() {
    let harness = Harness::start(Options::default()).await;
    harness.mock.serve(3);

    harness.refresh(7).await;

    assert!(harness.daemon.applied().is_empty());
    assert!(harness.cached().await.is_none());
    assert_eq!(harness.mock.list_requests(), 1);
    harness.finish().await;
}

#[tokio::test]
async fn document_equal_to_the_daemons_is_cached_but_not_applied() {
    let harness = Harness::start(Options::default()).await;
    harness.mock.serve(4);

    harness.refresh(4).await;

    assert!(harness.daemon.applied().is_empty());
    assert_eq!(harness.cached().await, Some(fixtures::envelope(4)));
    harness.finish().await;
}

#[tokio::test]
async fn untrusted_or_malformed_documents_are_never_applied_or_cached() {
    let wrong_key = sign_envelope(
        &fixtures::payload(8),
        "btguard-test-1",
        &fixtures::signing(9),
    );
    let unknown_key = sign_envelope(
        &fixtures::payload(8),
        "btguard-other",
        &fixtures::signing(7),
    );
    let wrong_format = sign_envelope(
        &serde_json::to_vec(&json!({"format": "fluxdown-sniff-pack", "sequence": 8}))
            .expect("json"),
        "btguard-test-1",
        &fixtures::signing(7),
    );
    let no_sequence = sign_envelope(
        &serde_json::to_vec(&json!({"format": "fluxdown-bt-guard-list/1"})).expect("json"),
        "btguard-test-1",
        &fixtures::signing(7),
    );
    let mut tampered: Value = serde_json::from_slice(&fixtures::envelope(8)).expect("json");
    let other: Value = serde_json::from_slice(&fixtures::envelope(9)).expect("json");
    tampered["payload"] = other["payload"].clone();
    let tampered = serde_json::to_vec(&tampered).expect("json");
    let cases: [(&str, Vec<u8>); 6] = [
        ("signed by a key that does not match the keyId", wrong_key),
        ("unknown keyId", unknown_key),
        ("wrong format", wrong_format),
        ("missing sequence", no_sequence),
        ("payload swapped after signing", tampered),
        ("not an envelope", b"<html>captive portal</html>".to_vec()),
    ];
    for (name, bytes) in cases {
        let harness = Harness::start(Options::default()).await;
        harness.mock.serve_raw(bytes, "\"x\"".to_owned());

        harness.refresh(0).await;

        assert_eq!(harness.mock.list_requests(), 1, "{name}");
        assert!(harness.daemon.applied().is_empty(), "{name}");
        assert!(harness.cached().await.is_none(), "{name}");
        harness.finish().await;
    }
}

#[tokio::test]
async fn oversized_responses_are_aborted_with_or_without_content_length() {
    for body in [ListBody::Oversize, ListBody::OversizeChunked] {
        let harness = Harness::start(Options::default()).await;
        harness.mock.set_list_body(body);

        harness.refresh(0).await;

        assert_eq!(harness.mock.list_requests(), 1);
        assert!(harness.daemon.applied().is_empty());
        assert!(harness.cached().await.is_none());
        harness.finish().await;
    }
}

#[tokio::test]
async fn disabled_cloud_list_neither_touches_the_network_nor_the_daemon() {
    let harness = Harness::start(Options {
        config: vec![("bt_guard_cloud_enabled", "false")],
        cache_seed: Some(fixtures::envelope(5)),
        ..Options::default()
    })
    .await;
    harness.mock.serve(6);

    harness.refresh(0).await;

    assert_eq!(harness.mock.list_requests(), 0);
    assert!(harness.daemon.applied().is_empty());
    assert_eq!(harness.daemon.state_calls(), 0);

    // 重新开启后才会联网（此时才读缓存、先交付缓存）。
    harness.set_config("bt_guard_cloud_enabled", "true");
    harness.refresh(0).await;
    assert_eq!(harness.mock.list_requests(), 1);
    assert_eq!(harness.daemon.applied_sequences(), [5, 6]);
    harness.finish().await;
}

#[tokio::test]
async fn cloud_list_defaults_to_enabled_when_the_setting_is_not_loaded_yet() {
    let harness = Harness::start(Options::default()).await;
    harness.mock.serve(2);

    harness.refresh(0).await;

    assert_eq!(harness.daemon.applied_sequences(), [2]);
    harness.finish().await;
}

#[tokio::test]
async fn cached_document_is_reverified_and_applied_before_fetching() {
    let harness = Harness::start(Options {
        cache_seed: Some(fixtures::envelope(5)),
        ..Options::default()
    })
    .await;
    harness.mock.serve(5);

    harness.refresh(0).await;

    // 先交付缓存，再联网；服务端同序号 → 不重复交付。
    let timeline = harness.mock.timeline.lock().expect("timeline lock").clone();
    assert_eq!(timeline, ["apply:5", "list"]);
    assert_eq!(harness.daemon.applied_sequences(), [5]);
    // 重启后第一次 200 补上验证器：之后是 304。
    harness.refresh(5).await;
    assert_eq!(harness.mock.conditional_hits(), 1);
    assert_eq!(harness.daemon.applied_sequences(), [5]);
    harness.finish().await;
}

#[tokio::test]
async fn cached_document_newer_than_the_server_blocks_a_rollback() {
    let harness = Harness::start(Options {
        cache_seed: Some(fixtures::envelope(8)),
        ..Options::default()
    })
    .await;
    harness.mock.serve(6);

    harness.refresh(0).await;

    assert_eq!(harness.daemon.applied_sequences(), [8]);
    assert_eq!(harness.cached().await, Some(fixtures::envelope(8)));
    harness.finish().await;
}

#[tokio::test]
async fn tampered_cache_is_discarded_and_replaced_by_the_fetched_document() {
    let forged = sign_envelope(
        &fixtures::payload(99),
        "btguard-test-1",
        &fixtures::signing(9),
    );
    let harness = Harness::start(Options {
        cache_seed: Some(forged),
        ..Options::default()
    })
    .await;
    harness.mock.serve(6);

    harness.refresh(0).await;

    // 伪造的 99 号缓存既不交付、也不构成防回滚基线。
    assert_eq!(harness.daemon.applied_sequences(), [6]);
    assert_eq!(harness.cached().await, Some(fixtures::envelope(6)));
    harness.finish().await;
}

#[tokio::test]
async fn oversized_cache_is_deleted_without_being_trusted_or_applied() {
    let harness = Harness::start(Options {
        cache_seed: Some(vec![b' '; MAX_ENVELOPE_BYTES + 1]),
        ..Options::default()
    })
    .await;
    harness.mock.serve(6);

    harness.refresh(0).await;

    // 超限缓存既不构成基线也不交付；之后落盘的是服务端的有效文档。
    assert_eq!(harness.daemon.applied_sequences(), [6]);
    assert_eq!(harness.cached().await, Some(fixtures::envelope(6)));
    harness.finish().await;
}

#[tokio::test]
async fn garbage_cache_is_deleted_even_when_the_network_is_down() {
    let harness = Harness::start(Options {
        cache_seed: Some(b"{not json".to_vec()),
        ..Options::default()
    })
    .await;
    harness.mock.set_list_status(Some(503));

    harness.refresh(0).await;

    assert!(harness.daemon.applied().is_empty());
    assert!(harness.cached().await.is_none());
    harness.finish().await;
}

#[tokio::test]
async fn cache_is_not_redelivered_when_the_daemon_already_holds_it() {
    let harness = Harness::start(Options {
        cache_seed: Some(fixtures::envelope(5)),
        ..Options::default()
    })
    .await;
    harness.mock.serve(5);

    harness.refresh(5).await;

    assert!(harness.daemon.applied().is_empty());
    assert_eq!(harness.mock.list_requests(), 1);
    harness.finish().await;
}

#[tokio::test]
async fn daemon_rejection_is_not_cached_and_is_not_a_failure() {
    let harness = Harness::start(Options::default()).await;
    harness
        .daemon
        .set_mode(ApplyMode::Reject("diff guard".to_owned()));
    harness.mock.serve(5);

    harness.refresh(0).await;
    // 被拒不进入退避：下一次请求照常联网。
    harness.refresh(0).await;

    assert_eq!(harness.daemon.applied_sequences(), [5, 5]);
    assert_eq!(harness.mock.list_requests(), 2);
    assert!(harness.cached().await.is_none());
    harness.finish().await;
}

#[tokio::test]
async fn rejected_cache_is_not_delivered_over_and_over() {
    let harness = Harness::start(Options {
        cache_seed: Some(fixtures::envelope(5)),
        ..Options::default()
    })
    .await;
    harness
        .daemon
        .set_mode(ApplyMode::Reject("expired".to_owned()));
    harness.mock.serve(5);

    harness.refresh(0).await;
    harness.refresh(0).await;

    assert_eq!(harness.daemon.applied_sequences(), [5]);
    harness.finish().await;
}

#[tokio::test]
async fn failure_backs_off_so_further_requests_do_not_hit_the_network() {
    let harness = Harness::start(Options::default()).await;
    harness.mock.set_list_status(Some(503));

    harness.refresh(0).await;
    harness.refresh(0).await;
    harness.refresh(0).await;

    assert_eq!(harness.mock.list_requests(), 1);
    // 退避窗口内的请求也不会去问 daemon（重试计时才会）。
    assert_eq!(harness.daemon.state_calls(), 0);
    harness.finish().await;
}

#[tokio::test]
async fn backoff_retry_runs_without_a_new_event_while_the_daemon_still_has_a_session() {
    let harness = Harness::start(Options {
        list_retry: fast_list_retry(),
        ..Options::default()
    })
    .await;
    harness.mock.set_list_status(Some(503));

    harness.refresh(0).await;
    assert_eq!(harness.mock.list_requests(), 1);
    // 服务端在重试到来之前恢复。
    harness.mock.set_list_status(None);
    harness.mock.serve(4);

    eventually("the backoff retry to apply the document", || {
        harness.daemon.applied_sequences() == [4]
    })
    .await;
    assert!(harness.daemon.state_calls() >= 1);
    // 成功后不再有重试。
    let requests = harness.mock.list_requests();
    observe().await;
    assert_eq!(harness.mock.list_requests(), requests);
    harness.finish().await;
}

#[tokio::test]
async fn backoff_retry_is_dropped_when_the_daemon_no_longer_has_a_bt_session() {
    let harness = Harness::start(Options {
        list_retry: fast_list_retry(),
        session_active: false,
        ..Options::default()
    })
    .await;
    harness.mock.set_list_status(Some(503));

    harness.refresh(0).await;
    eventually("the retry timer to consult the daemon", || {
        harness.daemon.state_calls() >= 1
    })
    .await;
    observe().await;

    assert_eq!(harness.mock.list_requests(), 1);
    assert!(harness.daemon.applied().is_empty());
    harness.finish().await;
}

#[tokio::test]
async fn disabling_the_cloud_list_cancels_a_pending_retry() {
    let harness = Harness::start(Options {
        list_retry: fast_list_retry(),
        ..Options::default()
    })
    .await;
    harness.mock.set_list_status(Some(503));
    harness.refresh(0).await;
    harness.set_config("bt_guard_cloud_enabled", "false");

    observe().await;

    assert_eq!(harness.mock.list_requests(), 1);
    assert_eq!(harness.daemon.state_calls(), 0);
    harness.finish().await;
}

#[tokio::test]
async fn connecting_to_a_daemon_with_a_live_bt_session_requests_the_list() {
    let harness = Harness::start(Options::default()).await;
    harness.mock.serve(3);
    let before = harness.cloud_rounds();

    harness
        .events
        .publish(AgentEvent::DaemonConnectionChanged(true));
    eventually("the probe round", || harness.cloud_rounds() > before).await;

    assert_eq!(harness.daemon.state_calls(), 1);
    assert_eq!(harness.daemon.applied_sequences(), [3]);
    harness.finish().await;
}

#[tokio::test]
async fn already_connected_agent_probes_once_at_startup() {
    let harness = Harness::start(Options {
        connected: true,
        serve: Some(3),
        ..Options::default()
    })
    .await;

    eventually("the startup probe to apply the list", || {
        harness.daemon.applied_sequences() == [3]
    })
    .await;
    assert_eq!(harness.daemon.state_calls(), 1);
    harness.finish().await;
}

#[tokio::test]
async fn lagged_event_stream_wakes_the_uploader_but_not_the_cloud_loop() {
    use fluxdown_protocol::{EventFrame, ServiceEvent};

    let (frames, receiver) = tokio::sync::broadcast::channel(2);
    for sequence in 1..=5 {
        frames
            .send(EventFrame {
                epoch: "e".to_owned(),
                sequence,
                previous: None,
                event: ServiceEvent::Agent(AgentEvent::DaemonConnectionChanged(false)),
            })
            .expect("receiver is alive");
    }
    let signals = Arc::new(Signals::default());
    let cancel = CancellationToken::new();
    let dispatcher = tokio::spawn({
        let signals = signals.clone();
        let cancel = cancel.clone();
        async move { super::dispatch(receiver, &signals, &cancel).await }
    });

    // 容量 2 的队列丢了 3 帧：上报侧被唤醒一次（条件不满足时它什么都不做）。
    tokio::time::timeout(Duration::from_secs(5), signals.evidence_wake.notified())
        .await
        .expect("uploader wake after lag");
    // 其余两帧是无关事件，云名单侧没有被唤醒。
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), signals.cloud_wake.notified())
            .await
            .is_err()
    );

    cancel.cancel();
    dispatcher.await.expect("dispatcher stops");
}

#[tokio::test]
async fn probing_a_daemon_without_a_bt_session_stays_offline() {
    let harness = Harness::start(Options {
        session_active: false,
        ..Options::default()
    })
    .await;
    harness.mock.serve(3);
    let before = harness.cloud_rounds();

    harness
        .events
        .publish(AgentEvent::DaemonConnectionChanged(true));
    eventually("the probe round", || harness.cloud_rounds() > before).await;

    assert_eq!(harness.daemon.state_calls(), 1);
    assert_eq!(harness.mock.list_requests(), 0);
    harness.finish().await;
}

#[tokio::test]
async fn disconnect_events_do_not_probe() {
    let harness = Harness::start(Options::default()).await;
    let before = harness.cloud_rounds();

    harness
        .events
        .publish(AgentEvent::DaemonConnectionChanged(false));
    observe().await;

    assert_eq!(harness.cloud_rounds(), before);
    assert_eq!(harness.daemon.state_calls(), 0);
    harness.finish().await;
}

// ---- 状态机层：退避递增 ----

#[tokio::test]
async fn backoff_doubles_to_the_cap_and_resets_after_a_success() {
    let timeline = Timeline::default();
    let mock = Arc::new(MockCloud {
        timeline: timeline.clone(),
        ..MockCloud::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let router = Router::new()
        .route(LIST_PATH, get(list_route))
        .with_state(mock.clone());
    let server = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router).await {
            tracing::debug!(%error, "mock cloud stopped");
        }
    });
    let dir = std::env::temp_dir().join(format!("fluxdown_bt_guard_sync_{}", uuid::Uuid::new_v4()));
    let timing = RetryTiming {
        first: Duration::from_millis(60),
        cap: Duration::from_millis(150),
    };
    let mut sync = CloudListSync::new(
        dir.clone(),
        fixtures::keys(),
        Egress::system_default(),
        timing,
    );
    let daemon = FakeDaemon::new(timeline, true);
    mock.set_list_status(Some(503));

    let Failed { retry_in, .. } = failed(sync.refresh(&base, 0, &daemon).await);
    assert_eq!(retry_in, Duration::from_millis(60));
    assert_eq!(sync.failures(), 1);

    // 窗口内：不联网。
    assert!(matches!(
        sync.refresh(&base, 0, &daemon).await,
        SyncOutcome::BackedOff { .. }
    ));
    assert_eq!(mock.list_requests(), 1);

    tokio::time::sleep(Duration::from_millis(70)).await;
    let Failed { retry_in, .. } = failed(sync.refresh(&base, 0, &daemon).await);
    assert_eq!(retry_in, Duration::from_millis(120));
    assert_eq!(sync.failures(), 2);

    tokio::time::sleep(Duration::from_millis(130)).await;
    let Failed { retry_in, .. } = failed(sync.refresh(&base, 0, &daemon).await);
    assert_eq!(retry_in, Duration::from_millis(150));
    assert_eq!(sync.failures(), 3);

    tokio::time::sleep(Duration::from_millis(160)).await;
    mock.set_list_status(None);
    mock.serve(2);
    assert_eq!(
        sync.refresh(&base, 0, &daemon).await,
        SyncOutcome::Applied { sequence: 2 }
    );
    assert_eq!(sync.failures(), 0);
    assert_eq!(sync.cached_sequence(), Some(2));
    assert_eq!(mock.list_requests(), 4);

    server.abort();
    if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
        tracing::debug!(%error, "bt guard test cleanup");
    }
}

struct Failed {
    retry_in: Duration,
}

fn failed(outcome: SyncOutcome) -> Failed {
    match outcome {
        SyncOutcome::Failed { retry_in, .. } => Failed { retry_in },
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test]
async fn daemon_failure_while_delivering_is_a_failure_and_keeps_the_cache_unwritten() {
    let harness = Harness::start(Options::default()).await;
    harness.daemon.set_mode(ApplyMode::Fail);
    harness.mock.serve(5);

    harness.refresh(0).await;
    // 退避窗口内不再重复调用 daemon。
    harness.refresh(0).await;

    assert_eq!(harness.daemon.applied_sequences(), [5]);
    assert!(harness.cached().await.is_none());
    harness.finish().await;
}

// ---------------------------------------------------------------------------
// 证据上报
// ---------------------------------------------------------------------------

fn report_on() -> Vec<(&'static str, &'static str)> {
    vec![("bt_guard_report_enabled", "true")]
}

#[tokio::test]
async fn evidence_is_drained_and_posted_in_batches_of_two_hundred() {
    let harness = Harness::start(Options {
        config: report_on(),
        ..Options::default()
    })
    .await;
    harness.daemon.push_drain(items(0..450));

    harness.evidence_ready().await;

    assert_eq!(harness.daemon.drains(), 1);
    assert_eq!(
        harness.mock.delivered_batch_sizes(),
        [BATCH_SIZE, BATCH_SIZE, 50]
    );
    assert_eq!(harness.mock.delivered_ids(), (0..450).collect::<Vec<_>>());
    let first = &harness.mock.delivered()[0];
    assert_eq!(first["clientVersion"], fluxdown_protocol::APP_VERSION);
    assert!(
        first["clientVersion"]
            .as_str()
            .is_some_and(|v| v.chars().count() <= 32)
    );
    assert_eq!(first["items"][0]["ipGroup"], "1.2.3.4");
    assert_eq!(first["items"][0]["rule"], "excessive");
    assert_eq!(
        first["items"][0]["infoHash"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(first["items"][0]["ratioMilli"], 3700);
    // 用已登录用户的鉴权客户端发出。
    let auth = harness
        .mock
        .evidence
        .lock()
        .expect("evidence lock")
        .auth
        .clone();
    assert!(
        auth.iter().all(|value| value == "Bearer access"),
        "{auth:?}"
    );
    harness.finish().await;
}

#[tokio::test]
async fn exactly_one_batch_boundary_is_not_split_or_padded() {
    let harness = Harness::start(Options {
        config: report_on(),
        ..Options::default()
    })
    .await;
    harness.daemon.push_drain(items(0..200));
    harness.evidence_ready().await;
    harness.daemon.push_drain(items(200..201));
    harness.evidence_ready().await;

    assert_eq!(harness.mock.delivered_batch_sizes(), [200, 1]);
    harness.finish().await;
}

#[tokio::test]
async fn logged_out_agent_never_drains() {
    let harness = Harness::start(Options {
        logged_in: false,
        config: report_on(),
        ..Options::default()
    })
    .await;
    harness.daemon.push_drain(items(0..5));

    harness.evidence_ready().await;
    harness.evidence_ready().await;

    assert_eq!(harness.daemon.drains(), 0);
    assert_eq!(harness.mock.evidence_requests(), 0);
    harness.finish().await;
}

#[tokio::test]
async fn reporting_is_off_by_default_and_never_drains() {
    let harness = Harness::start(Options::default()).await;
    harness.daemon.push_drain(items(0..5));

    harness.evidence_ready().await;

    assert_eq!(harness.daemon.drains(), 0);
    assert_eq!(harness.mock.evidence_requests(), 0);

    // 用户开启上报后，下一次通知才弹出并上报。
    harness.set_config("bt_guard_report_enabled", "true");
    harness.evidence_ready().await;
    assert_eq!(harness.daemon.drains(), 1);
    assert_eq!(harness.mock.delivered_ids(), [0, 1, 2, 3, 4]);
    harness.finish().await;
}

#[tokio::test]
async fn explicitly_disabled_reporting_never_drains() {
    let harness = Harness::start(Options {
        config: vec![("bt_guard_report_enabled", "false")],
        ..Options::default()
    })
    .await;

    harness.evidence_ready().await;

    assert_eq!(harness.daemon.drains(), 0);
    harness.finish().await;
}

#[tokio::test]
async fn failed_batches_are_retained_and_retried_without_duplicates() {
    let harness = Harness::start(Options {
        config: report_on(),
        upload_retry: fast_upload_retry(),
        ..Options::default()
    })
    .await;
    harness.mock.script_evidence(&[503, 503]);
    harness.daemon.push_drain(items(0..250));

    harness.evidence_ready().await;

    eventually("all evidence delivered by retries", || {
        harness.mock.delivered_ids().len() == 250
    })
    .await;
    assert_eq!(harness.mock.delivered_ids(), (0..250).collect::<Vec<_>>());
    assert_eq!(harness.mock.delivered_batch_sizes(), [200, 50]);
    // 2 次失败 + 2 个成功批次；成功后不再发送。
    observe().await;
    assert_eq!(harness.mock.evidence_requests(), 4);
    harness.finish().await;
}

#[tokio::test]
async fn retention_is_capped_and_keeps_the_newest_evidence() {
    let harness = Harness::start(Options {
        config: report_on(),
        // 重试计时不在测试期间触发：每个事件恰好一次尝试。
        upload_retry: UploadTiming::PRODUCTION,
        ..Options::default()
    })
    .await;
    harness.mock.evidence_always(Some(503));
    harness.daemon.push_drain(items(0..700));
    harness.evidence_ready().await;
    harness.daemon.push_drain(items(700..1400));
    harness.evidence_ready().await;
    assert_eq!(harness.mock.evidence_requests(), 2);
    assert!(harness.mock.delivered().is_empty());

    harness.mock.evidence_always(None);
    harness.evidence_ready().await;

    let ids = harness.mock.delivered_ids();
    assert_eq!(ids.len(), RETAIN_CAP);
    assert_eq!(ids.first().copied(), Some(400));
    assert_eq!(ids.last().copied(), Some(1399));
    assert_eq!(harness.mock.delivered_batch_sizes(), [BATCH_SIZE; 5]);
    harness.finish().await;
}

#[tokio::test]
async fn logging_out_discards_retained_evidence_and_cancels_the_retry() {
    let harness = Harness::start(Options {
        config: report_on(),
        upload_retry: fast_upload_retry(),
        ..Options::default()
    })
    .await;
    harness.mock.evidence_always(Some(503));
    harness.daemon.push_drain(items(0..10));
    harness.evidence_ready().await;
    assert!(harness.mock.evidence_requests() >= 1);

    harness.cloud_api.clear_session().await.expect("log out");
    let at_logout = harness.mock.evidence_requests();
    // 登出后即便云端恢复，也不会再发送已保留的证据，重试也已取消。
    harness.mock.evidence_always(None);
    observe().await;

    assert_eq!(harness.mock.evidence_requests(), at_logout);
    assert!(harness.mock.delivered().is_empty());
    harness.finish().await;
}

#[tokio::test]
async fn disabling_reporting_discards_retained_evidence() {
    let harness = Harness::start(Options {
        config: report_on(),
        upload_retry: UploadTiming::PRODUCTION,
        ..Options::default()
    })
    .await;
    harness.mock.evidence_always(Some(503));
    harness.daemon.push_drain(items(0..10));
    harness.evidence_ready().await;
    harness.mock.evidence_always(None);

    harness.set_config("bt_guard_report_enabled", "false");
    harness.evidence_ready().await;
    harness.set_config("bt_guard_report_enabled", "true");
    harness.evidence_ready().await;

    assert!(harness.mock.delivered().is_empty());
    assert_eq!(harness.mock.evidence_requests(), 1);
    harness.finish().await;
}

#[tokio::test]
async fn rejected_batches_are_dropped_and_not_retried() {
    let harness = Harness::start(Options {
        config: report_on(),
        upload_retry: fast_upload_retry(),
        ..Options::default()
    })
    .await;
    harness.mock.evidence_always(Some(400));
    harness.daemon.push_drain(items(0..5));
    harness.evidence_ready().await;
    observe().await;
    assert_eq!(harness.mock.evidence_requests(), 1);

    harness.mock.evidence_always(None);
    harness.daemon.push_drain(items(100..103));
    harness.evidence_ready().await;

    // 被拒的 5 条不会混进后来的批次。
    assert_eq!(harness.mock.delivered_ids(), [100, 101, 102]);
    harness.finish().await;
}

#[tokio::test]
async fn rate_limited_batches_are_retried() {
    let harness = Harness::start(Options {
        config: report_on(),
        upload_retry: fast_upload_retry(),
        ..Options::default()
    })
    .await;
    harness.mock.script_evidence(&[429]);
    harness.daemon.push_drain(items(0..3));

    harness.evidence_ready().await;

    eventually("the 429 batch to be retried", || {
        harness.mock.delivered_ids().len() == 3
    })
    .await;
    harness.finish().await;
}

#[tokio::test]
async fn drain_errors_do_not_stop_later_reports() {
    let harness = Harness::start(Options {
        config: report_on(),
        ..Options::default()
    })
    .await;
    harness.daemon.push_drain_error("daemon unavailable");
    harness.evidence_ready().await;
    assert_eq!(harness.mock.evidence_requests(), 0);

    harness.daemon.push_drain(items(0..2));
    harness.evidence_ready().await;

    assert_eq!(harness.mock.delivered_ids(), [0, 1]);
    harness.finish().await;
}

#[tokio::test]
async fn empty_drain_posts_nothing() {
    let harness = Harness::start(Options {
        config: report_on(),
        ..Options::default()
    })
    .await;

    harness.evidence_ready().await;

    assert_eq!(harness.daemon.drains(), 1);
    assert_eq!(harness.mock.evidence_requests(), 0);
    harness.finish().await;
}

#[tokio::test]
async fn retry_that_ends_in_a_rejection_does_not_leave_a_spinning_timer() {
    let harness = Harness::start(Options {
        config: report_on(),
        upload_retry: fast_upload_retry(),
        ..Options::default()
    })
    .await;
    // 第一次发送被限流（保留重试），重试时同一批被判为非法而丢弃。
    harness.mock.script_evidence(&[429, 400]);
    harness.daemon.push_drain(items(0..5));

    harness.evidence_ready().await;
    eventually("the retry to be rejected", || {
        harness.mock.evidence_requests() == 2
    })
    .await;
    let drains = harness.daemon.drains();
    observe().await;

    // 队列已空：没有残留的重试时刻，工作循环不再空转（不再 drain、不再发请求）。
    assert_eq!(harness.mock.evidence_requests(), 2);
    assert!(harness.daemon.drains() <= drains + 1);
    assert!(harness.mock.delivered().is_empty());
    harness.finish().await;
}
