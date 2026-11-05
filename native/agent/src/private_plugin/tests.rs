//! 私有插件商店服务：本地 mock FluxCloud（axum）+ 假 daemon。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use fluxdown_protocol::{
    ApplicationErrorCode, ErrorReason, InstalledPlugin, MarketEntryDto,
    PrivatePluginInstallRequest, PrivateRevocationDto, RpcErrorData,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{PrivatePluginHost, PrivatePluginService};
use crate::cloud::{CloudApi, CloudClient};
use crate::egress::Egress;
use crate::state::{AgentState, CloudCredentials, StateStore};

const PLUGIN_ID: &str = "acme@client-x";
const PACKAGE: &[u8] = b"fxplug package bytes";
const TICKET_URL: &str = "/api/v1/private-plugins/packages/tk-secret-ticket";

fn sha(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn entry_json(version: &str, sequence: u64, content_hash: &str, yanked: &str) -> Value {
    json!({
        "pluginId": PLUGIN_ID,
        "version": version,
        "sequence": sequence,
        "contentHash": content_hash,
        "minAppVersion": "0.6.0",
        "name": "Client X",
        "description": "d",
        "author": "a",
        "homepage": "",
        "publishTime": "2026-11-04T00:00:00.000Z",
        "yanked": yanked,
        "tags": ["private"],
        "permissions": ["ffmpeg"],
        "networkHosts": [],
        "channel": "private",
        "expiresAt": "2027-01-01T00:00:00Z",
        // 服务端若误带镜像，agent 也必须丢弃。
        "mirrors": ["https://cdn.example.com/leak.fxplug"],
    })
}

enum PackageBody {
    Fixed(Vec<u8>),
    /// 无 `content-length` 的流式分块：`chunks` 个 `chunk_len` 字节。
    Streamed {
        chunks: usize,
        chunk_len: usize,
    },
}

struct Mock {
    catalog: Mutex<Value>,
    /// 目录路由的 HTTP 状态（默认 200；404 模拟旧版 FluxCloud 无此路由）。
    catalog_status: Mutex<u16>,
    ticket: Mutex<Value>,
    package: Mutex<PackageBody>,
    list_hits: AtomicUsize,
    ticket_hits: AtomicUsize,
    package_hits: AtomicUsize,
    package_had_authorization: Mutex<Option<bool>>,
}

impl Mock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            catalog: Mutex::new(json!({ "entries": [], "revoked": [], "revision": 1 })),
            catalog_status: Mutex::new(200),
            ticket: Mutex::new(ticket_json(&sha(PACKAGE), PACKAGE.len() as u64)),
            package: Mutex::new(PackageBody::Fixed(PACKAGE.to_vec())),
            list_hits: AtomicUsize::new(0),
            ticket_hits: AtomicUsize::new(0),
            package_hits: AtomicUsize::new(0),
            package_had_authorization: Mutex::new(None),
        })
    }

    fn total_hits(&self) -> usize {
        self.list_hits.load(Ordering::SeqCst)
            + self.ticket_hits.load(Ordering::SeqCst)
            + self.package_hits.load(Ordering::SeqCst)
    }
}

fn ticket_json(content_hash: &str, bytes: u64) -> Value {
    json!({
        "ticket": "tk-secret-ticket",
        "url": TICKET_URL,
        "expiresAt": "2026-11-04T00:02:00Z",
        "contentHash": content_hash,
        "bytes": bytes,
    })
}

async fn list_route(State(mock): State<Arc<Mock>>) -> Response {
    mock.list_hits.fetch_add(1, Ordering::SeqCst);
    let status = *mock.catalog_status.lock().unwrap();
    let body = Json(mock.catalog.lock().unwrap().clone());
    (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), body).into_response()
}

async fn ticket_route(State(mock): State<Arc<Mock>>) -> Json<Value> {
    mock.ticket_hits.fetch_add(1, Ordering::SeqCst);
    Json(mock.ticket.lock().unwrap().clone())
}

async fn package_route(State(mock): State<Arc<Mock>>, headers: HeaderMap) -> Response {
    mock.package_hits.fetch_add(1, Ordering::SeqCst);
    *mock.package_had_authorization.lock().unwrap() =
        Some(headers.contains_key(header::AUTHORIZATION));
    match &*mock.package.lock().unwrap() {
        PackageBody::Fixed(bytes) => (StatusCode::OK, bytes.clone()).into_response(),
        PackageBody::Streamed { chunks, chunk_len } => {
            let chunk = Bytes::from(vec![7_u8; *chunk_len]);
            let stream = futures_util::stream::iter(
                (0..*chunks).map(move |_| Ok::<Bytes, std::io::Error>(chunk.clone())),
            );
            (StatusCode::OK, Body::from_stream(stream)).into_response()
        }
    }
}

#[derive(Default)]
struct FakeHost {
    installed: Mutex<Vec<(Vec<u8>, MarketEntryDto)>>,
    revoked: Mutex<Vec<Vec<PrivateRevocationDto>>>,
    scopes: Mutex<Vec<Option<Vec<String>>>>,
    fail_revoke: bool,
}

#[async_trait::async_trait]
impl PrivatePluginHost for FakeHost {
    async fn install_package(
        &self,
        bytes: Vec<u8>,
        entry: MarketEntryDto,
    ) -> Result<InstalledPlugin, RpcErrorData> {
        let identity = entry.plugin_id.clone();
        self.installed.lock().unwrap().push((bytes, entry));
        Ok(InstalledPlugin {
            identity,
            missing_components: Vec::new(),
        })
    }

    async fn revoke(
        &self,
        revoked: Vec<PrivateRevocationDto>,
        scope: Option<Vec<String>>,
    ) -> Result<(), RpcErrorData> {
        self.revoked.lock().unwrap().push(revoked);
        self.scopes.lock().unwrap().push(scope);
        if self.fail_revoke {
            return Err(RpcErrorData::new(ApplicationErrorCode::Unavailable, true));
        }
        Ok(())
    }
}

struct Harness {
    service: PrivatePluginService,
    mock: Arc<Mock>,
    host: Arc<FakeHost>,
    dir: PathBuf,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
        if let Err(error) = std::fs::remove_dir_all(&self.dir) {
            eprintln!("private plugin test dir removal failed: {error}");
        }
    }
}

impl Harness {
    async fn start(logged_in: bool, fail_revoke: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock cloud");
        let address = listener.local_addr().expect("mock cloud address");
        let mock = Mock::new();
        let router = Router::new()
            .route("/api/v1/private-plugins", get(list_route))
            .route(
                "/api/v1/private-plugins/{plugin_id}/download",
                post(ticket_route),
            )
            .route(
                "/api/v1/private-plugins/packages/{ticket}",
                get(package_route),
            )
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                eprintln!("mock cloud stopped: {error}");
            }
        });

        let dir = std::env::temp_dir().join(format!(
            "fluxdown_private_plugin_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let initial = AgentState {
            device_id: "device-1".to_owned(),
            credentials: logged_in.then(|| CloudCredentials {
                access_token: "access".to_owned(),
                refresh_token: "refresh".to_owned(),
                expires_at_unix: i64::MAX,
                session: None,
            }),
            ..AgentState::default()
        };
        store.save(&initial).await.expect("save state");
        let state = Arc::new(tokio::sync::Mutex::new(initial));
        let cloud = Arc::new(CloudApi::new(
            CloudClient::new(format!("http://{address}"), state, store).expect("cloud client"),
        ));
        let host = Arc::new(FakeHost {
            fail_revoke,
            ..FakeHost::default()
        });
        let service =
            PrivatePluginService::with_parts(cloud, host.clone(), Egress::system_default());
        Self {
            service,
            mock,
            host,
            dir,
            server,
        }
    }

    /// 旧版云端形状：无 `scope` 字段。
    fn set_catalog(&self, entries: Vec<Value>, revoked: Vec<Value>) {
        *self.mock.catalog.lock().unwrap() =
            json!({ "entries": entries, "revoked": revoked, "revision": 2 });
    }

    /// 软撤销版云端形状：带 `scope`（该账号曾有授权行的全部 pluginId）。
    fn set_scoped_catalog(&self, revoked: Vec<Value>, scope: &[&str]) {
        *self.mock.catalog.lock().unwrap() = json!({
            "entries": [entry_json("1.0.0", 1, &sha(b"v1"), "none")],
            "revoked": revoked,
            "scope": scope,
            "revision": 3,
        });
    }

    /// 当前已提交给 daemon 的 `(撤回版本数, scope)` 调用序列。
    fn revoke_scopes(&self) -> Vec<Option<Vec<String>>> {
        self.host.scopes.lock().unwrap().clone()
    }

    fn request(version: &str) -> PrivatePluginInstallRequest {
        PrivatePluginInstallRequest {
            plugin_id: PLUGIN_ID.to_owned(),
            version: version.to_owned(),
        }
    }

    fn installed(&self) -> usize {
        self.host.installed.lock().unwrap().len()
    }

    /// 后台提交的撤回至少有 `n` 次后返回（最多等 3 秒）。
    async fn revoke_calls(&self, n: usize) -> Vec<Vec<PrivateRevocationDto>> {
        for _ in 0..300 {
            {
                let calls = self.host.revoked.lock().unwrap();
                if calls.len() >= n {
                    return calls.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("expected {n} revocation call(s)");
    }
}

#[tokio::test]
async fn list_treats_a_404_catalog_as_an_empty_store_but_install_maps_it_to_not_in_store() {
    let h = Harness::start(true, false).await;
    *h.mock.catalog_status.lock().unwrap() = 404;
    let listed = h.service.list().await.expect("old cloud without the route");
    assert!(listed.signed_in);
    assert!(listed.entries.is_empty());
    let error = h
        .service
        .install(Harness::request("1.0.0"))
        .await
        .expect_err("install needs the catalog");
    assert_eq!(error.reason, Some(ErrorReason::PluginNotInMarket));
}

#[tokio::test]
async fn sync_revocations_is_silent_when_signed_out_and_applies_synchronously_when_signed_in() {
    let out = Harness::start(false, false).await;
    out.service
        .sync_revocations()
        .await
        .expect("signed out is a no-op");
    assert_eq!(out.mock.total_hits(), 0);

    let h = Harness::start(true, false).await;
    h.set_catalog(
        vec![entry_json("1.0.0", 1, &sha(b"v1"), "none")],
        vec![json!({ "pluginId": PLUGIN_ID, "version": "0.9.0", "yanked": "malicious" })],
    );
    h.service.sync_revocations().await.expect("sync");
    // 同步等待：返回时撤回已提交，不需要轮询。
    assert_eq!(h.host.revoked.lock().unwrap().len(), 1);

    // 旧版 FluxCloud 没有该路由：静默成功。
    *h.mock.catalog_status.lock().unwrap() = 404;
    h.service.sync_revocations().await.expect("404 is a no-op");
}

/// 软撤销版云端：`scope` 随撤回清单一并交给 daemon；清单为空但 scope 非空也要提交（解封信号）。
#[tokio::test]
async fn sync_forwards_scope_even_when_the_revoked_set_is_empty() {
    let h = Harness::start(true, false).await;
    h.set_scoped_catalog(Vec::new(), &[PLUGIN_ID, "other@private"]);
    h.service.sync_revocations().await.expect("sync");
    assert_eq!(
        h.revoke_scopes(),
        vec![Some(vec![PLUGIN_ID.to_owned(), "other@private".to_owned()])]
    );
    assert!(h.host.revoked.lock().unwrap()[0].is_empty());

    // 带撤回项时两者同时下发；非 malicious 的 yank 不进撤回集合。
    h.set_scoped_catalog(
        vec![
            json!({ "pluginId": PLUGIN_ID, "version": "0.9.0", "yanked": "malicious" }),
            json!({ "pluginId": PLUGIN_ID, "version": "0.8.0", "yanked": "deprecated" }),
        ],
        &[PLUGIN_ID],
    );
    h.service.sync_revocations().await.expect("sync");
    let calls = h.host.revoked.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].len(), 1);
    assert_eq!(calls[1][0].version, "0.9.0");
    assert_eq!(h.revoke_scopes()[1], Some(vec![PLUGIN_ID.to_owned()]));
}

/// 旧版云端不返回 `scope`：必须以 `None` 下发（daemon 据此不解封）；`scope: null` 同样视为缺失，
/// 与 `scope: []`（权威的「无授权」）区分。
#[tokio::test]
async fn missing_or_null_scope_is_forwarded_as_none_and_an_empty_scope_is_not() {
    let h = Harness::start(true, false).await;
    let revoked = vec![json!({ "pluginId": PLUGIN_ID, "version": "1.0.0", "yanked": "malicious" })];
    h.set_catalog(Vec::new(), revoked.clone());
    h.service.sync_revocations().await.expect("legacy cloud");

    *h.mock.catalog.lock().unwrap() = json!({
        "entries": [], "revoked": revoked, "scope": null, "revision": 3,
    });
    h.service.sync_revocations().await.expect("null scope");

    *h.mock.catalog.lock().unwrap() = json!({
        "entries": [], "revoked": revoked, "scope": [], "revision": 4,
    });
    h.service.sync_revocations().await.expect("empty scope");
    assert_eq!(h.revoke_scopes(), vec![None, None, Some(Vec::new())]);

    // 空 scope 且无撤回项：无事可做，不打扰 daemon（空闲静默）。
    let calls_before = h.host.revoked.lock().unwrap().len();
    h.set_scoped_catalog(Vec::new(), &[]);
    h.service.sync_revocations().await.expect("nothing to do");
    assert_eq!(h.host.revoked.lock().unwrap().len(), calls_before);
}

/// `list` 的后台提交同样携带 scope，且 DTO 不暴露 scope。
#[tokio::test]
async fn list_submits_scope_in_the_background() {
    let h = Harness::start(true, false).await;
    h.set_scoped_catalog(Vec::new(), &[PLUGIN_ID]);
    let listed = h.service.list().await.expect("list");
    assert_eq!(listed.entries.len(), 1);
    h.revoke_calls(1).await;
    assert_eq!(h.revoke_scopes(), vec![Some(vec![PLUGIN_ID.to_owned()])]);
}

#[tokio::test]
async fn ticket_urls_must_end_in_a_plain_ticket_segment() {
    let h = Harness::start(true, false).await;
    h.set_catalog(
        vec![entry_json("1.0.0", 1, &sha(PACKAGE), "none")],
        Vec::new(),
    );
    for bad in [
        "/api/v1/private-plugins/packages/",
        "/api/v1/private-plugins/packages/short",
        "/api/v1/private-plugins/packages/tk-secret-ticket/extra",
        "/api/v1/private-plugins/packages/tk-secret-ticket?x=1",
        "/api/v1/private-plugins/packages/tk-secret-ticket#f",
        "/api/v1/private-plugins/packages/tk.secret.ticket1",
        "/api/v1/private-plugins/packages/../packages/tk-secret-ticket",
    ] {
        let mut ticket = ticket_json(&sha(PACKAGE), PACKAGE.len() as u64);
        ticket["url"] = json!(bad);
        *h.mock.ticket.lock().unwrap() = ticket;
        let error = h
            .service
            .install(Harness::request("1.0.0"))
            .await
            .expect_err(bad);
        assert_eq!(
            error.reason,
            Some(ErrorReason::PluginDownloadFailed),
            "{bad}"
        );
        assert!(!error.retryable, "{bad}");
    }
    assert_eq!(h.mock.package_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn signed_out_list_sends_no_request_and_install_is_unauthorized() {
    let h = Harness::start(false, false).await;
    let listed = h.service.list().await.expect("list");
    assert!(!listed.signed_in);
    assert!(listed.entries.is_empty());
    let error = h
        .service
        .install(Harness::request("1.0.0"))
        .await
        .expect_err("install needs a session");
    assert_eq!(error.code, ApplicationErrorCode::Unauthorized);
    assert_eq!(
        h.mock.total_hits(),
        0,
        "signed-out client must not touch the cloud"
    );
    assert_eq!(h.installed(), 0);
}

#[tokio::test]
async fn list_forces_private_channel_drops_mirrors_and_forwards_only_malicious_revocations() {
    let h = Harness::start(true, false).await;
    h.set_catalog(
        vec![
            entry_json("1.0.0", 1, &sha(b"v1"), "none"),
            entry_json("0.9.0", 0, &sha(b"v0"), "malicious"),
        ],
        vec![
            json!({ "pluginId": PLUGIN_ID, "version": "0.9.0", "yanked": "malicious" }),
            json!({ "pluginId": PLUGIN_ID, "version": "0.8.0", "yanked": "vulnerable" }),
        ],
    );
    let listed = h.service.list().await.expect("list");
    assert!(listed.signed_in);
    assert_eq!(listed.entries.len(), 2);
    for entry in &listed.entries {
        assert_eq!(entry.channel.as_deref(), Some("private"));
        assert!(entry.mirrors.is_empty());
        assert_eq!(entry.expires_at.as_deref(), Some("2027-01-01T00:00:00Z"));
    }
    let calls = h.revoke_calls(1).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].len(), 1);
    assert_eq!(calls[0][0].plugin_id, PLUGIN_ID);
    assert_eq!(calls[0][0].version, "0.9.0");
}

#[tokio::test]
async fn revocation_failure_does_not_fail_the_listing_and_empty_revocations_skip_the_daemon() {
    let failing = Harness::start(true, true).await;
    failing.set_catalog(
        vec![entry_json("1.0.0", 1, &sha(b"v1"), "none")],
        vec![json!({ "pluginId": PLUGIN_ID, "version": "1.0.0", "yanked": "malicious" })],
    );
    let listed = failing
        .service
        .list()
        .await
        .expect("list survives a revoke failure");
    assert_eq!(listed.entries.len(), 1);
    assert_eq!(failing.revoke_calls(1).await.len(), 1);

    let quiet = Harness::start(true, false).await;
    quiet.set_catalog(
        vec![entry_json("1.0.0", 1, &sha(b"v1"), "none")],
        Vec::new(),
    );
    quiet.service.list().await.expect("list");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(quiet.host.revoked.lock().unwrap().is_empty());
}

/// 与 FluxCloud `private_plugins/dto.rs` 的序列化形状逐字段一致（无 `mirrors`、`expiresAt` 可为 null、
/// 带 `revision`、`revoked` 含 deprecated 与 malicious、`bytes` 为整数）：真实 list 不能因缺字段整体失败。
#[tokio::test]
async fn list_accepts_the_exact_cloud_wire_shape() {
    let h = Harness::start(true, false).await;
    let cloud_entry = |version: &str, sequence: u64, yanked: &str, expires: Value| {
        json!({
            "pluginId": PLUGIN_ID,
            "version": version,
            "sequence": sequence,
            "contentHash": sha(PACKAGE),
            "minAppVersion": "0.6.0",
            "name": "Client X",
            "description": "",
            "author": "",
            "homepage": "",
            "publishTime": "2026-11-04T00:00:00.000Z",
            "yanked": yanked,
            "tags": [],
            "permissions": [],
            "networkHosts": [],
            "channel": "private",
            "expiresAt": expires,
        })
    };
    *h.mock.catalog.lock().unwrap() = json!({
        "entries": [
            cloud_entry("1.1.0", 2, "none", Value::Null),
            cloud_entry("1.0.0", 1, "deprecated", json!("2027-01-01T00:00:00Z")),
        ],
        "revoked": [
            { "pluginId": PLUGIN_ID, "version": "0.9.0", "yanked": "malicious" },
            { "pluginId": PLUGIN_ID, "version": "0.8.0", "yanked": "deprecated" },
        ],
        "scope": [PLUGIN_ID],
        "revision": 9_007_199_254_740_991_u64,
    });
    let listed = h
        .service
        .list()
        .await
        .expect("exact cloud shape must parse");
    assert_eq!(listed.entries.len(), 2);
    assert_eq!(listed.entries[0].expires_at, None);
    assert_eq!(
        listed.entries[1].expires_at.as_deref(),
        Some("2027-01-01T00:00:00Z")
    );
    assert_eq!(listed.entries[1].yanked, "deprecated");
    let calls = h.revoke_calls(1).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].len(), 1);
    assert_eq!(calls[0][0].version, "0.9.0");
    assert_eq!(h.revoke_scopes(), vec![Some(vec![PLUGIN_ID.to_owned()])]);
}

#[tokio::test]
async fn install_downloads_with_ticket_pins_hash_and_hands_the_entry_to_the_daemon() {
    let h = Harness::start(true, false).await;
    h.set_catalog(
        vec![entry_json("1.0.0", 1, &sha(PACKAGE), "none")],
        Vec::new(),
    );
    let installed = h
        .service
        .install(Harness::request("1.0.0"))
        .await
        .expect("install");
    assert_eq!(installed.identity, PLUGIN_ID);
    let calls = h.host.installed.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, PACKAGE);
    assert_eq!(calls[0].1.channel.as_deref(), Some("private"));
    assert!(calls[0].1.mirrors.is_empty());
    assert_eq!(calls[0].1.content_hash, sha(PACKAGE));
    // 包请求不带账号凭据（票据即授权）。
    assert_eq!(
        *h.mock.package_had_authorization.lock().unwrap(),
        Some(false)
    );
}

#[tokio::test]
async fn install_rejects_hash_mismatch_without_touching_the_daemon() {
    let h = Harness::start(true, false).await;
    // 目录与票据都声明 H(PACKAGE)，但服务端实际送来别的字节。
    h.set_catalog(
        vec![entry_json("1.0.0", 1, &sha(PACKAGE), "none")],
        Vec::new(),
    );
    *h.mock.package.lock().unwrap() = PackageBody::Fixed(vec![9_u8; PACKAGE.len()]);
    let error = h
        .service
        .install(Harness::request("1.0.0"))
        .await
        .expect_err("hash mismatch");
    assert_eq!(error.reason, Some(ErrorReason::PluginDownloadFailed));
    assert!(
        !error.retryable,
        "a tampered package will not heal on retry"
    );
    assert_eq!(h.installed(), 0);
    assert_eq!(h.mock.package_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn install_rejects_a_ticket_whose_hash_differs_from_the_catalog_entry() {
    let h = Harness::start(true, false).await;
    h.set_catalog(
        vec![entry_json("1.0.0", 1, &sha(PACKAGE), "none")],
        Vec::new(),
    );
    *h.mock.ticket.lock().unwrap() = ticket_json(&sha(b"other"), PACKAGE.len() as u64);
    let error = h
        .service
        .install(Harness::request("1.0.0"))
        .await
        .expect_err("ticket hash differs");
    assert_eq!(error.reason, Some(ErrorReason::PluginDownloadFailed));
    assert!(!error.retryable, "hash disagreement is deterministic");
    assert_eq!(h.mock.package_hits.load(Ordering::SeqCst), 0);
    assert_eq!(h.installed(), 0);
}

#[tokio::test]
async fn install_aborts_when_the_body_exceeds_the_declared_size() {
    let h = Harness::start(true, false).await;
    h.set_catalog(
        vec![entry_json("1.0.0", 1, &sha(PACKAGE), "none")],
        Vec::new(),
    );

    // 带 content-length 的超长响应：读 body 之前就拒绝。
    *h.mock.package.lock().unwrap() = PackageBody::Fixed(vec![1_u8; 4096]);
    let error = h
        .service
        .install(Harness::request("1.0.0"))
        .await
        .expect_err("oversized fixed body");
    assert_eq!(error.reason, Some(ErrorReason::PluginPackageTooLarge));

    // 无 content-length 的流式响应：累计超过声明字节数立即中止（远小于流的全长）。
    *h.mock.package.lock().unwrap() = PackageBody::Streamed {
        chunks: 512,
        chunk_len: 1024,
    };
    let error = h
        .service
        .install(Harness::request("1.0.0"))
        .await
        .expect_err("oversized streamed body");
    assert_eq!(error.reason, Some(ErrorReason::PluginPackageTooLarge));
    assert_eq!(h.installed(), 0);
}

#[tokio::test]
async fn install_rejects_tickets_over_the_package_cap_or_pointing_elsewhere() {
    let h = Harness::start(true, false).await;
    h.set_catalog(
        vec![entry_json("1.0.0", 1, &sha(PACKAGE), "none")],
        Vec::new(),
    );

    *h.mock.ticket.lock().unwrap() = ticket_json(&sha(PACKAGE), 10 * 1024 * 1024 + 1);
    let error = h
        .service
        .install(Harness::request("1.0.0"))
        .await
        .expect_err("over the 10 MiB cap");
    assert_eq!(error.reason, Some(ErrorReason::PluginPackageTooLarge));

    let mut foreign = ticket_json(&sha(PACKAGE), PACKAGE.len() as u64);
    foreign["url"] = json!("https://evil.example/api/v1/private-plugins/packages/tk");
    *h.mock.ticket.lock().unwrap() = foreign;
    let error = h
        .service
        .install(Harness::request("1.0.0"))
        .await
        .expect_err("foreign ticket url");
    assert_eq!(error.reason, Some(ErrorReason::PluginDownloadFailed));
    assert!(!error.retryable, "shape errors are deterministic");

    assert_eq!(
        h.mock.package_hits.load(Ordering::SeqCst),
        0,
        "no package GET was made"
    );
    assert_eq!(h.installed(), 0);
}

#[tokio::test]
async fn install_pins_the_confirmed_version_and_requires_the_plugin_to_be_listed() {
    let h = Harness::start(true, false).await;
    h.set_catalog(
        vec![
            entry_json("1.1.0", 2, &sha(PACKAGE), "none"),
            entry_json("1.0.0", 1, &sha(PACKAGE), "none"),
        ],
        Vec::new(),
    );
    let error = h
        .service
        .install(Harness::request("1.0.0"))
        .await
        .expect_err("confirmed version is no longer the latest");
    assert_eq!(error.code, ApplicationErrorCode::Conflict);
    assert_eq!(error.reason, Some(ErrorReason::MarketVersionChanged));
    assert_eq!(h.mock.ticket_hits.load(Ordering::SeqCst), 0);

    h.set_catalog(Vec::new(), Vec::new());
    let error = h
        .service
        .install(Harness::request("1.1.0"))
        .await
        .expect_err("not authorised");
    assert_eq!(error.code, ApplicationErrorCode::NotFound);
    assert_eq!(error.reason, Some(ErrorReason::PluginNotInMarket));

    // 只剩被撤回的版本：没有可装条目。
    h.set_catalog(
        vec![entry_json("1.1.0", 2, &sha(PACKAGE), "malicious")],
        Vec::new(),
    );
    let error = h
        .service
        .install(Harness::request("1.1.0"))
        .await
        .expect_err("only yanked");
    assert_eq!(error.reason, Some(ErrorReason::PluginNotInMarket));
    assert_eq!(h.mock.ticket_hits.load(Ordering::SeqCst), 0);
}
