use std::{sync::Arc, time::Duration};

use fluxdown_protocol::method;
use futures_util::{SinkExt, StreamExt};
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

use super::{
    AgentClient, Bootstrap, ClientCommand, PreferenceWrites, ServiceBootstrap, Socket,
    SubmissionTransport, run_connected,
};
use crate::downloads_port::AgentDownloadsPort;
use fluxdown_ui_downloads::{
    DownloadsCommand, DownloadsPort, NewDownloadSubmission, SubmissionHooks, run_submission,
};
use serde_json::{Value, json};

pub(super) type Peer = WebSocketStream<TcpStream>;

pub(super) async fn connected_pair() -> (Socket, Peer) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind peer");
    let url = format!("ws://{}/", listener.local_addr().expect("peer address"));
    let (client, peer) = tokio::join!(tokio_tungstenite::connect_async(url), async {
        let (stream, _) = listener.accept().await.expect("accept client");
        tokio_tungstenite::accept_async(stream)
            .await
            .expect("accept websocket")
    });
    (client.expect("connect websocket").0, peer)
}

async fn request(peer: &mut Peer, method_name: &str) -> Value {
    let message = peer
        .next()
        .await
        .expect("request frame")
        .expect("valid frame");
    let Message::Text(text) = message else {
        panic!("expected text request")
    };
    let request: Value = serde_json::from_str(&text).expect("JSON request");
    assert_eq!(request["method"], method_name);
    request
}

async fn reply(peer: &mut Peer, request: &Value, result: Value) {
    peer.send(Message::Text(
        json!({
            "jsonrpc": "2.0", "id": request["id"], "result": result,
        })
        .to_string()
        .into(),
    ))
    .await
    .expect("reply");
}

async fn assert_no_request(peer: &mut Peer) {
    assert!(
        tokio::time::timeout(Duration::from_millis(30), peer.next())
            .await
            .is_err(),
        "a paused import must not send another request"
    );
}

#[test]
fn rpc_import_pause_failure_disconnect_and_resume_preserve_unsent_order() {
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (commands, mut command_rx) = mpsc::channel(64);
            let (events, _event_rx) = mpsc::channel(1024);
            let client = Arc::new(AgentClient {
                commands,
                runtime: runtime.clone(),
                bootstrap: Arc::new(ServiceBootstrap::new()),
                submissions: Arc::new(crate::downloads_port::SubmissionGate::default()),
            });
            let port = Arc::new(AgentDownloadsPort::new(client.clone()));
            let mut interruptions = client.submissions.interruption.subscribe();
            let (socket, mut peer) = connected_pair().await;
            client.submissions.set_ready(true);
            let first_events = events.clone();
            let mut transport = SubmissionTransport::new(client.submissions.clone());
            let first_connection = tokio::spawn(async move {
                let result = run_connected(
                    socket,
                    &mut command_rx,
                    &first_events,
                    Bootstrap::at("test", 0),
                    PreferenceWrites::new(0),
                    &mut transport,
                )
                .await;
                assert!(result.is_err(), "peer deliberately disconnects");
                (command_rx, transport)
            });
            // Non-idempotent creates: an uncertain one is never retried and suspends the rest.
            let submit_port = port.clone();
            let submission = tokio::spawn(async move {
                let mut created = Vec::new();
                let mut failed = false;
                for name in ["one", "uncertain", "three"] {
                    match submit_port.execute(create_command(name)).await {
                        Ok(result) => created.extend(result.created_task_ids()),
                        Err(_) => failed = true,
                    }
                }
                (created, failed)
            });
            let first = request(&mut peer, method::DAEMON_TASK_CREATE).await;
            assert_eq!(
                first["params"]["request"]["url"],
                "https://example.test/one"
            );
            // Pause closes admission before the in-flight create has acknowledged.
            let pause = tokio::spawn(port.execute(DownloadsCommand::PauseAll));
            reply(&mut peer, &first, json!({"taskId": "one"})).await;
            let pause_request = request(&mut peer, method::DAEMON_TASK_PAUSE_ALL).await;
            peer.send(Message::Text(
                json!({
                    "jsonrpc": "2.0", "id": pause_request["id"],
                    "error": { "code": -32603, "message": "injected pause failure" },
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("pause failure reply");
            assert!(pause.await.expect("pause task").is_err());
            assert_no_request(&mut peer).await;
            // A failed pause leaves the gate closed; successful ResumeAll reopens it.
            let resume = tokio::spawn(port.execute(DownloadsCommand::ResumeAll));
            let resume_request = request(&mut peer, method::DAEMON_TASK_RESUME_ALL).await;
            reply(&mut peer, &resume_request, json!({"ok": true})).await;
            assert!(resume.await.expect("resume task").is_ok());
            let uncertain = request(&mut peer, method::DAEMON_TASK_CREATE).await;
            assert_eq!(
                uncertain["params"]["request"]["url"],
                "https://example.test/uncertain"
            );
            // The server may have committed this request. Drop its response.
            drop(peer);
            interruptions
                .changed()
                .await
                .expect("interruption notification");
            assert_eq!(
                interruptions.borrow_and_update().as_deref(),
                Some("https://example.test/uncertain")
            );
            client.submissions.set_ready(false);
            let (mut command_rx, mut transport) = first_connection.await.expect("first connection");
            let (socket, mut peer) = connected_pair().await;
            let second_connection = tokio::spawn(async move {
                run_connected(
                    socket,
                    &mut command_rx,
                    &events,
                    Bootstrap::at("test-reconnected", 0),
                    PreferenceWrites::new(0),
                    &mut transport,
                )
                .await
            });
            client.submissions.set_ready(true);
            assert_no_request(&mut peer).await;
            let resume = tokio::spawn(port.execute(DownloadsCommand::ResumeAll));
            let resume_request = request(&mut peer, method::DAEMON_TASK_RESUME_ALL).await;
            reply(&mut peer, &resume_request, json!({"ok": true})).await;
            assert!(resume.await.expect("resume after reconnect").is_ok());
            let last = request(&mut peer, method::DAEMON_TASK_CREATE).await;
            assert_eq!(
                last["params"]["request"]["url"],
                "https://example.test/three"
            );
            reply(&mut peer, &last, json!({"taskId": "three"})).await;
            let (created, failed) = submission.await.expect("submission completes");
            assert!(failed);
            assert_eq!(created, ["one", "three"]);
            second_connection.abort();
            match second_connection.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("connection should be cancelled"),
            }
        })
        .await
        .expect("import test deadline");
    });
}

fn test_client(
    runtime: &Arc<tokio::runtime::Runtime>,
) -> (Arc<AgentClient>, mpsc::Receiver<ClientCommand>) {
    let (commands, receiver) = mpsc::channel(64);
    (
        Arc::new(AgentClient {
            commands,
            runtime: runtime.clone(),
            bootstrap: Arc::new(ServiceBootstrap::new()),
            submissions: Arc::new(crate::downloads_port::SubmissionGate::default()),
        }),
        receiver,
    )
}

fn create_command(name: &str) -> DownloadsCommand {
    DownloadsCommand::Create(Box::new(fluxdown_protocol::DaemonCreateTaskParams {
        request: serde_json::from_value(json!({"url": format!("https://example.test/{name}")}))
            .expect("create request"),
        torrent_blob_id: None,
        unattended: false,
        hint_file_size: None,
        name_hint: None,
        category_route: None,
    }))
}

async fn reject(peer: &mut Peer, request: &Value) {
    peer.send(Message::Text(
        json!({
            "jsonrpc": "2.0", "id": request["id"],
            "error": { "code": -32603, "message": "injected control failure" },
        })
        .to_string()
        .into(),
    ))
    .await
    .expect("failure reply");
}

#[test]
fn queued_unsent_create_survives_disconnect_and_pause_overtakes_it() {
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (client, mut command_rx) = test_client(&runtime);
            let port = AgentDownloadsPort::new(client.clone());
            let (events, _event_rx) = mpsc::channel(1024);
            client.submissions.set_ready(true);
            // Poll through AgentClient::call into mpsc, but do not send it.
            let mut first = port.execute(create_command("queued-before-disconnect"));
            assert!(futures_util::poll!(&mut first).is_pending());
            client.submissions.set_ready(false);
            let mut barrier = client.call::<Value, Value>(method::SYSTEM_SNAPSHOT, None);
            assert!(futures_util::poll!(&mut barrier).is_pending());
            let (socket, mut peer) = connected_pair().await;
            let mut transport = SubmissionTransport::new(client.submissions.clone());
            let first_events = events.clone();
            let connection = tokio::spawn(async move {
                let result = run_connected(
                    socket,
                    &mut command_rx,
                    &first_events,
                    Bootstrap::at("first", 0),
                    PreferenceWrites::new(0),
                    &mut transport,
                )
                .await;
                assert!(result.is_err());
                (command_rx, transport)
            });
            // This normal RPC is after create in mpsc and proves the transport
            // has deferred create rather than sending it while disconnected.
            request(&mut peer, method::SYSTEM_SNAPSHOT).await;
            drop(peer);
            let (mut command_rx, mut transport) = connection.await.expect("disconnected transport");
            assert_eq!(transport.unsent.len(), 1);
            assert!(barrier.await.is_err());
            assert!(futures_util::poll!(&mut first).is_pending());
            assert!(client.submissions.interruption.borrow().is_none());

            let pause = tokio::spawn(port.execute(DownloadsCommand::PauseAll));
            let (socket, mut peer) = connected_pair().await;
            let connection = tokio::spawn(async move {
                run_connected(
                    socket,
                    &mut command_rx,
                    &events,
                    Bootstrap::at("reconnected", 0),
                    PreferenceWrites::new(0),
                    &mut transport,
                )
                .await
            });
            client.submissions.set_ready(true);
            let pause_request = request(&mut peer, method::DAEMON_TASK_PAUSE_ALL).await;
            reply(&mut peer, &pause_request, json!({"ok": true})).await;
            assert!(pause.await.expect("pause").is_ok());
            let mut second = port.execute(create_command("new-while-paused"));
            assert!(futures_util::poll!(&mut second).is_pending());
            assert_no_request(&mut peer).await;

            let resume = tokio::spawn(port.execute(DownloadsCommand::ResumeAll));
            let resume_request = request(&mut peer, method::DAEMON_TASK_RESUME_ALL).await;
            reply(&mut peer, &resume_request, json!({"ok": true})).await;
            assert!(resume.await.expect("resume").is_ok());
            let first_request = request(&mut peer, method::DAEMON_TASK_CREATE).await;
            assert_eq!(
                first_request["params"]["request"]["url"],
                "https://example.test/queued-before-disconnect"
            );
            reply(&mut peer, &first_request, json!({"taskId": "first"})).await;
            assert!(first.await.is_ok());
            let second_request = request(&mut peer, method::DAEMON_TASK_CREATE).await;
            assert_eq!(
                second_request["params"]["request"]["url"],
                "https://example.test/new-while-paused"
            );
            reply(&mut peer, &second_request, json!({"taskId": "second"})).await;
            assert!(second.await.is_ok());
            assert_no_request(&mut peer).await;
            connection.abort();
            match connection.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("connection should be cancelled"),
            }
        })
        .await
        .expect("unsent create test deadline");
    });
}

#[test]
fn failed_resume_preserves_running_or_paused_import_state() {
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (client, mut command_rx) = test_client(&runtime);
            let port = AgentDownloadsPort::new(client.clone());
            let (events, _event_rx) = mpsc::channel(1024);
            let (socket, mut peer) = connected_pair().await;
            let mut transport = SubmissionTransport::new(client.submissions.clone());
            client.submissions.set_ready(true);
            let connection = tokio::spawn(async move {
                run_connected(
                    socket,
                    &mut command_rx,
                    &events,
                    Bootstrap::at("test", 0),
                    PreferenceWrites::new(0),
                    &mut transport,
                )
                .await
            });
            // A resume while already running must not manufacture a persistent
            // pause when the RPC fails, even with a create waiting behind it.
            let resume = tokio::spawn(port.execute(DownloadsCommand::ResumeAll));
            let mut first = port.execute(create_command("running"));
            assert!(futures_util::poll!(&mut first).is_pending());
            let resume_request = request(&mut peer, method::DAEMON_TASK_RESUME_ALL).await;
            reject(&mut peer, &resume_request).await;
            assert!(resume.await.expect("failed resume").is_err());
            let create = request(&mut peer, method::DAEMON_TASK_CREATE).await;
            reply(&mut peer, &create, json!({"taskId": "running"})).await;
            assert!(first.await.is_ok());

            let pause = tokio::spawn(port.execute(DownloadsCommand::PauseAll));
            let pause_request = request(&mut peer, method::DAEMON_TASK_PAUSE_ALL).await;
            reply(&mut peer, &pause_request, json!({"ok": true})).await;
            assert!(pause.await.expect("pause").is_ok());
            let resume = tokio::spawn(port.execute(DownloadsCommand::ResumeAll));
            let mut second = port.execute(create_command("paused"));
            assert!(futures_util::poll!(&mut second).is_pending());
            let resume_request = request(&mut peer, method::DAEMON_TASK_RESUME_ALL).await;
            reject(&mut peer, &resume_request).await;
            assert!(resume.await.expect("failed paused resume").is_err());
            assert_no_request(&mut peer).await;
            let resume = tokio::spawn(port.execute(DownloadsCommand::ResumeAll));
            let resume_request = request(&mut peer, method::DAEMON_TASK_RESUME_ALL).await;
            reply(&mut peer, &resume_request, json!({"ok": true})).await;
            assert!(resume.await.expect("successful resume").is_ok());
            let create = request(&mut peer, method::DAEMON_TASK_CREATE).await;
            assert_eq!(
                create["params"]["request"]["url"],
                "https://example.test/paused"
            );
            reply(&mut peer, &create, json!({"taskId": "paused"})).await;
            assert!(second.await.is_ok());
            connection.abort();
            match connection.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("connection should be cancelled"),
            }
        })
        .await
        .expect("resume failure test deadline");
    });
}

#[test]
fn overlapping_resume_success_then_failure_keeps_successful_resume() {
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (client, mut command_rx) = test_client(&runtime);
            let port = AgentDownloadsPort::new(client.clone());
            let (events, _event_rx) = mpsc::channel(1024);
            let (socket, mut peer) = connected_pair().await;
            let mut transport = SubmissionTransport::new(client.submissions.clone());
            client.submissions.set_ready(true);
            let connection = tokio::spawn(async move {
                run_connected(
                    socket,
                    &mut command_rx,
                    &events,
                    Bootstrap::at("test", 0),
                    PreferenceWrites::new(0),
                    &mut transport,
                )
                .await
            });
            let pause = tokio::spawn(port.execute(DownloadsCommand::PauseAll));
            let pause_request = request(&mut peer, method::DAEMON_TASK_PAUSE_ALL).await;
            reply(&mut peer, &pause_request, json!({"ok": true})).await;
            assert!(pause.await.expect("pause").is_ok());
            let mut create = port.execute(create_command("after-overlapping-resumes"));
            assert!(futures_util::poll!(&mut create).is_pending());

            let first = tokio::spawn(port.execute(DownloadsCommand::ResumeAll));
            let first_request = request(&mut peer, method::DAEMON_TASK_RESUME_ALL).await;
            // The second call is registered before the first reply. It blocks
            // admission, but must not invalidate the first successful resume.
            let second = tokio::spawn(port.execute(DownloadsCommand::ResumeAll));
            reply(&mut peer, &first_request, json!({"ok": true})).await;
            assert!(first.await.expect("first resume").is_ok());
            let second_request = request(&mut peer, method::DAEMON_TASK_RESUME_ALL).await;
            assert_no_request(&mut peer).await;
            reject(&mut peer, &second_request).await;
            assert!(second.await.expect("second resume fails").is_err());
            let created = request(&mut peer, method::DAEMON_TASK_CREATE).await;
            assert_eq!(
                created["params"]["request"]["url"],
                "https://example.test/after-overlapping-resumes"
            );
            reply(&mut peer, &created, json!({"taskId": "created"})).await;
            assert!(create.await.is_ok());
            connection.abort();
            match connection.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("connection should be cancelled"),
            }
        })
        .await
        .expect("overlapping resume test deadline");
    });
}

fn many_params(count: usize, batch_id: &str) -> Box<fluxdown_protocol::DaemonCreateTasksParams> {
    Box::new(fluxdown_protocol::DaemonCreateTasksParams {
        batch_id: batch_id.to_owned(),
        category_route: None,
        common: serde_json::from_value(json!({"url": ""})).expect("common request"),
        items: (0..count)
            .map(|index| fluxdown_protocol::CreateTaskItem {
                url: format!("https://example.test/{index}"),
                ..Default::default()
            })
            .collect(),
    })
}

fn many_reply(count: usize) -> Value {
    json!({
        "results": (0..count)
            .map(|index| json!({"taskId": format!("t{index}")}))
            .collect::<Vec<_>>(),
    })
}

#[test]
fn create_many_resends_same_batch_after_disconnect_without_pausing_the_gate() {
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (client, mut command_rx) = test_client(&runtime);
            let port = Arc::new(AgentDownloadsPort::new(client.clone()));
            let (events, _event_rx) = mpsc::channel(1024);
            client.submissions.set_ready(true);
            let (socket, mut peer) = connected_pair().await;
            let mut transport = SubmissionTransport::new(client.submissions.clone());
            let first_events = events.clone();
            let first_connection = tokio::spawn(async move {
                let result = run_connected(
                    socket,
                    &mut command_rx,
                    &first_events,
                    Bootstrap::at("first", 0),
                    PreferenceWrites::new(0),
                    &mut transport,
                )
                .await;
                assert!(result.is_err(), "peer deliberately disconnects");
                (command_rx, transport)
            });
            let submit_port = port.clone();
            let submission = tokio::spawn(run_submission(
                NewDownloadSubmission::Tasks {
                    common: Box::new(
                        serde_json::from_value(json!({"url": ""})).expect("common request"),
                    ),
                    items: (0..257)
                        .map(|index| fluxdown_protocol::CreateTaskItem {
                            url: format!("https://example.test/{index}"),
                            ..Default::default()
                        })
                        .collect(),
                    captures: Vec::new(),
                    category_route: None,
                    last_category_mode: "auto".to_owned(),
                    offered_save_dir: String::new(),
                },
                move |command| submit_port.execute(command),
                SubmissionHooks::new(|_| tokio::time::sleep(Duration::from_millis(1))),
            ));
            let first = request(&mut peer, method::DAEMON_TASK_CREATE_MANY).await;
            let batch_id = first["params"]["batchId"].clone();
            assert_eq!(first["params"]["items"].as_array().map(Vec::len), Some(256));
            // The server may have committed this chunk; the answer never arrives.
            drop(peer);
            let (mut command_rx, mut transport) = first_connection.await.expect("first connection");
            client.submissions.set_ready(false);
            // Idempotent chunks are resent by the caller: no pause, no interruption notice.
            assert!(client.submissions.interruption.borrow().is_none());
            let (socket, mut peer) = connected_pair().await;
            let second_connection = tokio::spawn(async move {
                run_connected(
                    socket,
                    &mut command_rx,
                    &events,
                    Bootstrap::at("second", 0),
                    PreferenceWrites::new(0),
                    &mut transport,
                )
                .await
            });
            client.submissions.set_ready(true);
            let resend = request(&mut peer, method::DAEMON_TASK_CREATE_MANY).await;
            assert_eq!(resend["params"]["batchId"], batch_id);
            assert_eq!(
                resend["params"]["items"].as_array().map(Vec::len),
                Some(256)
            );
            reply(&mut peer, &resend, many_reply(256)).await;
            let last = request(&mut peer, method::DAEMON_TASK_CREATE_MANY).await;
            assert_ne!(last["params"]["batchId"], batch_id);
            assert_eq!(last["params"]["items"].as_array().map(Vec::len), Some(1));
            reply(&mut peer, &last, many_reply(1)).await;
            // 上次目录、目录历史、分类模式记忆三条本机偏好。
            for _ in 0..3 {
                let preference = request(&mut peer, method::AGENT_PREFERENCES_PATCH).await;
                reply(&mut peer, &preference, json!({"ok": true})).await;
            }
            let report = submission.await.expect("submission completes");
            assert!(!report.failed() && !report.cancelled());
            assert_eq!(report.created_task_ids().len(), 257);
            assert!(client.submissions.interruption.borrow().is_none());
            second_connection.abort();
            match second_connection.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("connection should be cancelled"),
            }
        })
        .await
        .expect("createMany resend test deadline");
    });
}

#[test]
fn cancelled_unsent_create_many_is_skipped_by_the_transport() {
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (client, mut command_rx) = test_client(&runtime);
            let port = AgentDownloadsPort::new(client.clone());
            let (events, _event_rx) = mpsc::channel(1024);
            // Queued while the gate is closed, then abandoned by the caller.
            let mut cancelled = port.execute(DownloadsCommand::CreateMany(many_params(3, "gone")));
            assert!(futures_util::poll!(&mut cancelled).is_pending());
            drop(cancelled);
            let (socket, mut peer) = connected_pair().await;
            let mut transport = SubmissionTransport::new(client.submissions.clone());
            let connection = tokio::spawn(async move {
                run_connected(
                    socket,
                    &mut command_rx,
                    &events,
                    Bootstrap::at("cancelled", 0),
                    PreferenceWrites::new(0),
                    &mut transport,
                )
                .await
            });
            let mut kept = port.execute(DownloadsCommand::CreateMany(many_params(2, "kept")));
            assert!(futures_util::poll!(&mut kept).is_pending());
            client.submissions.set_ready(true);
            // The abandoned chunk is dropped unsent; the next request on the wire is the live one.
            let live = request(&mut peer, method::DAEMON_TASK_CREATE_MANY).await;
            assert_eq!(live["params"]["batchId"], "kept");
            reply(&mut peer, &live, many_reply(2)).await;
            assert!(kept.await.is_ok());
            assert_no_request(&mut peer).await;
            connection.abort();
            match connection.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("connection should be cancelled"),
            }
        })
        .await
        .expect("cancelled createMany test deadline");
    });
}
