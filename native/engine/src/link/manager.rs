//! [`LinkManager`] —— 设备互联子系统门面。
//!
//! 聚合身份、名册存储、配对协议（响应方 + 发起方）、mDNS 发现、可扩展传输栈，
//! 供宿主（hub 桌面 / headless server）驱动。宿主只跟本门面 + 一个事件通道打交道。
//!
//! # 角色
//! - **响应方**（被添加设备）：生成配对码、处理 `hello`/`confirm`、mDNS 广播。
//! - **发起方**（正在添加设备）：mDNS 浏览、`begin_pairing`（发 hello、算 SAS）、
//!   `confirm_pairing`（发 confirm、落库）。
//! - **数据面**：`dispatch`（把下载下发给已配对设备，走传输栈）、`authorize`
//!   （校验入站链路请求的 HMAC 鉴权）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use tokio::sync::mpsc;

use super::crypto::{LINK_AUTH_SKEW_SECS, link_auth_tag, verify_link_auth_tag};
use super::discovery::{self, MdnsAdvertiser, MdnsBrowser};
use super::error::{LinkError, LinkResult};
use super::identity::{IDENTITY_CONFIG_KEY, LinkIdentity};
use super::pairing::{HelloRequest, HelloResponse, PairingInitiator, PairingResponder, SelfInfo};
use super::store::LinkStore;
use super::transport::TransportStack;
use super::types::{DiscoveredPeer, PeerRecord};
use crate::db::Db;

/// 引擎侧设备互联事件（宿主消费：hub 转 rinf 信号，server 可广播 WS）。
#[derive(Debug, Clone)]
pub enum LinkEngineEvent {
    /// mDNS/手动发现到一台设备。
    Discovered(DiscoveredPeer),
    /// 一台设备完成配对并入册（含 link_secret，宿主转 UI 前须剥除敏感字段）。
    Paired(PeerRecord),
    /// 一台设备被解除配对（fingerprint）。
    Unpaired(String),
    /// 子系统错误（供 UI 提示）。
    Error(String),
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 发起方待确认会话（begin_pairing 与 confirm_pairing 之间的状态）。
struct PendingInit {
    initiator: PairingInitiator,
    session_id: String,
    peer_host: String,
    peer_port: u16,
    created: std::time::Instant,
}

/// 设备互联门面。宿主持 `Arc<LinkManager>`。
pub struct LinkManager {
    identity: LinkIdentity,
    self_info: SelfInfo,
    store: LinkStore,
    responder: PairingResponder,
    transport: TransportStack,
    client: reqwest::Client,
    api_port: u16,
    events: mpsc::Sender<LinkEngineEvent>,
    advertiser: Mutex<Option<MdnsAdvertiser>>,
    browser: Mutex<Option<MdnsBrowser>>,
    pending: Mutex<HashMap<String, PendingInit>>,
    /// 数据面防重放：时窗内已见过的 `(device:nonce, ts)`，authorize 剪枝保持有界。
    seen_nonces: Mutex<Vec<(String, i64)>>,
}

impl LinkManager {
    /// 从引擎数据库加载（或首次生成并持久化）本机身份，构造门面。
    ///
    /// `api_port` = 本机 fluxdown API 端口（mDNS 广播 + 自报候选用）。
    pub async fn load(
        db: Db,
        self_info: SelfInfo,
        api_port: u16,
        events: mpsc::Sender<LinkEngineEvent>,
    ) -> LinkResult<Arc<Self>> {
        let identity = Self::load_or_create_identity(&db).await?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let responder = PairingResponder::new(identity.clone(), self_info.clone());
        let transport = TransportStack::direct_only(client.clone());
        Ok(Arc::new(Self {
            identity,
            self_info,
            store: LinkStore::new(db),
            responder,
            transport,
            client,
            api_port,
            events,
            advertiser: Mutex::new(None),
            browser: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            seen_nonces: Mutex::new(Vec::new()),
        }))
    }

    async fn load_or_create_identity(db: &Db) -> LinkResult<LinkIdentity> {
        if let Some(b64) = db.get_config(IDENTITY_CONFIG_KEY).await?
            && let Ok(bytes) = B64.decode(b64.trim())
            && let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice())
        {
            return Ok(LinkIdentity::from_secret_bytes(&seed));
        }
        let identity = LinkIdentity::generate();
        db.set_config(IDENTITY_CONFIG_KEY, &B64.encode(identity.secret_bytes()))
            .await?;
        Ok(identity)
    }

    /// 本机设备指纹（设备 ID）。
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        self.identity.fingerprint()
    }

    /// 本机展示名（供 `/ping` 透出）。
    #[must_use]
    pub fn self_name(&self) -> &str {
        &self.self_info.name
    }

    /// 本机平台（供 `/ping` 透出）。
    #[must_use]
    pub fn self_platform(&self) -> Option<&str> {
        self.self_info.platform.as_deref()
    }

    // ── 响应方（被添加设备侧）─────────────────────────────────────────────

    /// 生成一次性配对码（在被添加设备 UI 展示），并确保 mDNS 广播已开启。
    pub fn generate_code(&self) -> String {
        self.ensure_advertising();
        self.responder.generate_code()
    }

    /// 处理入站 `hello`（HTTP 层解码 base64 后调用）。
    pub fn pair_hello(&self, req: HelloRequest) -> LinkResult<HelloResponse> {
        self.responder.handle_hello(&req)
    }

    /// 处理入站 `hello`（wire 形式，base64 编解码全在引擎内完成，宿主纯字段搬运）。
    pub fn pair_hello_wire(&self, w: WireHello) -> LinkResult<WireHelloResponse> {
        let req = HelloRequest {
            code: w.code,
            initiator_eph_pub: decode_b64_array::<32>(&w.initiator_eph_pub)?,
            initiator_id_pub: decode_b64_array::<32>(&w.initiator_id_pub)?,
            initiator_sig: decode_b64_array::<64>(&w.initiator_sig)?,
            name: w.name,
            platform: w.platform,
            app_version: w.app_version,
            initiator_addrs: w.initiator_addrs,
        };
        let resp = self.responder.handle_hello(&req)?;
        Ok(WireHelloResponse {
            session_id: resp.session_id,
            responder_eph_pub: B64.encode(resp.responder_eph_pub),
            responder_id_pub: B64.encode(resp.responder_id_pub),
            responder_sig: B64.encode(resp.responder_sig),
            name: resp.name,
            platform: resp.platform,
            app_version: resp.app_version,
            sas: resp.sas,
        })
    }

    /// 处理入站 `confirm`：确认成功则把发起方入册并广播 `Paired` 事件。
    pub async fn pair_confirm(&self, session_id: &str, confirm: bool) -> LinkResult<bool> {
        match self.responder.handle_confirm(session_id, confirm)? {
            Some(record) => {
                self.store.upsert(&record).await?;
                let _ = self.events.send(LinkEngineEvent::Paired(record)).await;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// 确保 mDNS 广播运行（幂等）。失败仅记 Error 事件，不阻断配对（手动地址可兜底）。
    fn ensure_advertising(&self) {
        let mut guard = match self.advertiser.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.is_some() {
            return;
        }
        match MdnsAdvertiser::start(
            self.api_port,
            self.identity.fingerprint(),
            &self.self_info.name,
            self.self_info.platform.as_deref(),
            self.self_info.app_version.as_deref(),
        ) {
            Ok(a) => *guard = Some(a),
            Err(e) => {
                let tx = self.events.clone();
                let msg = e.to_string();
                tokio::spawn(async move {
                    let _ = tx.send(LinkEngineEvent::Error(msg)).await;
                });
            }
        }
    }

    /// 主动开启 mDNS 广播（宿主在启用本地互联时调用）。
    pub fn start_advertising(&self) {
        self.ensure_advertising();
    }

    // ── 发现（发起方侧）───────────────────────────────────────────────────

    /// 开始 mDNS 浏览：发现的设备经事件通道以 `Discovered` 汇出（幂等）。
    pub fn start_discovery(&self) -> LinkResult<()> {
        let mut guard = self.browser.lock().map_err(|_| LinkError::Unavailable)?;
        if guard.is_some() {
            return Ok(());
        }
        let (tx, mut rx) = mpsc::channel::<DiscoveredPeer>(64);
        let out = self.events.clone();
        let self_fp = self.identity.fingerprint().to_string();
        tokio::spawn(async move {
            while let Some(peer) = rx.recv().await {
                // 过滤掉本机自身的广播。
                if peer.fingerprint.as_deref() == Some(self_fp.as_str()) {
                    continue;
                }
                if out.send(LinkEngineEvent::Discovered(peer)).await.is_err() {
                    break;
                }
            }
        });
        *guard = Some(MdnsBrowser::start(tx)?);
        Ok(())
    }

    /// 停止 mDNS 浏览。
    pub fn stop_discovery(&self) {
        if let Ok(mut guard) = self.browser.lock() {
            *guard = None; // Drop → daemon.shutdown()
        }
    }

    /// 手动地址探测（mDNS 失效兜底）：`/ping` 一台设备，返回其信息（不配对）。
    pub async fn probe(&self, host: &str, port: u16) -> LinkResult<DiscoveredPeer> {
        discovery::probe(&self.client, host, port).await
    }

    // ── 配对（发起方侧）───────────────────────────────────────────────────

    /// 发起配对：向 `host:port` 发送 `hello`（带配对码），返回 `(token, sas, 对端名)`。
    /// UI 展示 SAS 供用户与对端核对，随后调 [`confirm_pairing`]。
    pub async fn begin_pairing(
        &self,
        host: &str,
        port: u16,
        code: &str,
    ) -> LinkResult<BeginPairingResult> {
        let mut initiator = PairingInitiator::new(self.identity.clone());
        let addrs = discovery::local_direct_addrs(host, self.api_port);
        let hello = initiator.build_hello(code, &self.self_info, addrs);

        let body = serde_json::json!({
            "code": hello.code,
            "initiatorEphPub": B64.encode(hello.initiator_eph_pub),
            "initiatorIdPub": B64.encode(hello.initiator_id_pub),
            "initiatorSig": B64.encode(hello.initiator_sig),
            "name": hello.name,
            "platform": hello.platform.clone().unwrap_or_default(),
            "appVersion": hello.app_version.clone().unwrap_or_default(),
            "initiatorAddrs": hello.initiator_addrs,
        });
        let url = format!("http://{host}:{port}/api/v1/link/pair/hello");
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| LinkError::Io(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::BAD_REQUEST {
            return Err(LinkError::InvalidCode);
        }
        if !resp.status().is_success() {
            return Err(LinkError::Unreachable);
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| LinkError::Io(e.to_string()))?;
        let hello_resp = parse_hello_response(&json)?;

        let responder_addr = format!("{host}:{port}");
        let sas = initiator.on_hello_response(&hello_resp, &responder_addr)?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let peer_name = hello_resp.name.clone();
        if let Ok(mut pending) = self.pending.lock() {
            // 剪枝：丢弃用户已放弃、超过会话时窗的待确认项，避免无界增长 + 及时释放临时密钥。
            pending.retain(|_, p| p.created.elapsed().as_secs() < 180);
            pending.insert(
                token.clone(),
                PendingInit {
                    initiator,
                    session_id: hello_resp.session_id.clone(),
                    peer_host: host.to_string(),
                    peer_port: port,
                    created: std::time::Instant::now(),
                },
            );
        }
        Ok(BeginPairingResult {
            token,
            sas,
            peer_name,
            peer_fingerprint: super::crypto::fingerprint(&hello_resp.responder_id_pub),
        })
    }

    /// SAS 核对后确认/拒绝配对。`accept=true` 且对端确认成功 → 落库 + 广播 Paired。
    pub async fn confirm_pairing(
        &self,
        token: &str,
        accept: bool,
    ) -> LinkResult<Option<PeerRecord>> {
        let pending = {
            let mut guard = self.pending.lock().map_err(|_| LinkError::Unavailable)?;
            guard.remove(token)
        };
        let Some(pending) = pending else {
            return Err(LinkError::SessionExpired);
        };
        let body = serde_json::json!({ "sessionId": pending.session_id, "confirm": accept });
        let url = format!(
            "http://{}:{}/api/v1/link/pair/confirm",
            pending.peer_host, pending.peer_port
        );
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| LinkError::Io(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(LinkError::SessionExpired);
        }
        if !accept {
            return Ok(None);
        }
        let record = pending.initiator.finalize()?;
        self.store.upsert(&record).await?;
        let _ = self
            .events
            .send(LinkEngineEvent::Paired(record.clone()))
            .await;
        Ok(Some(record))
    }

    // ── 名册 ──────────────────────────────────────────────────────────────

    /// 全部已配对设备。
    pub async fn list_devices(&self) -> LinkResult<Vec<PeerRecord>> {
        self.store.list().await
    }

    /// 解除配对（删除设备），广播 Unpaired。
    pub async fn remove_device(&self, fingerprint: &str) -> LinkResult<bool> {
        let removed = self.store.remove(fingerprint).await?;
        if removed {
            let _ = self
                .events
                .send(LinkEngineEvent::Unpaired(fingerprint.to_string()))
                .await;
        }
        Ok(removed)
    }

    /// 探测一台已配对设备是否在线（走传输栈拨号），成功则刷新 last_seen。
    pub async fn is_online(&self, fingerprint: &str) -> bool {
        let Ok(Some(record)) = self.store.get(fingerprint).await else {
            return false;
        };
        match self.transport.connect(&record).await {
            Ok(_) => {
                let _ = self.store.touch(fingerprint, now_unix()).await;
                true
            }
            Err(_) => false,
        }
    }

    // ── 数据面 ────────────────────────────────────────────────────────────

    /// 把一个下载任务下发给已配对设备（发起方数据面）。走传输栈解析可达 base_url，
    /// 用每对独立链路密钥做 HMAC 鉴权，POST 对端 `/api/v1/link/tasks`。返回新任务 ID。
    pub async fn dispatch(
        &self,
        fingerprint: &str,
        url: &str,
        save_dir: Option<&str>,
        file_name: Option<&str>,
    ) -> LinkResult<String> {
        let record = self
            .store
            .get(fingerprint)
            .await?
            .ok_or(LinkError::Unauthorized)?;
        let conn = self.transport.connect(&record).await?;
        let path = "/api/v1/link/tasks";
        let ts = now_unix();
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        // 请求体序列化**一次**，同一份字节既用于 HMAC 也用于发送——保证签名覆盖
        // 的字节与对端收到并校验的字节完全一致（Option 空值序列化为 ""，非 null，
        // 否则响应方 `LinkTaskRequest`(非 Option String) 反序列化会 400）。
        let body_json = serde_json::json!({
            "url": url,
            "saveDir": save_dir.unwrap_or_default(),
            "fileName": file_name.unwrap_or_default(),
        });
        let body_bytes = serde_json::to_vec(&body_json).unwrap_or_default();
        let tag = link_auth_tag(&record.link_secret, "POST", path, ts, &nonce, &body_bytes);
        let resp = self
            .client
            .post(format!("{}{}", conn.base_url, path))
            .header("X-FluxLink-Device", self.identity.fingerprint())
            .header("X-FluxLink-Ts", ts.to_string())
            .header("X-FluxLink-Nonce", nonce)
            .header("X-FluxLink-Auth", tag)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body_bytes)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| LinkError::Io(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(LinkError::Unauthorized);
        }
        if !resp.status().is_success() {
            return Err(LinkError::Io(format!("dispatch failed: {}", resp.status())));
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| LinkError::Io(e.to_string()))?;
        let _ = self.store.touch(fingerprint, now_unix()).await;
        Ok(json
            .get("taskId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// 校验入站链路数据面请求的 HMAC 鉴权（响应方 `/api/v1/link/tasks` 调用）。
    /// 校验时间戳时窗 + HMAC（含 body 摘要）+ nonce 防重放。成功返回发起方指纹。
    #[allow(clippy::too_many_arguments)]
    pub async fn authorize(
        &self,
        method: &str,
        path: &str,
        device_fp: &str,
        ts: i64,
        nonce: &str,
        body: &[u8],
        tag: &str,
    ) -> LinkResult<String> {
        let now = now_unix();
        // i128 比较，防攻击者构造的极端 ts 触发 i64 溢出（debug 下 panic）。
        if (now as i128 - ts as i128).abs() > LINK_AUTH_SKEW_SECS as i128 {
            return Err(LinkError::Unauthorized);
        }
        let record = self
            .store
            .get(device_fp)
            .await?
            .ok_or(LinkError::Unauthorized)?;
        if !verify_link_auth_tag(&record.link_secret, method, path, ts, nonce, body, tag) {
            return Err(LinkError::Unauthorized);
        }
        // 防重放：同 (device, nonce) 在时窗内仅接受一次；顺带按时窗剪枝保持有界。
        {
            let mut seen = self
                .seen_nonces
                .lock()
                .map_err(|_| LinkError::Unavailable)?;
            seen.retain(|(_, seen_ts)| now - *seen_ts <= LINK_AUTH_SKEW_SECS);
            let key = format!("{device_fp}:{nonce}");
            if seen.iter().any(|(k, _)| *k == key) {
                return Err(LinkError::Unauthorized);
            }
            seen.push((key, now));
        }
        Ok(device_fp.to_string())
    }
}

/// [`LinkManager::begin_pairing`] 的结果：待确认令牌 + 供核对的 SAS + 对端信息。
#[derive(Debug, Clone)]
pub struct BeginPairingResult {
    pub token: String,
    pub sas: String,
    pub peer_name: String,
    pub peer_fingerprint: String,
}

fn b64_to_array<const N: usize>(json: &serde_json::Value, key: &str) -> LinkResult<[u8; N]> {
    let s = json
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| LinkError::BadPayload(format!("missing {key}")))?;
    let bytes = B64
        .decode(s)
        .map_err(|_| LinkError::BadPayload(format!("bad base64 {key}")))?;
    <[u8; N]>::try_from(bytes.as_slice())
        .map_err(|_| LinkError::BadPayload(format!("bad length {key}")))
}

fn parse_hello_response(json: &serde_json::Value) -> LinkResult<HelloResponse> {
    let get_str = |k: &str| {
        json.get(k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Ok(HelloResponse {
        session_id: get_str("sessionId")
            .ok_or_else(|| LinkError::BadPayload("missing sessionId".into()))?,
        responder_eph_pub: b64_to_array::<32>(json, "responderEphPub")?,
        responder_id_pub: b64_to_array::<32>(json, "responderIdPub")?,
        responder_sig: b64_to_array::<64>(json, "responderSig")?,
        name: get_str("name").unwrap_or_default(),
        platform: get_str("platform"),
        app_version: get_str("appVersion"),
        sas: get_str("sas").unwrap_or_default(),
    })
}

/// 入站 `hello` 的 wire 形式（base64 字符串字段），供 HTTP 宿主纯字段搬运。
#[derive(Debug, Clone)]
pub struct WireHello {
    pub code: String,
    pub initiator_eph_pub: String,
    pub initiator_id_pub: String,
    pub initiator_sig: String,
    pub name: String,
    pub platform: Option<String>,
    pub app_version: Option<String>,
    pub initiator_addrs: Vec<String>,
}

/// 出站 `hello` 回复的 wire 形式（base64 字符串字段）。
#[derive(Debug, Clone)]
pub struct WireHelloResponse {
    pub session_id: String,
    pub responder_eph_pub: String,
    pub responder_id_pub: String,
    pub responder_sig: String,
    pub name: String,
    pub platform: Option<String>,
    pub app_version: Option<String>,
    pub sas: String,
}

fn decode_b64_array<const N: usize>(s: &str) -> LinkResult<[u8; N]> {
    let bytes = B64
        .decode(s)
        .map_err(|_| LinkError::BadPayload("bad base64".into()))?;
    <[u8; N]>::try_from(bytes.as_slice()).map_err(|_| LinkError::BadPayload("bad length".into()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::link::crypto::link_auth_tag;

    async fn mgr_with_device(secret: Vec<u8>) -> (Arc<LinkManager>, String) {
        let url = format!(
            "sqlite:file:linkmgr_{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4().simple()
        );
        let db = Db::connect(&url).await.unwrap();
        let (tx, _rx) = mpsc::channel(8);
        let mgr = LinkManager::load(
            db,
            SelfInfo {
                name: "me".into(),
                platform: None,
                app_version: None,
            },
            17800,
            tx,
        )
        .await
        .unwrap();
        let fp = "peerfp".to_string();
        mgr.store
            .upsert(&PeerRecord {
                fingerprint: fp.clone(),
                identity_pub: vec![1u8; 32],
                name: "peer".into(),
                platform: None,
                link_secret: secret,
                candidates: vec![],
                paired_at: 0,
                last_seen_at: 0,
            })
            .await
            .unwrap();
        (mgr, fp)
    }

    #[tokio::test]
    async fn authorize_accepts_valid_then_rejects_replay_tamper_and_skew() {
        let secret = vec![7u8; 32];
        let (mgr, fp) = mgr_with_device(secret.clone()).await;
        let path = "/api/v1/link/tasks";
        let ts = now_unix();
        let body = br#"{"url":"http://x/f"}"#;
        let tag = link_auth_tag(&secret, "POST", path, ts, "n1", body);

        // 合法请求通过。
        assert!(
            mgr.authorize("POST", path, &fp, ts, "n1", body, &tag)
                .await
                .is_ok()
        );
        // 同 nonce 重放被拒（防重放）。
        assert!(matches!(
            mgr.authorize("POST", path, &fp, ts, "n1", body, &tag).await,
            Err(LinkError::Unauthorized)
        ));
        // 篡改 body（换 URL）→ tag 不符 → 拒。
        assert!(matches!(
            mgr.authorize(
                "POST",
                path,
                &fp,
                ts,
                "n2",
                br#"{"url":"http://evil"}"#,
                &tag
            )
            .await,
            Err(LinkError::Unauthorized)
        ));
        // 过期时间戳 → 拒。
        let old_ts = ts - LINK_AUTH_SKEW_SECS - 5;
        let old_tag = link_auth_tag(&secret, "POST", path, old_ts, "n3", body);
        assert!(matches!(
            mgr.authorize("POST", path, &fp, old_ts, "n3", body, &old_tag)
                .await,
            Err(LinkError::Unauthorized)
        ));
        // 未配对设备 → 拒。
        assert!(matches!(
            mgr.authorize("POST", path, "unknown", ts, "n4", body, &tag)
                .await,
            Err(LinkError::Unauthorized)
        ));
    }
}
