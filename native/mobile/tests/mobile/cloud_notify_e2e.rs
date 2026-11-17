//! 云端推送整条链路的进程内端到端：真实 `fluxdownd`（引擎）+ 嵌入式 agent + mock FluxCloud。
//!
//! 引擎下载完成 → `EngineEvent::TaskNotice` → daemon `DaemonEvent::TaskNotice` → agent 云端推送模块
//! （过滤 / 隐私裁剪 / 1s 批量）→ `POST /api/v1/notifications/events`。
//! 全程离线：下载源与「云端」都是测试内的 loopback `TcpListener`；agent 的云端地址经调试构建的
//! 持久化覆盖指向 mock（与 `CloudClient::restore_endpoint_override` 同一路径）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluxdown_agent::state::{AgentState, CloudCredentials, CloudNotifyPrefs, StateStore};
use fluxdown_mobile::{CreateTaskRequestDto, FluxCore, HostSession, LocalHostConfig};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const STEP: Duration = Duration::from_secs(60);
const FILE_LEN: u64 = 256 * 1024;
const DEVICE_ID: &str = "e2e-device";
const ACCESS_TOKEN: &str = "e2e-access";

fn byte_at(offset: u64) -> u8 {
    offset.wrapping_mul(31).wrapping_add(7).to_le_bytes()[0]
}

async fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() > 64 * 1024 {
            return Err(std::io::Error::other("request head too large"));
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::other("connection closed in request head"));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buffer.len() < head_end + length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::other("connection closed in request body"));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Ok((head, buffer[head_end..head_end + length].to_vec()))
}

fn header(head: &str, wanted: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case(wanted)
            .then(|| value.trim().to_owned())
    })
}

/// 支持 HEAD / GET / `Range` 的下载源：`/fast*.bin` 都是 [`FILE_LEN`] 字节。
struct FileServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl FileServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind files");
        let addr = listener.local_addr().expect("files addr");
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    if let Err(error) = serve_file(stream).await {
                        eprintln!("test file connection ended: {error}");
                    }
                });
            }
        });
        Self { addr, task }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}/{path}", self.addr)
    }
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_file(mut stream: TcpStream) -> std::io::Result<()> {
    let (head, _) = read_request(&mut stream).await?;
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    if !path.starts_with("/fast") {
        return stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    }
    let range = header(&head, "range").and_then(|value| {
        let spec = value.strip_prefix("bytes=")?.to_owned();
        let (start, end) = spec.split_once('-')?;
        let start: u64 = start.parse().ok()?;
        let end = match end {
            "" => FILE_LEN - 1,
            end => end.parse::<u64>().ok()?.min(FILE_LEN - 1),
        };
        (start <= end).then_some((start, end))
    });
    let (status, start, end) = match range {
        Some((start, end)) => ("206 Partial Content", start, end),
        None => ("200 OK", 0, FILE_LEN - 1),
    };
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n",
        end - start + 1
    );
    if range.is_some() {
        response.push_str(&format!(
            "Content-Range: bytes {start}-{end}/{FILE_LEN}\r\n"
        ));
    }
    response.push_str("\r\n");
    stream.write_all(response.as_bytes()).await?;
    if method.eq_ignore_ascii_case("HEAD") {
        return Ok(());
    }
    let body: Vec<u8> = (start..=end).map(byte_at).collect();
    stream.write_all(&body).await?;
    stream.shutdown().await
}

#[derive(Default)]
struct Recorded {
    overview_hits: usize,
    /// `(Authorization 头, 请求体)`。
    batches: Vec<(String, Value)>,
}

/// mock FluxCloud：只实现推送的两个端点，其余路径一律 404（agent 的同步 / 远程任务等后台服务
/// 会不断尝试并忽略失败）。
struct MockCloud {
    addr: SocketAddr,
    recorded: Arc<Mutex<Recorded>>,
    task: JoinHandle<()>,
}

impl MockCloud {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind cloud");
        let addr = listener.local_addr().expect("cloud addr");
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let shared = recorded.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let recorded = shared.clone();
                tokio::spawn(async move {
                    if let Err(error) = serve_cloud(stream, &recorded).await {
                        eprintln!("mock cloud connection ended: {error}");
                    }
                });
            }
        });
        Self {
            addr,
            recorded,
            task,
        }
    }

    fn batches(&self) -> Vec<(String, Value)> {
        self.recorded.lock().expect("recorded").batches.clone()
    }

    fn overview_hits(&self) -> usize {
        self.recorded.lock().expect("recorded").overview_hits
    }
}

impl Drop for MockCloud {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn usage(used: u32) -> Value {
    json!({
        "dailyUsed": used, "dailyLimit": 20, "monthlyUsed": used, "monthlyLimit": 300,
        "dailyResetAt": "2026-10-10T16:00:00Z", "monthlyResetAt": "2026-10-31T16:00:00Z"
    })
}

async fn serve_cloud(mut stream: TcpStream, recorded: &Mutex<Recorded>) -> std::io::Result<()> {
    let (head, body) = read_request(&mut stream).await?;
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let (status, payload) = match (method.as_str(), path.as_str()) {
        ("GET", "/api/v1/notifications/catalog") => (
            "200 OK",
            json!({"kinds": [{"kind": "email", "available": true}]}),
        ),
        ("GET", "/api/v1/notifications/overview") => {
            recorded.lock().expect("recorded").overview_hits += 1;
            (
                "200 OK",
                json!({
                    "enabled": true,
                    "usage": usage(0),
                    "maxChannels": 2,
                    "channels": [{
                        "id": "c1", "kind": "email", "name": "邮件", "enabled": true,
                        // 只订阅完成：created / started 不应上报。
                        "events": ["task.completed"], "deviceIds": [],
                        "target": "****abcd", "status": "ok", "createdAt": "2026-10-09T00:00:00Z"
                    }],
                    "accountEmail": "me@example.com"
                }),
            )
        }
        ("POST", "/api/v1/notifications/events") => {
            let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let results: Vec<Value> = parsed["events"]
                .as_array()
                .map(|events| {
                    events
                        .iter()
                        .map(|event| {
                            json!({"deliveryId": event["deliveryId"], "outcome": "accepted"})
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut recorded = recorded.lock().expect("recorded");
            recorded
                .batches
                .push((header(&head, "authorization").unwrap_or_default(), parsed));
            let used = u32::try_from(recorded.batches.len()).unwrap_or(u32::MAX);
            (
                "200 OK",
                json!({ "results": results, "usage": usage(used) }),
            )
        }
        _ => (
            "404 Not Found",
            json!({"code": "not_found", "message": "not mocked"}),
        ),
    };
    let payload = payload.to_string();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

struct Dirs {
    root: PathBuf,
}

impl Dirs {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "fluxdown_cloud_notify_e2e_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        Self { root }
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn save(&self) -> PathBuf {
        self.root.join("downloads")
    }

    fn config(&self) -> LocalHostConfig {
        LocalHostConfig {
            data_dir: self.data().display().to_string(),
            save_dir: self.save().display().to_string(),
            platform: "test".to_owned(),
            device_name: None,
        }
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!("test cleanup failed for {}: {error}", self.root.display());
        }
    }
}

fn create_request(url: String) -> CreateTaskRequestDto {
    CreateTaskRequestDto {
        url,
        file_name: String::new(),
        save_dir: String::new(),
        segments: 0,
        queue_id: String::new(),
        start_paused: false,
        cookies: String::new(),
        referrer: String::new(),
        user_agent: String::new(),
        proxy_url: String::new(),
        checksum: String::new(),
        ignore_tls_errors: false,
        headers: HashMap::new(),
        http_user: String::new(),
        http_password: String::new(),
        save_site_auth: false,
        unattended: false,
        offered_save_dir: None,
    }
}

/// 预置 agent 私有状态：已登录、本机上报开、地址指向 mock 云端。必须在宿主启动前写入并释放锁。
async fn seed_agent_state(dirs: &Dirs, cloud: SocketAddr) {
    let agent_dir = dirs.data().join("agent");
    let store = StateStore::open(agent_dir).await.expect("open agent state");
    let state = AgentState {
        device_id: DEVICE_ID.to_owned(),
        credentials: Some(CloudCredentials {
            access_token: ACCESS_TOKEN.to_owned(),
            refresh_token: "e2e-refresh".to_owned(),
            expires_at_unix: i64::MAX,
            // 缺会话的凭证会在启动自检时被判无效而清除。
            session: Some(
                serde_json::from_value(json!({
                    "user": { "id": "e2e-user", "email": "me@example.com" },
                    "currentPlan": null,
                    "device": { "id": "d1", "deviceId": DEVICE_ID }
                }))
                .expect("session fixture"),
            ),
            issuer: String::new(),
        }),
        cloud_base_url_override: Some(format!("http://{cloud}")),
        cloud_notify: CloudNotifyPrefs {
            reporting: true,
            include_url: false,
            include_save_dir: false,
        },
        ..AgentState::default()
    };
    store.save(&state).await.expect("seed agent state");
}

async fn rpc(session: &HostSession, method: &str, params: Option<Value>) -> Value {
    let text = tokio::time::timeout(
        STEP,
        session.call(method.to_owned(), params.map(|value| value.to_string())),
    )
    .await
    .unwrap_or_else(|_| panic!("{method} timed out"))
    .unwrap_or_else(|error| panic!("{method} failed: {error:?}"));
    serde_json::from_str(&text).expect("rpc result json")
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(STEP, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completed_download_reaches_the_cloud_trimmed_by_privacy_switches() {
    let dirs = Dirs::new();
    let files = FileServer::start().await;
    let cloud = MockCloud::start().await;
    seed_agent_state(&dirs, cloud.addr).await;

    let core = FluxCore::new().expect("core");
    let session = core.open_local(dirs.config()).await.expect("open local");

    // 启动即拉概览：本机渠道匹配所需的缓存就绪后才有事件可上报。
    wait_until("overview pulled by the agent", || {
        cloud.overview_hits() >= 1
    })
    .await;
    let state = loop {
        let state = rpc(&session, "agent.cloudNotify.get", None).await;
        if state["overview"]["channels"]
            .as_array()
            .is_some_and(|c| !c.is_empty())
            && state["catalog"][0]["kind"] == "email"
        {
            break state;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(state["reporting"], true);
    assert_eq!(state["includeUrl"], false);
    assert!(
        state["overview"].get("kinds").is_none(),
        "kinds 已由 catalog 取代"
    );

    // 隐私默认：只发文件名 / 大小 / 状态 / 队列名。
    session
        .create_task(create_request(files.url("fast1.bin")))
        .await
        .expect("create first task");
    wait_until("first batch", || !cloud.batches().is_empty()).await;
    let batches = cloud.batches();
    let (authorization, body) = &batches[0];
    assert_eq!(authorization, &format!("Bearer {ACCESS_TOKEN}"));
    let events = body["events"].as_array().expect("events");
    assert_eq!(events.len(), 1, "只订阅了 task.completed：{body}");
    assert_eq!(events[0]["event"], "task.completed");
    assert!(
        !events[0]["deliveryId"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    );
    let task = &events[0]["task"];
    assert_eq!(task["fileName"], "fast1.bin");
    assert_eq!(task["totalBytes"], i64::try_from(FILE_LEN).expect("len"));
    assert_eq!(task["status"], 3);
    for hidden in ["url", "saveDir", "id"] {
        assert!(task.get(hidden).is_none(), "{hidden} 不得上报：{task}");
    }

    // 打开两个隐私开关（走 agent RPC）后，下一次上报才带下载地址与保存目录。
    let state = rpc(
        &session,
        "agent.cloudNotify.setPrivacy",
        Some(json!({"includeUrl": true, "includeSaveDir": true})),
    )
    .await;
    assert_eq!(state["includeUrl"], true);
    assert_eq!(state["includeSaveDir"], true);
    session
        .create_task(create_request(files.url("fast2.bin")))
        .await
        .expect("create second task");
    wait_until("second batch", || cloud.batches().len() >= 2).await;
    let batches = cloud.batches();
    let task = &batches[1].1["events"][0]["task"];
    assert_eq!(task["fileName"], "fast2.bin");
    assert_eq!(task["url"], files.url("fast2.bin"));
    assert!(
        task["saveDir"]
            .as_str()
            .is_some_and(|dir| std::path::Path::new(dir) == dirs.save().as_path()),
        "saveDir = {}",
        task["saveDir"]
    );

    // 关闭本机上报：之后的完成事件不再发往云端。
    let state = rpc(
        &session,
        "agent.cloudNotify.setReporting",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(state["reporting"], false);
    session
        .create_task(create_request(files.url("fast3.bin")))
        .await
        .expect("create third task");
    wait_until("third file on disk", || {
        dirs.save().join("fast3.bin").is_file()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(cloud.batches().len(), 2, "上报关闭后不应再有批次");

    session.disconnect();
    tokio::time::timeout(STEP, core.shutdown_local())
        .await
        .expect("shutdown_local timeout");
}
