//! 嗅探规则包存储：磁盘缓存 + 验签 + 按需后台刷新。
//!
//! 只在浏览器扩展发起 `sniff_rules` 请求时才可能触发刷新；没有请求就没有网络与磁盘写入
//! （AGENTS「空闲静默」）。普通查询永不等待刷新完成，直接返回当前已验证的缓存；
//! `force` 查询（用户点「立即同步」）会等刷新完成（至多 [`FORCE_WAIT`]）再应答。
//!
//! 退避按文档独立：每份文档有自己的 `next_due`，成功后 6 h，失败按失败次数指数退避
//! （30 min 起，封顶 6 h），一份文档永久 404 不会拖着另外两份一起频繁重试。
//! 最近一次刷新尝试时间持久化在 `state.json`（仅刷新时写），agent 重启后不会立刻重拉。

mod keys;
mod verify;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};

use crate::egress::{Egress, EgressAttempt, EgressClient};
use verify::{DocKind, KeySet, MAX_ENVELOPE_BYTES, VerifiedDoc, verify_envelope};

pub use verify::SniffRuleError;

const SOURCES: &[&str] = &[
    "https://raw.githubusercontent.com/zerx-lab/fluxdown-sniff-rules/main/dist/",
    "https://cdn.jsdelivr.net/gh/zerx-lab/fluxdown-sniff-rules@main/dist/",
];
const REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const RETRY_INTERVAL: Duration = Duration::from_secs(30 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// `force` 查询等待刷新完成的上限；超时按当前缓存应答，`status.refreshing` 为 true。
const FORCE_WAIT: Duration = Duration::from_secs(15);
const STATE_FILE: &str = "state.json";

/// 扩展已持有的各文档 sequence；只返回比它新的文档。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Have {
    official: Option<u64>,
    community: Option<u64>,
    revocations: Option<u64>,
    plugin: Option<u64>,
    /// 扩展持有的 plugin 包所属 epoch；与 daemon 当前 epoch 不同时 `plugin` 序号作废。
    plugin_epoch: Option<String>,
}

impl Have {
    const fn of(&self, kind: DocKind) -> Option<u64> {
        match kind {
            DocKind::Official => self.official,
            DocKind::Community => self.community,
            DocKind::Revocations => self.revocations,
        }
    }

    /// 扩展已持有的 plugin 层规则包序号。
    pub(crate) const fn plugin(&self) -> Option<u64> {
        self.plugin
    }

    /// 扩展已持有的 plugin 层规则包 epoch。
    pub(crate) fn plugin_epoch(&self) -> Option<&str> {
        self.plugin_epoch.as_deref()
    }
}

/// 单份文档的缓存与重试状态。
#[derive(Default)]
struct DocSlot {
    doc: Option<VerifiedDoc>,
    failures: u32,
    next_due: Option<Instant>,
    error: Option<String>,
}

impl DocSlot {
    fn due(&self, now: Instant) -> bool {
        self.next_due.is_none_or(|at| at <= now)
    }
}

#[derive(Default)]
struct State {
    loaded: bool,
    docs: [DocSlot; 3],
    /// 按 URL 记的验证器；只在对应文档已被接受并落盘后记录，供条件请求使用。
    etags: HashMap<String, String>,
    last_attempt_unix: Option<u64>,
    last_fetch_unix: Option<u64>,
    refreshing: bool,
    /// 刷新进行中又收到 `force`：本轮结束后追加一轮强制刷新。
    rerun_forced: bool,
}

/// 持久化的刷新时间（`state.json`）。
#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedMeta {
    last_attempt_unix: Option<u64>,
    last_fetch_unix: Option<u64>,
}

struct Inner {
    dir: PathBuf,
    keys: KeySet,
    sources: Vec<String>,
    /// 宿主自身下载的统一出口：信封带 ed25519 签名，来源可交给插件改写（如 GitHub 加速）。
    egress: Egress,
    http: EgressClient,
    state: Mutex<State>,
    /// 每完成一次刷新（含追加的强制轮次）计数加一；`force` 查询据此等待。
    done: watch::Sender<u64>,
}

pub struct SniffRuleStore {
    inner: Arc<Inner>,
}

impl SniffRuleStore {
    /// 生产构造：内置公钥与规则源；`dir` 为 `<agent_data_dir>/sniff-rules`。
    #[must_use]
    pub fn new(dir: PathBuf, egress: Egress) -> Self {
        Self::build(
            dir,
            KeySet::production(),
            SOURCES.iter().map(|source| (*source).to_owned()).collect(),
            egress,
        )
    }

    #[cfg(test)]
    fn with(dir: PathBuf, keys: KeySet, sources: Vec<String>) -> Self {
        Self::build(dir, keys, sources, Egress::system_default())
    }

    fn build(dir: PathBuf, keys: KeySet, sources: Vec<String>, egress: Egress) -> Self {
        Self {
            inner: Arc::new(Inner {
                dir,
                keys,
                sources,
                egress,
                http: EgressClient::new(|| {
                    crate::tls::client_builder(crate::tls::HttpVersion::Any)
                        .timeout(REQUEST_TIMEOUT)
                        .user_agent(concat!("fluxdown-agent/", env!("CARGO_PKG_VERSION")))
                }),
                state: Mutex::new(State::default()),
                done: watch::channel(0).0,
            }),
        }
    }

    /// 按 C5 `sniff_rules` 应答构造 data。普通查询立即返回当前缓存（必要时后台启动刷新）；
    /// `force` 查询忽略退避并等刷新完成（至多 [`FORCE_WAIT`]）后再返回，保证用户手动同步拿到最新。
    pub async fn query(&self, have: &Have, force: bool) -> Value {
        let mut completion = None;
        {
            let mut state = self.inner.state.lock().await;
            self.inner.ensure_loaded(&mut state).await;
            let now = Instant::now();
            let due = state.docs.iter().any(|slot| slot.due(now));
            if force || due {
                if state.refreshing {
                    state.rerun_forced |= force;
                } else {
                    state.refreshing = true;
                    tokio::spawn(Arc::clone(&self.inner).refresh(force));
                }
                if force {
                    // 在持锁时订阅：刷新收尾也要拿这把锁，不会漏掉完成通知。
                    completion = Some(self.inner.done.subscribe());
                }
            }
        }
        if let Some(mut finished) = completion {
            match tokio::time::timeout(FORCE_WAIT, finished.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::debug!(%error, "sniff rule refresh channel closed"),
                Err(_) => {
                    tracing::debug!("forced sniff rule refresh still running; replying with cache")
                }
            }
        }

        let state = self.inner.state.lock().await;
        let newer = |kind: DocKind| {
            state.docs[kind.index()]
                .doc
                .as_ref()
                .filter(|doc| have.of(kind).is_none_or(|known| doc.sequence > known))
                .map_or(Value::Null, |doc| doc.payload.clone())
        };
        let error_of = |kind: DocKind| state.docs[kind.index()].error.clone();
        let last_error = {
            let parts: Vec<String> = DocKind::ALL
                .iter()
                .filter_map(|kind| {
                    error_of(*kind).map(|error| format!("{}: {error}", kind.file_name()))
                })
                .collect();
            (!parts.is_empty()).then(|| parts.join("; "))
        };
        json!({
            "official": newer(DocKind::Official),
            "community": newer(DocKind::Community),
            "revocations": newer(DocKind::Revocations),
            "status": {
                "lastFetchUnix": state.last_fetch_unix,
                "lastAttemptUnix": state.last_attempt_unix,
                "lastError": last_error,
                "errors": {
                    "official": error_of(DocKind::Official),
                    "community": error_of(DocKind::Community),
                    "revocations": error_of(DocKind::Revocations),
                },
                "refreshing": state.refreshing,
            },
        })
    }

    #[cfg(test)]
    async fn refresh_now(&self) {
        {
            let mut state = self.inner.state.lock().await;
            self.inner.ensure_loaded(&mut state).await;
            state.refreshing = true;
        }
        Arc::clone(&self.inner).refresh(true).await;
    }
}

/// 接受的文档来自哪个 URL、带什么验证器。
struct Accepted {
    url: String,
    etag: Option<String>,
}

enum Fetched {
    Updated(VerifiedDoc, Accepted),
    /// 与已持有的 sequence 相同；`None` 表示服务端 304，验证器不变。
    Current(Option<Accepted>),
}

enum Downloaded {
    NotModified,
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
}

/// 失败次数 → 下次重试等待：30 min 起每次翻倍，封顶 6 h。
fn retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(4);
    RETRY_INTERVAL
        .saturating_mul(1_u32 << doublings)
        .min(REFRESH_INTERVAL)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

impl Inner {
    async fn ensure_loaded(&self, state: &mut State) {
        if !state.loaded {
            self.load_from_disk(state).await;
            state.loaded = true;
        }
    }

    /// 重新验签磁盘上的每个文件；验签失败（被篡改 / 密钥轮换后失效）的文件删除。
    /// 同时读回最近刷新时间：距上次尝试不足一个周期的文档推迟到周期结束再拉。
    async fn load_from_disk(&self, state: &mut State) {
        for kind in DocKind::ALL {
            let path = self.dir.join(kind.file_name());
            let bytes = match tokio::fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    tracing::warn!(file = kind.file_name(), error = %error, "could not read sniff rule cache");
                    continue;
                }
            };
            match verify_envelope(&bytes, kind, &self.keys) {
                Ok(doc) => state.docs[kind.index()].doc = Some(doc),
                Err(error) => {
                    tracing::warn!(file = kind.file_name(), error = %error, "discarding invalid sniff rule cache");
                    if let Err(error) = tokio::fs::remove_file(&path).await {
                        tracing::warn!(file = kind.file_name(), error = %error, "could not delete invalid sniff rule cache");
                    }
                }
            }
        }
        let meta = match tokio::fs::read(self.dir.join(STATE_FILE)).await {
            Ok(bytes) => serde_json::from_slice::<PersistedMeta>(&bytes).unwrap_or_else(|error| {
                tracing::warn!(error = %error, "ignoring corrupt sniff rule state file");
                PersistedMeta::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => PersistedMeta::default(),
            Err(error) => {
                tracing::warn!(error = %error, "could not read sniff rule state file");
                PersistedMeta::default()
            }
        };
        state.last_attempt_unix = meta.last_attempt_unix;
        state.last_fetch_unix = meta.last_fetch_unix;
        if let Some(attempt) = meta.last_attempt_unix {
            let elapsed = now_unix().saturating_sub(attempt);
            if let Some(remaining) = REFRESH_INTERVAL.as_secs().checked_sub(elapsed) {
                let due = Instant::now() + Duration::from_secs(remaining);
                // 没有缓存的文档（被丢弃 / 从未取到）不推迟，让下一次查询补拉。
                for slot in state.docs.iter_mut().filter(|slot| slot.doc.is_some()) {
                    slot.next_due = Some(due);
                }
            }
        }
    }

    /// 刷新到所有文档都不再有待处理的强制请求为止；收尾时持久化时间并通知等待者。
    async fn refresh(self: Arc<Self>, mut force: bool) {
        let mut attempted = false;
        loop {
            attempted |= self.pass(force).await;
            let mut state = self.state.lock().await;
            if state.rerun_forced {
                state.rerun_forced = false;
                force = true;
                continue;
            }
            state.refreshing = false;
            if attempted {
                state.last_attempt_unix = Some(now_unix());
                self.persist_meta(&state).await;
            }
            break;
        }
        self.done
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// 取一遍到期（或 `force`）的文档；返回是否真的发起了请求。
    async fn pass(&self, force: bool) -> bool {
        let mut attempted = false;
        let mut fetched_any = false;
        for kind in DocKind::ALL {
            let have = {
                let state = self.state.lock().await;
                let slot = &state.docs[kind.index()];
                if !force && !slot.due(Instant::now()) {
                    continue;
                }
                slot.doc.as_ref().map(|doc| doc.sequence)
            };
            attempted = true;
            let outcome = match self.fetch_kind(kind, have).await {
                Ok(Fetched::Current(accepted)) => Ok((None, accepted)),
                Ok(Fetched::Updated(doc, accepted)) => {
                    write_atomic(&self.dir, kind.file_name(), &doc.envelope)
                        .await
                        .map(|()| (Some(doc), Some(accepted)))
                }
                Err(error) => Err(error),
            };
            let mut state = self.state.lock().await;
            let now = Instant::now();
            match outcome {
                Ok((doc, accepted)) => {
                    fetched_any = true;
                    if let Some(Accepted { url, etag }) = accepted {
                        match etag {
                            Some(etag) => {
                                state.etags.insert(url, etag);
                            }
                            None => {
                                state.etags.remove(&url);
                            }
                        }
                    }
                    let slot = &mut state.docs[kind.index()];
                    if doc.is_some() {
                        slot.doc = doc;
                    }
                    slot.failures = 0;
                    slot.error = None;
                    slot.next_due = Some(now + REFRESH_INTERVAL);
                }
                Err(error) => {
                    let slot = &mut state.docs[kind.index()];
                    slot.failures = slot.failures.saturating_add(1);
                    slot.error = Some(error.to_string());
                    slot.next_due = Some(now + retry_delay(slot.failures));
                }
            }
        }
        if fetched_any {
            self.state.lock().await.last_fetch_unix = Some(now_unix());
        }
        attempted
    }

    async fn persist_meta(&self, state: &State) {
        let meta = PersistedMeta {
            last_attempt_unix: state.last_attempt_unix,
            last_fetch_unix: state.last_fetch_unix,
        };
        let bytes = match serde_json::to_vec(&meta) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(error = %error, "could not encode sniff rule state");
                return;
            }
        };
        if let Err(error) = write_atomic(&self.dir, STATE_FILE, &bytes).await {
            tracing::warn!(error = %error, "could not persist sniff rule state");
        }
    }

    /// 按源顺序取第一个 2xx（或 304）且验签通过且不回滚的文档。每个源先试统一出口给出的
    /// 插件改写地址，下载失败或验签不过再回退原地址。
    async fn fetch_kind(
        &self,
        kind: DocKind,
        have: Option<u64>,
    ) -> Result<Fetched, SniffRuleError> {
        let mut last_error = SniffRuleError::Http("no sources configured".to_owned());
        for source in &self.sources {
            let url = format!("{source}{}", kind.file_name());
            // 只有已持有该文档时才发条件请求：否则 304 会让我们一直空手。
            let known = match have {
                Some(_) => self.state.lock().await.etags.get(&url).cloned(),
                None => None,
            };
            let plan = self.egress.plan(&url, true).await;
            let client = match self.http.get(&plan.network).await {
                Ok(client) => client,
                Err(error) => {
                    last_error = SniffRuleError::Http(error.to_string());
                    continue;
                }
            };
            for attempt in plan.attempts() {
                let (bytes, etag) = match self.download(&client, attempt, known.as_deref()).await {
                    Ok(Downloaded::NotModified) => return Ok(Fetched::Current(None)),
                    Ok(Downloaded::Body { bytes, etag }) => (bytes, etag),
                    Err(error) => {
                        last_error = error;
                        continue;
                    }
                };
                let doc = match verify_envelope(&bytes, kind, &self.keys) {
                    Ok(doc) => doc,
                    Err(error) => {
                        last_error = error;
                        continue;
                    }
                };
                let accepted = Accepted {
                    url: url.clone(),
                    etag,
                };
                match have {
                    Some(have) if doc.sequence < have => {
                        last_error = SniffRuleError::Rollback {
                            have,
                            got: doc.sequence,
                        };
                    }
                    Some(have) if doc.sequence == have => {
                        return Ok(Fetched::Current(Some(accepted)));
                    }
                    _ => return Ok(Fetched::Updated(doc, accepted)),
                }
            }
        }
        Err(last_error)
    }

    /// 流式累计响应体，超过信封上限即中止。错误串不含 URL（可能带查询参数）。
    async fn download(
        &self,
        client: &reqwest::Client,
        attempt: &EgressAttempt,
        etag: Option<&str>,
    ) -> Result<Downloaded, SniffRuleError> {
        let http_error =
            |error: reqwest::Error| SniffRuleError::Http(error.without_url().to_string());
        let mut request = client.get(&attempt.url);
        for (name, value) in &attempt.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let mut response = request.send().await.map_err(http_error)?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_MODIFIED && etag.is_some() {
            return Ok(Downloaded::NotModified);
        }
        if !status.is_success() {
            return Err(SniffRuleError::Http(format!("status {}", status.as_u16())));
        }
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(http_error)? {
            if body.len() + chunk.len() > MAX_ENVELOPE_BYTES {
                return Err(SniffRuleError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Downloaded::Body { bytes: body, etag })
    }
}

/// 先写 `<name>.tmp` 再 rename，崩溃时不会留下半截缓存。
async fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), SniffRuleError> {
    let io = |error: std::io::Error| SniffRuleError::Io(error.to_string());
    tokio::fs::create_dir_all(dir).await.map_err(io)?;
    let temp = dir.join(format!("{name}.tmp"));
    tokio::fs::write(&temp, bytes).await.map_err(io)?;
    tokio::fs::rename(&temp, dir.join(name)).await.map_err(io)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum::extract::State as AxumState;
    use axum::http::header::{ETAG, IF_NONE_MATCH};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use ed25519_dalek::SigningKey;
    use serde_json::json;
    use tokio::sync::Mutex;

    use super::verify::sign_envelope;
    use super::*;

    #[derive(Default)]
    struct ServedDoc {
        bytes: Option<Vec<u8>>,
        requests: u32,
        conditional_hits: u32,
    }

    type Served = Arc<Mutex<ServedDoc>>;

    fn signing() -> SigningKey {
        SigningKey::from_bytes(&[7_u8; 32])
    }

    fn keys() -> KeySet {
        KeySet::new(
            vec![("rel".to_owned(), signing().verifying_key().to_bytes())],
            Vec::new(),
        )
    }

    fn official(sequence: u64) -> Vec<u8> {
        let payload = serde_json::to_vec(&json!({
            "format": "fluxdown-sniff-pack", "packId": "official", "sequence": sequence,
            "engine": 1, "issuedAt": "2026-10-01T00:00:00Z", "rules": [],
        }))
        .expect("pack json");
        sign_envelope(&payload, "rel", &signing())
    }

    /// 只提供 `official.json`（带 `ETag`，支持 `If-None-Match`）；其余文件 404。
    /// 返回 `dist/` 源前缀与可替换 / 可统计的文档槽。
    async fn serve(initial: Vec<u8>) -> (String, Served) {
        let served: Served = Arc::new(Mutex::new(ServedDoc {
            bytes: Some(initial),
            ..ServedDoc::default()
        }));
        let app = Router::new()
            .route(
                "/dist/official.json",
                get(
                    |AxumState(served): AxumState<Served>, headers: HeaderMap| async move {
                        let mut doc = served.lock().await;
                        doc.requests += 1;
                        let Some(bytes) = doc.bytes.clone() else {
                            return StatusCode::NOT_FOUND.into_response();
                        };
                        let digest = bytes.iter().fold(0_u64, |acc, byte| {
                            acc.wrapping_mul(31).wrapping_add(u64::from(*byte))
                        });
                        let etag = format!("\"{}-{digest}\"", bytes.len());
                        if headers
                            .get(IF_NONE_MATCH)
                            .and_then(|value| value.to_str().ok())
                            == Some(etag.as_str())
                        {
                            doc.conditional_hits += 1;
                            return StatusCode::NOT_MODIFIED.into_response();
                        }
                        ([(ETAG, etag)], bytes).into_response()
                    },
                ),
            )
            .with_state(Arc::clone(&served));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::warn!(error = %error, "test sniff rule server stopped");
            }
        });
        (format!("http://{address}/dist/"), served)
    }

    fn temp_dir() -> tempfile_dir::Dir {
        tempfile_dir::Dir::new()
    }

    /// 无额外依赖的临时目录（agent 未依赖 tempfile）。
    mod tempfile_dir {
        use std::path::{Path, PathBuf};

        pub(super) struct Dir(PathBuf);

        impl Dir {
            pub(super) fn new() -> Self {
                let path = std::env::temp_dir()
                    .join(format!("fluxdown-sniff-test-{}", uuid::Uuid::new_v4()));
                Self(path)
            }

            pub(super) fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                if let Err(error) = std::fs::remove_dir_all(&self.0)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    eprintln!("could not remove test dir: {error}");
                }
            }
        }
    }

    async fn wait_for_official(store: &SniffRuleStore) -> Value {
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let reply = store.query(&Have::default(), false).await;
            if !reply["official"].is_null() {
                return reply;
            }
        }
        Value::Null
    }

    #[tokio::test]
    async fn first_query_is_empty_then_refresh_delivers_pack() {
        let (source, _served) = serve(official(5)).await;
        let dir = temp_dir();
        let store = SniffRuleStore::with(dir.path().to_path_buf(), keys(), vec![source]);

        let first = store.query(&Have::default(), false).await;
        assert!(first["official"].is_null());

        let delivered = wait_for_official(&store).await;
        assert_eq!(delivered["official"]["sequence"], 5);
        assert!(delivered["community"].is_null());
        assert!(delivered["status"]["lastFetchUnix"].is_u64());
        assert!(dir.path().join("official.json").exists());
    }

    #[tokio::test]
    async fn forced_query_waits_for_the_refresh_and_returns_the_fresh_pack() {
        let (source, _served) = serve(official(5)).await;
        let dir = temp_dir();
        let store = SniffRuleStore::with(dir.path().to_path_buf(), keys(), vec![source]);

        // 第一次查询就带 force：不能像普通查询那样返回空缓存。
        let reply = store.query(&Have::default(), true).await;
        assert_eq!(reply["official"]["sequence"], 5);
        assert_eq!(reply["status"]["refreshing"], false);
        assert!(reply["status"]["lastAttemptUnix"].is_u64());
    }

    #[tokio::test]
    async fn forced_query_ignores_backoff_that_blocks_plain_queries() {
        let (source, served) = serve(official(5)).await;
        let dir = temp_dir();
        let store = SniffRuleStore::with(dir.path().to_path_buf(), keys(), vec![source]);
        store.refresh_now().await;
        assert_eq!(served.lock().await.requests, 1);

        *served.lock().await = ServedDoc {
            bytes: Some(official(6)),
            ..ServedDoc::default()
        };
        // 普通查询：6 h 内不到期，不拉取，仍是旧包。
        let plain = store.query(&Have::default(), false).await;
        assert_eq!(plain["official"]["sequence"], 5);
        // 强制查询：立刻拉取并等待。
        let forced = store.query(&Have::default(), true).await;
        assert_eq!(forced["official"]["sequence"], 6);
    }

    #[tokio::test]
    async fn unchanged_refresh_sends_a_conditional_request_and_keeps_the_cache_file() {
        let (source, served) = serve(official(5)).await;
        let dir = temp_dir();
        let store = SniffRuleStore::with(dir.path().to_path_buf(), keys(), vec![source]);
        store.refresh_now().await;
        let path = dir.path().join("official.json");
        let before = std::fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");

        store.refresh_now().await;

        let served = served.lock().await;
        assert_eq!(served.requests, 2);
        assert_eq!(served.conditional_hits, 1);
        let after = std::fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn failing_document_backs_off_without_dragging_the_others() {
        let (source, served) = serve(official(5)).await;
        let dir = temp_dir();
        let store = SniffRuleStore::with(dir.path().to_path_buf(), keys(), vec![source]);
        store.refresh_now().await;
        assert_eq!(served.lock().await.requests, 1);

        let reply = store.query(&Have::default(), false).await;
        assert!(reply["status"]["errors"]["official"].is_null());
        assert!(reply["status"]["errors"]["community"].is_string());
        assert!(reply["status"]["errors"]["revocations"].is_string());

        // official 已成功（6 h 内不再拉）、community / revocations 在退避期内：普通查询不发请求。
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(served.lock().await.requests, 1);
    }

    #[tokio::test]
    async fn last_attempt_survives_restart_and_defers_the_first_refresh() {
        let (source, served) = serve(official(5)).await;
        let dir = temp_dir();
        let first = SniffRuleStore::with(dir.path().to_path_buf(), keys(), vec![source.clone()]);
        first.refresh_now().await;
        assert_eq!(served.lock().await.requests, 1);

        let restarted = SniffRuleStore::with(dir.path().to_path_buf(), keys(), vec![source]);
        let reply = restarted.query(&Have::default(), false).await;
        // 缓存来自磁盘；未到期的 official 不会因为重启而重拉。
        assert_eq!(reply["official"]["sequence"], 5);
        assert!(reply["status"]["lastAttemptUnix"].is_u64());
        assert!(reply["status"]["lastFetchUnix"].is_u64());
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(served.lock().await.requests, 1);
    }

    #[tokio::test]
    async fn have_current_sequence_returns_null() {
        let (source, _served) = serve(official(5)).await;
        let dir = temp_dir();
        let store = SniffRuleStore::with(dir.path().to_path_buf(), keys(), vec![source]);
        store.refresh_now().await;

        let current = Have {
            official: Some(5),
            ..Have::default()
        };
        assert!(store.query(&current, false).await["official"].is_null());
        let stale = Have {
            official: Some(4),
            ..Have::default()
        };
        assert_eq!(store.query(&stale, false).await["official"]["sequence"], 5);
    }

    #[tokio::test]
    async fn server_rollback_keeps_old_pack_and_reports_error() {
        let (source, served) = serve(official(5)).await;
        let dir = temp_dir();
        let store = SniffRuleStore::with(dir.path().to_path_buf(), keys(), vec![source]);
        store.refresh_now().await;

        served.lock().await.bytes = Some(official(3));
        store.refresh_now().await;

        let reply = store.query(&Have::default(), false).await;
        assert_eq!(reply["official"]["sequence"], 5);
        let error = reply["status"]["lastError"].as_str().expect("lastError");
        assert!(error.contains("Rollback"), "{error}");
    }

    #[tokio::test]
    async fn tampered_disk_file_is_deleted_on_load() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.path()).expect("mkdir");
        let path = dir.path().join("official.json");
        std::fs::write(&path, official(5)).expect("write");
        // 篡改：payload 解码后内容变化，签名不再匹配。
        let mut tampered = official(5);
        let needle = b"\"payload\":\"";
        let at = tampered
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("payload field")
            + needle.len();
        tampered[at] = if tampered[at] == b'A' { b'B' } else { b'A' };
        std::fs::write(&path, tampered).expect("write tampered");

        let store = SniffRuleStore::with(dir.path().to_path_buf(), keys(), Vec::new());
        let reply = store.query(&Have::default(), false).await;
        assert!(reply["official"].is_null());
        assert!(!path.exists());
    }

    #[test]
    fn retry_delay_doubles_and_caps_at_the_refresh_interval() {
        assert_eq!(retry_delay(1), RETRY_INTERVAL);
        assert_eq!(retry_delay(2), RETRY_INTERVAL * 2);
        assert_eq!(retry_delay(3), RETRY_INTERVAL * 4);
        assert_eq!(retry_delay(4), RETRY_INTERVAL * 8);
        assert_eq!(retry_delay(5), REFRESH_INTERVAL);
        assert_eq!(retry_delay(u32::MAX), REFRESH_INTERVAL);
    }
}
