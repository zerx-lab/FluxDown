#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fluxdown_protocol::handshake::{
    SYSTEM_AUTH_CHALLENGE, SYSTEM_AUTH_PROVE, client_proof, http_credential, verify_server_proof,
};
use fluxdown_protocol::method;
use serde_json::{Value, json};

const TOKEN: &str = "daemon-process-contract-token";
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

#[test]
fn daemon_process_enforces_wire_auth_conflict_body_limit_and_shutdown() {
    let address = reserve_loopback_address();
    let data_dir = unique_temp_dir("fluxdown-daemon-process-contract");
    std::fs::create_dir_all(&data_dir).expect("create daemon test data dir");
    std::fs::write(data_dir.join("daemon.token"), format!("{TOKEN}\n"))
        .expect("write daemon test token");

    let mut daemon = ProcessGuard::spawn(address, &data_dir);
    wait_until_listening(address, Duration::from_secs(30));
    let mut duplicate = ProcessGuard::spawn(address, &data_dir);
    duplicate.wait_for_success(Duration::from_secs(10));
    wait_until_listening(address, Duration::from_secs(10));

    // 浏览器页面发起的 WebSocket 必带 Origin：不能占用握手名额。
    let cross_site = http_request(
        address,
        "GET /rpc HTTP/1.1\r\nHost: localhost\r\nOrigin: http://evil.example\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: Zmx1eGRvd24tdW5hdXRob3JpemVk\r\n\r\n",
        None,
    );
    assert!(cross_site.starts_with("HTTP/1.1 403"), "{cross_site}");

    // 握手之前除握手帧外的一切（含 hello 与 shutdown）都被拒绝，daemon 不受影响。
    for (id, method_name, params) in [
        (1, method::SYSTEM_HELLO, Some(hello_params("daemon"))),
        (2, method::SYSTEM_SHUTDOWN, None),
        (3, method::DAEMON_TASK_LIST, None),
    ] {
        let mut anonymous = open_websocket(address);
        let refused = rpc_call(&mut anonymous, id, method_name, params);
        assert_eq!(
            refused["error"]["data"]["code"],
            json!("unauthorized"),
            "{method_name}: {refused}"
        );
    }

    // 不持有 token 的客户端算不出正确的证明。
    let mut wrong_token = open_websocket(address);
    let client_nonce = fresh_nonce();
    let challenge = rpc_call(
        &mut wrong_token,
        1,
        SYSTEM_AUTH_CHALLENGE,
        Some(json!({ "clientNonce": client_nonce })),
    );
    let server_nonce = challenge["result"]["serverNonce"]
        .as_str()
        .expect("server nonce")
        .to_owned();
    let forged = client_proof("not-the-daemon-token", &client_nonce, &server_nonce).expect("proof");
    let rejected = rpc_call(
        &mut wrong_token,
        2,
        SYSTEM_AUTH_PROVE,
        Some(json!({ "clientProof": forged })),
    );
    assert_eq!(rejected["error"]["data"]["code"], json!("unauthorized"));

    // 录下的合法证明在另一条连接上回放无效：服务端随机数每次连接都是新的。
    let mut recorded_socket = open_websocket(address);
    let recorded = authenticate(&mut recorded_socket, TOKEN);
    let mut replay = open_websocket(address);
    let challenge = rpc_call(
        &mut replay,
        1,
        SYSTEM_AUTH_CHALLENGE,
        Some(json!({ "clientNonce": recorded.client_nonce })),
    );
    assert_ne!(
        challenge["result"]["serverNonce"],
        json!(recorded.server_nonce)
    );
    let replayed = rpc_call(
        &mut replay,
        2,
        SYSTEM_AUTH_PROVE,
        Some(json!({ "clientProof": recorded.client_proof })),
    );
    assert_eq!(replayed["error"]["data"]["code"], json!("unauthorized"));
    drop(recorded_socket);

    let mut wrong_role = open_websocket(address);
    authenticate(&mut wrong_role, TOKEN);
    let wrong_role_response = rpc_call(
        &mut wrong_role,
        3,
        method::SYSTEM_HELLO,
        Some(hello_params("agent")),
    );
    assert_eq!(
        wrong_role_response["error"]["data"]["code"],
        json!("protocolIncompatible")
    );

    let mut socket = open_websocket(address);
    let session = authenticate(&mut socket, TOKEN);
    let hello = rpc_call(
        &mut socket,
        3,
        method::SYSTEM_HELLO,
        Some(hello_params("daemon")),
    );
    assert_eq!(hello["result"]["role"], json!("daemon"));
    assert_eq!(
        hello["result"]["protocolVersion"],
        json!(fluxdown_protocol::PROTOCOL_VERSION)
    );
    assert_eq!(
        hello["result"]["serviceVersion"],
        json!(fluxdown_protocol::APP_VERSION)
    );
    let again = rpc_call(
        &mut socket,
        4,
        SYSTEM_AUTH_CHALLENGE,
        Some(json!({ "clientNonce": fresh_nonce() })),
    );
    assert_eq!(again["error"]["data"]["code"], json!("conflict"));

    let config = rpc_call(&mut socket, 5, method::DAEMON_CONFIG_GET, None);
    let revision = config["result"]["revision"]
        .as_u64()
        .expect("config revision");
    let first_patch = rpc_call(
        &mut socket,
        6,
        method::DAEMON_CONFIG_PATCH,
        Some(json!({
            "expectedRevision": revision,
            "values": {"max_concurrent_tasks": "7"}
        })),
    );
    assert_eq!(first_patch["result"]["revision"], json!(revision + 1));

    let stale_patch = rpc_call(
        &mut socket,
        7,
        method::DAEMON_CONFIG_PATCH,
        Some(json!({
            "expectedRevision": revision,
            "values": {"max_concurrent_tasks": "8"}
        })),
    );
    assert_eq!(stale_patch["error"]["data"]["code"], json!("conflict"));
    assert_eq!(
        stale_patch["error"]["data"]["revision"],
        json!(revision + 1)
    );

    // 旧版 agent 仍在升级头带静态 Bearer：新 daemon 继续接受，无需挑战应答；错误 token 仍 401。
    let mut legacy = open_websocket_with_bearer(address, TOKEN);
    let legacy_hello = rpc_call(
        &mut legacy,
        1,
        method::SYSTEM_HELLO,
        Some(hello_params("daemon")),
    );
    assert_eq!(
        legacy_hello["result"]["role"],
        json!("daemon"),
        "{legacy_hello}"
    );
    drop(legacy);
    let wrong_bearer_upgrade = http_request(
        address,
        "GET /rpc HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer wrong\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: Zmx1eGRvd24tdW5hdXRob3JpemVk\r\n\r\n",
        None,
    );
    assert!(
        wrong_bearer_upgrade.starts_with("HTTP/1.1 401"),
        "{wrong_bearer_upgrade}"
    );

    // HTTP 端点接受本会话派生的凭据与旧版 agent 的静态 token，其余一律 401。
    let oversized_body = vec![0_u8; MAX_BODY_BYTES + 1];
    for (bearer, expected, body) in [
        (
            session.credential.as_str(),
            "HTTP/1.1 413",
            Some(oversized_body.as_slice()),
        ),
        (TOKEN, "HTTP/1.1 413", Some(oversized_body.as_slice())),
        // 未授权请求在读取请求体之前就被拒绝：只发请求头（仍声明超限长度），
        // 否则对端提前关闭会让写端收到 EPIPE。
        ("wrong", "HTTP/1.1 401", None),
    ] {
        let headers = format!(
            "POST /blobs/torrents HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {bearer}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            oversized_body.len()
        );
        let response = http_request(address, &headers, body);
        assert!(response.starts_with(expected), "{bearer}: {response}");
    }

    // 会话结束即撤销凭据。
    drop(socket);
    let revoked_request = format!(
        "POST /blobs/torrents HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        session.credential
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let response = http_request(address, &revoked_request, None);
        if response.starts_with("HTTP/1.1 401") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "session credential outlived its connection: {response}"
        );
        thread::sleep(Duration::from_millis(50));
    }

    daemon.terminate(Duration::from_secs(10));
    std::fs::remove_dir_all(&data_dir).expect("remove daemon test data dir");
}

/// 版本不兼容的新 agent 靠握手后、hello 之前的 `system.shutdown` 替换旧 daemon：旧进程必须
/// 正常退出并释放 data dir 租约，同一目录随即可以再起一个 daemon。
#[test]
fn authenticated_shutdown_exits_cleanly_and_releases_the_data_dir() {
    let address = reserve_loopback_address();
    let data_dir = unique_temp_dir("fluxdown-daemon-shutdown-contract");
    std::fs::create_dir_all(&data_dir).expect("create daemon test data dir");
    std::fs::write(data_dir.join("daemon.token"), format!("{TOKEN}\n"))
        .expect("write daemon test token");

    let mut daemon = ProcessGuard::spawn(address, &data_dir);
    wait_until_listening(address, Duration::from_secs(30));
    let mut socket = open_websocket(address);
    authenticate(&mut socket, TOKEN);
    let response = rpc_call(&mut socket, 3, method::SYSTEM_SHUTDOWN, None);
    assert_eq!(response["result"]["ok"], json!(true), "{response}");
    daemon.wait_for_success(Duration::from_secs(30));

    let mut successor = ProcessGuard::spawn(address, &data_dir);
    wait_until_listening(address, Duration::from_secs(30));
    successor.terminate(Duration::from_secs(10));
    std::fs::remove_dir_all(&data_dir).expect("remove daemon test data dir");
}

/// 站点代理规则写入前被严格校验（非法规则不落库，revision 不变），合法规则可被 proxyRoute 解释。
#[test]
fn proxy_rules_patch_is_validated() {
    let address = reserve_loopback_address();
    let data_dir = unique_temp_dir("fluxdown-daemon-proxy-rules-contract");
    std::fs::create_dir_all(&data_dir).expect("create daemon test data dir");
    std::fs::write(data_dir.join("daemon.token"), format!("{TOKEN}\n"))
        .expect("write daemon test token");
    let mut daemon = ProcessGuard::spawn(address, &data_dir);
    wait_until_listening(address, Duration::from_secs(30));

    let mut socket = open_websocket(address);
    authenticate(&mut socket, TOKEN);
    let hello = rpc_call(
        &mut socket,
        2,
        method::SYSTEM_HELLO,
        Some(hello_params("daemon")),
    );
    assert_eq!(hello["result"]["role"], json!("daemon"));
    let config = rpc_call(&mut socket, 3, method::DAEMON_CONFIG_GET, None);
    let revision = config["result"]["revision"]
        .as_u64()
        .expect("config revision");

    let bad = r#"[{"id":"a","match":"regex","pattern":"(","action":"direct"}]"#;
    let rejected = rpc_call(
        &mut socket,
        4,
        method::DAEMON_CONFIG_PATCH,
        Some(json!({"expectedRevision": revision, "values": {"proxy_rules": bad}})),
    );
    assert!(rejected.get("error").is_some(), "{rejected}");
    assert_eq!(
        rejected["error"]["data"]["code"],
        json!("invalidArgument"),
        "{rejected}"
    );
    let unchanged = rpc_call(&mut socket, 5, method::DAEMON_CONFIG_GET, None);
    assert_eq!(unchanged["result"]["revision"], json!(revision));

    let good = r#"[{"id":"a","match":"domain","pattern":"github.com","action":"direct"}]"#;
    let patched = rpc_call(
        &mut socket,
        6,
        method::DAEMON_CONFIG_PATCH,
        Some(json!({"expectedRevision": revision, "values": {"proxy_rules": good}})),
    );
    assert_eq!(
        patched["result"]["revision"],
        json!(revision + 1),
        "{patched}"
    );
    let routed = rpc_call(
        &mut socket,
        7,
        method::DAEMON_CONFIG_PROXY_ROUTE,
        Some(json!({"url": "https://x.github.com/a"})),
    );
    assert_eq!(routed["result"]["matched"]["index"], json!(0), "{routed}");

    drop(socket);
    daemon.terminate(Duration::from_secs(10));
    std::fs::remove_dir_all(&data_dir).expect("remove daemon test data dir");
}

/// 运行中的 daemon 在数据目录发布实际监听地址，优雅退出后删除该文件。
#[test]
fn running_daemon_publishes_its_endpoint_and_removes_it_on_shutdown() {
    let address = reserve_loopback_address();
    let data_dir = unique_temp_dir("fluxdown-daemon-endpoint-contract");
    std::fs::create_dir_all(&data_dir).expect("create daemon test data dir");
    std::fs::write(data_dir.join("daemon.token"), format!("{TOKEN}\n"))
        .expect("write daemon test token");
    let endpoint_path = data_dir.join("daemon-endpoint.json");

    let mut daemon = ProcessGuard::spawn(address, &data_dir);
    wait_until_listening(address, Duration::from_secs(30));
    wait_for_file(&endpoint_path, Duration::from_secs(30));
    let endpoint: fluxdown_protocol::local_service::LocalEndpoint =
        serde_json::from_slice(&std::fs::read(&endpoint_path).expect("read endpoint file"))
            .expect("parse endpoint file");
    assert_eq!(endpoint.rpc_url, format!("ws://{address}/rpc"));

    let mut socket = open_websocket(address);
    authenticate(&mut socket, TOKEN);
    let response = rpc_call(&mut socket, 3, method::SYSTEM_SHUTDOWN, None);
    assert_eq!(response["result"]["ok"], json!(true), "{response}");
    daemon.wait_for_success(Duration::from_secs(30));
    assert!(
        !endpoint_path.exists(),
        "endpoint file must be removed on exit"
    );
    std::fs::remove_dir_all(&data_dir).expect("remove daemon test data dir");
}

/// 慢调用（连通性测试要等十几秒才超时）期间，同一连接上的后续请求立即得到应答，事件照常推送。
#[test]
fn slow_call_does_not_block_inline_requests_or_event_delivery() {
    let address = reserve_loopback_address();
    let data_dir = unique_temp_dir("fluxdown-daemon-slow-call-contract");
    std::fs::create_dir_all(&data_dir).expect("create daemon test data dir");
    std::fs::write(data_dir.join("daemon.token"), format!("{TOKEN}\n"))
        .expect("write daemon test token");

    let mut daemon = ProcessGuard::spawn(address, &data_dir);
    wait_until_listening(address, Duration::from_secs(30));

    // 接受连接但永不应答的「代理」：连通性测试会一直等到自身超时。
    let blackhole = TcpListener::bind("127.0.0.1:0").expect("bind blackhole proxy");
    let blackhole_port = blackhole.local_addr().expect("blackhole address").port();
    thread::spawn(move || {
        let mut held = Vec::new();
        for stream in blackhole.incoming() {
            match stream {
                Ok(stream) => held.push(stream),
                Err(_) => break,
            }
        }
    });

    let mut socket = open_websocket(address);
    authenticate(&mut socket, TOKEN);
    let hello = rpc_call(
        &mut socket,
        3,
        method::SYSTEM_HELLO,
        Some(hello_params("daemon")),
    );
    assert_eq!(hello["result"]["role"], json!("daemon"));
    let config = rpc_call(&mut socket, 4, method::DAEMON_CONFIG_GET, None);
    let revision = config["result"]["revision"]
        .as_u64()
        .expect("config revision");

    send_request(
        &mut socket,
        10,
        method::DAEMON_CONFIG_PROXY_TEST,
        Some(json!({
            "proxyType": "http",
            "host": "127.0.0.1",
            "port": blackhole_port.to_string(),
            "username": "",
            "password": ""
        })),
    );
    let mut stash = Vec::new();
    send_request(
        &mut socket,
        11,
        method::DAEMON_CONFIG_PATCH,
        Some(json!({
            "expectedRevision": revision,
            "values": {"max_concurrent_tasks": "6"}
        })),
    );
    let patched = read_until_id(&mut socket, 11, &mut stash);
    assert_eq!(
        patched["result"]["revision"],
        json!(revision + 1),
        "{patched}"
    );
    // 配置变更事件必须在慢调用仍挂起时送达。
    while !stash
        .iter()
        .any(|frame| frame["method"] == json!(method::SERVICE_EVENT))
    {
        let frame = read_text_frame(&mut socket);
        stash.push(serde_json::from_slice(&frame).expect("JSON WebSocket frame"));
    }
    assert!(
        stash
            .iter()
            .all(|frame| frame.get("id").and_then(Value::as_i64) != Some(10)),
        "the slow call must still be pending: {stash:?}"
    );

    drop(socket);
    daemon.terminate(Duration::from_secs(10));
    std::fs::remove_dir_all(&data_dir).expect("remove daemon test data dir");
}

struct ProcessGuard {
    child: Option<Child>,
}

impl ProcessGuard {
    fn spawn(address: SocketAddr, data_dir: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_fluxdownd"))
            .env("FLUXDOWN_DAEMON_BIND", address.to_string())
            .env("FLUXDOWN_DATA_DIR", data_dir)
            .env_remove("FLUXDOWN_DATABASE_URL")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn fluxdownd");
        Self { child: Some(child) }
    }

    fn wait_for_success(&mut self, timeout: Duration) {
        self.wait_for_exit(timeout, true);
    }

    fn wait_for_exit(&mut self, timeout: Duration, require_success: bool) {
        let child = self.child.as_mut().expect("daemon child");
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait().expect("wait for fluxdownd") {
                if require_success {
                    assert!(
                        status.success(),
                        "fluxdownd exited unsuccessfully: {status}"
                    );
                }
                self.child = None;
                return;
            }
            assert!(Instant::now() < deadline, "fluxdownd did not stop in time");
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn terminate(&mut self, timeout: Duration) {
        {
            let child = self.child.as_mut().expect("daemon child");
            #[cfg(unix)]
            {
                let status = Command::new("kill")
                    .args(["-TERM", &child.id().to_string()])
                    .status()
                    .expect("send SIGTERM to fluxdownd");
                assert!(status.success(), "kill -TERM failed: {status}");
            }
            #[cfg(not(unix))]
            child.kill().expect("terminate fluxdownd");
        }
        // Windows kill is forced termination, not the graceful shutdown tested above.
        self.wait_for_exit(timeout, cfg!(unix));
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Err(error) = child.kill() {
                eprintln!("failed to kill fluxdownd during test cleanup: {error}");
            }
            // 即使 kill 失败也要尝试回收进程；Drop 不能在已有 panic 时再次 panic。
            if let Err(error) = child.wait() {
                eprintln!("failed to reap fluxdownd during test cleanup: {error}");
            }
        }
    }
}

fn reserve_loopback_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve daemon port");
    let address = listener.local_addr().expect("reserved address");
    drop(listener);
    address
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()))
}

fn wait_until_listening(address: SocketAddr, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("fluxdownd did not listen on {address}");
}

/// 端点文件在监听套接字绑定之后才发布（临时文件 + rename，出现即完整），
/// 能连上端口不代表文件已存在，需要轮询等待。
fn wait_for_file(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("{} was not published", path.display());
}

fn http_request(address: SocketAddr, headers: &str, body: Option<&[u8]>) -> String {
    let mut stream = connect(address);
    stream
        .write_all(headers.as_bytes())
        .expect("write HTTP headers");
    if let Some(body) = body {
        stream.write_all(body).expect("write HTTP body");
    }
    let response_headers = read_http_headers(&mut stream);
    let content_length = response_headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
        })
        .unwrap_or(0);
    let mut body = vec![0_u8; content_length];
    stream
        .read_exact(&mut body)
        .expect("read HTTP response body");
    format!("{response_headers}{}", String::from_utf8_lossy(&body))
}

/// WebSocket 升级本身不带任何凭据：身份证明在升级后的 `system.auth.*` 握手里完成。
fn open_websocket(address: SocketAddr) -> TcpStream {
    let mut stream = connect(address);
    let request = format!(
        "GET /rpc HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: Zmx1eGRvd24tcHJvY2Vzcy10ZXN0\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write WebSocket upgrade");
    let headers = read_http_headers(&mut stream);
    assert!(headers.starts_with("HTTP/1.1 101"), "{headers}");
    stream
}

/// 旧版 agent 的升级方式：升级头直接带静态 Bearer。
fn open_websocket_with_bearer(address: SocketAddr, token: &str) -> TcpStream {
    let mut stream = connect(address);
    let request = format!(
        "GET /rpc HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: Zmx1eGRvd24tcHJvY2Vzcy10ZXN0\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write WebSocket upgrade");
    let headers = read_http_headers(&mut stream);
    assert!(headers.starts_with("HTTP/1.1 101"), "{headers}");
    stream
}

fn connect(address: SocketAddr) -> TcpStream {
    let stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(2)).expect("connect to fluxdownd");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .expect("set write timeout");
    stream
}

fn read_http_headers(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream
            .read_exact(&mut byte)
            .expect("read HTTP upgrade response");
        bytes.push(byte[0]);
        assert!(bytes.len() < 16 * 1024, "HTTP headers too large");
    }
    String::from_utf8(bytes).expect("UTF-8 HTTP headers")
}

fn send_request(stream: &mut TcpStream, id: i64, method: &str, params: Option<Value>) {
    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    write_text_frame(stream, request.to_string().as_bytes());
}

/// 读到 `id` 对应的响应为止；途中收到的其它帧（事件通知、别的请求的响应）存入 `stash`。
fn read_until_id(stream: &mut TcpStream, id: i64, stash: &mut Vec<Value>) -> Value {
    loop {
        let response = read_text_frame(stream);
        let value: Value = serde_json::from_slice(&response).expect("JSON WebSocket frame");
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            return value;
        }
        stash.push(value);
    }
}

fn rpc_call(stream: &mut TcpStream, id: i64, method: &str, params: Option<Value>) -> Value {
    send_request(stream, id, method, params);
    read_until_id(stream, id, &mut Vec::new())
}

fn fresh_nonce() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// 一次已完成的握手：测试扮演 agent，记录下可供回放的材料。
struct Handshake {
    client_nonce: String,
    server_nonce: String,
    client_proof: String,
    /// 本会话的 HTTP 凭据（与 agent 一侧同一函数派生）。
    credential: String,
}

/// agent 一侧的双向挑战应答：先校验 daemon 的证明，通过后才提交自己的证明。
fn authenticate(stream: &mut TcpStream, token: &str) -> Handshake {
    let client_nonce = fresh_nonce();
    let challenge = rpc_call(
        stream,
        1,
        SYSTEM_AUTH_CHALLENGE,
        Some(json!({ "clientNonce": client_nonce })),
    );
    let server_nonce = challenge["result"]["serverNonce"]
        .as_str()
        .unwrap_or_else(|| panic!("challenge result: {challenge}"))
        .to_owned();
    let server_proof_value = challenge["result"]["serverProof"]
        .as_str()
        .expect("server proof");
    assert!(
        verify_server_proof(token, &client_nonce, &server_nonce, server_proof_value),
        "daemon must prove it holds the token before the client does"
    );
    let client_proof_value = client_proof(token, &client_nonce, &server_nonce).expect("proof");
    let proved = rpc_call(
        stream,
        2,
        SYSTEM_AUTH_PROVE,
        Some(json!({ "clientProof": client_proof_value })),
    );
    assert_eq!(proved["result"]["authenticated"], json!(true), "{proved}");
    Handshake {
        credential: http_credential(token, &client_nonce, &server_nonce).expect("credential"),
        client_proof: client_proof_value,
        client_nonce,
        server_nonce,
    }
}

fn hello_params(requested_role: &str) -> Value {
    json!({
        "clientName": "fluxdown-agent",
        "clientVersion": "test",
        "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
        "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
        "requestedRole": requested_role,
        "capabilities": []
    })
}

fn write_text_frame(stream: &mut TcpStream, payload: &[u8]) {
    let mask = [0x46_u8, 0x4c, 0x55, 0x58];
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x81);
    match payload.len() {
        0..=125 => frame.push(0x80 | payload.len() as u8),
        126..=65_535 => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        _ => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()]),
    );
    stream.write_all(&frame).expect("write WebSocket frame");
}

fn read_text_frame(stream: &mut TcpStream) -> Vec<u8> {
    loop {
        let mut header = [0_u8; 2];
        stream
            .read_exact(&mut header)
            .expect("read WebSocket header");
        let opcode = header[0] & 0x0f;
        let masked = header[1] & 0x80 != 0;
        let mut length = u64::from(header[1] & 0x7f);
        if length == 126 {
            let mut extended = [0_u8; 2];
            stream
                .read_exact(&mut extended)
                .expect("read WebSocket length");
            length = u64::from(u16::from_be_bytes(extended));
        } else if length == 127 {
            let mut extended = [0_u8; 8];
            stream
                .read_exact(&mut extended)
                .expect("read WebSocket length");
            length = u64::from_be_bytes(extended);
        }
        let mut mask = [0_u8; 4];
        if masked {
            stream.read_exact(&mut mask).expect("read WebSocket mask");
        }
        let mut payload = vec![0_u8; usize::try_from(length).expect("frame length")];
        stream
            .read_exact(&mut payload)
            .expect("read WebSocket payload");
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % mask.len()];
            }
        }
        match opcode {
            0x1 => return payload,
            0x8 => panic!("WebSocket closed before RPC response"),
            0x9 => write_control_frame(stream, 0xA, &payload),
            _ => {}
        }
    }
}

fn write_control_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) {
    assert!(payload.len() <= 125, "control frame payload too large");
    let mask = [0x44_u8, 0x41, 0x45, 0x4d];
    let mut frame = Vec::with_capacity(payload.len() + 6);
    frame.push(0x80 | opcode);
    frame.push(0x80 | payload.len() as u8);
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()]),
    );
    stream
        .write_all(&frame)
        .expect("write WebSocket control frame");
}
