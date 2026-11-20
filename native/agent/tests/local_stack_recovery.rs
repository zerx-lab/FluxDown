#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const TOKEN_WAIT: Duration = Duration::from_secs(30);
const RECOVERY_WAIT: Duration = Duration::from_secs(15);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires FLUXDOWN_TEST_DAEMON_BIN=<absolute fluxdownd path>"]
async fn client_observes_daemon_stale_recovery_and_agent_restart_state_restoration() {
    let daemon_binary = PathBuf::from(
        std::env::var_os("FLUXDOWN_TEST_DAEMON_BIN").expect("FLUXDOWN_TEST_DAEMON_BIN is required"),
    );
    assert!(
        daemon_binary.is_absolute(),
        "daemon test binary must be absolute"
    );
    assert!(daemon_binary.is_file(), "daemon test binary does not exist");

    let root = std::env::temp_dir().join(format!(
        "fluxdown-local-stack-recovery-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).expect("create local stack data dir");
    let daemon_pid_file = root.join("daemon.pid");
    let wrapper = create_daemon_wrapper(&root, &daemon_binary, &daemon_pid_file);
    let daemon_address = reserve_address();
    let agent_address = reserve_address();
    let agent_token_file = root.join("agent").join("agent.token");
    let mut stack = StackGuard::new(daemon_pid_file.clone());

    stack.agent = Some(spawn_agent(
        &root,
        &wrapper,
        daemon_address,
        agent_address,
        &agent_token_file,
    ));
    let token = wait_for_token(&agent_token_file, TOKEN_WAIT).await;
    let mut client = connect_agent(agent_address, &token).await;
    hello(&mut client, 1).await;
    let initial = wait_snapshot(&mut client, 2, RECOVERY_WAIT, |snapshot| {
        daemon_connected(snapshot)
    })
    .await;
    let initial_daemon_pid = wait_for_daemon_pid(&daemon_pid_file, None, RECOVERY_WAIT).await;
    let initial_revision = daemon_revision(&initial);

    send_signal(initial_daemon_pid, "KILL");
    wait_snapshot(&mut client, 10, RECOVERY_WAIT, |snapshot| {
        !daemon_connected(snapshot)
    })
    .await;
    let replacement_pid =
        wait_for_daemon_pid(&daemon_pid_file, Some(initial_daemon_pid), RECOVERY_WAIT).await;
    assert_ne!(replacement_pid, initial_daemon_pid);
    let recovered = wait_snapshot(&mut client, 20, RECOVERY_WAIT, |snapshot| {
        daemon_connected(snapshot)
    })
    .await;
    assert!(daemon_revision(&recovered) >= initial_revision);

    let revision = daemon_revision(&recovered);
    let patched = rpc_call(
        &mut client,
        30,
        fluxdown_protocol::method::DAEMON_CONFIG_PATCH,
        Some(json!({
            "expectedRevision": revision,
            "values": {"max_concurrent_tasks": "9"}
        })),
    )
    .await;
    assert_eq!(patched["result"]["revision"], json!(revision + 1));
    wait_snapshot(&mut client, 31, RECOVERY_WAIT, |snapshot| {
        daemon_config_value(snapshot, "max_concurrent_tasks") == Some("9")
    })
    .await;

    drop(client);
    terminate_child(
        stack.agent.as_mut().expect("agent child"),
        Duration::from_secs(10),
    )
    .await;
    stack.agent = None;
    assert!(
        process_exists(replacement_pid),
        "daemon must outlive the UI gateway"
    );

    stack.agent = Some(spawn_agent(
        &root,
        &wrapper,
        daemon_address,
        agent_address,
        &agent_token_file,
    ));
    let mut restarted_client = connect_agent(agent_address, &token).await;
    hello(&mut restarted_client, 40).await;
    let restored = wait_snapshot(&mut restarted_client, 41, RECOVERY_WAIT, |snapshot| {
        daemon_connected(snapshot)
            && daemon_config_value(snapshot, "max_concurrent_tasks") == Some("9")
    })
    .await;
    assert!(daemon_revision(&restored) > revision);

    drop(restarted_client);
    terminate_child(
        stack.agent.as_mut().expect("restarted agent"),
        Duration::from_secs(10),
    )
    .await;
    stack.agent = None;
    send_signal(replacement_pid, "TERM");
    wait_for_process_exit(replacement_pid, Duration::from_secs(10)).await;
    stack.daemon_cleaned = true;
    if let Err(error) = std::fs::remove_dir_all(&root) {
        tracing::warn!(path = %root.display(), %error, "local stack test cleanup failed");
    }
    let home = short_home(&root);
    if let Err(error) = std::fs::remove_dir_all(&home) {
        tracing::warn!(path = %home.display(), %error, "isolated agent HOME cleanup failed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires FLUXDOWN_TEST_DAEMON_BIN=<absolute fluxdownd path>"]
async fn rss_image_proxy_denial_survives_daemon_and_agent_transports() {
    use fluxdown_protocol::method;

    let daemon_binary = PathBuf::from(
        std::env::var_os("FLUXDOWN_TEST_DAEMON_BIN").expect("FLUXDOWN_TEST_DAEMON_BIN is required"),
    );
    assert!(daemon_binary.is_absolute() && daemon_binary.is_file());
    let root = std::env::temp_dir().join(format!("fluxdown-rss-preview-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create local stack data dir");
    let pid_file = root.join("daemon.pid");
    let wrapper = create_daemon_wrapper(&root, &daemon_binary, &pid_file);
    let address = reserve_address();
    let token_file = root.join("agent").join("agent.token");
    let mut stack = StackGuard::new(pid_file.clone());

    let feed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let feed_address = feed.local_addr().unwrap();
    let feed_task = tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/feed.xml",
            axum::routing::get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/rss+xml")],
                    r#"<rss version="2.0"><channel><title>Preview test</title>
                    <link>https://preview.invalid/</link><description>Preview test</description>
                    <item><guid isPermaLink="false">cover</guid><title>Cover</title>
                    <description><![CDATA[<img src="https://preview.invalid/cover.png">
                    <img src="http://127.0.0.1/private.png">]]></description>
                    <enclosure url="https://preview.invalid/download" length="1" type="application/octet-stream"/>
                    </item></channel></rss>"#,
                )
            }),
        );
        axum::serve(feed, app)
            .await
            .expect("serve local RSS fixture");
    });

    stack.agent = Some(spawn_agent(
        &root,
        &wrapper,
        reserve_address(),
        address,
        &token_file,
    ));
    let token = wait_for_token(&token_file, TOKEN_WAIT).await;
    let mut socket = connect_agent(address, &token).await;
    hello(&mut socket, 1).await;
    let snapshot = wait_snapshot(&mut socket, 2, RECOVERY_WAIT, daemon_connected).await;
    let created = rpc_call(
        &mut socket,
        100,
        method::DAEMON_RSS_CREATE_SOURCE,
        Some(json!({"url": format!("http://{feed_address}/feed.xml"), "autoDownload": false})),
    )
    .await;
    let source_id = created["result"]["sourceId"].as_str().expect("source ID");
    let refreshed = rpc_call(
        &mut socket,
        101,
        method::DAEMON_RSS_REFRESH_SOURCE,
        Some(json!({"sourceId": source_id})),
    )
    .await;
    assert_eq!(refreshed["result"]["ok"], true, "{refreshed}");
    let deadline = Instant::now() + RECOVERY_WAIT;
    let guid = loop {
        let items = rpc_call(
            &mut socket,
            102,
            method::DAEMON_RSS_GET_ITEMS,
            Some(json!({"sourceId": source_id})),
        )
        .await;
        let rows = items["result"].as_array().expect("RSS items result");
        if let Some(item) = rows.first() {
            assert_eq!(item["imageCount"], 2, "{item}");
            break item["guid"].as_str().expect("item GUID").to_owned();
        }
        assert!(Instant::now() < deadline, "RSS item was not persisted");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // Enable the proxy after fetching the fixture. Neither preview should reach the network.
    let revision = daemon_revision(&snapshot);
    let patched = rpc_call(
        &mut socket,
        103,
        method::DAEMON_CONFIG_PATCH,
        Some(json!({
            "expectedRevision": revision,
            "values": {"proxy_mode": "manual", "proxy_host": "127.0.0.1",
                       "proxy_port": feed_address.port().to_string(), "proxy_no_list": ""}
        })),
    )
    .await;
    assert_eq!(
        patched["result"]["revision"],
        json!(revision + 1),
        "{patched}"
    );
    let rpc = rpc_call(
        &mut socket,
        104,
        method::DAEMON_RSS_GET_IMAGE,
        Some(json!({"sourceId": source_id, "guid": guid, "index": 0})),
    )
    .await;
    assert_eq!(
        rpc["error"]["data"]["reason"], "rssImageProxyUnverifiable",
        "{rpc}"
    );

    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    for (index, status, marker) in [
        ("0", reqwest::StatusCode::FORBIDDEN, Some("1")),
        ("1", reqwest::StatusCode::BAD_GATEWAY, None),
    ] {
        let response = http
            .get(format!("http://{address}/api/web/rss-images"))
            .bearer_auth(&token)
            .query(&[
                ("sourceId", source_id),
                ("guid", guid.as_str()),
                ("index", index),
            ])
            .send()
            .await
            .expect("preview HTTP response");
        assert_eq!(response.status(), status, "image index {index}");
        assert_eq!(
            response
                .headers()
                .get("x-fluxdown-rss-image-proxy-unverifiable")
                .map(|value| value.to_str().unwrap()),
            marker,
            "image index {index}"
        );
    }

    drop(socket);
    terminate_child(stack.agent.as_mut().unwrap(), Duration::from_secs(10)).await;
    stack.agent = None;
    let daemon_pid = wait_for_daemon_pid(&pid_file, None, RECOVERY_WAIT).await;
    send_signal(daemon_pid, "TERM");
    wait_for_process_exit(daemon_pid, Duration::from_secs(10)).await;
    stack.daemon_cleaned = true;
    feed_task.abort();
    std::fs::remove_dir_all(short_home(&root)).expect("remove isolated agent HOME");
    std::fs::remove_dir_all(&root).expect("remove local stack data dir");
}

/// 完全退出（握手前 `system.shutdown`，即版本替换与托盘「退出」共用的路径）：daemon 先
/// 优雅退出，agent 随后以 `service-quit` 关闭 UI 连接并正常退出，且不再拉起 daemon。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires FLUXDOWN_TEST_DAEMON_BIN=<absolute fluxdownd path>"]
async fn full_quit_stops_daemon_then_agent_and_tells_clients_not_to_reconnect() {
    assert_full_quit(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires FLUXDOWN_TEST_DAEMON_BIN=<absolute fluxdownd path>"]
async fn scheduled_quit_stops_daemon_then_agent_and_tells_clients_not_to_reconnect() {
    assert_full_quit(true).await;
}

async fn assert_full_quit(scheduled: bool) {
    let daemon_binary = PathBuf::from(
        std::env::var_os("FLUXDOWN_TEST_DAEMON_BIN").expect("FLUXDOWN_TEST_DAEMON_BIN is required"),
    );
    assert!(daemon_binary.is_file(), "daemon test binary does not exist");
    let root = std::env::temp_dir().join(format!(
        "fluxdown-local-stack-quit-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).expect("create local stack data dir");
    let daemon_pid_file = root.join("daemon.pid");
    let wrapper = create_daemon_wrapper(&root, &daemon_binary, &daemon_pid_file);
    let daemon_address = reserve_address();
    let agent_address = reserve_address();
    let agent_token_file = root.join("agent").join("agent.token");
    let mut stack = StackGuard::new(daemon_pid_file.clone());
    stack.agent = Some(spawn_agent(
        &root,
        &wrapper,
        daemon_address,
        agent_address,
        &agent_token_file,
    ));
    let token = wait_for_token(&agent_token_file, TOKEN_WAIT).await;
    let mut client = connect_agent(agent_address, &token).await;
    hello(&mut client, 1).await;
    wait_snapshot(&mut client, 2, RECOVERY_WAIT, daemon_connected).await;
    let daemon_pid = wait_for_daemon_pid(&daemon_pid_file, None, RECOVERY_WAIT).await;

    if scheduled {
        let accepted = rpc_call(
            &mut client,
            3,
            fluxdown_protocol::method::AGENT_POWER_ARM,
            Some(json!({"delaySecs": 1, "action": "quit"})),
        )
        .await;
        assert_eq!(accepted["result"]["armed"], json!(true), "{accepted}");
    } else {
        let mut control = connect_agent(agent_address, &token).await;
        let accepted = rpc_call(
            &mut control,
            1,
            fluxdown_protocol::method::SYSTEM_SHUTDOWN,
            None,
        )
        .await;
        assert_eq!(accepted["result"]["ok"], json!(true), "{accepted}");
    }

    let close_reason = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match client.next().await {
                Some(Ok(Message::Close(frame))) => {
                    return frame.map(|frame| frame.reason.to_string());
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return None,
            }
        }
    })
    .await
    .expect("agent closed the UI connection");
    assert_eq!(
        close_reason.as_deref(),
        Some(fluxdown_protocol::CLOSE_REASON_SERVICE_QUIT)
    );
    assert!(
        !process_exists(daemon_pid),
        "daemon must be gone before the agent releases its UI connections"
    );

    let agent = stack.agent.as_mut().expect("agent child");
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = agent.try_wait().expect("wait for agent") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "agent did not exit after full quit"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(status.success(), "agent exited unsuccessfully: {status}");
    stack.agent = None;
    stack.daemon_cleaned = true;
    let respawned = std::fs::read_to_string(&daemon_pid_file)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    assert_eq!(
        respawned,
        Some(daemon_pid),
        "no daemon may be relaunched while quitting"
    );
    if let Err(error) = std::fs::remove_dir_all(&root) {
        tracing::warn!(path = %root.display(), %error, "local stack test cleanup failed");
    }
    let home = short_home(&root);
    if let Err(error) = std::fs::remove_dir_all(&home) {
        tracing::warn!(path = %home.display(), %error, "isolated agent HOME cleanup failed");
    }
}

struct StackGuard {
    agent: Option<Child>,
    daemon_pid_file: PathBuf,
    daemon_cleaned: bool,
}

impl StackGuard {
    fn new(daemon_pid_file: PathBuf) -> Self {
        Self {
            agent: None,
            daemon_pid_file,
            daemon_cleaned: false,
        }
    }
}

impl Drop for StackGuard {
    fn drop(&mut self) {
        if let Some(agent) = self.agent.as_mut() {
            match agent.try_wait() {
                Ok(Some(status)) => tracing::debug!(%status, "local stack agent already exited"),
                result => {
                    if let Err(error) = result {
                        tracing::warn!(%error, "could not inspect local stack agent during cleanup");
                    }
                    match agent.kill() {
                        Ok(()) => {
                            if let Err(error) = agent.wait() {
                                tracing::warn!(%error, "could not reap local stack agent during cleanup");
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "could not stop local stack agent during cleanup")
                        }
                    }
                }
            }
        }
        if self.daemon_cleaned {
            return;
        }
        let text = match std::fs::read_to_string(&self.daemon_pid_file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(path = %self.daemon_pid_file.display(), %error, "could not read local stack daemon PID during cleanup");
                return;
            }
        };
        let pid = match text.trim().parse::<u32>() {
            Ok(pid) => pid,
            Err(error) => {
                tracing::warn!(%error, "invalid local stack daemon PID during cleanup");
                return;
            }
        };
        if !process_exists(pid) {
            tracing::debug!(pid, "local stack daemon already exited");
            return;
        }
        match Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
        {
            Ok(status) if status.success() => {
                tracing::debug!(pid, "stopped local stack daemon during cleanup")
            }
            Ok(status) => {
                tracing::warn!(pid, %status, "could not stop local stack daemon during cleanup")
            }
            Err(error) => {
                tracing::warn!(pid, %error, "could not run local stack daemon cleanup command")
            }
        }
    }
}

fn create_daemon_wrapper(root: &Path, daemon: &Path, pid_file: &Path) -> PathBuf {
    let wrapper = root.join("fluxdownd-test-wrapper.sh");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" > '{}'\nexec '{}' \"$@\"\n",
        pid_file.display(),
        daemon.display()
    );
    std::fs::write(&wrapper, script).expect("write daemon wrapper");
    let mut permissions = std::fs::metadata(&wrapper)
        .expect("daemon wrapper metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&wrapper, permissions).expect("make daemon wrapper executable");
    wrapper
}

fn spawn_agent(
    root: &Path,
    daemon_wrapper: &Path,
    daemon_address: std::net::SocketAddr,
    agent_address: std::net::SocketAddr,
    agent_token_file: &Path,
) -> Child {
    // NMH 套接字与自启条目按 HOME 推导：隔离到短路径（Unix 套接字路径上限约 104 字节），
    // 避免与本机正在运行的 agent 冲突。
    let home = short_home(root);
    std::fs::create_dir_all(&home).expect("create isolated agent HOME");
    Command::new(env!("CARGO_BIN_EXE_fluxdown-agent"))
        .env("HOME", home)
        .env("FLUXDOWN_DATA_DIR", root)
        .env("FLUXDOWN_AGENT_DATA_DIR", root.join("agent"))
        .env("FLUXDOWN_AGENT_TOKEN_FILE", agent_token_file)
        .env("FLUXDOWN_DAEMON_TOKEN_FILE", root.join("daemon.token"))
        .env("FLUXDOWN_DAEMON_BIN", daemon_wrapper)
        .env("FLUXDOWN_DAEMON_BIND", daemon_address.to_string())
        .env("FLUXDOWN_DAEMON_URL", format!("ws://{daemon_address}/rpc"))
        .env("FLUXDOWN_AGENT_BIND", agent_address.to_string())
        .env("FLUXCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env_remove("FLUXDOWN_DATABASE_URL")
        .env_remove("FLUXDOWN_SHUTDOWN_DRY_RUN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn fluxdown-agent")
}

fn short_home(root: &Path) -> PathBuf {
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let suffix = &name[name.len().saturating_sub(12)..];
    PathBuf::from("/tmp").join(format!("fdh-{suffix}"))
}

fn reserve_address() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve stack port");
    let address = listener.local_addr().expect("reserved stack address");
    drop(listener);
    address
}

async fn wait_for_token(path: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(token) = tokio::fs::read_to_string(path).await
            && !token.trim().is_empty()
        {
            return token.trim().to_owned();
        }
        assert!(Instant::now() < deadline, "agent token was not created");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_daemon_pid(path: &Path, previous: Option<u32>, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = tokio::fs::read_to_string(path).await
            && let Ok(pid) = text.trim().parse::<u32>()
            && previous != Some(pid)
            && process_exists(pid)
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "daemon PID did not change in time"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn connect_agent(address: std::net::SocketAddr, token: &str) -> Socket {
    let deadline = Instant::now() + TOKEN_WAIT;
    loop {
        let mut request = format!("ws://{address}/rpc")
            .into_client_request()
            .expect("agent WebSocket request");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("agent authorization header"),
        );
        match tokio_tungstenite::connect_async(request).await {
            Ok((socket, _)) => return socket,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("agent gateway did not accept WebSocket: {error:#}"),
        }
    }
}

async fn hello(socket: &mut Socket, id: i64) {
    let response = rpc_call(
        socket,
        id,
        fluxdown_protocol::method::SYSTEM_HELLO,
        Some(json!({
            "clientName": "local-stack-recovery-test",
            "clientVersion": "test",
            "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
            "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
            "requestedRole": "agent",
            "capabilities": [fluxdown_protocol::method::CAPABILITY_CLIENT_SELECTIONS]
        })),
    )
    .await;
    assert_eq!(response["result"]["role"], json!("agent"));
}

async fn wait_snapshot(
    socket: &mut Socket,
    mut id: i64,
    timeout: Duration,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let response = rpc_call(socket, id, fluxdown_protocol::method::SYSTEM_SNAPSHOT, None).await;
        let snapshot = &response["result"];
        if predicate(snapshot) {
            return snapshot.clone();
        }
        assert!(
            Instant::now() < deadline,
            "snapshot predicate timed out: {snapshot}"
        );
        id = id.saturating_add(1);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn rpc_call(socket: &mut Socket, id: i64, method: &str, params: Option<Value>) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .expect("send agent RPC");
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("agent RPC response timeout")
            .expect("agent WebSocket closed")
            .expect("read agent WebSocket message");
        match message {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text).expect("agent JSON frame");
                if value.get("id").and_then(Value::as_i64) == Some(id) {
                    return value;
                }
            }
            Message::Ping(payload) => socket
                .send(Message::Pong(payload))
                .await
                .expect("send agent pong"),
            Message::Close(frame) => panic!("agent WebSocket closed: {frame:?}"),
            _ => {}
        }
    }
}

fn daemon_connected(snapshot: &Value) -> bool {
    snapshot["body"]["snapshot"]["daemonConnected"]
        .as_bool()
        .unwrap_or(false)
}

fn daemon_revision(snapshot: &Value) -> u64 {
    snapshot["body"]["snapshot"]["daemon"]["config"]["revision"]
        .as_u64()
        .expect("daemon config revision")
}

fn daemon_config_value<'a>(snapshot: &'a Value, key: &str) -> Option<&'a str> {
    snapshot["body"]["snapshot"]["daemon"]["config"]["values"][key].as_str()
}

fn send_signal(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status()
        .expect("send process signal");
    assert!(status.success(), "kill -{signal} {pid} failed: {status}");
}

fn process_exists(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn terminate_child(child: &mut Child, timeout: Duration) {
    send_signal(child.id(), "TERM");
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("wait for agent") {
            assert!(status.success(), "agent exited unsuccessfully: {status}");
            return;
        }
        assert!(Instant::now() < deadline, "agent did not stop in time");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_process_exit(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while process_exists(pid) {
        assert!(
            Instant::now() < deadline,
            "process {pid} did not stop in time"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
