use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http;

use super::{
    AgentClientConfig, AgentClientEvent, CONNECT_TIMEOUT_MAX, ConnectError, ConnectTimeout,
    NO_BEARER_LISTENER_GRACE, NoBearerGrace, connect, run_client, spawn_agent_if_needed,
};
use crate::service_bootstrap::{PROBE_TIMEOUT, ServiceBootstrap};
use crate::service_failure::ServiceFailure;

struct Fixture(PathBuf);

impl Fixture {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("fluxdown-connect-{label}-{}", std::process::id()));
        std::fs::create_dir(&path).expect("create connect fixture");
        std::fs::write(path.join("agent.token"), "test-bearer").expect("write bearer");
        Self(path)
    }

    fn config(&self, rpc_url: &str) -> AgentClientConfig {
        AgentClientConfig {
            rpc_url: Some(rpc_url.to_owned()),
            bearer_path: self.0.join("agent.token"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("could not remove connect fixture: {error}");
        }
    }
}

async fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let url = format!("ws://{}/rpc", listener.local_addr().expect("local addr"));
    (listener, url)
}

#[test]
fn connect_timeout_doubles_per_timeout_caps_and_resets_on_refusal_or_connection() {
    let mut timeout = ConnectTimeout::new();
    assert_eq!(timeout.current(), PROBE_TIMEOUT);
    let timed_out = ConnectError::TimedOut(PROBE_TIMEOUT);
    let mut seen = Vec::new();
    for _ in 0..8 {
        timeout.observe(Some(&timed_out));
        seen.push(timeout.current());
    }
    assert_eq!(
        seen[..4],
        [
            Duration::from_millis(600),
            Duration::from_millis(1200),
            Duration::from_millis(2400),
            Duration::from_millis(4800),
        ]
    );
    assert!(seen[4..].iter().all(|value| *value == CONNECT_TIMEOUT_MAX));

    // 没读到 bearer 的轮次没有触及网络：不打断连续超时。
    timeout.observe(Some(&ConnectError::NoBearer));
    assert_eq!(timeout.current(), CONNECT_TIMEOUT_MAX);

    timeout.observe(Some(&ConnectError::Refused));
    assert_eq!(timeout.current(), PROBE_TIMEOUT);

    timeout.observe(Some(&timed_out));
    assert_eq!(timeout.current(), PROBE_TIMEOUT * 2);
    timeout.observe(None);
    assert_eq!(timeout.current(), PROBE_TIMEOUT);

    // TCP 已连上、握手阶段失败同样说明建连没有超时。
    timeout.observe(Some(&timed_out));
    timeout.observe(Some(&ConnectError::Transient(
        "handshake: reset".to_owned(),
    )));
    assert_eq!(timeout.current(), PROBE_TIMEOUT);
}

#[test]
fn no_bearer_grace_expires_by_time_and_restarts_after_reset() {
    let start = Instant::now();
    let mut grace = NoBearerGrace::default();
    assert!(grace.within_grace(start));
    assert!(grace.within_grace(start + NO_BEARER_LISTENER_GRACE - Duration::from_millis(1)));
    assert!(!grace.within_grace(start + NO_BEARER_LISTENER_GRACE));
    assert!(!grace.within_grace(start + NO_BEARER_LISTENER_GRACE * 2));

    grace.reset();
    let later = start + NO_BEARER_LISTENER_GRACE * 3;
    assert!(grace.within_grace(later));
}

/// 对端只 accept 不响应：握手必须在总超时内以可重试错误返回，而不是永久阻塞。
#[tokio::test]
async fn peer_that_accepts_but_never_answers_times_out_as_transient() {
    let fixture = Fixture::new("silent-accept");
    let (listener, url) = bind().await;
    let server = tokio::spawn(async move {
        let _held = listener.accept().await;
        std::future::pending::<()>().await;
    });
    let started = Instant::now();
    let result = connect(
        &fixture.config(&url),
        &url,
        Duration::from_secs(2),
        Duration::from_millis(250),
        true,
    )
    .await;
    server.abort();
    match result {
        Err(ConnectError::Transient(reason)) => {
            assert!(reason.contains("handshake timed out"), "{reason}");
        }
        Err(other) => panic!("expected a retryable timeout, got {}", other.describe()),
        Ok(_) => panic!("a silent peer cannot complete the handshake"),
    }
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// WS 升级成功但 `system.hello` 永不应答（agent 挂死）：同样受总超时约束。
#[tokio::test]
async fn peer_that_upgrades_but_never_answers_hello_times_out_as_transient() {
    let fixture = Fixture::new("silent-hello");
    let (listener, url) = bind().await;
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept client");
        let _socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("complete the upgrade");
        std::future::pending::<()>().await;
    });
    let result = connect(
        &fixture.config(&url),
        &url,
        Duration::from_secs(2),
        Duration::from_millis(250),
        true,
    )
    .await;
    server.abort();
    match result {
        Err(ConnectError::Transient(reason)) => {
            assert!(reason.contains("handshake timed out"), "{reason}");
        }
        Err(other) => panic!("expected a retryable timeout, got {}", other.describe()),
        Ok(_) => panic!("an unanswered hello cannot complete the handshake"),
    }
}

fn unauthorized() -> ErrorResponse {
    http::Response::builder()
        .status(http::StatusCode::UNAUTHORIZED)
        .body(None)
        .expect("build 401 response")
}

/// 401 说明监听者不是本用户的 agent：连接循环必须继续重试（Stale），而不是 Fatal 后终止。
#[tokio::test]
#[allow(
    clippy::result_large_err,
    reason = "tungstenite's handshake callback fixes the Err type to http::Response"
)]
async fn unauthorized_listener_keeps_the_connect_loop_retrying() {
    let fixture = Fixture::new("unauthorized");
    let (listener, url) = bind().await;
    let (accepted_tx, mut accepted_rx) = mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.expect("accept client");
            if accepted_tx.send(()).is_err() {
                return;
            }
            let rejected =
                tokio_tungstenite::accept_hdr_async(stream, |_: &Request, _: Response| {
                    Err(unauthorized())
                })
                .await;
            assert!(rejected.is_err(), "the upgrade must be rejected");
        }
    });

    // 不真正拉起 agent：停止拉起后 `ensure_running` 直接返回。
    let bootstrap = Arc::new(ServiceBootstrap::new());
    bootstrap.stop();
    let (_commands_tx, commands_rx) = mpsc::channel(1);
    let (events_tx, mut events_rx) = mpsc::channel(64);
    let submissions = Arc::new(crate::downloads_port::SubmissionGate::default());
    let client = tokio::spawn(run_client(
        fixture.config(&url),
        bootstrap,
        commands_rx,
        events_tx,
        submissions,
    ));

    for round in 1..=2 {
        tokio::time::timeout(Duration::from_secs(10), accepted_rx.recv())
            .await
            .unwrap_or_else(|_| panic!("connection attempt {round} never happened"))
            .expect("server task alive");
    }
    assert!(!client.is_finished(), "401 must not end the connect loop");
    client.abort();
    server.abort();

    let mut stale = 0;
    while let Ok(event) = events_rx.try_recv() {
        match event {
            AgentClientEvent::Stale => stale += 1,
            AgentClientEvent::Fatal(_) => panic!("401 must not be fatal"),
            _ => {}
        }
    }
    assert!(stale >= 1, "the session must be told the link is stale");
}

/// 同一失败期只通知一次：暂停拉起期间的重复尝试不再重复发事件。
#[tokio::test]
async fn blocked_launch_is_reported_once_per_failure_period() {
    let fixture = Fixture::new("blocked-once");
    let bootstrap = ServiceBootstrap::new();
    bootstrap
        .record_launch_failure_for_test("fluxdown-agent", "permission denied")
        .await;
    let config = fixture.config("ws://127.0.0.1:1/rpc");
    let (events_tx, mut events_rx) = mpsc::channel(8);
    let mut reported = false;
    for _ in 0..3 {
        spawn_agent_if_needed(
            &bootstrap,
            &config,
            "ws://127.0.0.1:1/rpc",
            false,
            &mut reported,
            &events_tx,
        )
        .await
        .expect("receiver alive");
    }
    drop(events_tx);
    let mut failures = Vec::new();
    while let Some(event) = events_rx.recv().await {
        match event {
            AgentClientEvent::ServiceFailed(failure) => failures.push(failure),
            _ => panic!("only the failure event is expected"),
        }
    }
    assert_eq!(
        failures,
        [ServiceFailure::Launch {
            program: "fluxdown-agent".to_owned(),
            detail: "permission denied".to_owned()
        }]
    );
}

#[cfg(unix)]
mod ipc {
    use super::*;
    use fluxdown_protocol::local_service::GATEWAY_IPC_SOCKET_FILE;
    use tokio::net::UnixListener;

    /// 没有显式 RPC 地址的配置（生产形态）：IPC 端点由 bearer 所在目录推导。
    fn discovered(fixture: &Fixture) -> AgentClientConfig {
        AgentClientConfig {
            rpc_url: None,
            bearer_path: fixture.0.join("agent.token"),
        }
    }

    /// 一个确定无人监听的回环地址。
    async fn dead_tcp_url() -> String {
        let (listener, url) = bind().await;
        drop(listener);
        url
    }

    /// IPC 端点可用时优先经它握手：TCP 地址无人监听也不影响，结果来自 IPC 对端。
    #[tokio::test]
    async fn ipc_endpoint_is_preferred_over_loopback_tcp() {
        // 目录名保持简短：Unix socket 路径有长度上限。
        let fixture = Fixture::new("i1");
        let listener =
            UnixListener::bind(fixture.0.join(GATEWAY_IPC_SOCKET_FILE)).expect("bind ipc");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept client");
            let _socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("complete the upgrade");
            std::future::pending::<()>().await;
        });
        let url = dead_tcp_url().await;
        let result = connect(
            &discovered(&fixture),
            &url,
            Duration::from_secs(2),
            Duration::from_millis(250),
            true,
        )
        .await;
        server.abort();
        match result {
            Err(ConnectError::Transient(reason)) => {
                assert!(reason.contains("over local IPC"), "{reason}");
            }
            Err(other) => panic!(
                "expected the IPC handshake outcome, got {}",
                other.describe()
            ),
            Ok(_) => panic!("an unanswered hello cannot complete the handshake"),
        }
    }

    /// IPC 对端拒绝 bearer（不是本数据目录的 agent）：同一轮改走回环 TCP。
    #[tokio::test]
    #[allow(
        clippy::result_large_err,
        reason = "tungstenite's handshake callback fixes the Err type to http::Response"
    )]
    async fn ipc_endpoint_rejecting_the_bearer_falls_back_to_tcp() {
        let fixture = Fixture::new("i2");
        let listener =
            UnixListener::bind(fixture.0.join(GATEWAY_IPC_SOCKET_FILE)).expect("bind ipc");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept client");
            let rejected =
                tokio_tungstenite::accept_hdr_async(stream, |_: &Request, _: Response| {
                    Err(unauthorized())
                })
                .await;
            assert!(rejected.is_err());
        });
        let url = dead_tcp_url().await;
        let result = connect(
            &discovered(&fixture),
            &url,
            Duration::from_secs(2),
            Duration::from_secs(2),
            true,
        )
        .await;
        server.await.expect("ipc peer");
        assert!(
            matches!(result, Err(ConnectError::Refused)),
            "TCP outcome expected after the IPC 401"
        );
    }

    /// 没有 IPC 端点（agent 未运行或旧版 agent）：直接走回环 TCP 流程。
    #[tokio::test]
    async fn missing_ipc_endpoint_uses_tcp() {
        let fixture = Fixture::new("i3");
        let url = dead_tcp_url().await;
        let result = connect(
            &discovered(&fixture),
            &url,
            Duration::from_secs(2),
            Duration::from_secs(2),
            true,
        )
        .await;
        assert!(matches!(result, Err(ConnectError::Refused)));
    }

    /// 显式指定 RPC 地址（`FLUXDOWN_AGENT_URL`）时只连该地址，不碰 IPC 端点。
    #[tokio::test]
    async fn explicit_rpc_url_skips_the_ipc_endpoint() {
        let fixture = Fixture::new("i4");
        let _listener =
            UnixListener::bind(fixture.0.join(GATEWAY_IPC_SOCKET_FILE)).expect("bind ipc");
        let url = dead_tcp_url().await;
        let result = connect(
            &fixture.config(&url),
            &url,
            Duration::from_secs(2),
            Duration::from_millis(250),
            true,
        )
        .await;
        assert!(matches!(result, Err(ConnectError::Refused)));
    }
}
