//! 云端推送服务的行为测试：mock FluxCloud（axum）+ 真实 `CloudApi` / 事件 hub / 状态存储。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use fluxdown_protocol::{AgentSnapshot, DaemonEvent, TaskNoticeDto, TaskNoticeTaskDto};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{CloudNotifyService, initial_state, notice_channel, route_daemon_event};
use crate::cloud::{CloudApi, CloudClient};
use crate::event_hub::AgentEventHub;
use crate::state::{AgentState, CloudCredentials, CloudNotifyPrefs, StateStore};

const DEVICE_ID: &str = "device-1";

#[derive(Default)]
struct Mock {
    overview_calls: AtomicUsize,
    report_calls: AtomicUsize,
    /// 前 N 次上报返回的状态码（0 = 正常）；按调用序消费。
    scripted_failures: std::sync::Mutex<Vec<u16>>,
    batches: Mutex<Vec<Value>>,
    overview: std::sync::Mutex<Value>,
    catalog_calls: AtomicUsize,
    /// 带 `Authorization` 的目录请求数（目录是公开接口，应恒为 0）。
    catalog_authed_calls: AtomicUsize,
    catalog_down: std::sync::atomic::AtomicBool,
    catalog: std::sync::Mutex<Value>,
    deliveries: std::sync::Mutex<Value>,
    deliveries_calls: AtomicUsize,
}

fn overview_json(events: &[&str]) -> Value {
    json!({
        "enabled": true,
        "usage": {
            "dailyUsed": 1, "dailyLimit": 20, "monthlyUsed": 1, "monthlyLimit": 300,
            "dailyResetAt": "2026-10-10T16:00:00Z", "monthlyResetAt": "2026-10-31T16:00:00Z"
        },
        "maxChannels": 2,
        "channels": [{
            "id": "c1", "kind": "email", "name": "邮件", "enabled": true,
            "events": events, "target": "****abcd",
            "status": "ok", "createdAt": "2026-10-09T00:00:00Z"
        }],
        "accountEmail": "me@example.com"
    })
}

async fn overview_route(State(mock): State<Arc<Mock>>) -> Json<Value> {
    mock.overview_calls.fetch_add(1, Ordering::SeqCst);
    Json(
        mock.overview
            .lock()
            .map(|value| value.clone())
            .unwrap_or(Value::Null),
    )
}

async fn catalog_route(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    mock.catalog_calls.fetch_add(1, Ordering::SeqCst);
    if headers.contains_key("authorization") {
        mock.catalog_authed_calls.fetch_add(1, Ordering::SeqCst);
    }
    if mock.catalog_down.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"code": "down", "message": "catalog down"})),
        );
    }
    (
        StatusCode::OK,
        Json(
            mock.catalog
                .lock()
                .map(|value| value.clone())
                .unwrap_or(Value::Null),
        ),
    )
}

fn catalog_json(kinds: &[&str]) -> Value {
    json!({ "kinds": kinds.iter().map(|kind| json!({"kind": kind, "available": true})).collect::<Vec<_>>() })
}

fn delivery_json(id: &str, created_at: &str, status: &str) -> Value {
    json!({
        "id": id, "event": "task.completed", "title": "ubuntu.iso",
        "channelId": "c1", "channelKind": "email", "channelName": "邮件",
        "deviceName": "Box", "status": status, "attempts": 1, "maxAttempts": 4,
        "createdAt": created_at
    })
}

fn page_json(items: Vec<Value>, next_cursor: Option<&str>) -> Value {
    json!({ "items": items, "nextCursor": next_cursor })
}

fn delivery_dto(
    id: &str,
    created_at: &str,
    status: &str,
) -> fluxdown_protocol::CloudNotifyDeliveryDto {
    serde_json::from_value(delivery_json(id, created_at, status)).expect("delivery fixture")
}

async fn deliveries_route(State(mock): State<Arc<Mock>>) -> Json<Value> {
    mock.deliveries_calls.fetch_add(1, Ordering::SeqCst);
    let page = mock
        .deliveries
        .lock()
        .map(|value| value.clone())
        .unwrap_or(Value::Null);
    Json(if page.is_null() {
        page_json(Vec::new(), None)
    } else {
        page
    })
}

async fn report(
    State(mock): State<Arc<Mock>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    mock.report_calls.fetch_add(1, Ordering::SeqCst);
    let scripted = mock
        .scripted_failures
        .lock()
        .ok()
        .and_then(|mut failures| (!failures.is_empty()).then(|| failures.remove(0)))
        .unwrap_or(0);
    if scripted != 0 {
        return (
            StatusCode::from_u16(scripted).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"code": "scripted", "message": "scripted failure"})),
        );
    }
    let results: Vec<Value> = body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .map(|event| json!({"deliveryId": event["deliveryId"], "outcome": "accepted"}))
                .collect()
        })
        .unwrap_or_default();
    mock.batches.lock().await.push(body);
    (
        StatusCode::OK,
        Json(json!({
            "results": results,
            "usage": {
                "dailyUsed": 7, "dailyLimit": 20, "monthlyUsed": 9, "monthlyLimit": 300,
                "dailyResetAt": "2026-10-10T16:00:00Z", "monthlyResetAt": "2026-10-31T16:00:00Z"
            }
        })),
    )
}

struct Fixture {
    service: Arc<CloudNotifyService>,
    events: AgentEventHub,
    mock: Arc<Mock>,
    store: Arc<StateStore>,
    notices: super::NoticeSender,
    cancel: CancellationToken,
    dir: std::path::PathBuf,
    server: tokio::task::JoinHandle<()>,
    runner: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start(prefs: CloudNotifyPrefs, overview: Value) -> Self {
        Self::start_with(prefs, overview, true, false).await
    }

    async fn start_with(
        prefs: CloudNotifyPrefs,
        overview: Value,
        logged_in: bool,
        catalog_down: bool,
    ) -> Self {
        Self::start_full(prefs, overview, logged_in, catalog_down, None).await
    }

    async fn start_with_page(page: Value) -> Self {
        Self::start_full(
            on(),
            overview_json(&["task.completed"]),
            true,
            false,
            Some(page),
        )
        .await
    }

    async fn start_full(
        prefs: CloudNotifyPrefs,
        overview: Value,
        logged_in: bool,
        catalog_down: bool,
        page: Option<Value>,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock cloud");
        let address = listener.local_addr().expect("mock cloud address");
        let mock = Arc::new(Mock::default());
        *mock.overview.lock().expect("overview lock") = overview;
        *mock.catalog.lock().expect("catalog lock") = catalog_json(&["email", "telegram"]);
        mock.catalog_down.store(catalog_down, Ordering::SeqCst);
        if let Some(page) = page {
            *mock.deliveries.lock().expect("deliveries lock") = page;
        }
        let router = Router::new()
            .route("/api/v1/notifications/overview", get(overview_route))
            .route("/api/v1/notifications/catalog", get(catalog_route))
            .route("/api/v1/notifications/deliveries", get(deliveries_route))
            .route("/api/v1/notifications/events", post(report))
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::debug!(%error, "mock cloud stopped");
            }
        });
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_cloud_notify_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let initial = AgentState {
            device_id: DEVICE_ID.to_owned(),
            credentials: logged_in.then(|| CloudCredentials {
                access_token: "access".to_owned(),
                refresh_token: "refresh".to_owned(),
                expires_at_unix: i64::MAX,
                session: None,
            }),
            cloud_notify: prefs,
            ..AgentState::default()
        };
        store.save(&initial).await.expect("save state");
        let state = Arc::new(Mutex::new(initial));
        let events = AgentEventHub::new(AgentSnapshot {
            cloud_notify: initial_state(prefs),
            ..AgentSnapshot::default()
        });
        let cloud = CloudApi::new(
            CloudClient::new(format!("http://{address}"), state.clone(), store.clone())
                .expect("cloud client")
                .with_events(events.clone()),
        );
        let cancel = CancellationToken::new();
        let service = Arc::new(CloudNotifyService::new(
            cloud,
            events.clone(),
            state,
            store.clone(),
            cancel.clone(),
        ));
        let (notices, receiver) = notice_channel();
        let runner = tokio::spawn(service.clone().run(receiver));
        let fixture = Self {
            service,
            events,
            mock,
            store,
            notices,
            cancel,
            dir,
            server,
            runner,
        };
        if logged_in {
            fixture.wait_overview().await;
        }
        fixture
    }

    async fn wait_overview(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self
                .events
                .inspect(|snapshot| snapshot.cloud_notify.overview.is_none())
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("overview is pulled after startup");
    }

    fn catalog_kinds(&self) -> Option<Vec<String>> {
        self.events.inspect(|snapshot| {
            snapshot
                .cloud_notify
                .catalog
                .as_ref()
                .map(|kinds| kinds.iter().map(|kind| kind.kind.clone()).collect())
        })
    }

    async fn wait_catalog(&self, expected: Option<&[&str]>) {
        let expected: Option<Vec<String>> =
            expected.map(|kinds| kinds.iter().map(|kind| (*kind).to_owned()).collect());
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.catalog_kinds() != expected {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "catalog never became {expected:?}, now {:?}",
                self.catalog_kinds()
            )
        });
    }

    async fn send(&self, notice: TaskNoticeDto) {
        self.notices.send(notice).await.expect("notice intake open");
    }

    async fn wait_batches(&self, count: usize, within: Duration) {
        tokio::time::timeout(within, async {
            while self.mock.batches.lock().await.len() < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("expected batch reached the mock cloud");
    }

    async fn finish(self) {
        self.cancel.cancel();
        self.server.abort();
        if let Err(error) = self.runner.await {
            tracing::debug!(%error, "notify runner ended");
        }
        drop(self.service);
        drop(self.store);
        if let Err(error) = tokio::fs::remove_dir_all(&self.dir).await {
            tracing::warn!(path = %self.dir.display(), %error, "cloud notify test cleanup failed");
        }
    }
}

fn on() -> CloudNotifyPrefs {
    CloudNotifyPrefs {
        reporting: true,
        ..CloudNotifyPrefs::default()
    }
}

fn notice(id: &str, event: &str) -> TaskNoticeDto {
    TaskNoticeDto {
        delivery_id: id.to_owned(),
        event: event.to_owned(),
        timestamp_ms: 1_700_000_000_000,
        queue_id: "default".to_owned(),
        queue_name: "默认队列".to_owned(),
        task: Some(TaskNoticeTaskDto {
            id: "local-task".to_owned(),
            file_name: "ubuntu.iso".to_owned(),
            url: "https://example.com/private/ubuntu.iso".to_owned(),
            save_dir: "/home/me/Downloads".to_owned(),
            total_bytes: 42,
            status: 3,
            error_message: String::new(),
        }),
    }
}

#[tokio::test]
async fn matching_notices_are_trimmed_batched_and_update_usage() {
    let fixture = Fixture::start(on(), overview_json(&["task.completed", "task.failed"])).await;
    fixture.send(notice("d1", "task.completed")).await;
    fixture.send(notice("d2", "task.failed")).await;
    // 未订阅的事件不入队。
    fixture.send(notice("d3", "task.paused")).await;
    fixture.wait_batches(1, Duration::from_secs(5)).await;

    let batches = fixture.mock.batches.lock().await.clone();
    assert_eq!(batches.len(), 1, "1s 窗口内的事件合成一批");
    let events = batches[0]["events"].as_array().expect("events array");
    let ids: Vec<&str> = events
        .iter()
        .filter_map(|event| event["deliveryId"].as_str())
        .collect();
    assert_eq!(ids, ["d1", "d2"]);
    let task = &events[0]["task"];
    assert_eq!(task["fileName"], "ubuntu.iso");
    assert_eq!(task["totalBytes"], 42);
    assert_eq!(task["status"], 3);
    assert!(task.get("url").is_none(), "默认不上报下载地址");
    assert!(task.get("saveDir").is_none(), "默认不上报保存目录");
    assert!(task.get("id").is_none(), "本地任务 ID 不上报");
    assert_eq!(events[0]["queueName"], "默认队列");

    // 响应里的用量写回投影。
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.events.inspect(|snapshot| {
            snapshot
                .cloud_notify
                .overview
                .as_ref()
                .is_none_or(|overview| overview.usage.daily_used != 7)
        }) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("usage from the report response is projected");
    fixture.finish().await;
}

#[tokio::test]
async fn privacy_switches_add_url_and_save_dir() {
    let prefs = CloudNotifyPrefs {
        reporting: true,
        include_url: true,
        include_save_dir: true,
    };
    let fixture = Fixture::start(prefs, overview_json(&["task.completed"])).await;
    fixture.send(notice("d1", "task.completed")).await;
    fixture.wait_batches(1, Duration::from_secs(5)).await;
    let batches = fixture.mock.batches.lock().await.clone();
    let task = &batches[0]["events"][0]["task"];
    assert_eq!(task["url"], "https://example.com/private/ubuntu.iso");
    assert_eq!(task["saveDir"], "/home/me/Downloads");
    fixture.finish().await;
}

#[tokio::test]
async fn nothing_is_sent_when_reporting_is_off() {
    // 开关关（默认）：即使渠道匹配也不上报。
    let off = Fixture::start(
        CloudNotifyPrefs::default(),
        overview_json(&["task.completed"]),
    )
    .await;
    off.send(notice("d1", "task.completed")).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(off.mock.report_calls.load(Ordering::SeqCst), 0);
    off.finish().await;
}

#[tokio::test]
async fn transient_failures_retry_but_client_errors_do_not() {
    let fixture = Fixture::start(on(), overview_json(&["task.completed"])).await;
    // 第一次 503 → 退避 2s 后重试成功。
    fixture
        .mock
        .scripted_failures
        .lock()
        .expect("script lock")
        .push(503);
    fixture.send(notice("d1", "task.completed")).await;
    fixture.wait_batches(1, Duration::from_secs(8)).await;
    assert_eq!(fixture.mock.report_calls.load(Ordering::SeqCst), 2);

    // 422 属永久失败：只尝试一次，批次丢弃。
    fixture
        .mock
        .scripted_failures
        .lock()
        .expect("script lock")
        .push(422);
    fixture.send(notice("d2", "task.completed")).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(fixture.mock.report_calls.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.mock.batches.lock().await.len(), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn reporting_and_privacy_preferences_persist_locally_and_publish() {
    let fixture = Fixture::start(
        CloudNotifyPrefs::default(),
        overview_json(&["task.completed"]),
    )
    .await;
    let (mut receiver, _) = fixture.events.subscribe_and_snapshot();

    let state = fixture.service.set_reporting(true).await.expect("enable");
    assert!(state.reporting);
    let state = fixture
        .service
        .set_privacy(fluxdown_protocol::CloudNotifyPrivacyParams {
            include_url: true,
            include_save_dir: false,
        })
        .await
        .expect("privacy");
    assert!(state.reporting && state.include_url && !state.include_save_dir);

    let persisted = fixture.store.load().await.expect("reload state");
    assert_eq!(
        persisted.cloud_notify,
        CloudNotifyPrefs {
            reporting: true,
            include_url: true,
            include_save_dir: false
        }
    );
    let mut changes = 0;
    while let Ok(frame) = receiver.try_recv() {
        if matches!(
            frame.event,
            fluxdown_protocol::ServiceEvent::Agent(
                fluxdown_protocol::AgentEvent::CloudNotifyChanged(_)
            )
        ) {
            changes += 1;
        }
    }
    assert!(changes >= 2, "每次偏好变更都整体推送 CloudNotifyChanged");
    fixture.finish().await;
}

#[tokio::test]
async fn refresh_uses_the_cloud_overview_and_clears_after_logout() {
    let fixture = Fixture::start(on(), overview_json(&["task.completed"])).await;
    let state = fixture.service.refresh().await.expect("refresh");
    let overview = state.overview.expect("overview");
    assert!(overview.enabled);
    assert_eq!(overview.channels.len(), 1);
    assert_eq!(overview.account_email, "me@example.com");
    assert!(state.updated_at_unix_ms.is_some());
    assert!(!state.loading);
    assert!(fixture.mock.overview_calls.load(Ordering::SeqCst) >= 2);

    // 登出：会话变化 → 概览与队列清空（偏好保留）。
    fixture
        .service
        .cloud
        .clear_session()
        .await
        .expect("clear session");
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture
            .events
            .inspect(|snapshot| snapshot.cloud_notify.overview.is_some())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("overview cleared after logout");
    assert!(fixture.service.current().reporting, "偏好不随会话清除");
    fixture.finish().await;
}

#[tokio::test]
async fn task_notice_is_never_forwarded_to_ui_subscribers() {
    let events = AgentEventHub::new(AgentSnapshot::default());
    let (mut ui, _) = events.subscribe_and_snapshot();
    let (sender, mut receiver) = notice_channel();
    let (sound_sender, mut sound_receiver) = crate::sound::channel();
    let (failure_sender, mut failure_receiver) = crate::failure_notice::channel();

    route_daemon_event(
        &events,
        &sender,
        Some(&sound_sender),
        Some(&failure_sender),
        DaemonEvent::TaskNotice(notice("d1", "task.completed")),
    );
    assert!(ui.try_recv().is_err(), "TaskNotice 不得出现在 agent 事件流");
    assert_eq!(
        sound_receiver.try_recv().ok(),
        Some(crate::sound::SoundKind::TaskCompleted),
        "提示音模块同样收到该事件"
    );
    let routed = receiver
        .try_recv()
        .expect("notice reaches the cloud module");
    assert_eq!(routed.delivery_id, "d1");
    assert!(
        failure_receiver.try_recv().is_err(),
        "完成事件不进入失败通知"
    );

    route_daemon_event(
        &events,
        &sender,
        Some(&sound_sender),
        Some(&failure_sender),
        DaemonEvent::TaskNotice(notice("d2", "task.failed")),
    );
    assert!(ui.try_recv().is_err(), "失败事件同样不进入 agent 事件流");
    assert_eq!(
        failure_receiver.try_recv().ok().map(|failed| failed.name),
        Some("ubuntu.iso".to_owned()),
        "失败通知模块收到 task.failed"
    );
    assert_eq!(
        receiver.try_recv().ok().map(|routed| routed.delivery_id),
        Some("d2".to_owned()),
        "云端推送照常收到"
    );

    // 其余 daemon 事件照常转发。
    route_daemon_event(
        &events,
        &sender,
        None,
        None,
        DaemonEvent::TaskDeleted {
            task_id: "t".to_owned(),
        },
    );
    assert!(ui.try_recv().is_ok());
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn plugin_view_events_are_forwarded_verbatim_to_ui_subscribers() {
    use fluxdown_protocol::{AgentEvent, PluginViewFrameDto, ServiceEvent, ViewOp};

    let events = AgentEventHub::new(AgentSnapshot::default());
    let (mut ui, _) = events.subscribe_and_snapshot();
    let (sender, mut receiver) = notice_channel();
    let sent = [
        DaemonEvent::PluginViewFrame(PluginViewFrameDto {
            session: "s".to_owned(),
            seq: 1,
            ops: vec![ViewOp::Error {
                message: "boom".to_owned(),
            }],
            effects: Vec::new(),
        }),
        DaemonEvent::PluginViewClosed {
            session: "s".to_owned(),
            reason: "pluginUnloaded".to_owned(),
        },
        DaemonEvent::PluginToast {
            identity: "a@b".to_owned(),
            level: "info".to_owned(),
            text: "hi".to_owned(),
        },
    ];
    for event in sent.iter().cloned() {
        route_daemon_event(&events, &sender, None, None, event);
    }
    for expected in sent {
        let frame = ui
            .try_recv()
            .expect("plugin view event reaches the UI stream");
        let ServiceEvent::Agent(AgentEvent::Daemon(forwarded)) = frame.event else {
            panic!("plugin view events travel as forwarded daemon events");
        };
        assert_eq!(
            serde_json::to_value(&forwarded).expect("forwarded event serializes"),
            serde_json::to_value(&expected).expect("sent event serializes"),
        );
    }
    assert!(
        receiver.try_recv().is_err(),
        "cloud module sees none of them"
    );
}

#[tokio::test]
async fn catalog_is_pulled_anonymously_at_startup_even_when_logged_out() {
    let fixture = Fixture::start_with(on(), overview_json(&["task.completed"]), false, false).await;
    fixture.wait_catalog(Some(&["email", "telegram"])).await;
    assert_eq!(fixture.mock.catalog_authed_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.mock.overview_calls.load(Ordering::SeqCst), 0);
    assert!(
        fixture
            .events
            .inspect(|snapshot| snapshot.cloud_notify.overview.is_none()),
        "未登录没有概览"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn failed_catalog_fetch_keeps_unknown_and_get_retries_until_known() {
    // 启动拉取失败：目录保持 None（未知），不覆盖概览。
    let fixture = Fixture::start_with(on(), overview_json(&["task.completed"]), true, true).await;
    assert_eq!(fixture.catalog_kinds(), None);
    assert!(fixture.service.current().overview.is_some());
    assert_eq!(fixture.service.current().last_error_reason, None);

    // `get` 发现目录为 None：先返回当前状态，同时后台重拉。
    fixture.mock.catalog_down.store(false, Ordering::SeqCst);
    let state = fixture.service.get().await;
    assert!(state.catalog.is_none());
    fixture.wait_catalog(Some(&["email", "telegram"])).await;
    fixture.finish().await;
}

#[tokio::test]
async fn failed_refresh_keeps_the_previous_catalog_and_logout_does_not_clear_it() {
    let fixture = Fixture::start(on(), overview_json(&["task.completed"])).await;
    fixture.wait_catalog(Some(&["email", "telegram"])).await;

    // 失败：保留旧值；概览仍然刷新成功，不受目录失败影响。
    fixture.mock.catalog_down.store(true, Ordering::SeqCst);
    let state = fixture
        .service
        .refresh()
        .await
        .expect("overview refresh ok");
    assert!(state.overview.is_some());
    assert_eq!(
        fixture.catalog_kinds(),
        Some(vec!["email".into(), "telegram".into()])
    );
    assert_eq!(state.last_error_reason, None);

    // 成功：整体替换。
    fixture.mock.catalog_down.store(false, Ordering::SeqCst);
    *fixture.mock.catalog.lock().expect("catalog lock") = catalog_json(&["telegram"]);
    fixture.service.refresh().await.expect("refresh");
    fixture.wait_catalog(Some(&["telegram"])).await;

    // 登出：概览清空，目录是公开信息，保留。
    fixture.service.cloud.clear_session().await.expect("logout");
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture
            .events
            .inspect(|snapshot| snapshot.cloud_notify.overview.is_some())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("overview cleared");
    assert_eq!(fixture.catalog_kinds(), Some(vec!["telegram".into()]));
    fixture.finish().await;
}

#[tokio::test]
async fn empty_catalog_stops_reporting_until_a_kind_is_enabled_again() {
    let fixture = Fixture::start(on(), overview_json(&["task.completed"])).await;
    fixture.wait_catalog(Some(&["email", "telegram"])).await;

    // 管理员关闭全部种类 → SSE `notify.changed` 唤醒刷新 → 目录为 Some(空)。
    *fixture.mock.catalog.lock().expect("catalog lock") = catalog_json(&[]);
    fixture.service.feed().changed();
    fixture.wait_catalog(Some(&[])).await;
    fixture.send(notice("d1", "task.completed")).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        fixture.mock.report_calls.load(Ordering::SeqCst),
        0,
        "目录为空不得上报任何事件"
    );

    // 重新启用：恢复上报。
    *fixture.mock.catalog.lock().expect("catalog lock") = catalog_json(&["email"]);
    fixture.service.feed().changed();
    fixture.wait_catalog(Some(&["email"])).await;
    fixture.send(notice("d2", "task.completed")).await;
    fixture.wait_batches(1, Duration::from_secs(5)).await;
    fixture.finish().await;
}

#[tokio::test]
async fn catalog_changes_are_published_as_cloud_notify_changed() {
    let fixture = Fixture::start(on(), overview_json(&["task.completed"])).await;
    fixture.wait_catalog(Some(&["email", "telegram"])).await;
    let (mut receiver, _) = fixture.events.subscribe_and_snapshot();
    *fixture.mock.catalog.lock().expect("catalog lock") = catalog_json(&["telegram"]);
    fixture.service.refresh().await.expect("refresh");
    let mut saw_catalog = false;
    while let Ok(frame) = receiver.try_recv() {
        if let fluxdown_protocol::ServiceEvent::Agent(
            fluxdown_protocol::AgentEvent::CloudNotifyChanged(state),
        ) = frame.event
        {
            saw_catalog |= state
                .catalog
                .as_ref()
                .is_some_and(|kinds| kinds.len() == 1 && kinds[0].kind == "telegram");
        }
    }
    assert!(saw_catalog, "目录变化要经 CloudNotifyChanged 推送");
    fixture.finish().await;
}

fn recent(fixture: &Fixture) -> Vec<(String, String)> {
    fixture.events.inspect(|snapshot| {
        snapshot
            .cloud_notify
            .recent_deliveries
            .iter()
            .map(|item| (item.id.clone(), item.status.clone()))
            .collect()
    })
}

async fn wait_recent(fixture: &Fixture, expected: &[(&str, &str)]) {
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(id, status)| ((*id).to_owned(), (*status).to_owned()))
        .collect();
    tokio::time::timeout(Duration::from_secs(5), async {
        while recent(fixture) != expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "recent deliveries never became {expected:?}, now {:?}",
            recent(fixture)
        )
    });
}

fn set_page(fixture: &Fixture, page: Value) {
    *fixture.mock.deliveries.lock().expect("deliveries lock") = page;
}

#[tokio::test]
async fn first_page_is_loaded_with_the_overview_and_replaced_by_refresh() {
    let fixture = Fixture::start_with_page(page_json(
        vec![delivery_json("d1", "2026-10-09T12:00:01.000Z", "pending")],
        Some("cursor-1"),
    ))
    .await;
    // 启动即与概览一并拉取第一页（不需要手动刷新）。
    wait_recent(&fixture, &[("d1", "pending")]).await;
    assert_eq!(
        fixture.service.current().recent_next_cursor.as_deref(),
        Some("cursor-1")
    );

    set_page(
        &fixture,
        page_json(
            vec![
                delivery_json("d2", "2026-10-09T12:00:02.000Z", "pending"),
                delivery_json("d1", "2026-10-09T12:00:01.000Z", "sent"),
            ],
            None,
        ),
    );
    fixture.service.refresh().await.expect("refresh");
    wait_recent(&fixture, &[("d2", "pending"), ("d1", "sent")]).await;
    assert_eq!(fixture.service.current().recent_next_cursor, None);
    fixture.finish().await;
}

#[tokio::test]
async fn sse_delivery_increments_upsert_in_order_and_are_published() {
    let fixture = Fixture::start(on(), overview_json(&["task.completed"])).await;
    let epoch = fixture.service.cloud.request_epoch();
    let feed = fixture.service.feed();
    let (mut receiver, _) = fixture.events.subscribe_and_snapshot();

    // 新建 pending → 状态变化覆盖同 id → 更新的记录排在前面。
    feed.deliveries(
        vec![delivery_dto("d1", "2026-10-09T12:00:01.000Z", "pending")],
        epoch,
    );
    wait_recent(&fixture, &[("d1", "pending")]).await;
    feed.deliveries(
        vec![
            delivery_dto("d1", "2026-10-09T12:00:01.000Z", "sent"),
            delivery_dto("d2", "2026-10-09T12:00:02.000Z", "pending"),
        ],
        epoch,
    );
    wait_recent(&fixture, &[("d2", "pending"), ("d1", "sent")]).await;

    let mut pushed = 0;
    while let Ok(frame) = receiver.try_recv() {
        if matches!(
            frame.event,
            fluxdown_protocol::ServiceEvent::Agent(
                fluxdown_protocol::AgentEvent::CloudNotifyChanged(_)
            )
        ) {
            pushed += 1;
        }
    }
    assert!(
        pushed >= 2,
        "每次增量都推送 CloudNotifyChanged，实际 {pushed}"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn overflowing_the_window_truncates_and_refetches_the_first_page() {
    let fixture = Fixture::start(on(), overview_json(&["task.completed"])).await;
    let epoch = fixture.service.cloud.request_epoch();
    let calls_before = fixture.mock.deliveries_calls.load(Ordering::SeqCst);
    set_page(
        &fixture,
        page_json(
            vec![delivery_json("srv", "2026-10-09T13:00:00.000Z", "sent")],
            Some("fresh-cursor"),
        ),
    );
    let burst: Vec<_> = (0..51)
        .map(|index| {
            delivery_dto(
                &format!("id{index:02}"),
                &format!("2026-10-09T10:{index:02}:00.000Z"),
                "sent",
            )
        })
        .collect();
    fixture.service.feed().deliveries(burst, epoch);
    // 截断后游标失效 → 重拉第一页，最终以云端为准。
    wait_recent(&fixture, &[("srv", "sent")]).await;
    assert!(fixture.mock.deliveries_calls.load(Ordering::SeqCst) > calls_before);
    assert_eq!(
        fixture.service.current().recent_next_cursor.as_deref(),
        Some("fresh-cursor")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn sse_reconnect_overwrites_with_the_server_first_page() {
    let fixture = Fixture::start(on(), overview_json(&["task.completed"])).await;
    let epoch = fixture.service.cloud.request_epoch();
    let feed = fixture.service.feed();
    // 离线前本地看到的状态。
    feed.deliveries(
        vec![
            delivery_dto("ghost", "2026-10-09T12:00:01.000Z", "pending"),
            delivery_dto("kept", "2026-10-09T12:00:00.000Z", "pending"),
        ],
        epoch,
    );
    wait_recent(&fixture, &[("ghost", "pending"), ("kept", "pending")]).await;

    // 离线期间服务端发生了变化：ghost 被清理，kept 已发送，出现了新记录。
    set_page(
        &fixture,
        page_json(
            vec![
                delivery_json("new", "2026-10-09T12:00:05.000Z", "failed"),
                delivery_json("kept", "2026-10-09T12:00:00.000Z", "sent"),
            ],
            Some("c9"),
        ),
    );
    feed.connected();
    wait_recent(&fixture, &[("new", "failed"), ("kept", "sent")]).await;
    assert_eq!(
        fixture.service.current().recent_next_cursor.as_deref(),
        Some("c9")
    );

    // `notify.changed` / `resync` 同样整页纠正。
    set_page(
        &fixture,
        page_json(
            vec![delivery_json("only", "2026-10-09T12:01:00.000Z", "sent")],
            None,
        ),
    );
    feed.resync();
    wait_recent(&fixture, &[("only", "sent")]).await;
    fixture.finish().await;
}

#[tokio::test]
async fn logout_clears_deliveries_and_late_events_of_the_old_session_are_dropped() {
    let fixture = Fixture::start_with_page(page_json(
        vec![delivery_json("d1", "2026-10-09T12:00:01.000Z", "sent")],
        Some("c1"),
    ))
    .await;
    wait_recent(&fixture, &[("d1", "sent")]).await;
    let old_epoch = fixture.service.cloud.request_epoch();

    fixture.service.cloud.clear_session().await.expect("logout");
    wait_recent(&fixture, &[]).await;
    assert_eq!(fixture.service.current().recent_next_cursor, None);

    // 旧会话在途的增量不得写回。
    fixture.service.feed().deliveries(
        vec![delivery_dto("late", "2026-10-09T12:00:09.000Z", "sent")],
        old_epoch,
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        recent(&fixture).is_empty(),
        "旧会话事件被丢弃：{:?}",
        recent(&fixture)
    );
    fixture.finish().await;
}
