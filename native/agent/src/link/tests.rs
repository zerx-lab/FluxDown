//! 局域网直连的端到端测试：两个进程内 `LinkService`（各自独立的状态文件）经**真实 HTTP**
//! （`fluxdown_api::server::api_router` + 回环端口）完成 probe → hello → reveal → SAS → confirm →
//! 信息交换 → 下发；另用裸 TCP 服务模拟反代返回的非 FluxDown 响应、旧版对端与冒充设备的代理。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use fluxdown_api::server::ApiServerConfig;
use fluxdown_api::service::{ApiError, ApiHost};
use fluxdown_link::{LinkError, PeerAddress};
use fluxdown_protocol::{
    AgentSnapshot, CreateTaskRequest, DaemonConfigSnapshot, DaemonSnapshot, DownloadRequest,
    ErrorReason, LinkAuth, LinkPairConfirmOutcome, LinkPairConfirmRequest, LinkPairHelloRequest,
    LinkPairHelloResponse, LinkPairRevealRequest, LinkPairRevealResponse, LinkPingInfo, PathStyle,
    QueueDto, TaskDto,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use super::{
    ErrorContext, LinkNetworkOptions, LinkOpError, LinkService, LinkServiceParts, LinkTaskCreator,
    lan_base_urls, resolve_receive_dir, rpc_error,
};
use crate::event_hub::AgentEventHub;
use crate::state::{AgentState, CloudCredentials, StateStore};

// ── 测试夹具 ───────────────────────────────────────────────────────────────

/// 记录每次建任务尝试；`reject_with_dir` 时带目录的尝试一律失败（模拟目录不可写）。
#[derive(Default)]
struct RecordingTasks {
    attempts: StdMutex<Vec<CreateTaskRequest>>,
    routes: StdMutex<Vec<Option<fluxdown_protocol::CategoryRouteDto>>>,
    reject_with_dir: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl LinkTaskCreator for RecordingTasks {
    async fn create_task_with_route(
        &self,
        request: CreateTaskRequest,
        category_route: Option<fluxdown_protocol::CategoryRouteDto>,
    ) -> Result<String, ApiError> {
        let mut attempts = self.attempts.lock().unwrap();
        let reject = self
            .reject_with_dir
            .load(std::sync::atomic::Ordering::Relaxed)
            && !request.save_dir.is_empty();
        attempts.push(request);
        self.routes.lock().unwrap().push(category_route);
        if reject {
            return Err(ApiError::Internal("directory not writable".to_owned()));
        }
        Ok(format!("task-{}", attempts.len()))
    }
}

/// 只实现互联相关路由的宿主（其余下载能力不在本测试范围）。
struct LinkOnlyHost {
    link: Arc<LinkService>,
}

#[async_trait]
impl ApiHost for LinkOnlyHost {
    async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError> {
        Ok(Vec::new())
    }
    async fn get_task(&self, _task_id: &str) -> Result<Option<TaskDto>, ApiError> {
        Ok(None)
    }
    async fn create_task(&self, _req: CreateTaskRequest) -> Result<String, ApiError> {
        Err(ApiError::Unavailable)
    }
    async fn delete_task(&self, _task_id: &str, _delete_files: bool) -> Result<(), ApiError> {
        Err(ApiError::Unavailable)
    }
    async fn pause_task(&self, _task_id: &str) -> Result<(), ApiError> {
        Err(ApiError::Unavailable)
    }
    async fn continue_task(&self, _task_id: &str) -> Result<(), ApiError> {
        Err(ApiError::Unavailable)
    }
    async fn pause_all(&self) -> Result<(), ApiError> {
        Err(ApiError::Unavailable)
    }
    async fn continue_all(&self) -> Result<(), ApiError> {
        Err(ApiError::Unavailable)
    }
    async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
        Ok(Vec::new())
    }
    async fn submit_external(&self, _req: DownloadRequest) -> Result<(), ApiError> {
        Err(ApiError::Unavailable)
    }

    async fn link_ping_info(&self) -> Option<LinkPingInfo> {
        self.link.api_ping_info()
    }
    async fn link_pair_hello(
        &self,
        req: LinkPairHelloRequest,
        source: Option<IpAddr>,
    ) -> Result<LinkPairHelloResponse, ApiError> {
        self.link.api_pair_hello(req, source).await
    }
    async fn link_pair_reveal(
        &self,
        req: LinkPairRevealRequest,
    ) -> Result<LinkPairRevealResponse, ApiError> {
        self.link.api_pair_reveal(req).await
    }
    async fn link_pair_confirm(
        &self,
        req: LinkPairConfirmRequest,
    ) -> Result<LinkPairConfirmOutcome, ApiError> {
        self.link.api_pair_confirm(req).await
    }
    async fn link_create_task(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        self.link.api_create_task(auth, body).await
    }
    async fn link_peer_info(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        self.link.api_peer_info(auth, body).await
    }
    async fn link_unpair(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        self.link.api_unpair(auth, body).await
    }
}

/// 一个完整的互联节点：独立状态目录 + 独立身份 + 回环 HTTP 服务。
struct Node {
    service: Arc<LinkService>,
    events: AgentEventHub,
    state: Arc<Mutex<AgentState>>,
    store: Arc<StateStore>,
    tasks: Arc<RecordingTasks>,
    addr: SocketAddr,
    dir: PathBuf,
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.dir) {
            tracing::warn!(path = %self.dir.display(), %error, "link test node cleanup failed");
        }
    }
}

fn local_abs_dir(name: &str) -> String {
    if cfg!(windows) {
        format!(r"D:\{name}")
    } else {
        format!("/srv/{name}")
    }
}

fn foreign_abs_dir(name: &str) -> String {
    if cfg!(windows) {
        format!("/srv/{name}")
    } else {
        format!(r"D:\{name}")
    }
}

fn no_mdns() -> LinkNetworkOptions {
    LinkNetworkOptions {
        mdns: false,
        advertise: Vec::new(),
    }
}

impl Node {
    async fn start(label: &str) -> Node {
        Self::start_with(label, false).await
    }

    /// 网关监听非回环地址（通配）、自报地址覆盖为 `127.0.0.1:<实际端口>`：本机对外可达，配对与
    /// 信息交换里会向对端自报可回连地址（测试里 `127.0.0.1` 就是真能连上的那个）。
    async fn start_reachable(label: &str) -> Node {
        Self::start_with(label, true).await
    }

    async fn start_with(label: &str, reachable: bool) -> Node {
        let advertise = if reachable {
            vec!["127.0.0.1".to_owned()]
        } else {
            Vec::new()
        };
        Self::start_custom(label, reachable, advertise).await
    }

    /// `wildcard`：网关监听通配地址；`advertise`：自报地址覆盖条目。两者独立，才能构造「只监听
    /// 回环、但配置了覆盖地址（反代 / 端口映射）」的节点。
    async fn start_custom(label: &str, wildcard: bool, advertise: Vec<String>) -> Node {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown-link-test-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.unwrap());
        let state = Arc::new(Mutex::new(AgentState {
            device_name: label.to_owned(),
            ..AgentState::default()
        }));
        let mut snapshot = AgentSnapshot::default();
        snapshot.preferences.values.insert(
            fluxdown_protocol::CUSTOM_CATEGORIES_PREF_KEY.to_owned(),
            serde_json::json!([
                {"id":"bin","name":"Binary","extensions":["bin"],"saveDir":"category-bin"}
            ]),
        );
        let events = AgentEventHub::new(snapshot);
        events.replace_daemon_snapshot(DaemonSnapshot {
            config: DaemonConfigSnapshot {
                revision: 1,
                values: [("default_save_dir".to_owned(), local_abs_dir(label))].into(),
            },
            ..DaemonSnapshot::default()
        });
        let tasks = Arc::new(RecordingTasks::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let bound = if wildcard {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), addr.port())
        } else {
            addr
        };
        let service = LinkService::new(LinkServiceParts {
            events: events.clone(),
            state: state.clone(),
            store: store.clone(),
            tasks: tasks.clone(),
            bound,
            server_mode: false,
            network: LinkNetworkOptions {
                mdns: false,
                advertise,
            },
        });
        service.start().await.unwrap();
        let router = fluxdown_api::server::api_router(
            Arc::new(LinkOnlyHost {
                link: service.clone(),
            }),
            ApiServerConfig::from_config_map(&HashMap::new(), "test"),
        );
        tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("serve link test node");
        });
        Node {
            service,
            events,
            state,
            store,
            tasks,
            addr,
            dir,
        }
    }

    fn address(&self) -> String {
        self.addr.to_string()
    }

    fn fingerprint(&self) -> String {
        self.service.api_ping_info().unwrap().fingerprint
    }

    fn attempts(&self) -> Vec<CreateTaskRequest> {
        self.tasks.attempts.lock().unwrap().clone()
    }
}

async fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    for _ in 0..250 {
        if let Some(value) = probe() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

/// 完整配对：`initiator` 用 `responder` 展示的码配对，响应端批准。
async fn pair(responder: &Node, initiator: &Node) {
    let code = responder.service.pairing_code().await.unwrap();
    let begin = initiator
        .service
        .pair_begin(&responder.address(), &code.code, false)
        .await
        .unwrap();
    let request = wait_for("incoming pairing request", || {
        responder
            .events
            .inspect(|snapshot| snapshot.link_pairing_requests.first().cloned())
    })
    .await;
    let finisher = {
        let service = initiator.service.clone();
        let token = begin.token.clone();
        tokio::spawn(async move { service.pair_finish(&token, true).await })
    };
    responder
        .service
        .approve(&request.session_id, true)
        .unwrap();
    let device = finisher.await.unwrap().unwrap();
    assert!(device.is_some(), "initiator must end up paired");
    let initiator_fp = initiator.fingerprint();
    wait_for("responder roster entry", || {
        responder
            .events
            .inspect(|snapshot| {
                snapshot
                    .linked_devices
                    .iter()
                    .any(|device| device.fingerprint == initiator_fp)
            })
            .then_some(())
    })
    .await;
}

#[tokio::test]
async fn bound_update_during_start_does_not_wait_for_readiness_or_expand_reachability() {
    let node = Node::start("port-rebind-start").await;
    let service = LinkService::new(LinkServiceParts {
        events: node.events.clone(),
        state: node.state.clone(),
        store: node.store.clone(),
        tasks: node.tasks.clone(),
        bound: node.addr,
        server_mode: false,
        network: no_mdns(),
    });
    let locked_state = node.state.lock().await;
    let startup = service.start();
    tokio::pin!(startup);
    tokio::select! {
        result = &mut startup => panic!("startup completed with its state locked: {result:?}"),
        () = tokio::task::yield_now() => {}
    }
    let mut rebound = node.addr;
    rebound.set_port(17801);
    tokio::time::timeout(Duration::from_secs(1), service.update_bound(rebound))
        .await
        .expect("a bound update must not wait for link startup")
        .unwrap();
    drop(locked_state);
    startup.await.unwrap();
    let code = service.pairing_code().await.unwrap();
    assert_eq!(code.fingerprint, node.fingerprint());
    assert!(
        code.addresses.is_empty(),
        "loopback must stay private after rebinding"
    );
    assert!(matches!(
        service.update_bound("0.0.0.0:17801".parse().unwrap()).await,
        Err(LinkOpError::Invalid("bound"))
    ));
    service.update_bound(node.addr).await.unwrap();
    assert_eq!(
        service.api_ping_info().unwrap().fingerprint,
        node.fingerprint()
    );
}

/// 裸 TCP「反代」：对任何请求回固定的 HTTP 响应，并记录首行请求行。
async fn raw_server(response: impl Into<String>) -> (SocketAddr, Arc<StdMutex<Vec<String>>>) {
    let response: Arc<str> = Arc::from(response.into());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let request_lines = Arc::new(StdMutex::new(Vec::new()));
    let recorded = request_lines.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let recorded = recorded.clone();
            let response = response.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 8192];
                let read = match socket.read(&mut buffer).await {
                    Ok(read) => read,
                    Err(error) => {
                        tracing::debug!(%error, "raw link test client closed before its request");
                        return;
                    }
                };
                let text = String::from_utf8_lossy(&buffer[..read]).into_owned();
                if let Some(line) = text.lines().next() {
                    recorded.lock().unwrap().push(line.to_owned());
                }
                // TLS/plaintext mismatch tests can close the connection as soon as they reject the reply.
                if let Err(error) = socket.write_all(response.as_bytes()).await {
                    tracing::debug!(%error, "raw link test client closed before its response");
                    return;
                }
                if let Err(error) = socket.shutdown().await {
                    tracing::debug!(%error, "raw link test client closed before writer shutdown");
                }
            });
        }
    });
    (addr, request_lines)
}

/// 带 JSON 体的固定 HTTP 响应（供 [`raw_server`] 使用；`Content-Length` 自动计算）。
fn http_json(status_line: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// 裸 TCP 冒充代理：`GET /ping` 由它自己应答并声称设备指纹是 `spoofed_fingerprint`，
/// 其余连接原样转发给 `upstream`——等价于网络上的中间人在发现阶段之后换了一台设备。
async fn spoofing_proxy(upstream: SocketAddr, spoofed_fingerprint: String) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else {
                break;
            };
            let fingerprint = spoofed_fingerprint.clone();
            tokio::spawn(async move {
                let mut first = [0u8; 16];
                let Ok(read) = client.peek(&mut first).await else {
                    return;
                };
                if first[..read].starts_with(b"GET /ping") {
                    let mut request = vec![0u8; 4096];
                    let read = client
                        .read(&mut request)
                        .await
                        .expect("read spoofed ping request");
                    assert!(read > 0, "spoofed ping request must be present");
                    let body = serde_json::json!({
                        "success": true,
                        "app": "FluxDown",
                        "version": "9.9.9",
                        "message": "pong",
                        "linkFingerprint": fingerprint,
                        "linkName": "spoofed",
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    client
                        .write_all(response.as_bytes())
                        .await
                        .expect("write spoofed ping response");
                    if let Err(error) = client.shutdown().await {
                        tracing::debug!(%error, "spoofing proxy client closed before writer shutdown");
                    }
                    return;
                }
                let Ok(mut server) = tokio::net::TcpStream::connect(upstream).await else {
                    return;
                };
                // 身份校验失败会让调用方提前关闭代理连接。
                if let Err(error) = tokio::io::copy_bidirectional(&mut client, &mut server).await {
                    tracing::debug!(%error, "spoofing proxy connection closed during relay");
                }
            });
        }
    });
    addr
}

/// 直接对 `addr` 发一条 JSON POST，返回 `(状态码, JSON 响应体)`。
async fn raw_post(addr: SocketAddr, path: &str, body: &str) -> (u16, serde_json::Value) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8(response).unwrap();
    let status: u16 = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
    (status, serde_json::from_str(body).unwrap())
}

const NGINX_400: &str = "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nContent-Length: 61\r\nConnection: close\r\n\r\n<html><body>The plain HTTP request was sent to HTTPS</body></html>";
const SPA_200: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 28\r\nConnection: close\r\n\r\n<!doctype html><html></html>";

// ── 端到端 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pairs_over_real_http_exchanges_info_and_dispatches() {
    let responder = Node::start("nas").await;
    let initiator = Node::start("laptop").await;

    // 只监听回环：局域网设备连不上本机，配对码结果不带地址。
    let code = responder.service.pairing_code().await.unwrap();
    assert!(code.addresses.is_empty());
    assert_eq!(code.fingerprint, responder.fingerprint());
    assert_eq!(code.code.len(), 6);

    // 手动探测：`/ping` 带 `linkFingerprint`（Flutter 旧版对端靠它做 TOFU）。
    let peer = initiator.service.probe(&responder.address()).await.unwrap();
    assert_eq!(peer.fingerprint.as_deref(), Some(code.fingerprint.as_str()));
    assert_eq!(peer.source, "manual");

    let begin = initiator
        .service
        .pair_begin(&responder.address(), &code.code, false)
        .await
        .unwrap();
    assert_eq!(begin.peer_fingerprint, responder.fingerprint());
    // 入站请求进快照，两端 SAS 一致，由响应端用户决定。
    let request = wait_for("incoming pairing request", || {
        responder
            .events
            .inspect(|snapshot| snapshot.link_pairing_requests.first().cloned())
    })
    .await;
    assert_eq!(request.sas, begin.sas);
    assert_eq!(request.peer_fingerprint, initiator.fingerprint());
    assert_eq!(request.peer_name, "laptop");

    let finisher = {
        let service = initiator.service.clone();
        let token = begin.token.clone();
        tokio::spawn(async move { service.pair_finish(&token, true).await })
    };
    responder
        .service
        .approve(&request.session_id, true)
        .unwrap();
    let paired = finisher.await.unwrap().unwrap().unwrap();
    assert_eq!(paired.fingerprint, responder.fingerprint());
    assert_eq!(paired.name, "nas");
    // 处理过的请求不再留在快照里。
    assert!(
        responder
            .events
            .inspect(|snapshot| snapshot.link_pairing_requests.is_empty())
    );

    // 探测在线并交换默认目录 / 路径风格。
    let devices = initiator.service.refresh().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert!(devices[0].online);
    assert_eq!(
        devices[0].default_save_dir.as_deref(),
        Some(local_abs_dir("nas").as_str())
    );
    assert_eq!(devices[0].path_style, Some(PathStyle::current()));
    let published = initiator
        .events
        .inspect(|snapshot| snapshot.linked_devices.clone());
    assert!(published[0].online);
    assert_eq!(published[0].default_save_dir, devices[0].default_save_dir);

    // 响应端也从这次已认证请求里学到了发起端的目录（写进它自己的名册）。
    let initiator_fp = initiator.fingerprint();
    let learned = wait_for("responder to learn initiator info", || {
        responder.events.inspect(|snapshot| {
            snapshot
                .linked_devices
                .iter()
                .find(|device| device.fingerprint == initiator_fp)
                .and_then(|device| device.default_save_dir.clone())
        })
    })
    .await;
    assert_eq!(learned, local_abs_dir("laptop"));
    // 名册（含链路密钥）持久化在私有状态里，快照 / 事件里没有密钥。
    let roster = responder.state.lock().await.linked_devices.clone();
    assert_eq!(roster.len(), 1);
    assert!(roster[0].get("linkSecretB64").is_some());
    let snapshot_json = serde_json::to_string(&responder.events.snapshot()).unwrap();
    assert!(!snapshot_json.contains("linkSecret"));

    // 下发：带本机风格绝对目录 → 原样到达接收端；不带目录 → 空串（用接收端默认目录）。
    let target = responder.fingerprint();
    let dir = local_abs_dir("incoming");
    let task_id = initiator
        .service
        .dispatch(
            &target,
            "https://example.com/big.iso",
            Some("big.iso"),
            Some(&dir),
        )
        .await
        .unwrap();
    assert_eq!(task_id, "task-1");
    let default_dir_id = initiator
        .service
        .dispatch(&target, "https://example.com/other.bin", None, None)
        .await
        .unwrap();
    assert_eq!(default_dir_id, "task-2");
    let attempts = responder.attempts();
    assert_eq!(attempts[0].url, "https://example.com/big.iso");
    assert_eq!(attempts[0].file_name, "big.iso");
    assert_eq!(attempts[0].save_dir, dir);
    assert_eq!(attempts[1].save_dir, "");

    // 发起端按对端上报的路径风格校验目录：外来风格路径在发送前就被拒绝。
    let error = initiator
        .service
        .dispatch(
            &target,
            "https://example.com/x",
            None,
            Some(&foreign_abs_dir("x")),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, LinkOpError::SaveDirUnavailable));
    let data = rpc_error(&error, ErrorContext::Peer);
    assert_eq!(data.reason, Some(ErrorReason::SaveDirUnavailable));
    assert_eq!(responder.attempts().len(), 2, "rejected before any request");

    // 解除配对。
    initiator.service.remove(&target).await.unwrap();
    assert!(
        initiator
            .events
            .inspect(|snapshot| snapshot.linked_devices.is_empty())
    );
    let error = initiator.service.remove(&target).await.unwrap_err();
    assert_eq!(
        rpc_error(&error, ErrorContext::Peer).reason,
        Some(ErrorReason::PeerNotPaired)
    );
}

#[tokio::test]
async fn receiver_falls_back_to_default_dir_and_retries_without_dir() {
    let responder = Node::start("nas").await;
    let initiator = Node::start("laptop").await;
    pair(&responder, &initiator).await;
    let target = responder.fingerprint();

    // 绕过发送端校验（模拟旧版发起端）：外来风格绝对路径 → 接收端用默认目录。
    let manager = initiator.service.manager().unwrap();
    manager
        .dispatch(
            &target,
            "https://example.com/a.bin",
            Some(&foreign_abs_dir("nope")),
            None,
        )
        .await
        .unwrap();
    assert_eq!(responder.attempts()[0].save_dir, "");
    assert_eq!(responder.attempts().len(), 1);
    {
        let routes = responder.tasks.routes.lock().unwrap();
        assert!(
            routes[0].is_some(),
            "foreign-style directory fallback enables routing"
        );
    }

    // 带目录建任务失败 → 去掉目录重试一次，任务仍然建成。
    responder
        .tasks
        .reject_with_dir
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let dir = local_abs_dir("readonly");
    let task_id = initiator
        .service
        .dispatch(&target, "https://example.com/b.bin", None, Some(&dir))
        .await
        .unwrap();
    assert_eq!(task_id, "task-3");
    let attempts = responder.attempts();
    assert_eq!(attempts.len(), 3);
    assert_eq!(attempts[1].save_dir, dir);
    assert_eq!(attempts[2].save_dir, "");
    let routes = responder.tasks.routes.lock().unwrap();
    assert!(
        routes[1].is_none(),
        "explicit usable directory disables routing"
    );
    assert!(
        routes[2].is_some(),
        "unavailable directory fallback enables routing"
    );
}

#[tokio::test]
async fn wrong_code_is_reported_as_invalid_code_not_as_a_network_problem() {
    let responder = Node::start("nas").await;
    let initiator = Node::start("laptop").await;
    let code = responder.service.pairing_code().await.unwrap();
    let wrong = if code.code == "000000" {
        "000001"
    } else {
        "000000"
    };
    let error = initiator
        .service
        .pair_begin(&responder.address(), wrong, false)
        .await
        .unwrap_err();
    assert!(matches!(error, LinkOpError::Link(LinkError::InvalidCode)));
    assert_eq!(
        rpc_error(&error, ErrorContext::Pairing).reason,
        Some(ErrorReason::PairingCodeInvalid)
    );
}

#[tokio::test]
async fn pairing_aborts_before_reveal_when_peer_identity_differs_from_the_probed_one() {
    // 发现阶段（/ping）看到该地址是设备 F；握手时地址上出示的身份却是另一台设备：
    // 发起方必须在揭示临时公钥之前就终止，响应方不会弹出任何待核对的入站配对。
    let responder = Node::start("nas").await;
    let initiator = Node::start("laptop").await;
    let spoofed = "f".repeat(64);
    let proxy = spoofing_proxy(responder.addr, spoofed.clone())
        .await
        .to_string();

    let probed = initiator.service.probe(&proxy).await.unwrap();
    assert_eq!(probed.fingerprint.as_deref(), Some(spoofed.as_str()));

    let code = responder.service.pairing_code().await.unwrap();
    let error = initiator
        .service
        .pair_begin(&proxy, &code.code, false)
        .await
        .unwrap_err();
    assert!(
        matches!(error, LinkOpError::Link(LinkError::IdentityMismatch(_))),
        "{error:?}"
    );
    assert_eq!(
        rpc_error(&error, ErrorContext::Pairing).reason,
        Some(ErrorReason::PairingSignatureInvalid)
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        responder
            .events
            .inspect(|snapshot| snapshot.link_pairing_requests.is_empty()),
        "no SAS may be shown on the responder before the initiator reveals"
    );
    assert!(
        initiator
            .events
            .inspect(|snapshot| snapshot.linked_devices.is_empty())
    );
}

#[tokio::test]
async fn old_protocol_peers_are_rejected_with_an_explicit_version_error() {
    let initiator = Node::start("laptop").await;

    // 旧版响应方：看不懂新 hello，以「invalid link hello payload」拒绝。
    let rejection = http_json(
        "400 Bad Request",
        r#"{"success":false,"message":"invalid link hello payload: missing field `initiatorEphPub` at line 1 column 120"}"#,
    );
    let (addr, _) = raw_server(rejection).await;
    let error = initiator
        .service
        .pair_begin(&addr.to_string(), "123456", false)
        .await
        .unwrap_err();
    assert!(
        matches!(error, LinkOpError::Link(LinkError::UnsupportedVersion)),
        "{error:?}"
    );
    let data = rpc_error(&error, ErrorContext::Pairing);
    assert_eq!(data.code, fluxdown_protocol::ApplicationErrorCode::Conflict);
    assert!(!data.retryable);
    assert_eq!(
        data.reason,
        Some(fluxdown_protocol::ErrorReason::PairingVersionMismatch)
    );

    // 旧版响应方的 hello 回复：带明文 SAS 与自己的签名、没有 protocolVersion。
    let legacy_reply = http_json(
        "200 OK",
        r#"{"sessionId":"s","responderEphPub":"AA","responderIdPub":"AA","responderSig":"AA","name":"old-nas","sas":"123456"}"#,
    );
    let (addr, _) = raw_server(legacy_reply).await;
    let error = initiator
        .service
        .pair_begin(&addr.to_string(), "123456", false)
        .await
        .unwrap_err();
    assert!(
        matches!(error, LinkOpError::Link(LinkError::UnsupportedVersion)),
        "{error:?}"
    );

    // 旧版发起方的 hello（带临时公钥明文、没有 protocolVersion / 承诺）：响应方以版本
    // 不兼容拒绝，且不消耗配对码。
    let responder = Node::start("nas").await;
    let code = responder.service.pairing_code().await.unwrap();
    let legacy_hello = serde_json::json!({
        "code": code.code,
        "initiatorEphPub": "AAAA",
        "initiatorIdPub": "AAAA",
        "initiatorSig": "AAAA",
        "name": "old",
        "platform": "",
        "appVersion": "",
        "initiatorAddrs": [],
    })
    .to_string();
    let (status, body) = raw_post(responder.addr, "/api/v1/link/pair/hello", &legacy_hello).await;
    assert_eq!(status, 400);
    assert_eq!(body["success"], false);
    let message = body["message"].as_str().unwrap();
    assert!(matches!(
        LinkError::from_wire_message(message),
        Some(LinkError::UnsupportedVersion)
    ));
    initiator
        .service
        .pair_begin(&responder.address(), &code.code, false)
        .await
        .expect("the pairing code must survive a version-mismatched hello");
}

#[tokio::test]
async fn proxy_400_html_maps_to_not_fluxdown_and_uses_the_base_path() {
    let initiator = Node::start("laptop").await;
    let (addr, request_lines) = raw_server(NGINX_400).await;

    // https 反代通常挂在子路径下：请求必须落在 `<base>/api/v1/link/...`。
    let error = initiator
        .service
        .pair_begin(&format!("http://{addr}/fluxdown/"), "123456", false)
        .await
        .unwrap_err();
    assert!(
        matches!(error, LinkOpError::Link(LinkError::NotFluxDown(_))),
        "an HTML 400 must not be reported as a wrong pairing code: {error:?}"
    );
    let data = rpc_error(&error, ErrorContext::Pairing);
    assert_eq!(data.reason, Some(ErrorReason::PairingNotFluxDown));
    assert!(!data.retryable);
    let lines = request_lines.lock().unwrap().clone();
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].starts_with("POST /fluxdown/api/v1/link/pair/hello "),
        "unexpected request line: {}",
        lines[0]
    );

    // 探测也一样：SPA 兜底页 200 不是 FluxDown。
    let (spa, _) = raw_server(SPA_200).await;
    let error = initiator
        .service
        .probe(&format!("http://{spa}"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        LinkOpError::Link(LinkError::NotFluxDown(_))
    ));
}

#[tokio::test]
async fn unreachable_and_bad_addresses_are_distinguished() {
    let initiator = Node::start("laptop").await;

    // 端口上没有监听者：网络失败 → 对端不可达（可重试）。
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    };
    let error = initiator
        .service
        .pair_begin(&closed.to_string(), "123456", false)
        .await
        .unwrap_err();
    let data = rpc_error(&error, ErrorContext::Pairing);
    assert_eq!(data.reason, Some(ErrorReason::PairingPeerUnreachable));
    assert!(data.retryable);

    // https 地址打到明文端口：TLS 握手失败也是网络失败，且根因进错误信息。
    let (plain, _) = raw_server(NGINX_400).await;
    let error = initiator
        .service
        .pair_begin(&format!("https://{plain}"), "123456", false)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, LinkOpError::Link(LinkError::Io(cause)) if !cause.is_empty()),
        "{error:?}"
    );

    // 地址本身非法 → 参数错误并指明字段。
    let error = initiator
        .service
        .pair_begin("ftp://nas.local", "123456", false)
        .await
        .unwrap_err();
    let data = rpc_error(&error, ErrorContext::Pairing);
    assert_eq!(data.field.as_deref(), Some("address"));
}

#[tokio::test]
async fn rpc_dispatch_maps_methods_and_errors() {
    let node = Node::start("nas").await;
    let code = node
        .service
        .rpc(fluxdown_protocol::method::AGENT_LINK_PAIRING_CODE, None)
        .await
        .unwrap();
    assert_eq!(code["addresses"], serde_json::json!([]));
    assert_eq!(code["code"].as_str().unwrap().len(), 6);

    let error = node
        .service
        .rpc(
            fluxdown_protocol::method::AGENT_LINK_REMOVE,
            Some(serde_json::json!({ "fingerprint": "missing" })),
        )
        .await
        .unwrap_err();
    assert_eq!(error.reason, Some(ErrorReason::PeerNotPaired));

    // 参数缺字段 → InvalidArgument，而不是 panic 或假成功。
    let error = node
        .service
        .rpc(fluxdown_protocol::method::AGENT_LINK_PROBE, None)
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        fluxdown_protocol::ApplicationErrorCode::InvalidArgument
    );

    let stopped = node
        .service
        .rpc(fluxdown_protocol::method::AGENT_LINK_STOP_PAIRING, None)
        .await
        .unwrap();
    assert_eq!(stopped["ok"], true);
    // 已停止的配对码不能再被使用。
    let peer = Node::start("laptop").await;
    let error = peer
        .service
        .pair_begin(&node.address(), code["code"].as_str().unwrap(), false)
        .await
        .unwrap_err();
    assert!(matches!(error, LinkOpError::Link(LinkError::InvalidCode)));
}

// ── 纯函数 ─────────────────────────────────────────────────────────────────

#[test]
fn receive_dir_only_accepts_local_style_absolute_paths() {
    assert_eq!(resolve_receive_dir(""), None);
    assert_eq!(resolve_receive_dir("   "), None);
    assert_eq!(resolve_receive_dir("relative/dir"), None);
    assert_eq!(resolve_receive_dir(&foreign_abs_dir("x")), None);
    let local = local_abs_dir("x");
    assert_eq!(resolve_receive_dir(&format!("  {local} ")), Some(local));
}

#[test]
fn receive_dir_rejects_parent_segments_and_unc_or_device_paths() {
    let local = local_abs_dir("x");
    assert_eq!(resolve_receive_dir(&format!("{local}/../y")), None);
    assert_eq!(resolve_receive_dir(r"\\host\share"), None);
    assert_eq!(resolve_receive_dir(r"\\?\C:\x"), None);
    assert_eq!(resolve_receive_dir("//host/share"), None);
}

#[test]
fn remote_file_names_must_be_plain_and_not_reserved_device_names() {
    assert!(super::valid_file_name("movie.mkv"));
    assert!(!super::valid_file_name("a/b"));
    assert!(!super::valid_file_name(".."));
    assert!(!super::valid_file_name("CON"));
    assert!(!super::valid_file_name("nul.txt"));
    assert!(!super::valid_file_name("com1.log"));
    assert!(super::valid_file_name("com10.log"));
}

#[test]
fn advertised_addresses_follow_the_bound_interface() {
    // 桌面默认只监听回环：没有任何局域网可达地址。
    assert!(lan_base_urls("127.0.0.1:17800".parse().unwrap()).is_empty());
    assert!(lan_base_urls("[::1]:17800".parse().unwrap()).is_empty());
    // 绑定了具体网卡地址：就是它。
    assert_eq!(
        lan_base_urls("192.168.1.20:17800".parse().unwrap()),
        vec!["http://192.168.1.20:17800".to_owned()]
    );
    // 通配地址：只会给出可路由的本机地址，绝不含回环 / 通配。
    for url in lan_base_urls("0.0.0.0:17800".parse().unwrap()) {
        let addr: SocketAddr = url.trim_start_matches("http://").parse().unwrap();
        assert!(
            !addr.ip().is_loopback() && !addr.ip().is_unspecified(),
            "{url}"
        );
        assert_eq!(addr.port(), 17800);
    }
}

#[test]
fn link_address_parser_is_shared_with_the_service() {
    // agent.link.* 的 `address` 与兼容 API 的 host+port 落到同一个候选形态。
    assert_eq!(
        PeerAddress::parse("nas.local").unwrap().to_candidate(),
        "nas.local:17800"
    );
    assert_eq!(
        PeerAddress::from_host_port("nas.local", 17800)
            .unwrap()
            .to_candidate(),
        "nas.local:17800"
    );
}

// ── v2 数据面 / 连通性维护 / 解除配对 ─────────────────────────────────────────

fn unix_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

/// 带自定义请求头的原始 HTTP 请求（绕过 `LinkManager`），返回 `(状态码, 响应头, 响应体)`。
async fn raw_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> (u16, HashMap<String, String>, Vec<u8>) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (key, value) in headers {
        request.push_str(&format!("{key}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    let head = String::from_utf8_lossy(&response[..split]).into_owned();
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    (status, headers, response[split + 4..].to_vec())
}

fn link_headers(device: &str, ts: i64, enc: &str) -> Vec<(&'static str, String)> {
    vec![
        ("X-FluxLink-Device", device.to_owned()),
        ("X-FluxLink-Ts", ts.to_string()),
        ("X-FluxLink-Nonce", "n1".to_owned()),
        ("X-FluxLink-Tag", "0".repeat(64)),
        ("X-FluxLink-Enc", enc.to_owned()),
    ]
}

#[tokio::test]
async fn data_plane_rejects_old_or_unknown_callers_with_diagnostic_reasons() {
    let node = Node::start("nas").await;
    let stranger = "ab".repeat(32);
    let now = unix_now();
    for path in [
        "/api/v1/link/tasks",
        "/api/v1/link/info",
        "/api/v1/link/unpair",
    ] {
        let cases = [
            // 旧方案（v1）与缺失方案头：一律拒绝，reason 只作诊断。
            (link_headers(&stranger, now, "v1"), "unauthorized"),
            (link_headers(&stranger, now, ""), "unauthorized"),
            // 方案对、形态对、但设备没配对。
            (link_headers(&stranger, now, "v2"), "notPaired"),
            // 时间戳相差超出容忍窗口：时钟偏差。
            (link_headers(&stranger, now - 3600, "v2"), "clockSkew"),
            // 缺少必须的头。
            (Vec::new(), "unauthorized"),
        ];
        for (headers, reason) in cases {
            let (status, _, body) = raw_request(node.addr, "POST", path, &headers, b"junk").await;
            assert_eq!(status, 401, "{path} {reason}");
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["reason"], reason, "{path}");
            assert_eq!(json["success"], false);
            assert!(json["serverTime"].as_i64().unwrap() >= now, "{path}");
        }
    }
}

#[tokio::test]
async fn ping_advertises_the_link_protocol_version() {
    let node = Node::start("nas").await;
    let (status, _, body) = raw_request(node.addr, "GET", "/ping", &[], b"").await;
    assert_eq!(status, 200);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["linkProtocol"], fluxdown_link::LINK_PROTOCOL_VERSION);
    assert_eq!(json["linkProtocol"], 2);
    assert_eq!(json["linkFingerprint"], node.fingerprint());
    // 声明支持「请求控制」的配对：发起方据此预检，旧版对端没有该字段。
    assert_eq!(json["linkControl"], fluxdown_link::LINK_CONTROL_VERSION);
    assert_eq!(json["linkControl"], 1);
    assert_eq!(
        node.service.api_ping_info().unwrap().link_control,
        fluxdown_link::LINK_CONTROL_VERSION
    );
    assert_eq!(
        node.service.api_ping_info().unwrap().link_protocol,
        fluxdown_link::LINK_PROTOCOL_VERSION
    );
}

#[tokio::test]
async fn pairing_handshake_routes_have_a_small_body_limit() {
    let node = Node::start("nas").await;
    let oversized = vec![b'x'; 20 * 1024];
    for path in [
        "/api/v1/link/pair/hello",
        "/api/v1/link/pair/reveal",
        "/api/v1/link/pair/confirm",
    ] {
        let (status, _, _) = raw_request(node.addr, "POST", path, &[], &oversized).await;
        assert_eq!(status, 413, "{path}");
        // 正常大小的非法载荷是 400，不是 413。
        let (status, _, _) = raw_request(node.addr, "POST", path, &[], b"{}").await;
        assert_eq!(status, 400, "{path}");
    }
}

#[tokio::test]
async fn an_unreadable_identity_seed_disables_the_link_service_without_replacing_the_identity() {
    let dir = std::env::temp_dir().join(format!(
        "fluxdown-link-test-seed-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let store = Arc::new(StateStore::open(dir.clone()).await.unwrap());
    let state = Arc::new(Mutex::new(AgentState {
        link_identity: Some(serde_json::Value::String("not base64!".into())),
        ..AgentState::default()
    }));
    let service = LinkService::new(LinkServiceParts {
        events: AgentEventHub::new(AgentSnapshot::default()),
        state: state.clone(),
        store,
        tasks: Arc::new(RecordingTasks::default()),
        bound: "127.0.0.1:1".parse().unwrap(),
        server_mode: false,
        network: no_mdns(),
    });
    assert!(service.start().await.is_err());
    // 坏种子原样保留，没有被新生成的身份覆盖。
    assert_eq!(
        state.lock().await.link_identity,
        Some(serde_json::Value::String("not base64!".into()))
    );
    // 服务没有启动：RPC 报「本机直连服务不可用」，而不是「对端不可达」。
    let error = service
        .rpc(fluxdown_protocol::method::AGENT_LINK_PAIRING_CODE, None)
        .await
        .unwrap_err();
    assert_eq!(error.reason, Some(ErrorReason::LinkUnavailable));
    assert_eq!(
        error.code,
        fluxdown_protocol::ApplicationErrorCode::Unavailable
    );
    assert!(service.api_ping_info().is_none());
    if let Err(error) = std::fs::remove_dir_all(&dir) {
        tracing::warn!(%error, "link test cleanup failed");
    }
}

#[tokio::test]
async fn peers_with_an_older_data_plane_surface_as_peer_outdated() {
    let responder = Node::start("nas").await;
    let initiator = Node::start("laptop").await;
    pair(&responder, &initiator).await;
    let target = responder.fingerprint();

    // 把候选地址换成一个冒充「旧版对端」的假服务：指纹对，但 `/ping` 没有 `linkProtocol`。
    let legacy = http_json(
        "200 OK",
        &serde_json::json!({ "success": true, "app": "FluxDown", "linkFingerprint": target })
            .to_string(),
    );
    let (legacy_addr, _) = raw_server(legacy).await;
    let storage = super::AgentLinkStorage::new(initiator.state.clone(), initiator.store.clone());
    fluxdown_link::LinkStorage::update_candidates(
        &storage,
        &target,
        &[fluxdown_link::PeerCandidate {
            kind: fluxdown_link::TransportKind::Direct,
            address: legacy_addr.to_string(),
        }],
    )
    .await
    .unwrap();

    let error = initiator
        .service
        .dispatch(&target, "https://example.com/x", None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, LinkOpError::Link(LinkError::PeerOutdated)),
        "{error:?}"
    );
    let data = rpc_error(&error, ErrorContext::Peer);
    assert_eq!(data.reason, Some(ErrorReason::PeerOutdated));
    assert!(!data.retryable);
    // 请求从未发出：真正的对端没有收到任何任务。
    assert!(responder.attempts().is_empty());
}

#[tokio::test]
async fn pairing_code_addresses_follow_the_advertise_override() {
    let node = Node::start_reachable("nas").await;
    let code = node.service.pairing_code().await.unwrap();
    assert_eq!(
        code.addresses,
        vec![format!("http://127.0.0.1:{}", node.addr.port())]
    );
}

#[tokio::test]
async fn rename_sets_a_local_alias_that_outranks_the_peers_own_name() {
    let alpha = Node::start_reachable("alpha").await;
    let bravo = Node::start_reachable("bravo").await;
    pair(&bravo, &alpha).await;
    let bravo_fp = bravo.fingerprint();

    let renamed = alpha
        .service
        .rename(&bravo_fp, Some("  书房 PC "))
        .await
        .unwrap();
    assert_eq!(renamed.name, "书房 PC");
    assert_eq!(renamed.custom_name.as_deref(), Some("书房 PC"));
    let published = alpha
        .events
        .inspect(|snapshot| snapshot.linked_devices.clone());
    assert_eq!(published[0].name, "书房 PC");

    // 对端改名：对端自报名更新，但生效名仍是本机备注名。
    bravo.state.lock().await.device_name = "bravo-2".to_owned();
    bravo.service.sync_self_identity().await;
    let manager = alpha.service.manager().unwrap();
    let mut record = manager.get_device(&bravo_fp).await.unwrap().unwrap();
    for _ in 0..250 {
        if record.name == "bravo-2" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        record = manager.get_device(&bravo_fp).await.unwrap().unwrap();
    }
    assert_eq!(record.name, "bravo-2");
    assert_eq!(record.effective_name(), "书房 PC");
    assert_eq!(
        alpha
            .events
            .inspect(|snapshot| snapshot.linked_devices[0].name.clone()),
        "书房 PC"
    );

    // RPC：空名清除备注名，恢复对端自报名。
    let cleared = alpha
        .service
        .rpc(
            fluxdown_protocol::method::AGENT_LINK_RENAME,
            Some(serde_json::json!({ "fingerprint": bravo_fp, "name": "  " })),
        )
        .await
        .unwrap();
    assert_eq!(cleared["name"], "bravo-2");
    assert!(cleared.get("customName").is_none());

    // 非法名称：超长 / 含控制字符 → InvalidArgument(field=name)；未配对 → PeerNotPaired。
    for bad in ["x".repeat(65), "a\nb".to_owned()] {
        let error = alpha
            .service
            .rename(&bravo_fp, Some(&bad))
            .await
            .unwrap_err();
        let data = rpc_error(&error, ErrorContext::Peer);
        assert_eq!(data.field.as_deref(), Some("name"));
        assert_eq!(
            data.code,
            fluxdown_protocol::ApplicationErrorCode::InvalidArgument
        );
    }
    let error = alpha
        .service
        .rename("missing", Some("x"))
        .await
        .unwrap_err();
    assert_eq!(
        rpc_error(&error, ErrorContext::Peer).reason,
        Some(ErrorReason::PeerNotPaired)
    );
}

/// 一次性冒烟：两台设备经真实 HTTP 配对 → 互相下发 → A 改名 / 登录云账号 / 改地址后经信息
/// 交换让 B 的名册跟进（只在内容变化时写盘）→ A 解除配对 → B 标记 revoked 且不能再下发 →
/// 重新配对清除 revoked。
#[tokio::test]
async fn two_devices_pair_dispatch_follow_identity_changes_and_unpair() {
    let alpha = Node::start_reachable("alpha").await;
    let bravo = Node::start_reachable("bravo").await;
    pair(&bravo, &alpha).await;
    let alpha_fp = alpha.fingerprint();
    let bravo_fp = bravo.fingerprint();
    let find = |node: &Node, fp: &str| {
        let fp = fp.to_owned();
        node.events.inspect(move |snapshot| {
            snapshot
                .linked_devices
                .iter()
                .find(|device| device.fingerprint == fp)
                .cloned()
        })
    };

    // 下发（双向）：alpha 配对时自报了回连地址，所以 bravo 也能反向下发。
    let id = alpha
        .service
        .dispatch(&bravo_fp, "https://example.com/a.bin", None, None)
        .await
        .unwrap();
    assert_eq!(id, "task-1");
    assert_eq!(bravo.attempts()[0].url, "https://example.com/a.bin");
    let id = bravo
        .service
        .dispatch(&alpha_fp, "https://example.com/b.bin", None, None)
        .await
        .unwrap();
    assert_eq!(id, "task-1");
    assert_eq!(alpha.attempts()[0].url, "https://example.com/b.bin");

    // A 改名：B 的名册跟进。
    alpha.state.lock().await.device_name = "alpha-renamed".to_owned();
    alpha.service.sync_self_identity().await;
    wait_for("bravo to learn the new name", || {
        find(&bravo, &alpha_fp).filter(|device| device.name == "alpha-renamed")
    })
    .await;

    // A 登录云账号：B 学到 A 的云设备 ID（只经已认证链路）；登出后清除。
    {
        let mut state = alpha.state.lock().await;
        state.device_id = "cloud-alpha".to_owned();
        state.credentials = Some(CloudCredentials {
            access_token: "access".to_owned(),
            ..CloudCredentials::default()
        });
    }
    alpha.service.sync_self_identity().await;
    wait_for("bravo to learn the cloud device id", || {
        find(&bravo, &alpha_fp)
            .filter(|device| device.cloud_device_id.as_deref() == Some("cloud-alpha"))
    })
    .await;
    alpha.state.lock().await.credentials = None;
    alpha.service.sync_self_identity().await;
    wait_for("bravo to drop the cloud device id", || {
        find(&bravo, &alpha_fp).filter(|device| device.cloud_device_id.is_none())
    })
    .await;

    // A 的监听端口变化：自报地址随之变化，B 把新地址放到候选首位、旧地址留作回退。
    let spare = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };
    alpha
        .service
        .update_bound(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), spare))
        .await
        .unwrap();
    let bravo_manager = bravo.service.manager().unwrap();
    let new_address = format!("127.0.0.1:{spare}");
    let mut candidates = Vec::new();
    for _ in 0..250 {
        candidates = bravo_manager
            .get_device(&alpha_fp)
            .await
            .unwrap()
            .unwrap()
            .candidates;
        if candidates.first().is_some_and(|c| c.address == new_address) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(candidates[0].address, new_address);
    assert_eq!(
        candidates[1].address,
        format!("127.0.0.1:{}", alpha.addr.port())
    );
    // 新地址没人监听：拨号先失败再回退到旧地址，下发照常成功。
    bravo
        .service
        .dispatch(&alpha_fp, "https://example.com/c.bin", None, None)
        .await
        .unwrap();

    // 空闲时双方互相交换信息：没有任何变化 → 状态文件一个字节都不写，也不推送设备列表事件。
    tokio::time::sleep(Duration::from_millis(400)).await;
    let state_file = |node: &Node| node.dir.join("agent-state.json");
    let alpha_before = tokio::fs::read(state_file(&alpha)).await.unwrap();
    let bravo_before = tokio::fs::read(state_file(&bravo)).await.unwrap();
    let sequence_before = bravo.events.snapshot().sequence;
    assert_eq!(bravo.service.exchange_info(&alpha_fp).await, Some(false));
    assert_eq!(alpha.service.exchange_info(&bravo_fp).await, Some(false));
    assert_eq!(
        tokio::fs::read(state_file(&alpha)).await.unwrap(),
        alpha_before
    );
    assert_eq!(
        tokio::fs::read(state_file(&bravo)).await.unwrap(),
        bravo_before
    );
    assert_eq!(bravo.events.snapshot().sequence, sequence_before);
    assert!(!bravo.service.info_exchange_due(&alpha_fp));

    // A 解除配对：先通知在线的 B，再本地删除。
    alpha.service.remove(&bravo_fp).await.unwrap();
    assert!(
        alpha
            .events
            .inspect(|snapshot| snapshot.linked_devices.is_empty())
    );
    let revoked = wait_for("bravo to see the revocation", || {
        find(&bravo, &alpha_fp).filter(|device| device.revoked)
    })
    .await;
    assert!(!revoked.online);
    // 持久化：重启后依然是 revoked。
    let roster = bravo.state.lock().await.linked_devices.clone();
    assert_eq!(roster[0]["revoked"], true);
    // B 不能再向 A 下发（直接 PeerNotPaired，不发请求）。
    let before = alpha.attempts().len();
    let error = bravo
        .service
        .dispatch(&alpha_fp, "https://example.com/d.bin", None, None)
        .await
        .unwrap_err();
    assert_eq!(
        rpc_error(&error, ErrorContext::Peer).reason,
        Some(ErrorReason::PeerNotPaired)
    );
    assert_eq!(alpha.attempts().len(), before);

    // 重新配对同一设备：覆盖记录并清除 revoked。先把 A 的监听端口改回真实端口——此前为了验证
    // 地址变化把它改成了一个没人监听的端口，重新配对时 A 自报的地址必须是真能连上的那个。
    alpha
        .service
        .update_bound(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            alpha.addr.port(),
        ))
        .await
        .unwrap();
    pair(&bravo, &alpha).await;
    let again = bravo_manager.get_device(&alpha_fp).await.unwrap().unwrap();
    assert!(!again.revoked);
    wait_for("bravo roster to show the repaired device", || {
        find(&bravo, &alpha_fp).filter(|device| !device.revoked)
    })
    .await;
    bravo
        .service
        .dispatch(&alpha_fp, "https://example.com/e.bin", None, None)
        .await
        .unwrap();
}

// ── 设备控制：请求 → 批准 → 凭据 ────────────────────────────────────────────

/// 同 [`pair`]，但发起方请求「完全控制」；返回响应端弹窗里看到的请求与发起方得到的设备视图。
async fn pair_requesting_control(
    responder: &Node,
    initiator: &Node,
) -> (
    fluxdown_protocol::LinkPairingRequestDto,
    fluxdown_protocol::LinkDeviceInfo,
) {
    let code = responder.service.pairing_code().await.unwrap();
    let begin = initiator
        .service
        .pair_begin(&responder.address(), &code.code, true)
        .await
        .unwrap();
    let request = wait_for("incoming pairing request", || {
        responder
            .events
            .inspect(|snapshot| snapshot.link_pairing_requests.first().cloned())
    })
    .await;
    let finisher = {
        let service = initiator.service.clone();
        let token = begin.token.clone();
        tokio::spawn(async move { service.pair_finish(&token, true).await })
    };
    responder
        .service
        .approve(&request.session_id, true)
        .unwrap();
    let device = finisher.await.unwrap().unwrap().expect("paired");
    let initiator_fp = initiator.fingerprint();
    wait_for("responder roster entry", || {
        responder
            .events
            .inspect(|snapshot| {
                snapshot
                    .linked_devices
                    .iter()
                    .any(|device| device.fingerprint == initiator_fp)
            })
            .then_some(())
    })
    .await;
    (request, device)
}

#[tokio::test]
async fn control_request_is_announced_granted_and_turned_into_a_credential() {
    let host = Node::start("nas").await;
    let phone = Node::start("phone").await;
    let roster_changes = host.service.subscribe_roster();

    let (request, device) = pair_requesting_control(&host, &phone).await;
    // 响应端弹窗要能警示「请求完全控制」；相对 TTL 随请求一起给出。
    assert!(request.requests_control);
    assert!(request.ttl_ms > 0 && request.ttl_ms <= 60_000);
    // 发起方得到「本机可控制」的设备；响应端的名册里该设备被授权控制本机。
    assert!(device.controllable && !device.grants_control);
    let host_view = host
        .events
        .inspect(|snapshot| snapshot.linked_devices.clone());
    assert_eq!(host_view.len(), 1);
    assert!(host_view[0].grants_control && !host_view[0].controllable);
    assert!(
        roster_changes.has_changed().unwrap(),
        "pairing must bump the roster epoch"
    );

    // 发起方签发凭据，主机校验通过并指回发起方；密钥相同的伪造 / 篡改凭据一律拒绝。
    let host_fp = host.fingerprint();
    let phone_fp = phone.fingerprint();
    let token = phone
        .service
        .control_credential(&host_fp)
        .await
        .unwrap()
        .token;
    assert!(token.starts_with(&format!("fdl1.{phone_fp}.")));
    assert_eq!(
        host.service.verify_control_credential(&token).await,
        Some(phone_fp.clone())
    );
    let mut tampered = token.clone();
    tampered.pop();
    tampered.push('x');
    assert_eq!(
        host.service.verify_control_credential(&tampered).await,
        None
    );
    assert_eq!(
        host.service
            .verify_control_credential("not-a-credential")
            .await,
        None
    );

    // 同一指纹重新配对且这次不请求控制：授权消失，旧凭据（link_secret 也变了）立即失效。
    pair(&host, &phone).await;
    assert_eq!(host.service.verify_control_credential(&token).await, None);
    assert!(matches!(
        phone.service.control_credential(&host_fp).await,
        Err(LinkOpError::Link(LinkError::NotPaired))
    ));
    let data = rpc_error(
        &phone
            .service
            .control_credential(&host_fp)
            .await
            .unwrap_err(),
        ErrorContext::Peer,
    );
    assert_eq!(data.reason, Some(ErrorReason::PeerNotPaired));
}

#[tokio::test]
async fn plain_pairing_grants_nothing_and_yields_no_usable_credential() {
    let host = Node::start("nas").await;
    let phone = Node::start("phone").await;
    let code = host.service.pairing_code().await.unwrap();
    let begin = phone
        .service
        .pair_begin(&host.address(), &code.code, false)
        .await
        .unwrap();
    let request = wait_for("incoming pairing request", || {
        host.events
            .inspect(|snapshot| snapshot.link_pairing_requests.first().cloned())
    })
    .await;
    assert!(!request.requests_control);
    let finisher = {
        let service = phone.service.clone();
        let token = begin.token.clone();
        tokio::spawn(async move { service.pair_finish(&token, true).await })
    };
    host.service.approve(&request.session_id, true).unwrap();
    let device = finisher.await.unwrap().unwrap().expect("paired");
    assert!(!device.controllable && !device.grants_control);

    // 发起方没有可签发的凭据；就算它用共享的链路密钥手工拼一个，主机也不认（未授权控制）。
    let host_fp = host.fingerprint();
    let phone_fp = phone.fingerprint();
    assert!(matches!(
        phone.service.control_credential(&host_fp).await,
        Err(LinkOpError::Link(LinkError::NotPaired))
    ));
    let secret = phone
        .service
        .manager()
        .unwrap()
        .get_device(&host_fp)
        .await
        .unwrap()
        .unwrap()
        .link_secret;
    let hand_made =
        fluxdown_link::crypto::control_credential(&secret, &phone_fp, &host_fp).unwrap();
    wait_for("responder roster entry", || {
        host.events
            .inspect(|snapshot| (!snapshot.linked_devices.is_empty()).then_some(()))
    })
    .await;
    assert_eq!(
        host.service.verify_control_credential(&hand_made).await,
        None
    );
}

#[tokio::test]
async fn requesting_control_from_a_peer_without_the_capability_is_a_version_mismatch() {
    let phone = Node::start("phone").await;
    // 旧版对端：`/ping` 有 `linkProtocol` 却没有 `linkControl`。
    let legacy = http_json(
        "200 OK",
        &serde_json::json!({
            "success": true,
            "app": "FluxDown",
            "linkFingerprint": "ab".repeat(32),
            "linkProtocol": 2,
        })
        .to_string(),
    );
    let (legacy_addr, seen) = raw_server(legacy).await;
    let error = phone
        .service
        .pair_begin(&legacy_addr.to_string(), "123456", true)
        .await
        .unwrap_err();
    assert!(
        matches!(error, LinkOpError::Link(LinkError::UnsupportedVersion)),
        "{error:?}"
    );
    assert_eq!(
        rpc_error(&error, ErrorContext::Pairing).reason,
        Some(ErrorReason::PairingVersionMismatch)
    );
    // 只探测了 `/ping`，没有发出 hello。
    let requests = seen.lock().unwrap().clone();
    assert!(
        requests.iter().all(|request| request.contains("/ping")),
        "{requests:?}"
    );
}

// ── 相对 TTL（ttlMs）────────────────────────────────────────────────────────

fn unix_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

#[tokio::test]
async fn pairing_code_carries_the_remaining_lifetime_in_milliseconds() {
    let node = Node::start("nas").await;
    let code = node.service.pairing_code().await.unwrap();
    assert_eq!(code.ttl_ms, fluxdown_link::pairing::CODE_TTL_SECS * 1000);
    // 绝对时刻仍然保留（旧客户端依赖它）。
    assert!(code.expires_at_unix_ms > unix_ms());
}

#[tokio::test]
async fn pairing_request_ttl_is_recomputed_for_every_published_snapshot() {
    let node = Node::start("nas").await;
    let request = |session: &str, expires_in_ms: i64| fluxdown_protocol::LinkPairingRequestDto {
        session_id: session.to_owned(),
        peer_name: "phone".to_owned(),
        peer_fingerprint: "ab".repeat(32),
        peer_platform: None,
        sas: "123456".to_owned(),
        expires_at_unix_ms: unix_ms() + expires_in_ms,
        // 存着的值是建请求时的旧值：快照必须按取快照时刻重算。
        ttl_ms: 999_999,
        requests_control: false,
    };
    node.events
        .publish(fluxdown_protocol::AgentEvent::LinkPairingRequestsChanged(
            vec![request("live", 5_000), request("expired", -1_000)],
        ));
    for snapshot in [node.events.snapshot(), node.events.wire_snapshot()] {
        let fluxdown_protocol::SnapshotBody::Agent(body) = snapshot.body else {
            panic!("agent snapshot expected");
        };
        let ttl = |session: &str| {
            body.link_pairing_requests
                .iter()
                .find(|request| request.session_id == session)
                .unwrap()
                .ttl_ms
        };
        assert!(ttl("live") > 0 && ttl("live") <= 5_000, "{}", ttl("live"));
        assert_eq!(ttl("expired"), 0);
    }
}

#[tokio::test]
async fn stop_pairing_with_a_code_only_revokes_that_code() {
    let host = Node::start("nas").await;
    let phone = Node::start("phone").await;
    let stale = host.service.pairing_code().await.unwrap();
    // 另一个客户端刷新出了新码；第一个客户端关闭自己的面板时带着旧码：新码不受影响。
    let current = host.service.pairing_code().await.unwrap();
    assert_ne!(stale.code, current.code, "极小概率随机撞码，重跑即可");
    host.service.stop_pairing(Some(&stale.code)).unwrap();
    let begin = phone
        .service
        .pair_begin(&host.address(), &current.code, false)
        .await
        .expect("the current code must survive a stale stopPairing");
    phone
        .service
        .pair_finish(&begin.token, false)
        .await
        .unwrap();

    // 带当前码：作废；不带码：无条件作废（旧行为）。
    for stop in [
        serde_json::json!({ "code": "PLACEHOLDER" }),
        serde_json::json!({}),
    ] {
        let fresh = host.service.pairing_code().await.unwrap();
        let mut params = stop;
        if params.get("code").is_some() {
            params["code"] = serde_json::json!(fresh.code);
        }
        host.service
            .rpc(
                fluxdown_protocol::method::AGENT_LINK_STOP_PAIRING,
                Some(params),
            )
            .await
            .unwrap();
        let error = phone
            .service
            .pair_begin(&host.address(), &fresh.code, false)
            .await
            .unwrap_err();
        assert!(
            matches!(error, LinkOpError::Link(LinkError::InvalidCode)),
            "{error:?}"
        );
    }
    // 没有参数对象同样可用（旧客户端）。
    let fresh = host.service.pairing_code().await.unwrap();
    host.service
        .rpc(fluxdown_protocol::method::AGENT_LINK_STOP_PAIRING, None)
        .await
        .unwrap();
    assert!(
        phone
            .service
            .pair_begin(&host.address(), &fresh.code, false)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_advertise_override_makes_a_loopback_bound_node_reachable() {
    use fluxdown_link::LinkStorage;

    // 只监听回环（反代 / 端口映射 / 容器内部端口），但配置了对外自报地址。
    let plain = Node::start("plain").await;
    let code = plain.service.pairing_code().await.unwrap();
    assert!(
        code.addresses.is_empty(),
        "without an override a loopback-only node has nothing to advertise"
    );

    let behind_proxy = Node::start_custom("proxied", false, vec!["10.9.8.7".to_owned()]).await;
    let code = behind_proxy.service.pairing_code().await.unwrap();
    assert_eq!(
        code.addresses,
        vec![format!("http://10.9.8.7:{}", behind_proxy.addr.port())],
        "the override is what peers are told to dial"
    );

    // 配对时向对端自报的回连地址也用覆盖值：对端存下的候选就是它。
    let responder = Node::start("nas").await;
    let responder_code = responder.service.pairing_code().await.unwrap();
    let begin = behind_proxy
        .service
        .pair_begin(&responder.address(), &responder_code.code, false)
        .await
        .unwrap();
    let request = wait_for("incoming pairing request", || {
        responder
            .events
            .inspect(|snapshot| snapshot.link_pairing_requests.first().cloned())
    })
    .await;
    let finisher = {
        let service = behind_proxy.service.clone();
        let token = begin.token.clone();
        tokio::spawn(async move { service.pair_finish(&token, true).await })
    };
    responder
        .service
        .approve(&request.session_id, true)
        .unwrap();
    finisher.await.unwrap().unwrap().expect("paired");
    let storage = super::AgentLinkStorage::new(responder.state.clone(), responder.store.clone());
    let stored = wait_for("responder roster entry", || {
        futures_util::FutureExt::now_or_never(storage.get(&behind_proxy.fingerprint()))
            .and_then(Result::ok)
            .flatten()
    })
    .await;
    assert_eq!(
        stored
            .candidates
            .iter()
            .map(|c| c.address.as_str())
            .collect::<Vec<_>>(),
        vec![format!("10.9.8.7:{}", behind_proxy.addr.port())]
    );
}
