//! FluxCloud HTTPS 客户端、并发 401 单飞刷新与一次重放。
//!
//! 服务地址：`default_base_url` 由启动环境固定；仅调试构建允许经
//! [`CloudClient::set_endpoint`] 覆盖（持久化在 agent 私有状态；GPUI 账户页同样只在调试构建展示该入口）
//! ——正式包锁定默认地址，避免残留覆盖值指向失效地址。

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxdown_protocol::{
    AgentEvent, ApplicationErrorCode, CloudEndpointDto, ErrorReason, RpcErrorData,
};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};

use super::models::{AuthResponse, CloudErrorBody, RefreshRequest};
use crate::event_hub::AgentEventHub;
use crate::http_client::LazyHttpClient;
use crate::state::{AgentState, CloudCredentials, StateStore};

/// 是否允许运行期覆盖 FluxCloud 地址；仅调试构建。
const ENDPOINT_EDITABLE: bool = cfg!(debug_assertions);

/// 云端 WebSocket 接收单消息 / 单帧上限；与远程任务通道的 SSE 单行上限一致。
const MAX_SOCKET_MESSAGE_BYTES: usize = 1024 * 1024;

/// `/api/v1/tasks/ws` 握手被判定为「网络 / 中间层 / 服务端不支持 WebSocket」时 `CloudError::code` 的取值。
pub(crate) const WEBSOCKET_UNSUPPORTED: &str = "websocket_unsupported";

/// 握手等响应头超时时 `CloudError::code` 的取值（仍是 `unreachable` 网络类错误）。
pub(crate) const WEBSOCKET_HANDSHAKE_TIMEOUT: &str = "websocket_handshake_timeout";

/// 升级成功的云端 WebSocket（HTTP/1.1 Upgrade，沿用 reqwest 的 TLS / 代理栈）。
pub(crate) type CloudSocket = tokio_tungstenite::WebSocketStream<reqwest::Upgraded>;

/// 云端连接的 TCP keepalive 探测间隔。
const TCP_KEEPALIVE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RequestEpoch(u64);

#[derive(Clone)]
pub struct CloudClient {
    default_base_url: String,
    base_url: Arc<RwLock<String>>,
    http: Arc<LazyHttpClient>,
    stream_http: Arc<LazyHttpClient>,
    /// WebSocket 握手专用：Upgrade 只存在于 HTTP/1.1，禁用 ALPN 协商 h2。
    ws_http: Arc<LazyHttpClient>,
    state: Arc<Mutex<AgentState>>,
    store: Arc<StateStore>,
    refresh: Arc<Mutex<()>>,
    session_generation: watch::Sender<u64>,
    /// 会话被清除（退出 / 刷新令牌被拒 / 远端撤销）时投影 `SessionChanged(None)`，
    /// 让 UI 与 agent 私有状态永不脱节。
    events: Option<AgentEventHub>,
    /// 本机时钟相对 FluxCloud 的偏移（毫秒，`服务端 - 本机`），取自最近一次响应的 `Date` 头；
    /// 没有样本时为 0（直接用本机时间）。仅内存，进程内所有克隆共享。
    clock_offset_ms: Arc<AtomicI64>,
}

impl CloudClient {
    // Capture before starting account-scoped work. Token rotation does not
    // advance this epoch; only explicit session replacement/clear does.
    pub(crate) fn request_epoch(&self) -> RequestEpoch {
        RequestEpoch(*self.session_generation.borrow())
    }

    // The guard is the commit boundary for both state and event projections.
    // A transport-only check is insufficient: another connection can log in
    // between receiving the HTTP response and acquiring the state lock.
    pub(crate) async fn lock_epoch(
        &self,
        epoch: RequestEpoch,
    ) -> Result<tokio::sync::MutexGuard<'_, AgentState>, CloudError> {
        let state = self.state.lock().await;
        if *self.session_generation.borrow() != epoch.0 {
            return Err(CloudError {
                status: None,
                code: Some("session_changed".to_owned()),
                message: "account-scoped response belongs to an ended session".to_owned(),
                retryable: false,
                unreachable: false,
            });
        }
        Ok(state)
    }

    pub fn new(
        base_url: String,
        state: Arc<Mutex<AgentState>>,
        store: Arc<StateStore>,
    ) -> Result<Self, CloudError> {
        validate_base_url(&base_url)?;
        let http = LazyHttpClient::new(|| {
            reqwest::Client::builder()
                .user_agent(format!("FluxDown/{}", fluxdown_protocol::APP_VERSION))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(15))
                .pool_idle_timeout(Duration::from_secs(15))
                .tcp_keepalive(Some(TCP_KEEPALIVE))
        });
        // SSE 长连接：TCP keepalive 让内核尽早发现半开连接（合盖唤醒 / NAT 静默断流），
        // 与应用层空闲看门狗互为补充。
        let stream_http = LazyHttpClient::new(|| {
            reqwest::Client::builder()
                .user_agent(format!("FluxDown/{}", fluxdown_protocol::APP_VERSION))
                .connect_timeout(Duration::from_secs(10))
                .pool_idle_timeout(Duration::from_secs(90))
                .tcp_keepalive(Some(TCP_KEEPALIVE))
        });
        let ws_http = LazyHttpClient::new(|| {
            reqwest::Client::builder()
                .user_agent(format!("FluxDown/{}", fluxdown_protocol::APP_VERSION))
                .connect_timeout(Duration::from_secs(10))
                .pool_idle_timeout(Duration::from_secs(90))
                .tcp_keepalive(Some(TCP_KEEPALIVE))
                .http1_only()
                // 3xx 说明中间层没有透传 Upgrade：不跟随，交给握手分类归为「不支持」。
                .redirect(reqwest::redirect::Policy::none())
        });
        let default_base_url = normalize_base_url(&base_url);
        Ok(Self {
            base_url: Arc::new(RwLock::new(default_base_url.clone())),
            default_base_url,
            http: Arc::new(http),
            stream_http: Arc::new(stream_http),
            ws_http: Arc::new(ws_http),
            state,
            store,
            refresh: Arc::new(Mutex::new(())),
            session_generation: watch::channel(0).0,
            events: None,
            clock_offset_ms: Arc::new(AtomicI64::new(0)),
        })
    }

    /// 按 FluxCloud 时钟估计的当前 unix 毫秒（本机时钟 + `Date` 头偏移）。
    pub(crate) fn server_now_ms(&self) -> i64 {
        unix_ms().saturating_add(self.clock_offset_ms.load(Ordering::Relaxed))
    }

    /// 用响应 `Date` 头（秒精度，取该秒中点）与请求往返中点估计偏移；往返过长的样本误差过大，丢弃。
    fn observe_server_clock(
        &self,
        response: &reqwest::Response,
        sent_at_ms: i64,
        received_at_ms: i64,
    ) {
        const MAX_SAMPLE_RTT_MS: i64 = 5_000;
        if received_at_ms.saturating_sub(sent_at_ms) > MAX_SAMPLE_RTT_MS {
            return;
        }
        let Some(server_ms) = response
            .headers()
            .get(reqwest::header::DATE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_http_date_ms)
        else {
            return;
        };
        let local_mid = sent_at_ms.saturating_add(received_at_ms.saturating_sub(sent_at_ms) / 2);
        self.clock_offset_ms.store(
            server_ms.saturating_add(500).saturating_sub(local_mid),
            Ordering::Relaxed,
        );
    }

    /// 接入事件枢纽；生产运行链路必须调用，否则会话清除不会通知 UI。
    #[must_use]
    pub fn with_events(mut self, events: AgentEventHub) -> Self {
        self.events = Some(events);
        self
    }

    /// 启动时套用已持久化的地址覆盖；正式构建或覆盖值非法时保持默认地址。
    pub async fn restore_endpoint_override(&self) {
        if !ENDPOINT_EDITABLE {
            return;
        }
        let Some(base_url) = self.state.lock().await.cloud_base_url_override.clone() else {
            return;
        };
        match validate_base_url(&base_url) {
            Ok(()) => {
                self.swap_base_url(normalize_base_url(&base_url));
                tracing::info!(base_url = %base_url, "using FluxCloud endpoint override");
            }
            Err(error) => {
                tracing::warn!(base_url = %base_url, %error, "ignoring invalid FluxCloud endpoint override");
            }
        }
    }

    /// 当前生效地址、构建期默认地址与是否可改。
    pub fn endpoint(&self) -> CloudEndpointDto {
        CloudEndpointDto {
            base_url: self.current_base_url(),
            default_base_url: self.default_base_url.clone(),
            editable: ENDPOINT_EDITABLE,
        }
    }

    /// 覆盖（空串 = 恢复默认）FluxCloud 地址并持久化；后续请求立即使用新地址。
    /// 正式构建拒绝调用。
    pub async fn set_endpoint(&self, base_url: &str) -> Result<CloudEndpointDto, CloudError> {
        if !ENDPOINT_EDITABLE {
            return Err(CloudError::unsupported());
        }
        let trimmed = base_url.trim();
        let next = if trimmed.is_empty() {
            None
        } else {
            validate_base_url(trimmed)?;
            Some(normalize_base_url(trimmed)).filter(|url| *url != self.default_base_url)
        };
        {
            let mut state = self.state.lock().await;
            state.cloud_base_url_override.clone_from(&next);
        }
        self.store
            .persist(&self.state)
            .await
            .map_err(CloudError::from_state)?;
        self.swap_base_url(next.unwrap_or_else(|| self.default_base_url.clone()));
        Ok(self.endpoint())
    }

    fn current_base_url(&self) -> String {
        self.base_url
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn swap_base_url(&self, base_url: String) {
        *self
            .base_url
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = base_url;
    }

    /// 本机设备身份 `(device_id, device_name, platform)`，供认证请求体与请求头共用。
    pub(crate) async fn device_identity(&self) -> (String, String, String) {
        let state = self.state.lock().await;
        (
            state.device_id.clone(),
            state.device_name.clone(),
            state.platform.clone(),
        )
    }

    /// 无登录端点调用。
    pub async fn public<P: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&P>,
    ) -> Result<R, CloudError> {
        let body = body
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| CloudError::invalid(error.to_string()))?;
        let response = self.send_once(method, path, body, None).await?;
        decode(response).await
    }

    /// 登录端点调用；401 共享一次刷新并只重放一次原请求。
    pub async fn authenticated<P: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&P>,
    ) -> Result<R, CloudError> {
        let epoch = self.request_epoch();
        self.authenticated_epoch(method, path, body, epoch).await
    }

    pub(crate) async fn authenticated_epoch<P: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&P>,
        epoch: RequestEpoch,
    ) -> Result<R, CloudError> {
        let body = body
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| CloudError::invalid(error.to_string()))?;
        let attempted = self
            .lock_epoch(epoch)
            .await?
            .credentials
            .as_ref()
            .map(|credentials| credentials.access_token.clone())
            .filter(|token| !token.is_empty())
            .ok_or_else(CloudError::unauthorized)?;
        let response = self
            .send_once(method.clone(), path, body.clone(), Some(&attempted))
            .await?;
        if response.status() != StatusCode::UNAUTHORIZED {
            let result = decode(response).await;
            drop(self.lock_epoch(epoch).await?);
            return result;
        }
        let replay_token = self.refreshed_access_token(&attempted, epoch).await?;
        drop(self.lock_epoch(epoch).await?);
        let replay = self
            .send_once(method, path, body, Some(&replay_token))
            .await?;
        let result = decode(replay).await;
        drop(self.lock_epoch(epoch).await?);
        result
    }

    /// 401 后取可重放的 access token：单飞刷新，其他调用方已轮换过则直接复用新令牌。
    /// 普通请求与 SSE 必须共用这一入口，否则并发刷新会用同一枚 refresh token 撞上服务端的一次性轮换。
    async fn refreshed_access_token(
        &self,
        attempted: &str,
        epoch: RequestEpoch,
    ) -> Result<String, CloudError> {
        let _guard = self.refresh.lock().await;
        drop(self.lock_epoch(epoch).await?);
        let current = self.access_token().await?;
        if current != attempted {
            return Ok(current);
        }
        self.refresh_session(true).await
    }

    /// 显式登录/注册成功后，先原子持久化令牌轮换再返回无令牌会话；
    /// 同时把账号维度的状态（同步水位 / 脏键 / 远程任务）切换到该账号。
    pub(crate) async fn persist_auth(
        &self,
        auth: AuthResponse,
    ) -> Result<fluxdown_protocol::AgentSessionDto, CloudError> {
        let session = auth.session();
        let switched = {
            let mut state = self.state.lock().await;
            self.session_generation
                .send_modify(|generation| *generation = generation.wrapping_add(1));
            state.credentials = Some(CloudCredentials {
                access_token: auth.access_token,
                refresh_token: auth.refresh_token,
                expires_at_unix: now_unix().saturating_add(auth.expires_in),
                session: Some(session.clone()),
            });
            state
                .bind_account(Some(&session.user.id))
                .then(|| state.sync.clone())
        };
        self.store
            .persist(&self.state)
            .await
            .map_err(CloudError::from_state)?;
        // 换号 / 首次登录：账号维度的投影（旧账号的设备、远程任务、同步状态）立即清空。
        if let (Some(sync), Some(events)) = (switched, &self.events) {
            events.publish(AgentEvent::RemoteTasksChanged(Vec::new()));
            events.publish(AgentEvent::CloudDevicesChanged(Vec::new()));
            events.publish(AgentEvent::SyncChanged(sync));
        }
        Ok(session)
    }

    /// 刷新成功后落盘轮换出的令牌；只在凭证仍是被刷新的那一份时写入，
    /// 登出 / 撤销期间完成的刷新结果不得让会话复活。
    async fn persist_refreshed(
        &self,
        auth: AuthResponse,
        rotated_from: &str,
    ) -> Result<String, CloudError> {
        let access_token = auth.access_token.clone();
        {
            let mut state = self.state.lock().await;
            match state.credentials.as_ref() {
                None => return Err(CloudError::unauthorized()),
                // 期间已重新登录：沿用新凭证，丢弃过期副本的刷新结果。
                Some(current) if current.refresh_token != rotated_from => {
                    return Ok(current.access_token.clone());
                }
                Some(_) => {}
            }
            let session = auth.session();
            state.credentials = Some(CloudCredentials {
                access_token: auth.access_token,
                refresh_token: auth.refresh_token,
                expires_at_unix: now_unix().saturating_add(auth.expires_in),
                session: Some(session),
            });
        }
        self.store
            .persist(&self.state)
            .await
            .map_err(CloudError::from_state)?;
        Ok(access_token)
    }

    /// 资料修改成功后更新无令牌会话并原子持久化。
    pub(crate) async fn persist_profile(
        &self,
        epoch: RequestEpoch,
        profile: fluxdown_protocol::CloudProfile,
    ) -> Result<fluxdown_protocol::AgentSessionDto, CloudError> {
        let updated = {
            let mut state = self.lock_epoch(epoch).await?;
            let session = state
                .credentials
                .as_mut()
                .and_then(|credentials| credentials.session.as_mut())
                .ok_or_else(CloudError::unauthorized)?;
            session.user = profile.user;
            session.entitlements = profile.entitlements;
            session.current_plan = profile.current_plan;
            session.active_subscription = profile.active_subscription;
            let updated = session.clone();
            if let Some(events) = &self.events {
                events.publish(AgentEvent::SessionChanged(Box::new(Some(updated.clone()))));
            }
            updated
        };
        self.store
            .persist(&self.state)
            .await
            .map_err(CloudError::from_state)?;
        Ok(updated)
    }

    /// 当前登录账号 id；未登录为 `None`。
    pub(crate) async fn current_user_id(&self) -> Option<String> {
        self.state
            .lock()
            .await
            .credentials
            .as_ref()
            .and_then(|credentials| credentials.session.as_ref())
            .map(|session| session.user.id.clone())
    }

    /// 清除完整会话（显式退出 / 用户删除本设备）并投影账号维度的清空事件；不发 `SessionRevoked`。
    /// 与进行中的令牌刷新互斥：刷新结果不会在清除之后复活会话。
    pub async fn clear_session(&self) -> Result<(), CloudError> {
        let _guard = self.refresh.lock().await;
        self.clear_session_locked(None).await
    }

    /// 非用户主动结束会话（云端撤销 / 设备被移除 / 令牌被拒）：先发一次性 `SessionRevoked(reason)`，
    /// 再走与 [`Self::clear_session`] 相同的清理。已经登出时不重复通知。
    pub async fn revoke_session(&self, reason: ErrorReason) -> Result<(), CloudError> {
        let _guard = self.refresh.lock().await;
        let announce = self.state.lock().await.credentials.is_some();
        self.clear_session_locked(announce.then_some(reason)).await
    }

    pub(crate) async fn revoke_session_epoch(
        &self,
        reason: ErrorReason,
        epoch: RequestEpoch,
    ) -> Result<(), CloudError> {
        let _guard = self.refresh.lock().await;
        let announce = self.lock_epoch(epoch).await?.credentials.is_some();
        self.clear_session_epoch_locked(announce.then_some(reason), Some(epoch))
            .await
    }

    /// 调用方必须持有 `self.refresh` 锁。清空凭证、账号维度状态（同步水位 / 脏键按账号暂存、
    /// 远程任务与接单绑定丢弃），先投影事件再报告落盘错误，保证 UI 与内存状态一致。
    /// `revoked` 为 `Some` 时先于 `SessionChanged(None)` 发布 `SessionRevoked`。
    async fn clear_session_locked(&self, revoked: Option<ErrorReason>) -> Result<(), CloudError> {
        self.clear_session_epoch_locked(revoked, None).await
    }

    pub(crate) async fn clear_session_epoch(&self, epoch: RequestEpoch) -> Result<(), CloudError> {
        let _guard = self.refresh.lock().await;
        self.clear_session_epoch_locked(None, Some(epoch)).await
    }

    async fn clear_session_epoch_locked(
        &self,
        revoked: Option<ErrorReason>,
        epoch: Option<RequestEpoch>,
    ) -> Result<(), CloudError> {
        {
            let mut state = match epoch {
                Some(epoch) => self.lock_epoch(epoch).await?,
                None => self.state.lock().await,
            };
            self.session_generation
                .send_modify(|generation| *generation = generation.wrapping_add(1));
            state.credentials = None;
            state.bind_account(None);
            if let Some(events) = &self.events {
                if let Some(reason) = revoked {
                    events.publish(AgentEvent::SessionRevoked(reason));
                }
                events.publish(AgentEvent::SessionChanged(Box::new(None)));
                events.publish(AgentEvent::RemoteTasksChanged(Vec::new()));
                events.publish(AgentEvent::CloudDevicesChanged(Vec::new()));
                events.publish(AgentEvent::SyncChanged(state.sync.clone()));
            }
        }
        self.store
            .persist(&self.state)
            .await
            .map_err(CloudError::from_state)
    }

    /// 退出登录：持有刷新锁完成「服务端吊销 + 本地清除」，避免与刷新竞态导致会话复活。
    /// 服务端吊销失败也会清除本地会话；清理同时失败时保留先发生的吊销错误并记录落盘错误。
    pub async fn logout(&self) -> Result<(), CloudError> {
        let _guard = self.refresh.lock().await;
        let (access_token, refresh_token) = {
            let state = self.state.lock().await;
            let credentials = state
                .credentials
                .as_ref()
                .filter(|credentials| !credentials.refresh_token.is_empty())
                .ok_or_else(CloudError::unauthorized)?;
            (
                credentials.access_token.clone(),
                credentials.refresh_token.clone(),
            )
        };
        let remote = self.revoke_on_server(&access_token, &refresh_token).await;
        // 吊销途中刷新令牌被拒时会话已被清除，不重复清理 / 通知。
        let cleanup = if self.state.lock().await.credentials.is_some() {
            self.clear_session_locked(None).await
        } else {
            Ok(())
        };
        match (remote, cleanup) {
            (Err(error), Err(cleanup_error)) => {
                tracing::error!(error = %cleanup_error, "could not persist local logout after server revocation failed");
                Err(error)
            }
            (Err(error), Ok(())) => Err(error),
            (Ok(()), cleanup) => cleanup,
        }
    }

    /// 调用方必须持有 `self.refresh` 锁：access 过期时先刷新再吊销。
    async fn revoke_on_server(
        &self,
        access_token: &str,
        refresh_token: &str,
    ) -> Result<(), CloudError> {
        let body = json!({ "refreshToken": refresh_token });
        let response = self
            .send_once(
                Method::POST,
                "/api/v1/auth/logout",
                Some(body.clone()),
                Some(access_token),
            )
            .await?;
        let response = if response.status() == StatusCode::UNAUTHORIZED {
            match self.refresh_session(false).await {
                Ok(fresh) => {
                    self.send_once(
                        Method::POST,
                        "/api/v1/auth/logout",
                        Some(body),
                        Some(&fresh),
                    )
                    .await?
                }
                // 刷新令牌已被服务端拒绝：会话在服务端本就失效，无需再吊销。
                Err(error) if error.status == Some(401) => return Ok(()),
                Err(error) => return Err(error),
            }
        } else {
            response
        };
        decode::<Value>(response).await.map(|_| ())
    }

    /// 调用方必须持有 `self.refresh` 锁。
    /// `announce`：刷新令牌被拒导致清会话时是否发布 `SessionRevoked`（登出流程里为 `false`）。
    async fn refresh_session(&self, announce: bool) -> Result<String, CloudError> {
        let refresh_token = {
            let state = self.state.lock().await;
            state
                .credentials
                .as_ref()
                .ok_or_else(CloudError::unauthorized)?
                .refresh_token
                .clone()
        };
        let response = self
            .send_once(
                Method::POST,
                "/api/v1/auth/refresh",
                Some(
                    serde_json::to_value(RefreshRequest {
                        refresh_token: &refresh_token,
                    })
                    .map_err(|error| CloudError::invalid(error.to_string()))?,
                ),
                None,
            )
            .await?;
        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            // 403 携带具体原因（账号停用 / 设备被移除）：清除会话后原样上报，不折叠成「登录过期」。
            let rejection = if response.status() == StatusCode::FORBIDDEN {
                Some(response_error(response).await)
            } else {
                None
            };
            return self
                .reject_refresh_token(&refresh_token, rejection, announce)
                .await;
        }
        let auth = decode::<AuthResponse>(response).await?;
        self.persist_refreshed(auth, &refresh_token).await
    }

    /// 服务端拒绝了 `rejected`：仅当它仍是当前凭证时才登出；期间凭证已被替换
    /// （并发刷新 / 重新登录）说明被拒的只是过期副本，沿用当前 access token。
    /// 调用方必须持有 `self.refresh` 锁。
    async fn reject_refresh_token(
        &self,
        rejected: &str,
        rejection: Option<CloudError>,
        announce: bool,
    ) -> Result<String, CloudError> {
        let current = {
            let state = self.state.lock().await;
            state.credentials.as_ref().map(|credentials| {
                (
                    credentials.refresh_token.clone(),
                    credentials.access_token.clone(),
                )
            })
        };
        match current {
            Some((refresh, access)) if refresh != rejected && !access.is_empty() => Ok(access),
            other => {
                let error = rejection.unwrap_or_else(CloudError::unauthorized);
                let revoked = (announce && other.is_some()).then(|| revocation_reason(&error));
                self.clear_session_locked(revoked).await?;
                Err(error)
            }
        }
    }

    async fn access_token(&self) -> Result<String, CloudError> {
        self.state
            .lock()
            .await
            .credentials
            .as_ref()
            .map(|credentials| credentials.access_token.clone())
            .filter(|token| !token.is_empty())
            .ok_or_else(CloudError::unauthorized)
    }

    pub(crate) async fn is_authenticated(&self) -> bool {
        self.state
            .lock()
            .await
            .credentials
            .as_ref()
            .is_some_and(|credentials| {
                !credentials.access_token.is_empty() && !credentials.refresh_token.is_empty()
            })
    }

    pub(crate) async fn authenticated_stream_epoch(
        &self,
        path: &str,
        epoch: RequestEpoch,
    ) -> Result<reqwest::Response, CloudError> {
        let access_token = self
            .lock_epoch(epoch)
            .await?
            .credentials
            .as_ref()
            .map(|credentials| credentials.access_token.clone())
            .filter(|token| !token.is_empty())
            .ok_or_else(CloudError::unauthorized)?;
        let response = self.send_stream_once(path, &access_token, epoch).await?;
        if response.status() != StatusCode::UNAUTHORIZED {
            let result = ensure_success(response).await;
            drop(self.lock_epoch(epoch).await?);
            return result;
        }
        let replay_token = self.refreshed_access_token(&access_token, epoch).await?;
        drop(self.lock_epoch(epoch).await?);
        let result = ensure_success(self.send_stream_once(path, &replay_token, epoch).await?).await;
        drop(self.lock_epoch(epoch).await?);
        result
    }

    /// 与 [`Self::authenticated_stream_epoch`] 同构的 WebSocket 版本：401 单飞刷新后重放一次，
    /// 升级成功后再校验会话 epoch。握手被判定为「不支持 WebSocket」时返回
    /// `code == "websocket_unsupported"` 的错误（见 [`CloudError::is_websocket_unsupported`]）。
    pub(crate) async fn authenticated_socket_epoch(
        &self,
        path: &str,
        epoch: RequestEpoch,
    ) -> Result<CloudSocket, CloudError> {
        let access_token = self
            .lock_epoch(epoch)
            .await?
            .credentials
            .as_ref()
            .map(|credentials| credentials.access_token.clone())
            .filter(|token| !token.is_empty())
            .ok_or_else(CloudError::unauthorized)?;
        let socket = match self.send_socket_once(path, &access_token, epoch).await? {
            Some(socket) => socket,
            None => {
                let replay_token = self.refreshed_access_token(&access_token, epoch).await?;
                drop(self.lock_epoch(epoch).await?);
                match self.send_socket_once(path, &replay_token, epoch).await? {
                    Some(socket) => socket,
                    None => {
                        drop(self.lock_epoch(epoch).await?);
                        return Err(CloudError::unauthorized());
                    }
                }
            }
        };
        drop(self.lock_epoch(epoch).await?);
        Ok(socket)
    }

    /// 一次握手：`Ok(None)` 表示 401（由调用方单飞刷新后重放）；其它非 101 响应转 `CloudError`。
    async fn send_socket_once(
        &self,
        path: &str,
        bearer: &str,
        epoch: RequestEpoch,
    ) -> Result<Option<CloudSocket>, CloudError> {
        use tokio_tungstenite::tungstenite::handshake::client::generate_key;
        use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
        use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

        let (device_id, device_name, platform) = self.device_identity().await;
        let client = self
            .ws_http
            .get()
            .await
            .map_err(|error| CloudError::local(format!("cloud WebSocket client: {error:#}")))?;
        drop(self.lock_epoch(epoch).await?);
        let key = generate_key();
        let request = client
            .get(format!("{}{}", self.current_base_url(), path))
            .bearer_auth(bearer)
            .header(reqwest::header::CONNECTION, "Upgrade")
            .header(reqwest::header::UPGRADE, "websocket")
            .header(reqwest::header::SEC_WEBSOCKET_VERSION, "13")
            .header(reqwest::header::SEC_WEBSOCKET_KEY, &key)
            .header("X-FluxDown-Device-Id", device_id)
            .header("X-FluxDown-Device-Name", device_name)
            .header("X-FluxDown-Platform", platform)
            .header("X-FluxDown-Version", fluxdown_protocol::APP_VERSION)
            .send();
        let sent_at = unix_ms();
        let response = tokio::time::timeout(Duration::from_secs(15), request)
            .await
            .map_err(|_| {
                let mut error =
                    CloudError::network("cloud WebSocket response headers timeout".to_owned());
                error.code = Some(WEBSOCKET_HANDSHAKE_TIMEOUT.to_owned());
                error
            })?
            .map_err(|error| CloudError::network(error_chain(&error)))?;
        self.observe_server_clock(&response, sent_at, unix_ms());
        let status = response.status();
        if status == StatusCode::SWITCHING_PROTOCOLS {
            let upgrade_ok = response
                .headers()
                .get_all(reqwest::header::UPGRADE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .flat_map(|value| value.split(','))
                .any(|token| token.trim().eq_ignore_ascii_case("websocket"));
            let accept_ok = response
                .headers()
                .get(reqwest::header::SEC_WEBSOCKET_ACCEPT)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|accept| accept.trim() == derive_accept_key(key.as_bytes()));
            if !upgrade_ok || !accept_ok {
                return Err(websocket_unsupported(
                    status,
                    "WebSocket upgrade response failed validation",
                ));
            }
            let upgraded = response
                .upgrade()
                .await
                .map_err(|error| CloudError::network(error_chain(&error)))?;
            let config = WebSocketConfig::default()
                .max_message_size(Some(MAX_SOCKET_MESSAGE_BYTES))
                .max_frame_size(Some(MAX_SOCKET_MESSAGE_BYTES));
            let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                upgraded,
                Role::Client,
                Some(config),
            )
            .await;
            return Ok(Some(socket));
        }
        if status == StatusCode::UNAUTHORIZED {
            return Ok(None);
        }
        // 2xx / 3xx 是中间层吞掉了 Upgrade，正文可能是任意（甚至无限）内容，不读取。
        if status.is_success() || status.is_redirection() {
            return Err(websocket_unsupported(
                status,
                "WebSocket upgrade was answered without switching protocols",
            ));
        }
        let mut error = response_error(response).await;
        if handshake_means_unsupported(status, error.code.as_deref()) {
            error.code = Some(WEBSOCKET_UNSUPPORTED.to_owned());
            error.retryable = false;
        }
        Err(error)
    }

    pub(crate) fn session_changes(&self) -> watch::Receiver<u64> {
        self.session_generation.subscribe()
    }

    async fn send_stream_once(
        &self,
        path: &str,
        bearer: &str,
        epoch: RequestEpoch,
    ) -> Result<reqwest::Response, CloudError> {
        let (device_id, device_name, platform) = self.device_identity().await;
        let client = self
            .stream_http
            .get()
            .await
            .map_err(|error| CloudError::local(format!("cloud stream client: {error:#}")))?;
        drop(self.lock_epoch(epoch).await?);
        let request = client
            .get(format!("{}{}", self.current_base_url(), path))
            .bearer_auth(bearer)
            .header("Accept", "text/event-stream")
            .header("X-FluxDown-Device-Id", device_id)
            .header("X-FluxDown-Device-Name", device_name)
            .header("X-FluxDown-Platform", platform)
            .header("X-FluxDown-Version", fluxdown_protocol::APP_VERSION)
            .send();
        let sent_at = unix_ms();
        // 仅限制响应头，不给无限 SSE body 加总寿命；包括成功 TCP 后不回头的反代。
        let response = tokio::time::timeout(Duration::from_secs(15), request)
            .await
            .map_err(|_| CloudError::network("cloud SSE response headers timeout".to_owned()))?
            .map_err(|error| CloudError::network(error_chain(&error)))?;
        self.observe_server_clock(&response, sent_at, unix_ms());
        Ok(response)
    }

    async fn send_once(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        bearer: Option<&str>,
    ) -> Result<reqwest::Response, CloudError> {
        let (device_id, device_name, platform) = self.device_identity().await;
        let client = self
            .http
            .get()
            .await
            .map_err(|error| CloudError::local(format!("cloud HTTP client: {error:#}")))?;
        let mut request = client
            .request(method, format!("{}{}", self.current_base_url(), path))
            .header("X-FluxDown-Device-Id", device_id)
            .header("X-FluxDown-Device-Name", device_name)
            .header("X-FluxDown-Platform", platform)
            .header("X-FluxDown-Version", fluxdown_protocol::APP_VERSION);
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let sent_at = unix_ms();
        let response = request
            .send()
            .await
            .map_err(|error| CloudError::network(error_chain(&error)))?;
        self.observe_server_clock(&response, sent_at, unix_ms());
        Ok(response)
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// 解析 RFC 9110 的 IMF-fixdate（`Sun, 06 Nov 1994 08:49:37 GMT`）为 unix 毫秒（秒精度）。
/// 服务端 `Date` 头固定使用该格式；其它格式视为无样本。
fn parse_http_date_ms(value: &str) -> Option<i64> {
    let mut parts = value.split_ascii_whitespace();
    let weekday = parts.next()?;
    if !weekday.ends_with(',') {
        return None;
    }
    let day = parts.next()?.parse::<i64>().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = parts.next()?.parse::<i64>().ok()?;
    let mut clock = parts.next()?.split(':');
    let hour = clock.next()?.parse::<i64>().ok()?;
    let minute = clock.next()?.parse::<i64>().ok()?;
    let second = clock.next()?.parse::<i64>().ok()?;
    if clock.next().is_some()
        || parts.next() != Some("GMT")
        || parts.next().is_some()
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..61).contains(&second)
    {
        return None;
    }
    // Howard Hinnant 的 days_from_civil：公历日期 → 1970-01-01 起的天数。
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1_000)
}

/// 令牌被拒导致清会话时通知 UI 的原因：账号停用 / 设备被移除保留，其余一律是会话过期。
fn revocation_reason(error: &CloudError) -> ErrorReason {
    match error.reason() {
        Some(reason @ (ErrorReason::AccountDisabled | ErrorReason::DeviceUntrusted)) => reason,
        _ => ErrorReason::SessionExpired,
    }
}

/// WebSocket 握手响应是否说明「这条链路不支持 WebSocket」（只看响应，不看网络错误）：
/// 404 / 405 / 426 / 501，或 400 / 403 且正文不是 FluxCloud `{code}` JSON（`code` 缺失：
/// 中间层 / WAF 拦截 Upgrade）。FluxCloud 自己的 403（如 `online_device_limit`）带 `code`，不回退。
/// 2xx / 3xx / 101 校验失败由调用方直接判定。
fn handshake_means_unsupported(status: StatusCode, error_code: Option<&str>) -> bool {
    matches!(status.as_u16(), 404 | 405 | 426 | 501)
        || (matches!(status, StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN)
            && error_code.is_none())
}

fn websocket_unsupported(status: StatusCode, message: &str) -> CloudError {
    CloudError {
        status: Some(status.as_u16()),
        code: Some(WEBSOCKET_UNSUPPORTED.to_owned()),
        message: message.to_owned(),
        retryable: false,
        unreachable: false,
    }
}

async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response, CloudError> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(response_error(response).await)
    }
}

/// 非 2xx 响应转 [`CloudError`]：优先解析 `{code, message}`；正文不是约定 JSON
/// （反代 / 网关 HTML、限流页）时保留状态码与正文片段，便于定位。
async fn response_error(response: reqwest::Response) -> CloudError {
    let status = response.status();
    let retryable = status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::debug!(%status, error = %error_chain(&error), "FluxCloud error body unreadable");
            Default::default()
        }
    };
    error_from_body(status.as_u16(), retryable, &bytes)
}

fn error_from_body(status: u16, retryable: bool, body: &[u8]) -> CloudError {
    match serde_json::from_slice::<CloudErrorBody>(body) {
        Ok(parsed) if parsed.code.is_some() || parsed.message.is_some() => CloudError {
            status: Some(status),
            message: parsed
                .message
                .clone()
                .unwrap_or_else(|| format!("FluxCloud HTTP {status}")),
            code: parsed.code,
            retryable,
            unreachable: false,
        },
        _ => {
            let text = String::from_utf8_lossy(body);
            let snippet = text.trim().chars().take(200).collect::<String>();
            tracing::debug!(status, body = %snippet, "FluxCloud error body is not {{code, message}} JSON");
            CloudError {
                status: Some(status),
                code: None,
                message: if snippet.is_empty() {
                    format!("FluxCloud HTTP {status}")
                } else {
                    format!("FluxCloud HTTP {status}: {snippet}")
                },
                retryable,
                unreachable: false,
            }
        }
    }
}

async fn decode<R: DeserializeOwned>(response: reqwest::Response) -> Result<R, CloudError> {
    let status = response.status();
    if !status.is_success() {
        return Err(response_error(response).await);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| CloudError::network(error_chain(&error)))?;
    // 204 / 空正文（如 presence 心跳）按 JSON null 解码。
    let parsed = if bytes.is_empty() {
        serde_json::from_value::<R>(Value::Null)
    } else {
        serde_json::from_slice::<R>(&bytes)
    };
    parsed.map_err(|error| {
        let mut invalid = CloudError::invalid_response(format!(
            "FluxCloud HTTP {status}: response body could not be decoded: {error:#}"
        ));
        invalid.status = Some(status.as_u16());
        invalid
    })
}

/// reqwest 的 `Display`（含 `{:#}`）只输出最外层「error sending request for url」，
/// 超时 / 连接重置 / TLS / 代理等根因都在 `source()` 链里，必须逐层拼出。
fn error_chain(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

fn validate_base_url(base_url: &str) -> Result<(), CloudError> {
    let url =
        reqwest::Url::parse(base_url).map_err(|error| CloudError::invalid(error.to_string()))?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if url.scheme() == "https" || (loopback && url.scheme() == "http") {
        Ok(())
    } else {
        Err(CloudError::invalid(
            "FluxCloud URL must use HTTPS outside loopback".to_owned(),
        ))
    }
}

fn normalize_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_owned()
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64)
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CloudError {
    pub status: Option<u16>,
    pub code: Option<String>,
    pub message: String,
    pub retryable: bool,
    /// 网络 / TLS / DNS / 超时：请求没能到达云端或响应没能读完。
    pub unreachable: bool,
}

impl CloudError {
    /// 握手被判定为「不支持 WebSocket」：调用方据此粘性回退 SSE。
    pub(crate) fn is_websocket_unsupported(&self) -> bool {
        self.code.as_deref() == Some(WEBSOCKET_UNSUPPORTED)
    }

    /// 握手等响应头超时（TCP 已通但没有响应）：调用方连续多次后视为通道不可用。
    pub(crate) fn is_websocket_handshake_timeout(&self) -> bool {
        self.code.as_deref() == Some(WEBSOCKET_HANDSHAKE_TIMEOUT)
    }

    pub(crate) fn unauthorized() -> Self {
        Self {
            status: Some(401),
            code: Some("unauthorized".to_owned()),
            message: "authentication required".to_owned(),
            retryable: false,
            unreachable: false,
        }
    }

    fn unsupported() -> Self {
        Self {
            status: None,
            code: Some("unsupported".to_owned()),
            message: "FluxCloud endpoint is fixed in release builds".to_owned(),
            retryable: false,
            unreachable: false,
        }
    }

    /// 网络层失败（可重试）。
    pub(crate) fn network(message: String) -> Self {
        Self {
            status: None,
            code: None,
            message,
            retryable: true,
            unreachable: true,
        }
    }

    /// 本机失败（构建 HTTP 客户端 / 落盘）：与云端无关，重试没有意义。
    pub(crate) fn local(message: String) -> Self {
        Self {
            status: None,
            code: Some("internal".to_owned()),
            message,
            retryable: false,
            unreachable: false,
        }
    }

    pub(crate) fn from_state(error: crate::state::StateError) -> Self {
        Self::local(format!("agent state persistence failed: {error:#}"))
    }

    fn invalid(message: String) -> Self {
        Self {
            status: None,
            code: Some("invalidArgument".to_owned()),
            message,
            retryable: false,
            unreachable: false,
        }
    }

    /// 云端错误码 / HTTP 状态 → 稳定的细分原因（契约 §3）。网络类失败为 `cloudUnreachable`。
    #[must_use]
    pub fn reason(&self) -> Option<ErrorReason> {
        if self.unreachable {
            return Some(ErrorReason::CloudUnreachable);
        }
        let by_code = match self.code.as_deref() {
            Some("invalid_credentials") => Some(ErrorReason::InvalidCredentials),
            Some("invalid_code") => Some(ErrorReason::InvalidVerificationCode),
            Some("wrong_password") => Some(ErrorReason::WrongPassword),
            Some("rate_limited") => Some(ErrorReason::RateLimited),
            Some("email_taken") => Some(ErrorReason::EmailTaken),
            Some("origin_id_taken") => Some(ErrorReason::OriginIdTaken),
            Some("origin_id_change_not_allowed") => Some(ErrorReason::OriginIdChangeNotAllowed),
            Some("origin_id_already_changed") => Some(ErrorReason::OriginIdAlreadyChanged),
            Some("account_disabled") => Some(ErrorReason::AccountDisabled),
            Some("registration_closed") => Some(ErrorReason::RegistrationClosed),
            Some("registration_incomplete") => Some(ErrorReason::RegistrationIncomplete),
            Some("mail_not_configured") => Some(ErrorReason::MailNotConfigured),
            Some("device_limit") => Some(ErrorReason::DeviceLimit),
            Some("online_device_limit") => Some(ErrorReason::OnlineDeviceLimit),
            Some("sync_device_untrusted") => Some(ErrorReason::DeviceUntrusted),
            Some("unauthorized") => Some(ErrorReason::SessionExpired),
            Some("target_device_offline") => Some(ErrorReason::TargetDeviceOffline),
            Some("task_state_conflict") => Some(ErrorReason::TaskStateConflict),
            Some("task_device_mismatch") => Some(ErrorReason::TaskDeviceMismatch),
            Some("notify_channel_limit") => Some(ErrorReason::NotifyChannelLimit),
            Some("notify_disabled") => Some(ErrorReason::NotifyDisabled),
            Some(
                "notify_target_invalid" | "notify_kind_unavailable" | "notify_email_unverified",
            ) => Some(ErrorReason::NotifyTargetInvalid),
            Some("payment_disabled" | "usdt_disabled") => Some(ErrorReason::PaymentDisabled),
            Some("gateway_error") => Some(ErrorReason::PaymentGatewayFailed),
            Some("plan_not_purchasable" | "usdt_not_available") => {
                Some(ErrorReason::PlanNotPurchasable)
            }
            Some("plan_already_owned") => Some(ErrorReason::PlanAlreadyOwned),
            Some("not_an_upgrade") => Some(ErrorReason::PlanNotUpgrade),
            Some("plan_tier_not_higher") => Some(ErrorReason::PlanTierNotHigher),
            Some("subscription_conflict") => Some(ErrorReason::SubscriptionConflict),
            Some("referral_code_invalid") => Some(ErrorReason::ReferralCodeInvalid),
            Some("referral_self_use") => Some(ErrorReason::ReferralSelfUse),
            Some("referral_already_used") => Some(ErrorReason::ReferralAlreadyUsed),
            Some("referral_plan_excluded") => Some(ErrorReason::ReferralPlanExcluded),
            Some("referral_code_taken") => Some(ErrorReason::ReferralCodeTaken),
            _ => None,
        };
        by_code.or(match self.status {
            Some(401) => Some(ErrorReason::SessionExpired),
            Some(429) => Some(ErrorReason::RateLimited),
            _ => None,
        })
    }

    /// 本机 RPC 错误：`reason` 让客户端给出可操作的本地化文案；`code` 供未识别原因时回退。
    #[must_use]
    pub fn to_rpc_error(&self) -> RpcErrorData {
        let reason = self.reason();
        let (code, retryable) = match reason {
            Some(
                ErrorReason::InvalidCredentials
                | ErrorReason::AccountDisabled
                | ErrorReason::DeviceUntrusted
                | ErrorReason::SessionExpired,
            ) => (ApplicationErrorCode::Unauthorized, false),
            Some(
                ErrorReason::InvalidVerificationCode
                | ErrorReason::WrongPassword
                | ErrorReason::NotifyTargetInvalid
                | ErrorReason::ReferralCodeInvalid
                | ErrorReason::ReferralSelfUse
                | ErrorReason::ReferralAlreadyUsed
                | ErrorReason::ReferralPlanExcluded,
            ) => (ApplicationErrorCode::InvalidArgument, false),
            Some(ErrorReason::RateLimited) => (ApplicationErrorCode::Unavailable, true),
            Some(
                ErrorReason::EmailTaken
                | ErrorReason::OriginIdTaken
                | ErrorReason::OriginIdAlreadyChanged
                | ErrorReason::RegistrationIncomplete
                | ErrorReason::DeviceLimit
                | ErrorReason::OnlineDeviceLimit
                | ErrorReason::TaskStateConflict
                | ErrorReason::TaskDeviceMismatch
                | ErrorReason::NotifyChannelLimit
                | ErrorReason::PlanAlreadyOwned
                | ErrorReason::PlanNotUpgrade
                | ErrorReason::PlanTierNotHigher
                | ErrorReason::SubscriptionConflict
                | ErrorReason::ReferralCodeTaken,
            ) => (ApplicationErrorCode::Conflict, false),
            Some(
                ErrorReason::RegistrationClosed
                | ErrorReason::OriginIdChangeNotAllowed
                | ErrorReason::NotifyDisabled
                | ErrorReason::PaymentDisabled
                | ErrorReason::PlanNotPurchasable,
            ) => (ApplicationErrorCode::Unsupported, false),
            Some(ErrorReason::MailNotConfigured) => (ApplicationErrorCode::Unavailable, false),
            Some(
                ErrorReason::CloudUnreachable
                | ErrorReason::TargetDeviceOffline
                | ErrorReason::PaymentGatewayFailed,
            ) => (ApplicationErrorCode::Unavailable, true),
            _ => {
                let code = match (self.status, self.code.as_deref()) {
                    (Some(404), _) => ApplicationErrorCode::NotFound,
                    (Some(400 | 422), _) | (_, Some("invalidArgument" | "validation_error")) => {
                        ApplicationErrorCode::InvalidArgument
                    }
                    (_, Some("unsupported")) => ApplicationErrorCode::Unsupported,
                    _ if self.retryable => ApplicationErrorCode::Unavailable,
                    _ => ApplicationErrorCode::Internal,
                };
                (code, self.retryable)
            }
        };
        let data = RpcErrorData::new(code, retryable);
        match reason {
            Some(reason) => data.with_reason(reason),
            None => data,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use serde_json::{Value, json};
    use tokio::sync::Mutex;

    use fluxdown_protocol::ErrorReason;

    use super::{CloudClient, CloudError, CloudSocket, handshake_means_unsupported};
    use crate::state::{AgentState, CloudCredentials, StateStore};

    #[derive(Clone)]
    struct MockState {
        refreshes: Arc<AtomicUsize>,
    }

    async fn protected(headers: HeaderMap) -> Response {
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some("Bearer new-access")
        {
            axum::Json(json!({ "ok": true })).into_response()
        } else {
            StatusCode::UNAUTHORIZED.into_response()
        }
    }

    async fn refresh(State(state): State<MockState>) -> Response {
        state.refreshes.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({
            "accessToken": "new-access",
            "refreshToken": "new-refresh",
            "expiresIn": 3600,
            "user": {
                "id": "u1",
                "email": "user@example.com",
                "nickname": "User",
                "plan": "free",
                "status": "active",
                "createdAt": ""
            },
            "entitlements": {},
            "device": {
                "id": "row1",
                "deviceId": "device1",
                "name": "Desktop",
                "createdAt": "",
                "lastSeenAt": ""
            }
        }))
        .into_response()
    }

    async fn reject_refresh() -> StatusCode {
        StatusCode::UNAUTHORIZED
    }

    #[tokio::test]
    async fn concurrent_unauthorized_requests_share_one_refresh_and_replay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock cloud");
        let address = listener.local_addr().expect("mock address");
        let refreshes = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/api/v1/test", get(protected))
            .route("/api/v1/auth/refresh", post(refresh))
            .with_state(MockState {
                refreshes: refreshes.clone(),
            });
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve refresh mock");
        });

        let dir = std::env::temp_dir().join(format!(
            "fluxdown_cloud_test_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let state = Arc::new(Mutex::new(AgentState {
            device_id: "device1".to_owned(),
            device_name: "Desktop".to_owned(),
            platform: "linux".to_owned(),
            credentials: Some(CloudCredentials {
                access_token: "old-access".to_owned(),
                refresh_token: "refresh".to_owned(),
                expires_at_unix: 0,
                session: None,
            }),
            ..AgentState::default()
        }));
        {
            let state = state.lock().await;
            store.save(&state).await.expect("save state");
        }
        let client =
            CloudClient::new(format!("http://{address}"), state, store).expect("cloud client");
        let first =
            client.authenticated::<Value, Value>(reqwest::Method::GET, "/api/v1/test", None);
        let second =
            client.authenticated::<Value, Value>(reqwest::Method::GET, "/api/v1/test", None);
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.expect("first replay")["ok"], true);
        assert_eq!(second.expect("second replay")["ok"], true);
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        drop(client);
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), error = %error, "remove refresh test directory");
        }
    }

    #[tokio::test]
    async fn revoked_refresh_clears_credentials_in_memory_and_on_disk() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind revoked cloud mock");
        let address = listener.local_addr().expect("revoked cloud address");
        let app = Router::new()
            .route("/api/v1/test", get(protected))
            .route("/api/v1/auth/refresh", post(reject_refresh));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve revoked refresh mock");
        });

        let dir = std::env::temp_dir().join(format!(
            "fluxdown_cloud_revoked_test_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(
            StateStore::open(dir.clone())
                .await
                .expect("revoked state store"),
        );
        let state = Arc::new(Mutex::new(AgentState {
            device_id: "device1".to_owned(),
            credentials: Some(CloudCredentials {
                access_token: "revoked-access".to_owned(),
                refresh_token: "revoked-refresh".to_owned(),
                expires_at_unix: 0,
                session: None,
            }),
            ..AgentState::default()
        }));
        {
            let state = state.lock().await;
            store.save(&state).await.expect("save revoked state");
        }
        let client = CloudClient::new(format!("http://{address}"), state.clone(), store.clone())
            .expect("revoked cloud client");

        let error = client
            .authenticated::<Value, Value>(reqwest::Method::GET, "/api/v1/test", None)
            .await
            .expect_err("revoked refresh must fail");
        assert_eq!(error.status, Some(StatusCode::UNAUTHORIZED.as_u16()));
        assert!(state.lock().await.credentials.is_none());
        assert!(
            store
                .load()
                .await
                .expect("reload revoked state")
                .credentials
                .is_none()
        );
        drop(client);
        drop(state);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove revoked refresh test directory");
        }
    }

    async fn whoami(State(name): State<&'static str>) -> Response {
        axum::Json(json!({ "server": name })).into_response()
    }

    async fn spawn_named(name: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind named mock");
        let address = listener.local_addr().expect("named mock address");
        let app = Router::new()
            .route("/api/v1/whoami", get(whoami))
            .with_state(name);
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve named cloud mock");
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn endpoint_override_persists_routes_and_resets() {
        let default_url = spawn_named("default").await;
        let override_url = spawn_named("override").await;
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_cloud_endpoint_test_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let state = Arc::new(Mutex::new(AgentState::default()));
        let client = CloudClient::new(default_url.clone(), state.clone(), store.clone())
            .expect("cloud client");

        let endpoint = client.endpoint();
        assert!(endpoint.editable, "tests run under debug_assertions");
        assert_eq!(endpoint.base_url, default_url);
        assert_eq!(endpoint.default_base_url, default_url);

        let set = client
            .set_endpoint(&format!("{override_url}/"))
            .await
            .expect("override accepted");
        assert_eq!(set.base_url, override_url);
        assert_eq!(set.default_base_url, default_url);
        let persisted = store.load().await.expect("reload state");
        assert_eq!(
            persisted.cloud_base_url_override.as_deref(),
            Some(override_url.as_str())
        );
        let who = client
            .public::<Value, Value>(reqwest::Method::GET, "/api/v1/whoami", None)
            .await
            .expect("override reachable");
        assert_eq!(who["server"], "override");

        let restored = CloudClient::new(default_url.clone(), state.clone(), store.clone())
            .expect("second client");
        restored.restore_endpoint_override().await;
        assert_eq!(restored.endpoint().base_url, override_url);

        let rejected = client
            .set_endpoint("http://example.com")
            .await
            .expect_err("non-loopback http must be rejected");
        assert_eq!(rejected.code.as_deref(), Some("invalidArgument"));
        assert_eq!(client.endpoint().base_url, override_url);

        let reset = client.set_endpoint("  ").await.expect("reset accepted");
        assert_eq!(reset.base_url, default_url);
        assert!(
            store
                .load()
                .await
                .expect("reload after reset")
                .cloud_base_url_override
                .is_none()
        );
        let who = client
            .public::<Value, Value>(reqwest::Method::GET, "/api/v1/whoami", None)
            .await
            .expect("default reachable");
        assert_eq!(who["server"], "default");

        drop(client);
        drop(restored);
        drop(state);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove endpoint test directory");
        }
    }

    /// 服务端一次性轮换：refresh token 用过即作废，复用返回 401。
    #[derive(Clone, Default)]
    struct RotatingCloud {
        inner: Arc<Mutex<(String, String, usize)>>,
    }

    fn bearer(headers: &HeaderMap) -> String {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default()
            .to_owned()
    }

    async fn rotating_protected(
        State(cloud): State<RotatingCloud>,
        headers: HeaderMap,
    ) -> Response {
        assert_eq!(
            headers.get(header::USER_AGENT).expect("product user agent"),
            format!("FluxDown/{}", fluxdown_protocol::APP_VERSION).as_str()
        );
        if bearer(&headers) == cloud.inner.lock().await.0 {
            axum::Json(json!({ "ok": true })).into_response()
        } else {
            StatusCode::UNAUTHORIZED.into_response()
        }
    }

    async fn rotating_refresh(
        State(cloud): State<RotatingCloud>,
        axum::Json(body): axum::Json<Value>,
    ) -> Response {
        // 模拟真实往返延迟，放大并发刷新窗口。
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mut inner = cloud.inner.lock().await;
        if body["refreshToken"].as_str() != Some(inner.1.as_str()) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        inner.2 += 1;
        inner.0 = format!("access-{}", inner.2);
        inner.1 = format!("refresh-{}", inner.2);
        axum::Json(json!({
            "accessToken": inner.0,
            "refreshToken": inner.1,
            "expiresIn": 900,
            "user": { "id": "u1", "email": "user@example.com" },
            "device": { "id": "row1", "deviceId": "device1" }
        }))
        .into_response()
    }

    /// 重启后 access 已过期：普通请求与 SSE 重连同时 401，必须只刷新一次且保住会话
    /// （曾因 SSE 绕过单飞锁，第二次刷新被拒后清空了刚轮换到手的新凭证）。
    #[tokio::test]
    async fn concurrent_request_and_stream_refresh_once_and_keep_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind rotating cloud");
        let address = listener.local_addr().expect("rotating address");
        let cloud = RotatingCloud::default();
        *cloud.inner.lock().await = ("access-0".to_owned(), "refresh-0".to_owned(), 0);
        let app = Router::new()
            .route("/api/v1/test", get(rotating_protected))
            .route("/api/v1/stream", get(rotating_protected))
            .route("/api/v1/auth/refresh", post(rotating_refresh))
            .with_state(cloud.clone());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve rotating cloud mock");
        });

        let dir = std::env::temp_dir().join(format!(
            "fluxdown_cloud_rotation_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let session = serde_json::from_value(json!({
            "user": { "id": "u1", "email": "user@example.com" },
            "device": { "id": "row1", "deviceId": "device1" }
        }))
        .expect("session dto");
        let state = Arc::new(Mutex::new(AgentState {
            device_id: "device1".to_owned(),
            credentials: Some(CloudCredentials {
                access_token: "expired-access".to_owned(),
                refresh_token: "refresh-0".to_owned(),
                expires_at_unix: 0,
                session: Some(session),
            }),
            ..AgentState::default()
        }));
        let client = CloudClient::new(format!("http://{address}"), state.clone(), store.clone())
            .expect("cloud client");

        let request =
            client.authenticated::<Value, Value>(reqwest::Method::GET, "/api/v1/test", None);
        let stream = client.authenticated_stream_epoch("/api/v1/stream", client.request_epoch());
        let (request, stream) = tokio::join!(request, stream);
        assert_eq!(request.expect("request replay")["ok"], true);
        assert!(stream.expect("stream replay").status().is_success());
        assert_eq!(cloud.inner.lock().await.2, 1, "exactly one rotation");
        let persisted = store
            .load()
            .await
            .expect("reload state")
            .credentials
            .expect("session survives");
        assert_eq!(persisted.refresh_token, "refresh-1");

        drop(client);
        drop(state);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove rotation test directory");
        }
    }

    // ───────────────────────── 错误 reason 映射 ─────────────────────────

    fn cloud_error(status: u16, code: &str) -> super::CloudError {
        super::error_from_body(
            status,
            status >= 500 || status == 429,
            json!({ "code": code, "message": "m" })
                .to_string()
                .as_bytes(),
        )
    }

    #[test]
    fn cloud_error_codes_map_to_reasons_and_application_codes() {
        use fluxdown_protocol::{ApplicationErrorCode as Code, ErrorReason as Reason};
        let table = [
            (
                401,
                "invalid_credentials",
                Reason::InvalidCredentials,
                Code::Unauthorized,
            ),
            (
                400,
                "invalid_code",
                Reason::InvalidVerificationCode,
                Code::InvalidArgument,
            ),
            (429, "rate_limited", Reason::RateLimited, Code::Unavailable),
            (409, "email_taken", Reason::EmailTaken, Code::Conflict),
            (
                403,
                "account_disabled",
                Reason::AccountDisabled,
                Code::Unauthorized,
            ),
            (
                403,
                "registration_closed",
                Reason::RegistrationClosed,
                Code::Unsupported,
            ),
            (
                403,
                "registration_incomplete",
                Reason::RegistrationIncomplete,
                Code::Conflict,
            ),
            (
                503,
                "mail_not_configured",
                Reason::MailNotConfigured,
                Code::Unavailable,
            ),
            (403, "device_limit", Reason::DeviceLimit, Code::Conflict),
            (
                403,
                "online_device_limit",
                Reason::OnlineDeviceLimit,
                Code::Conflict,
            ),
            (
                403,
                "sync_device_untrusted",
                Reason::DeviceUntrusted,
                Code::Unauthorized,
            ),
            (
                401,
                "unauthorized",
                Reason::SessionExpired,
                Code::Unauthorized,
            ),
            (
                409,
                "target_device_offline",
                Reason::TargetDeviceOffline,
                Code::Unavailable,
            ),
            (
                409,
                "task_state_conflict",
                Reason::TaskStateConflict,
                Code::Conflict,
            ),
            (
                403,
                "task_device_mismatch",
                Reason::TaskDeviceMismatch,
                Code::Conflict,
            ),
            (
                400,
                "wrong_password",
                Reason::WrongPassword,
                Code::InvalidArgument,
            ),
        ];
        for (status, code, reason, application) in table {
            let data = cloud_error(status, code).to_rpc_error();
            assert_eq!(data.reason, Some(reason), "{code}");
            assert_eq!(data.code, application, "{code}");
        }
        // 403 不再一律折叠成 Unauthorized：未识别的 403 保持 Internal，没有伪造的 reason。
        let unknown = cloud_error(403, "something_new").to_rpc_error();
        assert_eq!(unknown.reason, None);
        assert_eq!(unknown.code, Code::Internal);
        // 无 code 的 401（旧云端 / 网关）仍是登录过期。
        let bare = super::error_from_body(401, false, b"");
        assert_eq!(bare.to_rpc_error().reason, Some(Reason::SessionExpired));
        // 404 / 校验错误保持按状态码回退。
        assert_eq!(
            cloud_error(404, "not_found").to_rpc_error().code,
            Code::NotFound
        );
        assert_eq!(
            cloud_error(422, "validation_error").to_rpc_error().code,
            Code::InvalidArgument
        );
    }

    #[test]
    fn origin_id_rejections_preserve_actionable_reasons_without_retrying() {
        for (status, cloud_code, code, reason) in [
            (409, "origin_id_taken", "conflict", "originIdTaken"),
            (
                403,
                "origin_id_change_not_allowed",
                "unsupported",
                "originIdChangeNotAllowed",
            ),
            (
                409,
                "origin_id_already_changed",
                "conflict",
                "originIdAlreadyChanged",
            ),
        ] {
            let data = cloud_error(status, cloud_code).to_rpc_error();
            assert_eq!(
                serde_json::to_value(data).expect("serialize RPC rejection"),
                json!({ "code": code, "retryable": false, "reason": reason }),
                "{cloud_code}",
            );
        }
    }

    #[test]
    fn rate_limited_and_unreachable_are_retryable_but_credential_errors_are_not() {
        assert!(cloud_error(429, "rate_limited").to_rpc_error().retryable);
        assert!(
            !cloud_error(401, "invalid_credentials")
                .to_rpc_error()
                .retryable
        );
        let network = super::CloudError::network("dns failure".to_owned());
        let data = network.to_rpc_error();
        assert_eq!(
            data.reason,
            Some(fluxdown_protocol::ErrorReason::CloudUnreachable)
        );
        assert!(data.retryable);
    }

    #[test]
    fn non_json_error_bodies_keep_the_status_and_a_body_snippet() {
        let error = super::error_from_body(
            502,
            true,
            b"<html><body>Bad Gateway from nginx</body></html>",
        );
        assert_eq!(error.status, Some(502));
        assert_eq!(error.code, None);
        assert!(error.message.contains("502"));
        assert!(error.message.contains("Bad Gateway from nginx"));
        assert!(error.retryable);
        assert!(!error.unreachable);
        // 空正文只保留状态码。
        assert_eq!(
            super::error_from_body(500, true, b"").message,
            "FluxCloud HTTP 500"
        );
    }

    async fn temp_client(
        label: &str,
        base_url: String,
        credentials: Option<CloudCredentials>,
    ) -> (
        CloudClient,
        Arc<Mutex<AgentState>>,
        Arc<StateStore>,
        std::path::PathBuf,
    ) {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_cloud_{label}_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let session = credentials
            .as_ref()
            .and_then(|credentials| credentials.session.clone());
        let state = Arc::new(Mutex::new(AgentState {
            device_id: "device1".to_owned(),
            account_uid: session.map(|session| session.user.id),
            credentials,
            ..AgentState::default()
        }));
        let client =
            CloudClient::new(base_url, state.clone(), store.clone()).expect("cloud client");
        (client, state, store, dir)
    }

    #[tokio::test]
    async fn unreachable_cloud_is_a_retryable_network_error_with_a_reason() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        drop(listener);
        let (client, _state, store, dir) =
            temp_client("unreachable", format!("http://{address}"), None).await;
        let error = client
            .public::<Value, Value>(reqwest::Method::GET, "/api/v1/plans/catalog", None)
            .await
            .expect_err("closed port");
        assert!(error.unreachable);
        assert_eq!(
            error.to_rpc_error().reason,
            Some(fluxdown_protocol::ErrorReason::CloudUnreachable)
        );
        drop(client);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove unreachable cloud test directory");
        }
    }

    #[tokio::test]
    async fn logout_preserves_server_failure_when_local_persistence_also_fails() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind closed cloud");
        let address = listener.local_addr().expect("closed cloud address");
        drop(listener);
        let (client, state, store, dir) = temp_client(
            "logout_both_fail",
            format!("http://{address}"),
            Some(credentials_of("u1", "refresh")),
        )
        .await;
        tokio::fs::create_dir(dir.join("agent-state.json"))
            .await
            .expect("block state persistence");
        let error = client
            .logout()
            .await
            .expect_err("remote and local logout must fail");
        assert!(
            error.unreachable,
            "server network error must remain the primary failure"
        );
        assert!(
            state.lock().await.credentials.is_none(),
            "local in-memory session must still be cleared"
        );
        assert!(
            dir.join("agent-state.json").is_dir(),
            "failed save must not replace its destination"
        );
        drop(client);
        drop(state);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "could not remove logout failure test directory");
        }
    }

    fn session_of(user_id: &str) -> fluxdown_protocol::AgentSessionDto {
        serde_json::from_value(json!({
            "user": { "id": user_id, "email": "user@example.com" },
            "device": { "id": "row1", "deviceId": "device1" }
        }))
        .expect("session dto")
    }

    fn credentials_of(user_id: &str, refresh: &str) -> CloudCredentials {
        CloudCredentials {
            access_token: "old-access".to_owned(),
            refresh_token: refresh.to_owned(),
            expires_at_unix: 0,
            session: Some(session_of(user_id)),
        }
    }

    async fn slow_refresh() -> Response {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        axum::Json(json!({
            "accessToken": "new-access",
            "refreshToken": "new-refresh",
            "expiresIn": 3600,
            "user": { "id": "u1", "email": "user@example.com" },
            "device": { "id": "row1", "deviceId": "device1" }
        }))
        .into_response()
    }

    /// 登出与进行中的刷新互斥：刷新的结果不能在登出之后把会话「复活」。
    #[tokio::test]
    async fn a_refresh_finishing_after_logout_cannot_resurrect_the_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock cloud");
        let address = listener.local_addr().expect("address");
        let refresh_started = Arc::new(tokio::sync::Notify::new());
        let app = Router::new()
            .route("/api/v1/test", get(protected))
            .route(
                "/api/v1/auth/refresh",
                post({
                    let refresh_started = refresh_started.clone();
                    move || {
                        let refresh_started = refresh_started.clone();
                        async move {
                            refresh_started.notify_one();
                            slow_refresh().await
                        }
                    }
                }),
            )
            .route(
                "/api/v1/auth/logout",
                post(|| async { StatusCode::NO_CONTENT }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve logout race mock");
        });
        let (client, state, store, dir) = temp_client(
            "logout_race",
            format!("http://{address}"),
            Some(credentials_of("u1", "refresh")),
        )
        .await;
        let request = {
            let client = client.clone();
            tokio::spawn(async move {
                client
                    .authenticated::<Value, Value>(reqwest::Method::GET, "/api/v1/test", None)
                    .await
            })
        };
        // The server observes refresh only after the request holds the refresh lock.
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            refresh_started.notified(),
        )
        .await
        .expect("request enters refresh before logout");
        client.logout().await.expect("logout after refresh");
        assert_eq!(
            request
                .await
                .expect("join request")
                .expect("replayed request")["ok"],
            true
        );
        assert!(state.lock().await.credentials.is_none(), "logout wins");
        assert!(store.load().await.expect("reload").credentials.is_none());
        drop(client);
        drop(state);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove logout race test directory");
        }
    }

    /// 登出 / 撤销清理账号维度状态：同步数据按账号暂存、远程任务与绑定丢弃，并推送清空事件。
    #[tokio::test]
    async fn clearing_the_session_isolates_account_state_and_projects_empty_lists() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock cloud");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            axum::serve(listener, Router::new())
                .await
                .expect("serve session clear mock");
        });
        let (client, state, store, dir) = temp_client(
            "clear_scope",
            format!("http://{address}"),
            Some(credentials_of("u1", "refresh")),
        )
        .await;
        let events = crate::event_hub::AgentEventHub::new(fluxdown_protocol::AgentSnapshot {
            session: Some(session_of("u1")),
            remote_tasks: vec![serde_json::from_value(json!({"id": "r1"})).expect("task")],
            cloud_devices: vec![
                serde_json::from_value(json!({"id": "1", "deviceId": "d"})).expect("device"),
            ],
            ..fluxdown_protocol::AgentSnapshot::default()
        });
        let client = client.with_events(events.clone());
        {
            let mut state = state.lock().await;
            state.sync.revision = 33;
            state.sync_entries.insert(
                "general.locale".to_owned(),
                crate::state::PersistedSyncEntry {
                    value: json!("zh"),
                    version: 1,
                    dirty: true,
                    deleted: false,
                    edited_at_ms: None,
                },
            );
            state
                .remote_bindings
                .insert("r1".to_owned(), "t1".to_owned());
        }
        client.clear_session().await.expect("clear");
        {
            let state = state.lock().await;
            assert!(state.credentials.is_none());
            assert_eq!(state.sync.revision, 0);
            assert!(state.sync_entries.is_empty());
            assert!(state.sync.dirty_keys.is_empty());
            assert!(state.remote_bindings.is_empty());
            assert_eq!(state.sync_stash["u1"].revision, 33);
        }
        let fluxdown_protocol::SnapshotBody::Agent(snapshot) = events.snapshot().body else {
            panic!("agent snapshot expected");
        };
        assert!(snapshot.session.is_none());
        assert!(snapshot.remote_tasks.is_empty());
        assert!(snapshot.cloud_devices.is_empty());
        drop(client);
        drop(state);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove session clear test directory");
        }
    }

    fn recorded_session_events(
        receiver: &mut tokio::sync::broadcast::Receiver<fluxdown_protocol::EventFrame>,
    ) -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            let frame = match receiver.try_recv() {
                Ok(frame) => frame,
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    tracing::debug!("session event receiver closed after test publisher shutdown");
                    break;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                    panic!("session event assertion missed {skipped} events");
                }
            };
            if let fluxdown_protocol::ServiceEvent::Agent(event) = frame.event {
                match event {
                    fluxdown_protocol::AgentEvent::SessionRevoked(reason) => {
                        seen.push(format!("revoked:{reason:?}"));
                    }
                    fluxdown_protocol::AgentEvent::SessionChanged(session) => {
                        seen.push(format!("session:{}", session.is_some()));
                    }
                    _ => {}
                }
            }
        }
        seen
    }

    async fn forbidden_refresh() -> Response {
        (
            StatusCode::FORBIDDEN,
            axum::Json(json!({"code": "account_disabled", "message": "disabled"})),
        )
            .into_response()
    }

    /// 用户主动登出不发 `SessionRevoked`（即使登出途中 access 过期、刷新令牌又被拒）；
    /// 刷新令牌被拒导致的清会话才是「被动」结束：401 → sessionExpired，403 account_disabled → accountDisabled。
    #[tokio::test]
    async fn only_involuntary_session_endings_publish_session_revoked() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock cloud");
        let address = listener.local_addr().expect("address");
        let app = Router::new()
            .route("/api/v1/test", get(protected))
            .route(
                "/api/v1/auth/logout",
                post(|| async { StatusCode::UNAUTHORIZED }),
            )
            .route("/api/v1/auth/refresh", post(reject_refresh));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve session revocation mock");
        });

        // 用户登出：access 过期 → 刷新被拒，仍然不是「被撤销」。
        let (client, _state, store, dir) = temp_client(
            "logout_silent",
            format!("http://{address}"),
            Some(credentials_of("u1", "refresh")),
        )
        .await;
        let events =
            crate::event_hub::AgentEventHub::new(fluxdown_protocol::AgentSnapshot::default());
        let client = client.with_events(events.clone());
        let (mut receiver, _) = events.subscribe_and_snapshot();
        client
            .logout()
            .await
            .expect("logout with rejected refresh clears session");
        assert_eq!(recorded_session_events(&mut receiver), ["session:false"]);

        // 普通请求遇到刷新令牌被拒（401）→ sessionExpired，且先于 SessionChanged(None)。
        let (client, _state2, store2, dir2) = temp_client(
            "refresh_rejected",
            format!("http://{address}"),
            Some(credentials_of("u1", "refresh")),
        )
        .await;
        let events =
            crate::event_hub::AgentEventHub::new(fluxdown_protocol::AgentSnapshot::default());
        let client = client.with_events(events.clone());
        let (mut receiver, _) = events.subscribe_and_snapshot();
        let error = client
            .authenticated::<Value, Value>(reqwest::Method::GET, "/api/v1/test", None)
            .await
            .expect_err("rejected refresh");
        assert_eq!(error.status, Some(401));
        assert_eq!(
            recorded_session_events(&mut receiver),
            ["revoked:SessionExpired", "session:false"]
        );

        // 403 account_disabled → accountDisabled。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind disabled mock");
        let disabled_address = listener.local_addr().expect("address");
        let app = Router::new()
            .route("/api/v1/test", get(protected))
            .route("/api/v1/auth/refresh", post(forbidden_refresh));
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve disabled account mock");
        });
        let (client, _state3, store3, dir3) = temp_client(
            "account_disabled",
            format!("http://{disabled_address}"),
            Some(credentials_of("u1", "refresh")),
        )
        .await;
        let events =
            crate::event_hub::AgentEventHub::new(fluxdown_protocol::AgentSnapshot::default());
        let client = client.with_events(events.clone());
        let (mut receiver, _) = events.subscribe_and_snapshot();
        let error = client
            .authenticated::<Value, Value>(reqwest::Method::GET, "/api/v1/test", None)
            .await
            .expect_err("disabled account");
        assert_eq!(error.code.as_deref(), Some("account_disabled"));
        assert_eq!(
            recorded_session_events(&mut receiver),
            ["revoked:AccountDisabled", "session:false"]
        );

        // 用户主动 clear_session（删除本设备）同样不发。
        let (client, _state4, store4, dir4) = temp_client(
            "explicit_clear",
            format!("http://{address}"),
            Some(credentials_of("u1", "refresh")),
        )
        .await;
        let events =
            crate::event_hub::AgentEventHub::new(fluxdown_protocol::AgentSnapshot::default());
        let client = client.with_events(events.clone());
        let (mut receiver, _) = events.subscribe_and_snapshot();
        client.clear_session().await.expect("explicit clear");
        assert_eq!(recorded_session_events(&mut receiver), ["session:false"]);

        drop(store);
        drop(store2);
        drop(store3);
        drop(store4);
        for dir in [dir, dir2, dir3, dir4] {
            if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!(path = %dir.display(), error = %error, "remove session event test directory");
            }
        }
    }

    #[test]
    fn notify_error_codes_map_to_stable_reasons_and_rpc_codes() {
        use fluxdown_protocol::{ApplicationErrorCode, ErrorReason};

        use super::CloudError;
        let error = |status: u16, code: &str| CloudError {
            status: Some(status),
            code: Some(code.to_owned()),
            message: String::new(),
            retryable: status == 429,
            unreachable: false,
        };
        let cases = [
            (
                error(403, "notify_channel_limit"),
                ErrorReason::NotifyChannelLimit,
                ApplicationErrorCode::Conflict,
            ),
            (
                error(403, "notify_disabled"),
                ErrorReason::NotifyDisabled,
                ApplicationErrorCode::Unsupported,
            ),
            (
                error(400, "notify_email_unverified"),
                ErrorReason::NotifyTargetInvalid,
                ApplicationErrorCode::InvalidArgument,
            ),
            (
                error(400, "invalid_code"),
                ErrorReason::InvalidVerificationCode,
                ApplicationErrorCode::InvalidArgument,
            ),
            (
                error(422, "notify_target_invalid"),
                ErrorReason::NotifyTargetInvalid,
                ApplicationErrorCode::InvalidArgument,
            ),
            (
                error(422, "notify_kind_unavailable"),
                ErrorReason::NotifyTargetInvalid,
                ApplicationErrorCode::InvalidArgument,
            ),
            (
                error(429, "rate_limited"),
                ErrorReason::RateLimited,
                ApplicationErrorCode::Unavailable,
            ),
        ];
        for (error, reason, code) in cases {
            assert_eq!(error.reason(), Some(reason), "{:?}", error.code);
            assert_eq!(error.to_rpc_error().code, code, "{:?}", error.code);
        }
        let unreachable = CloudError::network("dns".to_owned());
        assert_eq!(unreachable.reason(), Some(ErrorReason::CloudUnreachable));
    }

    #[test]
    fn billing_error_codes_map_to_stable_reasons_and_rpc_codes() {
        use fluxdown_protocol::{ApplicationErrorCode, ErrorReason};

        use super::CloudError;
        let error = |status: u16, code: &str| CloudError {
            status: Some(status),
            code: Some(code.to_owned()),
            message: String::new(),
            retryable: false,
            unreachable: false,
        };
        let cases = [
            (
                503,
                "payment_disabled",
                ErrorReason::PaymentDisabled,
                ApplicationErrorCode::Unsupported,
            ),
            (
                502,
                "gateway_error",
                ErrorReason::PaymentGatewayFailed,
                ApplicationErrorCode::Unavailable,
            ),
            (
                422,
                "plan_not_purchasable",
                ErrorReason::PlanNotPurchasable,
                ApplicationErrorCode::Unsupported,
            ),
            (
                409,
                "plan_already_owned",
                ErrorReason::PlanAlreadyOwned,
                ApplicationErrorCode::Conflict,
            ),
            (
                422,
                "not_an_upgrade",
                ErrorReason::PlanNotUpgrade,
                ApplicationErrorCode::Conflict,
            ),
            (
                422,
                "plan_tier_not_higher",
                ErrorReason::PlanTierNotHigher,
                ApplicationErrorCode::Conflict,
            ),
            (
                409,
                "subscription_conflict",
                ErrorReason::SubscriptionConflict,
                ApplicationErrorCode::Conflict,
            ),
            (
                400,
                "referral_code_invalid",
                ErrorReason::ReferralCodeInvalid,
                ApplicationErrorCode::InvalidArgument,
            ),
            (
                400,
                "referral_self_use",
                ErrorReason::ReferralSelfUse,
                ApplicationErrorCode::InvalidArgument,
            ),
            (
                400,
                "referral_already_used",
                ErrorReason::ReferralAlreadyUsed,
                ApplicationErrorCode::InvalidArgument,
            ),
            (
                400,
                "referral_plan_excluded",
                ErrorReason::ReferralPlanExcluded,
                ApplicationErrorCode::InvalidArgument,
            ),
            (
                400,
                "referral_code_taken",
                ErrorReason::ReferralCodeTaken,
                ApplicationErrorCode::Conflict,
            ),
        ];
        for (status, code, reason, rpc_code) in cases {
            let error = error(status, code);
            assert_eq!(error.reason(), Some(reason), "{code}");
            let data = error.to_rpc_error();
            assert_eq!(data.code, rpc_code, "{code}");
            assert_eq!(data.reason, Some(reason), "{code}");
        }
        // 网关失败可重试：客户端据此提示「稍后重试」而不是终止。
        assert!(error(502, "gateway_error").to_rpc_error().retryable);
    }

    #[test]
    fn http_date_parses_to_unix_milliseconds() {
        assert_eq!(
            super::parse_http_date_ms("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777_000)
        );
        assert_eq!(
            super::parse_http_date_ms("Fri, 01 Jan 2100 00:00:00 GMT"),
            Some(4_102_444_800_000)
        );
        assert_eq!(
            super::parse_http_date_ms("Thu, 01 Jan 1970 00:00:00 GMT"),
            Some(0)
        );
        assert_eq!(
            super::parse_http_date_ms("Sunday, 06-Nov-94 08:49:37 GMT"),
            None
        );
        assert_eq!(super::parse_http_date_ms("garbage"), None);
    }

    #[tokio::test]
    async fn response_date_header_calibrates_the_server_clock_estimate() {
        async fn dated() -> Response {
            (
                [(header::DATE, "Fri, 01 Jan 2100 00:00:00 GMT")],
                axum::Json(json!({ "ok": true })),
            )
                .into_response()
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock cloud");
        let address = listener.local_addr().expect("mock address");
        let app = Router::new().route("/api/v1/dated", get(dated));
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve dated mock");
        });
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_cloud_clock_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let state = Arc::new(Mutex::new(AgentState::default()));
        let client =
            CloudClient::new(format!("http://{address}"), state, store).expect("cloud client");
        let before = client.server_now_ms();
        assert!(before < 4_000_000_000_000, "no sample yet: raw local time");
        let _: Value = client
            .public::<Value, Value>(reqwest::Method::GET, "/api/v1/dated", None)
            .await
            .expect("dated response");
        // 样本取该秒中点；允许往返与调度抖动。
        let drift = (client.server_now_ms() - 4_102_444_800_500).abs();
        assert!(drift < 2_000, "estimate drifted by {drift}ms");
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "could not remove clock test directory");
        }
    }

    #[test]
    fn websocket_handshake_classification_only_trusts_the_response() {
        for status in [404_u16, 405, 426, 501] {
            let status = StatusCode::from_u16(status).expect("status");
            assert!(handshake_means_unsupported(status, None), "{status}");
        }
        // 400：只有正文不是 FluxCloud `{code}` JSON 才算中间层问题。
        assert!(handshake_means_unsupported(StatusCode::BAD_REQUEST, None));
        assert!(!handshake_means_unsupported(
            StatusCode::BAD_REQUEST,
            Some("invalid_argument")
        ));
        assert!(handshake_means_unsupported(StatusCode::FORBIDDEN, None));
        assert!(!handshake_means_unsupported(
            StatusCode::FORBIDDEN,
            Some("online_device_limit")
        ));
        for status in [429_u16, 500, 502, 503] {
            let status = StatusCode::from_u16(status).expect("status");
            assert!(!handshake_means_unsupported(status, None), "{status}");
        }
    }

    /// 对任何连接读完请求头后原样回写 `response` 的最小 HTTP/1.1 服务器。
    async fn spawn_raw_http(response: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind raw mock");
        let address = listener.local_addr().expect("raw mock address");
        let response: Arc<str> = Arc::from(response);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let response = response.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&chunk[..read]),
                        }
                    }
                    if let Err(error) = stream.write_all(response.as_bytes()).await {
                        tracing::debug!(%error, "raw mock write failed");
                    }
                    // 保持连接直到客户端关闭，避免过早 EOF 干扰断言。
                    let closed = stream.read(&mut chunk).await;
                    tracing::debug!(?closed, "raw mock connection closed");
                });
            }
        });
        format!("http://{address}")
    }

    async fn socket_attempt(response: &str) -> Result<CloudSocket, CloudError> {
        let base = spawn_raw_http(response).await;
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_cloud_ws_test_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let state = Arc::new(Mutex::new(AgentState {
            device_id: "device1".to_owned(),
            credentials: Some(CloudCredentials {
                access_token: "access".to_owned(),
                refresh_token: "refresh".to_owned(),
                expires_at_unix: i64::MAX,
                session: None,
            }),
            ..AgentState::default()
        }));
        let client = CloudClient::new(base, state, store).expect("cloud client");
        let result = client
            .authenticated_socket_epoch("/api/v1/tasks/ws?deviceId=device1", client.request_epoch())
            .await;
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "could not remove ws test directory");
        }
        result
    }

    #[tokio::test]
    async fn websocket_handshake_failures_are_classified_from_the_response() {
        // 中间层吞掉 Upgrade：200 / 3xx 以外的成功类。
        let swallowed = socket_attempt("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .expect_err("200 is not an upgrade");
        assert!(swallowed.is_websocket_unsupported());
        assert_eq!(swallowed.status, Some(200));
        // 101 但 Accept 不匹配。
        let bad_accept = socket_attempt(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: bogus\r\n\r\n",
        )
        .await
        .expect_err("mismatched accept key");
        assert!(bad_accept.is_websocket_unsupported());
        assert_eq!(bad_accept.status, Some(101));
        // 缺路由。
        let missing = socket_attempt("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
            .await
            .expect_err("404");
        assert!(missing.is_websocket_unsupported());
        assert_eq!(missing.status, Some(404));
        // 非 FluxCloud 的 400。
        let proxy_400 =
            socket_attempt("HTTP/1.1 400 Bad Request\r\nContent-Length: 11\r\n\r\nbad gateway")
                .await
                .expect_err("plain 400");
        assert!(proxy_400.is_websocket_unsupported());
        // 非 FluxCloud 的 403（WAF / 中间层）同样视为不支持。
        let proxy_403 =
            socket_attempt("HTTP/1.1 403 Forbidden\r\nContent-Length: 9\r\n\r\nforbidden")
                .await
                .expect_err("plain 403");
        assert!(proxy_403.is_websocket_unsupported());
        // 3xx 不跟随（`redirect(none)`），按中间层吞掉 Upgrade 归为不支持。
        let redirect = socket_attempt(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/elsewhere\r\nContent-Length: 0\r\n\r\n",
        )
        .await
        .expect_err("302 is not an upgrade");
        assert!(redirect.is_websocket_unsupported());
        assert_eq!(redirect.status, Some(302));
        // FluxCloud 的业务错误不回退。
        let body = r#"{"code":"online_device_limit","message":"full"}"#;
        let limited = socket_attempt(&format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ))
        .await
        .expect_err("403");
        assert!(!limited.is_websocket_unsupported());
        assert_eq!(limited.code.as_deref(), Some("online_device_limit"));
        assert_eq!(limited.reason(), Some(ErrorReason::OnlineDeviceLimit));
        // 5xx 同样不回退。
        let gateway = socket_attempt("HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
            .await
            .expect_err("502");
        assert!(!gateway.is_websocket_unsupported());
        assert!(gateway.retryable);
    }
}
