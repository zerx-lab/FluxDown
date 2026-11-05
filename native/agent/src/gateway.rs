//! agent 官方 UI Gateway：单一 RPC 会话、agent 快照与 daemon 透明转发。

mod local;
mod restart;
mod session_auth;

pub use local::{LocalConnection, LocalEventError};
pub(crate) use session_auth::RpcCredential;
use session_auth::{
    CLOSE_CODE_CREDENTIAL_REVOKED, CLOSE_REASON_CREDENTIAL_REVOKED, SessionContext, wait_changed,
};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use axum::extract::{ConnectInfo, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use fluxdown_protocol::method;
use fluxdown_protocol::{
    ApplicationErrorCode, ClientHello, EventFrame, RpcErrorData, RpcErrorObject, RpcRequest,
    RpcResponse, ServiceHello, ServiceRole, validate_first_request,
};
use futures_util::StreamExt;
use reqwest::Method;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::api_host::AgentApiHost;
use crate::capture::{BlobError, BlobKind, CaptureError, CaptureService, DaemonBlobClient};
use crate::cloud::{CloudApi, CloudAuthService, CloudError};
use crate::cloud_notify::CloudNotifyService;
use crate::daemon_client::DaemonClient;
use crate::diagnostics::{DiagnosticsError, DiagnosticsService};
use crate::event_hub::AgentEventHub;
use crate::event_pump::{
    EventPump, MAX_PENDING_FRAMES, OUTBOX_CAPACITY, finish_writer, run_writer,
};
use crate::lifecycle::Lifecycle;
use crate::link::LinkService;
use crate::platform::PlatformError;
use crate::power::PowerService;
use crate::private_plugin::PrivatePluginService;
use crate::remote::{RemoteError, RemoteTaskService};
use crate::server_mode::ServerHandle;
use crate::shell::ShellState;
use crate::sync::SyncService;
use crate::update::{UpdateError, UpdateService};

/// daemon `/blobs/*` 请求体上限（与 `fluxdown_daemon::http::REQUEST_BODY_LIMIT` 一致）。
const BLOB_UPLOAD_LIMIT: u64 = 4 * 1024 * 1024;

/// 网关依赖的本机外壳服务：UI 在线计数 / 驻留、完成后关机、进程生命周期、系统通知。
pub struct GatewayShell {
    pub shell: Arc<ShellState>,
    pub power: Arc<PowerService>,
    pub lifecycle: Arc<Lifecycle>,
    pub notifier: Arc<crate::notification::Notifier>,
}

pub struct GatewayService {
    daemon: Arc<DaemonClient>,
    events: AgentEventHub,
    auth: Arc<CloudAuthService>,
    cloud: Arc<CloudApi>,
    sync: Arc<SyncService>,
    remote: Arc<RemoteTaskService>,
    notify: Arc<CloudNotifyService>,
    capture: Arc<CaptureService>,
    blobs: Arc<DaemonBlobClient>,
    private_plugins: PrivatePluginService,
    diagnostics: Arc<DiagnosticsService>,
    update: Arc<UpdateService>,
    state: Arc<tokio::sync::Mutex<crate::state::AgentState>>,
    store: Arc<crate::state::StateStore>,
    api_switches: Arc<fluxdown_api::server::ApiRuntimeSwitches>,
    api_token: fluxdown_api::auth::TokenCell,
    hello: ServiceHello,
    open_associations: crate::open_association::OpenAssociationGuard,
    local: GatewayShell,
    server_mode: bool,
    /// 嵌入宿主：`exportLogs` 的目标路径由同进程 App 给出（应用沙盒内），server 模式下仍放行。
    local_log_export: bool,
    link: Option<Arc<LinkService>>,
    /// 访问密钥被改时递增：以旧密钥认证的 `/rpc` 会话据此重新校验，失效即断开。
    access_key_epoch: tokio::sync::watch::Sender<u64>,
    gateway_patch_lock: tokio::sync::Mutex<()>,
    listener_control: restart::GatewayControl,
}

impl GatewayService {
    #[allow(
        clippy::too_many_arguments,
        reason = "gateway composition requires each state owner explicitly"
    )]
    #[must_use]
    pub fn new(
        daemon: Arc<DaemonClient>,
        events: AgentEventHub,
        auth: Arc<CloudAuthService>,
        cloud: Arc<CloudApi>,
        sync: Arc<SyncService>,
        remote: Arc<RemoteTaskService>,
        notify: Arc<CloudNotifyService>,
        capture: Arc<CaptureService>,
        blobs: Arc<DaemonBlobClient>,
        diagnostics: Arc<DiagnosticsService>,
        update: Arc<UpdateService>,
        state: Arc<tokio::sync::Mutex<crate::state::AgentState>>,
        store: Arc<crate::state::StateStore>,
        api_switches: Arc<fluxdown_api::server::ApiRuntimeSwitches>,
        api_token: fluxdown_api::auth::TokenCell,
        local: GatewayShell,
    ) -> Self {
        let open_associations = crate::open_association::OpenAssociationGuard::new(
            events.clone(),
            Arc::clone(&local.notifier),
        );
        let private_plugins =
            PrivatePluginService::new(Arc::clone(&cloud), Arc::clone(&daemon), Arc::clone(&blobs));
        Self {
            daemon,
            events,
            auth,
            cloud,
            sync,
            remote,
            notify,
            capture,
            blobs,
            private_plugins,
            diagnostics,
            update,
            state,
            store,
            api_switches,
            api_token,
            hello: crate::service_hello(
                Uuid::new_v4().to_string(),
                vec![
                    method::CAPABILITY_AGENT_GATEWAY.to_owned(),
                    method::CAPABILITY_AGENT_AUTH.to_owned(),
                    method::CAPABILITY_AGENT_SYNC.to_owned(),
                    method::CAPABILITY_AGENT_REMOTE_TASKS.to_owned(),
                    method::CAPABILITY_AGENT_BILLING.to_owned(),
                    method::CAPABILITY_AGENT_REFERRALS.to_owned(),
                    method::CAPABILITY_AGENT_EXTERNAL_CAPTURE.to_owned(),
                ],
            ),
            open_associations,
            local,
            server_mode: false,
            local_log_export: false,
            link: None,
            access_key_epoch: tokio::sync::watch::Sender::new(0),
            gateway_patch_lock: tokio::sync::Mutex::new(()),
            listener_control: restart::GatewayControl::new(),
        }
    }

    /// server 模式：桌面平台操作、宿主诊断修复与路径日志导出返回 Unsupported，令牌遵循访问密钥策略。
    #[must_use]
    pub fn with_server_mode(mut self, enabled: bool) -> Self {
        self.server_mode = enabled;
        self
    }

    /// 嵌入宿主（移动端进程内 agent）：只有同进程 App 能调用，放行写入 App 自选路径的日志导出。
    #[must_use]
    pub fn with_local_log_export(mut self) -> Self {
        self.local_log_export = true;
        self
    }

    /// 装配局域网直连服务（`agent.link.*`）。只有直连服务存在时才下发 `agent.deviceLink` 能力，
    /// 客户端据此显示 / 隐藏直连入口（嵌入式移动宿主不带直连服务，不会宣告它）。
    #[must_use]
    pub fn with_link(mut self, link: Arc<LinkService>) -> Self {
        self.link = Some(link);
        let capability = method::CAPABILITY_AGENT_DEVICE_LINK.to_owned();
        if !self.hello.capabilities.contains(&capability) {
            self.hello.capabilities.push(capability);
        }
        self
    }

    /// 构造 RPC 响应，并把连接级上下文（「本机来源」判定、会话凭据）透传给 [`Self::dispatch_with`]。
    async fn call_with(&self, session: &SessionContext, request: RpcRequest) -> RpcResponse {
        let id = request.id.clone();
        match self.dispatch_with(request, session).await {
            Ok(value) => RpcResponse::success(id, value),
            Err(data) => {
                RpcResponse::failure(id, RpcErrorObject::application("agent RPC failed", data))
            }
        }
    }

    /// agent 内部组件（托盘、剪贴板监听）复用与 UI 相同的 RPC 分发入口。
    pub async fn dispatch_local(
        &self,
        method_name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        self.dispatch(RpcRequest::new(
            fluxdown_protocol::RequestId::String("agent-local".to_owned()),
            method_name,
            Some(params),
        ))
        .await
    }

    /// agent 内部调用方（托盘、剪贴板监听、NMH）的入口：桌面宿主里它们就是本机，可用桌面集成；
    /// server 模式沿用「默认不放行」。
    async fn dispatch(&self, request: RpcRequest) -> Result<serde_json::Value, RpcErrorData> {
        self.dispatch_with(request, &SessionContext::local(!self.server_mode))
            .await
    }

    /// 与 [`Self::dispatch`] 相同，但带连接级上下文。
    ///
    /// `session.allow_local_platform` 是建连时对整条连接的一次判定：桌面宿主的 `agent.token` 与进程内
    /// 连接为 `true`；server 模式由 `ServerHandle::local_platform_permitted` 决定——TCP 对端、整条转发链与所有
    /// `Host` / `Origin` / `X-Forwarded-Host` / `Forwarded` authority 都是字面本机，或运维显式
    /// 设置 `FLUXDOWN_ALLOW_LOCAL_PLATFORM`。即使为 `true`，server 模式也只放行
    /// [`local_platform_method_allowed`] 中的打开 / 定位任务产物，其余 `agent.platform.*` 仍
    /// 返回 `Unsupported`。设备控制凭据会话（局域网配对的手机）恒为 `false`：桌面模式下同样拒绝
    /// 全部 `agent.platform.*`，远程设备不能触发主机本地的平台能力。
    async fn dispatch_with(
        &self,
        request: RpcRequest,
        session: &SessionContext,
    ) -> Result<serde_json::Value, RpcErrorData> {
        // 桌面专属集成（打开 / 定位文件、开机自启、文件与协议关联）在 headless 宿主不存在；
        // 唯一例外是本机来源连接的「打开 / 定位任务产物」。桌面宿主只拒绝非本机来源的连接
        // （设备控制凭据会话）。
        // `agent.sound.preview` 同属本机平台能力（在本机扬声器发声）：与 `agent.platform.*` 同一套准入。
        if request.method.starts_with("agent.platform.")
            || request.method == method::AGENT_SOUND_PREVIEW
        {
            let permitted = if self.server_mode {
                session.allow_local_platform && local_platform_method_allowed(&request.method)
            } else {
                session.allow_local_platform
            };
            if !permitted {
                return Err(RpcErrorData::new(ApplicationErrorCode::Unsupported, false));
            }
        }
        if self.server_mode && server_mode_denies(&request, self.local_log_export) {
            return Err(RpcErrorData::new(ApplicationErrorCode::Unsupported, false));
        }
        if internal_daemon_method(&request.method) {
            return Err(RpcErrorData::new(ApplicationErrorCode::Unsupported, false));
        }
        match request.method.as_str() {
            method::SYSTEM_PING => Ok(serde_json::json!({ "ok": true })),
            // 线上快照只带头；任务表与队列位置表经 `system.taskPage` 分页（协议 `table_sync`）。
            method::SYSTEM_SNAPSHOT => serde_json::to_value(self.events.wire_snapshot())
                .map_err(|error| internal_error("system snapshot", error)),
            method::SYSTEM_TASK_PAGE => {
                let params = parse_params::<fluxdown_protocol::TablePageParams>(request.params)?;
                let page = self.events.task_page(&params).map_err(|_| {
                    let mut data = RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false);
                    data.field = Some("cursor".to_owned());
                    data
                })?;
                serde_json::to_value(page)
                    .map_err(|error| internal_error("system task page", error))
            }
            method::SYSTEM_SHUTDOWN => {
                self.local.lifecycle.request_quit();
                Ok(serde_json::json!({ "ok": true }))
            }
            method::AGENT_POWER_ARM => {
                let params = parse_params::<fluxdown_protocol::PowerArmParams>(request.params)?;
                let armed = self.local.power.arm(
                    std::time::Duration::from_secs(params.delay_secs),
                    params.action,
                );
                Ok(serde_json::json!({ "armed": armed }))
            }
            method::AGENT_POWER_DISARM => {
                self.local.power.disarm();
                Ok(serde_json::json!({ "ok": true }))
            }
            method::AGENT_SESSION_GET => {
                let session = self.events.inspect(|snapshot| snapshot.session.clone());
                serde_json::to_value(session)
                    .map_err(|error| internal_error("agent session", error))
            }
            method::AGENT_GATEWAY_GET => {
                let gateway = self.events.inspect(|snapshot| snapshot.gateway.clone());
                serde_json::to_value(gateway)
                    .map_err(|error| internal_error("gateway status", error))
            }
            method::AGENT_GATEWAY_PATCH => {
                self.gateway_patch(params_or_empty(request.params), session)
                    .await
            }
            method::AGENT_GATEWAY_REVEAL_TOKEN => {
                let token = self.state.lock().await.gateway_user_token.clone();
                Ok(serde_json::json!({ "userToken": token }))
            }
            method::AGENT_AUTH_LOGIN => {
                cloud_value(self.auth.login(&params_or_empty(request.params)).await)
            }
            method::AGENT_AUTH_LOGIN_VERIFY => cloud_value(
                self.auth
                    .login_verify(&params_or_empty(request.params))
                    .await,
            ),
            method::AGENT_AUTH_REGISTER => {
                cloud_value(self.auth.register(&params_or_empty(request.params)).await)
            }
            method::AGENT_AUTH_REGISTER_VERIFY => cloud_value(
                self.auth
                    .register_verify(&params_or_empty(request.params))
                    .await,
            ),
            method::AGENT_AUTH_SEND_CODE => {
                cloud_value(self.auth.send_code(&params_or_empty(request.params)).await)
            }
            method::AGENT_AUTH_VERIFY_CODE => cloud_value(
                self.auth
                    .verify_code(&params_or_empty(request.params))
                    .await,
            ),
            method::AGENT_AUTH_SEND_PASSWORD_RESET_CODE => cloud_value(
                self.auth
                    .send_password_reset_code(&params_or_empty(request.params))
                    .await,
            ),
            method::AGENT_AUTH_RESET_PASSWORD => cloud_value(
                self.auth
                    .reset_password(&params_or_empty(request.params))
                    .await
                    .map(|()| serde_json::json!({ "ok": true })),
            ),
            method::AGENT_AUTH_LOGOUT => cloud_value(
                self.auth
                    .logout()
                    .await
                    .map(|()| serde_json::json!({ "ok": true })),
            ),
            method::AGENT_AUTH_REFRESH_PROFILE => {
                self.profile_request(Method::GET, "", None, true).await
            }
            method::AGENT_PROFILE_SEND_EMAIL_CODE => {
                self.profile_request(Method::POST, "/email/code", None, false)
                    .await
            }
            method::AGENT_PROFILE_SEND_NEW_EMAIL_CODE => {
                self.profile_request(
                    Method::POST,
                    "/email/code/new",
                    Some(params_or_empty(request.params)),
                    false,
                )
                .await
            }
            method::AGENT_PROFILE_CHANGE_EMAIL => {
                self.profile_request(
                    Method::POST,
                    "/email",
                    Some(params_or_empty(request.params)),
                    true,
                )
                .await
            }
            method::AGENT_PROFILE_SEND_PASSWORD_CODE => {
                self.profile_request(Method::POST, "/password/code", None, false)
                    .await
            }
            method::AGENT_PROFILE_CHANGE_PASSWORD => {
                self.profile_request(
                    Method::POST,
                    "/password",
                    Some(params_or_empty(request.params)),
                    true,
                )
                .await
            }
            method::AGENT_PROFILE_SEND_DELETE_CODE => {
                self.profile_request(Method::POST, "/delete/code", None, false)
                    .await
            }
            method::AGENT_PROFILE_DELETE_ACCOUNT => {
                self.delete_account(params_or_empty(request.params)).await
            }
            method::AGENT_PROFILE_RANDOM_ORIGIN_ID => {
                self.profile_request(Method::GET, "/origin-id/random", None, false)
                    .await
            }
            method::AGENT_PROFILE_CHECK_ORIGIN_ID => {
                let params = params_or_empty(request.params);
                match required_i64(&params, "value") {
                    Ok(value) => {
                        self.profile_request(
                            Method::GET,
                            &format!("/origin-id/check?value={value}"),
                            None,
                            false,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                }
            }
            method::AGENT_PROFILE_CHANGE_ORIGIN_ID => {
                self.profile_request(
                    Method::PUT,
                    "/origin-id",
                    Some(params_or_empty(request.params)),
                    true,
                )
                .await
            }
            method::AGENT_PROFILE_CHANGE_NICKNAME => {
                self.profile_request(
                    Method::PUT,
                    "/nickname",
                    Some(params_or_empty(request.params)),
                    true,
                )
                .await
            }
            method::AGENT_DEVICE_LIST => self.device_list().await,
            method::AGENT_REMOTE_RECONNECT => {
                if !self.cloud.is_authenticated().await {
                    return Err(RpcErrorData::new(
                        fluxdown_protocol::ApplicationErrorCode::Unauthorized,
                        false,
                    ));
                }
                self.remote.request_reconnect();
                Ok(serde_json::json!({"accepted": true}))
            }
            method::AGENT_DEVICE_RENAME => {
                self.device_rename(params_or_empty(request.params)).await
            }
            method::AGENT_DEVICE_DELETE => {
                self.device_delete(params_or_empty(request.params)).await
            }
            method::AGENT_DEVICE_DISCONNECT => {
                self.device_disconnect(params_or_empty(request.params))
                    .await
            }
            method::AGENT_PLAN_LIST => cloud_value(self.cloud.plans().await),
            method::AGENT_ORDER_CREATE => cloud_value(
                self.cloud
                    .create_order(&params_or_empty(request.params))
                    .await,
            ),
            method::AGENT_ORDER_GET => self.order_get(params_or_empty(request.params)).await,
            method::AGENT_ORDER_LIST => cloud_value(self.cloud.orders().await),
            method::AGENT_PLUGIN_FAVORITE_LIST => cloud_value(self.cloud.plugin_favorites().await),
            method::AGENT_PLUGIN_FAVORITE_SET => {
                let request =
                    parse_params::<fluxdown_protocol::PluginFavoriteSetRequest>(request.params)?;
                cloud_value(self.cloud.set_plugin_favorite(request).await)
            }
            method::AGENT_PRIVATE_PLUGIN_LIST => {
                self.private_plugins.list().await.and_then(to_value)
            }
            method::AGENT_PRIVATE_PLUGIN_INSTALL => {
                let request =
                    parse_params::<fluxdown_protocol::PrivatePluginInstallRequest>(request.params)?;
                self.private_plugins
                    .install(request)
                    .await
                    .and_then(to_value)
            }
            method::AGENT_REFERRAL_SUMMARY => {
                cloud_value(self.cloud.referral("/summary", Method::GET, None).await)
            }
            method::AGENT_REFERRAL_LIST_CODES => {
                self.referral_list_codes(params_or_empty(request.params))
                    .await
            }
            method::AGENT_REFERRAL_CREATE_CODE => cloud_value(
                self.cloud
                    .referral(
                        "/codes",
                        Method::POST,
                        Some(&params_or_empty(request.params)),
                    )
                    .await,
            ),
            method::AGENT_REFERRAL_DELETE_CODE => {
                self.referral_delete_code(params_or_empty(request.params))
                    .await
            }
            method::AGENT_REFERRAL_LIST_RECORDS => {
                self.referral_list_records(params_or_empty(request.params))
                    .await
            }
            method::AGENT_REFERRAL_VALIDATE => {
                self.referral_validate(params_or_empty(request.params))
                    .await
            }
            method::AGENT_PREFERENCES_PATCH => {
                self.preferences_patch(params_or_empty(request.params))
                    .await
            }
            method::AGENT_CLOUD_NOTIFY_GET => to_value(self.notify.get().await),
            method::AGENT_CLOUD_NOTIFY_REFRESH => cloud_value(self.notify.refresh().await),
            method::AGENT_CLOUD_NOTIFY_SET_REPORTING => {
                let params =
                    parse_params::<fluxdown_protocol::CloudNotifyReportingParams>(request.params)?;
                cloud_value(self.notify.set_reporting(params.enabled).await)
            }
            method::AGENT_CLOUD_NOTIFY_SET_PRIVACY => {
                let params =
                    parse_params::<fluxdown_protocol::CloudNotifyPrivacyParams>(request.params)?;
                cloud_value(self.notify.set_privacy(params).await)
            }
            method::AGENT_CLOUD_NOTIFY_CREATE_CHANNEL => {
                let params = parse_params::<fluxdown_protocol::CloudNotifyChannelCreateParams>(
                    request.params,
                )?;
                cloud_value(self.notify.create_channel(&params).await)
            }
            method::AGENT_CLOUD_NOTIFY_UPDATE_CHANNEL => {
                let params = parse_params::<fluxdown_protocol::CloudNotifyChannelUpdateParams>(
                    request.params,
                )?;
                cloud_value(self.notify.update_channel(&params).await)
            }
            method::AGENT_CLOUD_NOTIFY_DELETE_CHANNEL => {
                let params =
                    parse_params::<fluxdown_protocol::CloudNotifyChannelIdParams>(request.params)?;
                cloud_value(
                    self.notify
                        .delete_channel(&params.id)
                        .await
                        .map(|()| serde_json::json!({ "ok": true })),
                )
            }
            method::AGENT_CLOUD_NOTIFY_TEST_CHANNEL => {
                let params =
                    parse_params::<fluxdown_protocol::CloudNotifyChannelIdParams>(request.params)?;
                cloud_value(self.notify.test_channel(&params.id).await)
            }
            method::AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_START => {
                cloud_value(self.notify.telegram_bind_start().await)
            }
            method::AGENT_CLOUD_NOTIFY_SEND_EMAIL_CODE => {
                let params =
                    parse_params::<fluxdown_protocol::CloudNotifyEmailCodeParams>(request.params)?;
                cloud_value(self.notify.send_email_code(&params.address).await)
            }
            method::AGENT_CLOUD_NOTIFY_VERIFY_EMAIL => {
                let params = parse_params::<fluxdown_protocol::CloudNotifyEmailVerifyParams>(
                    request.params,
                )?;
                cloud_value(
                    self.notify
                        .verify_email(&params.address, &params.code)
                        .await
                        .map(|()| serde_json::json!({ "ok": true })),
                )
            }
            method::AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_STATUS => {
                let params = parse_params::<fluxdown_protocol::CloudNotifyTelegramBindStatusParams>(
                    request.params,
                )?;
                cloud_value(self.notify.telegram_bind_status(&params.code).await)
            }
            method::AGENT_CLOUD_NOTIFY_DELIVERIES => {
                let params =
                    parse_params::<fluxdown_protocol::CloudNotifyDeliveriesParams>(request.params)?;
                cloud_value(self.notify.deliveries(&params).await)
            }
            method::AGENT_SYNC_GET => to_value(self.sync.status().await),
            method::AGENT_SYNC_ENABLE => sync_value(self.sync.set_enabled(true).await),
            method::AGENT_SYNC_DISABLE => sync_value(self.sync.set_enabled(false).await),
            method::AGENT_SYNC_NOW => sync_value(self.sync.sync_now().await),
            method::AGENT_SYNC_SET_LOCAL_ONLY => {
                let params =
                    parse_params::<fluxdown_protocol::SyncLocalOnlyParams>(request.params)?;
                self.sync
                    .set_local_only(&params.keys, params.local_only)
                    .await
                    .map_err(sync_error_data)
                    .and_then(to_value)
            }
            method::AGENT_CLOUD_ENDPOINT_GET => to_value(self.cloud.endpoint()),
            method::AGENT_CLOUD_ENDPOINT_SET => {
                let params =
                    parse_params::<fluxdown_protocol::CloudEndpointSetParams>(request.params)?;
                cloud_value(self.cloud.set_endpoint(&params.base_url).await)
            }
            method::AGENT_REMOTE_LIST => remote_value(self.remote.refresh_snapshot().await),
            method::AGENT_REMOTE_DISPATCH => {
                let params =
                    parse_params::<fluxdown_protocol::RemoteDispatchParams>(request.params)?;
                remote_value(self.remote.dispatch(params).await)
            }
            method::AGENT_REMOTE_COMMAND => {
                let params =
                    parse_params::<fluxdown_protocol::RemoteCommandParams>(request.params)?;
                remote_value(
                    self.remote
                        .command(params)
                        .await
                        .map(|()| serde_json::json!({ "ok": true })),
                )
            }
            method::AGENT_LINK_PAIRING_CODE
            | method::AGENT_LINK_STOP_PAIRING
            | method::AGENT_LINK_DISCOVERY_SET
            | method::AGENT_LINK_PROBE
            | method::AGENT_LINK_PAIR_BEGIN
            | method::AGENT_LINK_PAIR_FINISH
            | method::AGENT_LINK_APPROVE
            | method::AGENT_LINK_REMOVE
            | method::AGENT_LINK_RENAME
            | method::AGENT_LINK_REFRESH
            | method::AGENT_LINK_CONTROL_CREDENTIAL
            | method::AGENT_LINK_DISPATCH => match &self.link {
                Some(link) => link.rpc(&request.method, request.params).await,
                // 互联服务未装配（如精简测试网关）：明确报不支持，不做假成功。
                None => Err(RpcErrorData::new(ApplicationErrorCode::Unsupported, false)),
            },
            method::AGENT_CAPTURE_SUBMIT => {
                self.capture_submit(params_or_empty(request.params)).await
            }
            method::AGENT_CAPTURE_SUBMIT_TORRENT_FILE => {
                self.capture_submit_torrent_file(params_or_empty(request.params))
                    .await
            }
            method::AGENT_CAPTURE_LIST => serde_json::to_value(self.capture.list().await)
                .map_err(|error| internal_error("capture list", error)),
            method::AGENT_CAPTURE_RESOLVE => {
                self.capture_resolve(params_or_empty(request.params)).await
            }
            method::AGENT_CAPTURE_PREVIEW => {
                self.capture_preview(params_or_empty(request.params)).await
            }
            method::AGENT_CAPTURE_CREATE_GROUP => {
                self.capture_create_group(params_or_empty(request.params))
                    .await
            }
            method::AGENT_PLUGIN_INSTALL_FILE => {
                self.plugin_install_file(params_or_empty(request.params))
                    .await
            }
            method::AGENT_PLATFORM_OPEN_TASK => {
                self.platform_task(params_or_empty(request.params), false)
                    .await
            }
            method::AGENT_PLATFORM_REVEAL_TASK => {
                self.platform_task(params_or_empty(request.params), true)
                    .await
            }
            method::AGENT_PLATFORM_COPY_TASK_FILES => {
                let params =
                    parse_params::<fluxdown_protocol::PlatformCopyTaskFilesParams>(request.params)?;
                self.platform_copy_task_files(params).await
            }
            method::AGENT_PLATFORM_OPEN_PATH => {
                let params =
                    parse_params::<fluxdown_protocol::PlatformOpenPathParams>(request.params)?;
                platform_blocking(move || {
                    crate::platform::open_path(Path::new(&params.path), params.reveal)
                })
                .await
                .map(|()| serde_json::json!({ "ok": true }))
            }
            method::AGENT_PLATFORM_FILE_ICON => {
                use base64::Engine as _;

                let params =
                    parse_params::<fluxdown_protocol::PlatformFileIconParams>(request.params)?;
                let png =
                    platform_blocking(move || crate::platform::file_icon_png(&params)).await?;
                to_value(fluxdown_protocol::PlatformFileIconDto {
                    png: base64::engine::general_purpose::STANDARD.encode(png),
                })
            }
            method::AGENT_PLATFORM_SET_APP_ICON => {
                let params = parse_params::<fluxdown_protocol::app_icon::PlatformSetAppIconParams>(
                    request.params,
                )?;
                self.platform_set_app_icon(params).await
            }
            method::AGENT_PLATFORM_APP_ICON_IMAGE => {
                let params = parse_params::<fluxdown_protocol::app_icon::PlatformAppIconImageParams>(
                    request.params,
                )?;
                self.platform_app_icon_image(params).await
            }
            method::AGENT_PLATFORM_SYNC_WINDOW_ICONS => self.platform_sync_window_icons().await,
            method::AGENT_SOUND_PREVIEW => {
                let params =
                    parse_params::<fluxdown_protocol::sound::SoundPreviewParams>(request.params)?;
                let value = params.value;
                let outcome = tokio::task::spawn_blocking({
                    let value = value.clone();
                    move || crate::sound::preview(&value)
                })
                .await
                .map_err(|error| internal_error("sound preview join", error))?;
                outcome
                    .map_err(|error| {
                        tracing::warn!(value = %value, error = %error, "sound preview failed");
                        sound_preview_error(error)
                    })
                    .map(|()| serde_json::json!({ "ok": true }))
            }
            method::AGENT_PLATFORM_INTEGRATION_GET => {
                platform_blocking(|| Ok(crate::platform::integration_status()))
                    .await
                    .and_then(to_value)
            }
            method::AGENT_PLATFORM_SET_AUTOSTART => {
                let params =
                    parse_params::<fluxdown_protocol::PlatformToggleParams>(request.params)?;
                platform_integration_apply(move || crate::platform::set_autostart(params.enabled))
                    .await
            }
            method::AGENT_PLATFORM_SET_FILE_ASSOCIATION => {
                let params =
                    parse_params::<fluxdown_protocol::PlatformToggleParams>(request.params)?;
                platform_integration_apply(move || {
                    crate::platform::set_file_association(params.enabled)
                })
                .await
            }
            method::AGENT_PLATFORM_SET_URL_PROTOCOL => {
                let params =
                    parse_params::<fluxdown_protocol::PlatformUrlProtocolParams>(request.params)?;
                platform_integration_apply(move || {
                    crate::platform::set_url_protocol(&params.scheme, params.enabled)
                })
                .await
            }
            method::AGENT_DIAGNOSTICS_RUN => {
                diagnostics_value(self.diagnostics.run().await).and_then(to_value)
            }
            method::AGENT_DIAGNOSTICS_REPAIR => {
                let params =
                    parse_params::<fluxdown_protocol::DiagnosticRepairParams>(request.params)?;
                diagnostics_value(self.diagnostics.repair(&params).await)
            }
            method::AGENT_DIAGNOSTICS_LOG_PATHS => to_value(self.diagnostics.log_paths().await),
            method::AGENT_DIAGNOSTICS_EXPORT_LOGS => {
                let params = parse_params::<fluxdown_protocol::LogExportParams>(request.params)?;
                diagnostics_value(self.diagnostics.export_logs(&params).await).and_then(to_value)
            }
            method::AGENT_UPDATE_CHECK => self.update_check(params_or_empty(request.params)).await,
            method::AGENT_UPDATE_DOWNLOAD => self
                .update
                .download()
                .await
                .map_err(update_error)
                .and_then(to_value),
            method::AGENT_UPDATE_INSTALL => self
                .update
                .install()
                .await
                .map_err(update_error)
                .and_then(to_value),
            method::AGENT_UPDATE_CANCEL => to_value(self.update.cancel()),
            name if name.starts_with("daemon.") => {
                self.daemon
                    .call::<serde_json::Value, serde_json::Value>(name, request.params)
                    .await
            }
            _ => Err(RpcErrorData::new(ApplicationErrorCode::Unsupported, false)),
        }
    }

    /// 删除云账号：服务端 204 后按用户主动路径清本地会话（不发 `SessionRevoked`）。
    /// 服务端同时广播的 `session.revoked(accountDeleted)` 可能先一步清掉会话，此时视为已完成。
    async fn delete_account(
        &self,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let epoch = self.cloud.request_epoch();
        self.cloud
            .at_epoch(epoch)
            .profile_call(Method::POST, "/delete", Some(&body))
            .await
            .map_err(cloud_error_data)?;
        match self.cloud.clear_session_epoch(epoch).await {
            Ok(()) => {}
            Err(error) if error.code.as_deref() == Some("session_changed") => {}
            Err(error) => return Err(cloud_error_data(error)),
        }
        Ok(serde_json::json!({ "ok": true }))
    }

    async fn profile_request(
        &self,
        method: Method,
        suffix: &str,
        body: Option<serde_json::Value>,
        persist: bool,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let epoch = self.cloud.request_epoch();
        let value = self
            .cloud
            .at_epoch(epoch)
            .profile_call(method, suffix, body.as_ref())
            .await
            .map_err(cloud_error_data)?;
        if persist {
            self.cloud
                .persist_profile(epoch, value.clone())
                .await
                .map_err(cloud_error_data)?;
        }
        drop(
            self.cloud
                .lock_epoch(epoch)
                .await
                .map_err(cloud_error_data)?,
        );
        Ok(value)
    }

    // Keep prepare, link update, persistence, commit and their rollbacks in one serialized
    // transaction so a partial failure cannot publish a port/token combination that never ran.
    async fn gateway_patch(
        &self,
        params: serde_json::Value,
        session: &SessionContext,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let patch = parse_params::<fluxdown_protocol::GatewayPatchParams>(Some(params))?;
        let _transaction = self.gateway_patch_lock.lock().await;
        // server 模式的密钥同时是首次设置的开关：空值会把服务重新暴露给匿名 setup，
        // 不合规的值会让 Web 登录页拒绝自己。二者都在这里拒绝。
        if self.server_mode
            && patch
                .user_token
                .as_deref()
                .is_some_and(|token| crate::server_mode::validate_access_key(token).is_err())
        {
            return Err(invalid_field("userToken"));
        }
        let port_change = {
            let state = self.state.lock().await;
            // Reject the entire patch before binding or changing token/runtime switches.
            if let Some(port) = patch.port
                && (port < 1024 || self.server_mode || !state.gateway.port_editable)
            {
                return Err(invalid_field("port"));
            }
            patch.port.filter(|port| *port != state.gateway.port)
        };
        // The readiness handshake must not hold state: the same router shares its owner.
        let prepared = match port_change {
            Some(port) => Some(
                self.listener_control
                    .prepare(port)
                    .await
                    .map_err(|error| error.rpc_data())?,
            ),
            None => None,
        };
        if let Some(prepared) = &prepared
            && let Some(link) = &self.link
            && let Err(error) = link.update_bound(prepared.bound).await
        {
            self.rollback_listener(prepared).await?;
            return Err(restart::RestartError::Failed(error.to_string()).rpc_data());
        }
        let mut state = self.state.lock().await;
        let previous_gateway = state.gateway.clone();
        let mut previous_user_token = None;
        let api_was_enabled = state.gateway.api_enabled;
        let mcp_was_enabled = state.gateway.mcp_enabled;
        if let Some(value) = patch.takeover_enabled {
            state.gateway.takeover_enabled = value;
        }
        if let Some(value) = patch.jsonrpc_enabled {
            state.gateway.jsonrpc_enabled = value;
        }
        if let Some(value) = patch.api_enabled {
            state.gateway.api_enabled = value;
        }
        if let Some(value) = patch.mcp_enabled {
            state.gateway.mcp_enabled = value;
        }
        if let Some(value) = patch.cors_enabled {
            state.gateway.cors_enabled = value;
        }
        if let Some(value) = patch.lan_enabled {
            state.gateway.lan_enabled = value;
        }
        if let Some(port) = patch.port {
            state.gateway.port = port;
        }
        let token_set_explicitly = patch.user_token.is_some();
        if patch.regenerate_user_token {
            previous_user_token = Some(std::mem::replace(
                &mut state.gateway_user_token,
                generate_user_token(),
            ));
            state.gateway.user_token_configured = true;
        } else if let Some(token) = patch.user_token {
            previous_user_token = Some(std::mem::replace(&mut state.gateway_user_token, token));
            state.gateway.user_token_configured = !state.gateway_user_token.trim().is_empty();
        }
        // 已配置且本次不改 token 时无需分配回滚副本；空值可能被鉴权规则自动补齐。
        if previous_user_token.is_none() && state.gateway_user_token.trim().is_empty() {
            previous_user_token = Some(state.gateway_user_token.clone());
        }
        ensure_forced_auth_token(&mut state, api_was_enabled, mcp_was_enabled);
        if !self.server_mode {
            ensure_exposed_auth_token(&mut state);
        }
        // 本次显式清空 token 且没有同时开启强制鉴权开关（否则上一步已补 token）：
        // 管理 API / MCP 空 token 会拒绝全部请求，随清空一并关闭，保持「开关开 ⇒ 有 token」。
        if token_set_explicitly && state.gateway_user_token.trim().is_empty() {
            state.gateway.api_enabled = false;
            state.gateway.mcp_enabled = false;
        }
        let gateway = state.gateway.clone();
        let user_token = state.gateway_user_token.clone();
        let persisted = match &prepared {
            Some(prepared) => {
                self.store
                    .save_gateway_change(
                        &state,
                        prepared
                            .endpoint_dir
                            .as_deref()
                            .unwrap_or_else(|| self.store.data_dir()),
                        prepared.bound,
                    )
                    .await
            }
            None => self.store.save(&state).await,
        };
        if let Err(error) = persisted {
            // Failed persistence cannot alter runtime switches, the snapshot or the old service.
            state.gateway = previous_gateway;
            if let Some(token) = previous_user_token {
                state.gateway_user_token = token;
            }
            drop(state);
            if let Some(prepared) = &prepared {
                self.rollback_listener(prepared).await?;
                return Err(restart::RestartError::Failed(error.to_string()).rpc_data());
            }
            if patch.port.is_some() {
                return Err(restart::RestartError::Failed(error.to_string()).rpc_data());
            }
            return Err(internal_error("agent state", error));
        }
        if let Some(prepared) = &prepared
            && let Err(error) = self.listener_control.commit().await
        {
            state.gateway = previous_gateway;
            if let Some(token) = previous_user_token {
                state.gateway_user_token = token;
            }
            let rollback = self
                .store
                .save_gateway_change(
                    &state,
                    prepared
                        .endpoint_dir
                        .as_deref()
                        .unwrap_or_else(|| self.store.data_dir()),
                    prepared.previous,
                )
                .await;
            drop(state);
            if let Err(listener_rollback) = self.rollback_listener(prepared).await {
                if let Err(rollback) = rollback {
                    tracing::error!(error = %rollback, "gateway state rollback also failed");
                }
                return Err(listener_rollback);
            }
            if let Err(rollback) = rollback {
                return Err(restart::RestartError::Failed(format!(
                    "{error}; state rollback failed: {rollback}"
                ))
                .rpc_data());
            }
            return Err(error.rpc_data());
        }
        drop(state);
        self.api_switches.update(
            gateway.takeover_enabled,
            gateway.jsonrpc_enabled,
            gateway.api_enabled,
            gateway.mcp_enabled,
            gateway.cors_enabled,
        );
        let access_key_changed = *self.api_token.get() != *user_token;
        self.api_token.set(user_token.as_str());
        if access_key_changed {
            // 先把发起修改的会话记住的值改成新值，再通知其它会话重新校验：发起者不被自己踢掉，
            // 用旧密钥认证的其它会话随即断开。
            session.auth.rebind_access_key(&user_token);
            self.access_key_epoch.send_modify(|epoch| *epoch += 1);
        }
        self.events
            .publish(fluxdown_protocol::AgentEvent::GatewayChanged(
                gateway.clone(),
            ));
        serde_json::to_value(gateway).map_err(|error| internal_error("gateway status", error))
    }

    async fn rollback_listener(
        &self,
        prepared: &restart::PreparedListener,
    ) -> Result<(), RpcErrorData> {
        let listener = self.listener_control.rollback().await;
        let link = match &self.link {
            Some(link) => link.update_bound(prepared.previous).await,
            None => Ok(()),
        };
        if let Err(error) = link {
            if let Err(listener) = listener {
                tracing::error!(error = %listener, "gateway listener rollback also failed");
            }
            return Err(restart::RestartError::Failed(format!(
                "link endpoint rollback failed: {error}"
            ))
            .rpc_data());
        }
        listener.map_err(|error| error.rpc_data())
    }

    async fn device_list(&self) -> Result<serde_json::Value, RpcErrorData> {
        let epoch = self.cloud.request_epoch();
        let device_id = self.state.lock().await.device_id.clone();
        let value = self
            .cloud
            .at_epoch(epoch)
            .devices(&device_id)
            .await
            .map_err(cloud_error_data)?;
        let devices = cloud_devices_from_value(&value)?;
        let _state = self
            .cloud
            .lock_epoch(epoch)
            .await
            .map_err(cloud_error_data)?;
        self.events
            .publish(fluxdown_protocol::AgentEvent::CloudDevicesChanged(devices));
        Ok(value)
    }

    async fn device_rename(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let epoch = self.cloud.request_epoch();
        let id = required_string(&params, "id")?;
        let name = required_string(&params, "name")?;
        if name.chars().count() > 64 {
            return Err(invalid_field("name"));
        }
        let value = self
            .cloud
            .at_epoch(epoch)
            .rename_device(&id, &name)
            .await
            .map_err(cloud_error_data)?;
        let updated = serde_json::from_value::<fluxdown_protocol::CloudDevice>(value.clone())
            .map_err(|error| internal_error("device rename response", error))?;
        let mut state = self
            .cloud
            .lock_epoch(epoch)
            .await
            .map_err(cloud_error_data)?;
        if updated.is_current {
            state.device_name.clone_from(&updated.name);
        }
        let mut devices = agent_snapshot(&self.events)?.cloud_devices;
        if let Some(existing) = devices.iter_mut().find(|device| device.id == updated.id) {
            existing.clone_from(&updated);
        } else {
            devices.push(updated);
        }
        self.events
            .publish(fluxdown_protocol::AgentEvent::CloudDevicesChanged(devices));
        drop(state);
        self.store
            .persist(&self.state)
            .await
            .map_err(|error| internal_error("persist renamed device", error))?;
        Ok(value)
    }

    async fn device_delete(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let epoch = self.cloud.request_epoch();
        let id = required_string(&params, "id")?;
        let devices = agent_snapshot(&self.events)?.cloud_devices;
        let deleting_current = devices
            .iter()
            .find(|device| device.id == id)
            .is_some_and(|device| device.is_current);
        let value = self
            .cloud
            .at_epoch(epoch)
            .delete_device(&id)
            .await
            .map_err(cloud_error_data)?;
        if deleting_current {
            // `clear_session` 自身投影 `SessionChanged(None)`。
            self.cloud
                .clear_session_epoch(epoch)
                .await
                .map_err(cloud_error_data)?;
        } else {
            let _state = self
                .cloud
                .lock_epoch(epoch)
                .await
                .map_err(cloud_error_data)?;
            let mut devices = agent_snapshot(&self.events)?.cloud_devices;
            devices.retain(|device| device.id != id);
            self.events
                .publish(fluxdown_protocol::AgentEvent::CloudDevicesChanged(devices));
        }
        Ok(value)
    }

    /// 让同账号另一台在线设备下线以释放在线名额。成功后本机若正处于在线名额受限的等待中，
    /// 立即重连任务通道与配置同步，不必等下一个 30 秒重试。
    async fn device_disconnect(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let epoch = self.cloud.request_epoch();
        let id = required_string(&params, "id")?;
        self.cloud
            .at_epoch(epoch)
            .disconnect_device(&id)
            .await
            .map_err(cloud_error_data)?;
        let snapshot = agent_snapshot(&self.events)?;
        if snapshot.cloud_connection.state != fluxdown_protocol::CloudConnectionState::Connected {
            self.remote.request_reconnect();
        }
        if !snapshot.sync.connected {
            self.sync.request_reconnect();
        }
        Ok(serde_json::json!({"ok": true}))
    }

    async fn order_get(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let order_no = required_string(&params, "orderNo")?;
        let result = self.cloud.order(&order_no).await;
        cloud_value(result)
    }

    async fn referral_list_codes(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let (page, page_size) = pagination(&params)?;
        cloud_value(self.cloud.referral_codes(page, page_size).await)
    }

    async fn referral_delete_code(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let id = required_string(&params, "id")?;
        cloud_value(self.cloud.delete_referral_code(&id).await)
    }

    async fn referral_list_records(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let (page, page_size) = pagination(&params)?;
        let search = params.get("search").and_then(serde_json::Value::as_str);
        cloud_value(self.cloud.referral_records(page, page_size, search).await)
    }

    async fn referral_validate(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let code = required_string(&params, "code")?;
        let plan_code = required_string(&params, "planCode")?;
        cloud_value(self.cloud.validate_referral(&code, &plan_code).await)
    }

    async fn preferences_patch(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let values = params
            .get("values")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| invalid_field("values"))?;
        let sync = params
            .get("sync")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let mut revision = 0_u64;
        for (key, value) in values {
            // JSON null = 恢复默认（墓碑）：本机移除该偏好 / daemon 键回到默认，并把删除同步给云端。
            let deleted = value.is_null();
            let result = if sync {
                self.sync
                    .mark_local(key.clone(), value.clone(), deleted)
                    .await
            } else {
                self.sync
                    .set_local_preference(key.clone(), value.clone(), deleted)
                    .await
            };
            revision = revision.max(result.map_err(sync_error_data)?);
        }
        to_value(fluxdown_protocol::AgentPreferencesPatchResult { ok: true, revision })
    }

    async fn capture_submit(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        if self.open_associations.intercept(&params)? {
            return Ok(ignored_capture());
        }
        let request_value = params
            .get("request")
            .cloned()
            .unwrap_or_else(|| params.clone());
        let request = serde_json::from_value::<fluxdown_protocol::DownloadRequest>(request_value)
            .map_err(|error| {
            tracing::debug!(error = %error, "rejected capture request");
            RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
        })?;
        // 本机调用方：`silent=true`（系统打开链接 / 拖入）直接建任务；否则（剪贴板监听）
        // 恒请用户确认。外部接管走 HTTP / NMH，由 `CaptureOrigin::External` 按免打扰偏好分流。
        let silent = params
            .get("silent")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let origin = if silent {
            crate::capture::CaptureOrigin::Direct
        } else {
            crate::capture::CaptureOrigin::Prompt
        };
        capture_value(self.capture.submit(request, origin).await)
    }

    async fn capture_resolve(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let params = serde_json::from_value::<fluxdown_protocol::CaptureResolveParams>(params)
            .map_err(|error| {
                tracing::debug!(error = %error, "rejected capture resolve params");
                RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
            })?;
        capture_value(
            self.capture
                .resolve(
                    &params.transaction_id,
                    params.accepted,
                    params.disable_category_route,
                    params.request,
                )
                .await,
        )
    }

    async fn capture_preview(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let params = serde_json::from_value::<fluxdown_protocol::CapturePreviewParams>(params)
            .map_err(|error| {
                tracing::debug!(error = %error, "rejected capture preview params");
                RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
            })?;
        capture_value(
            self.capture
                .preview(&params.transaction_id, params.request)
                .await,
        )
    }

    async fn capture_create_group(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let params = serde_json::from_value::<fluxdown_protocol::CaptureCreateGroupParams>(params)
            .map_err(|error| {
                tracing::debug!(error = %error, "rejected capture group params");
                RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
            })?;
        capture_value(
            self.capture
                .create_group(&params.transaction_id, params.request, params.context)
                .await,
        )
    }

    /// 读取本机 `.torrent`，上传 daemon blob 后走捕获路径建任务。`silent=false`（用户主动选择）
    /// 不静默：由 daemon 发 BT 文件选择请求；`saveDir` / `queueId` / `startPaused` 缺省维持旧行为。
    async fn capture_submit_torrent_file(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        if self.open_associations.intercept(&params)? {
            return Ok(ignored_capture());
        }
        // 缺失 / 空路径先按字段报错，其余参数再整体解析。
        required_string(&params, "path")?;
        let params =
            parse_params::<fluxdown_protocol::agent::CaptureSubmitTorrentFileParams>(Some(params))?;
        let path = PathBuf::from(&params.path);
        let bytes = read_upload_file(&path).await?;
        let blob_id = blob_value(self.blobs.upload(BlobKind::Torrent, bytes).await)?;
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let filename = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let request = fluxdown_protocol::DownloadRequest {
            url: format!("torrent-file://{file_name}"),
            filename,
            save_dir: params
                .save_dir
                .filter(|dir| !dir.trim().is_empty())
                .unwrap_or_default(),
            referrer: String::new(),
            cookies: String::new(),
            headers: None,
            file_size: None,
            mime_type: None,
            method: None,
            body: None,
            audio_url: None,
        };
        let options = crate::capture::TorrentCreateOptions {
            unattended: params.silent,
            queue_id: params.queue_id.filter(|id| !id.trim().is_empty()),
            start_paused: params.start_paused.unwrap_or(false),
        };
        capture_value(self.capture.create_torrent(request, blob_id, options).await)
    }

    /// 读取本机插件包，上传 daemon blob 后转 `daemon.plugin.install`。
    async fn plugin_install_file(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let path = PathBuf::from(required_string(&params, "path")?);
        let bytes = read_upload_file(&path).await?;
        let blob_id = blob_value(self.blobs.upload(BlobKind::Plugin, bytes).await)?;
        self.daemon
            .call::<serde_json::Value, serde_json::Value>(
                method::DAEMON_PLUGIN_INSTALL,
                Some(serde_json::json!({ "blobId": blob_id })),
            )
            .await
    }

    async fn update_check(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let params = parse_params::<fluxdown_protocol::UpdateCheckParams>(Some(params))?;
        let channel = match params.channel {
            Some(channel) => channel,
            None => self
                .state
                .lock()
                .await
                .preferences
                .values
                .get("general.update_channel")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("stable")
                .to_owned(),
        };
        self.update
            .check(&channel)
            .await
            .map_err(update_error)
            .and_then(to_value)
    }

    async fn platform_task(
        &self,
        params: serde_json::Value,
        reveal: bool,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let task_id = required_string(&params, "taskId")?;
        let task: fluxdown_protocol::TaskDto = self
            .daemon
            .call(
                fluxdown_protocol::method::DAEMON_TASK_GET,
                Some(serde_json::json!({ "taskId": task_id })),
            )
            .await?;
        platform_blocking(move || {
            if reveal {
                crate::platform::reveal_task(&task)
            } else {
                crate::platform::open_task(&task)
            }
        })
        .await
        .map(|()| serde_json::json!({ "ok": true }))
    }

    /// `agent.platform.copyTaskFiles`：逐个向 daemon 取任务事实（与打开 / 定位同一来源），再在阻塞
    /// 线程上整体写入系统剪贴板。
    async fn platform_copy_task_files(
        &self,
        params: fluxdown_protocol::PlatformCopyTaskFilesParams,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let mut task_ids = params.task_ids;
        if task_ids.is_empty()
            || task_ids.len() > fluxdown_protocol::PLATFORM_COPY_TASK_FILES_MAX
            || task_ids.iter().any(String::is_empty)
        {
            return Err(invalid_field("taskIds"));
        }
        if !crate::platform::copies_files_to_clipboard() {
            return Err(RpcErrorData::new(ApplicationErrorCode::Unsupported, false));
        }
        // 剪贴板里的文件顺序无意义（文件管理器粘贴时自行排序），排序去重即可。
        task_ids.sort_unstable();
        task_ids.dedup();
        let mut tasks = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            let task: fluxdown_protocol::TaskDto = self
                .daemon
                .call(
                    fluxdown_protocol::method::DAEMON_TASK_GET,
                    Some(serde_json::json!({ "taskId": task_id })),
                )
                .await?;
            tasks.push(task);
        }
        platform_blocking(move || crate::platform::copy_task_files(&tasks))
            .await
            .map(|()| serde_json::json!({ "ok": true }))
    }

    /// 导入 / 校验应用图标图片，再写设备本地偏好 `desktop.app_icon`（偏好永远指向已落盘的图片）；
    /// 之后由 `app_icon` 服务按偏好变化落实托盘、窗口、快捷方式。偏好已是目标值时不写。
    #[cfg(feature = "desktop")]
    async fn platform_set_app_icon(
        &self,
        params: fluxdown_protocol::app_icon::PlatformSetAppIconParams,
    ) -> Result<serde_json::Value, RpcErrorData> {
        use fluxdown_protocol::app_icon::{APP_ICON_PREF_KEY, AppIconStateDto};

        // 导入与写偏好对 `app_icon` 服务的「回落默认」互斥（见 `app_icon::preference_lock`）。
        let _guard = crate::app_icon::preference_lock().await;
        let store = crate::app_icon::IconStore::new(self.store.data_dir());
        let choice =
            platform_blocking(move || crate::app_icon::resolve_request(&store, &params)).await?;
        let value = choice.to_preference();
        let unchanged = self.events.inspect(|snapshot| {
            snapshot
                .preferences
                .values
                .get(APP_ICON_PREF_KEY)
                .and_then(serde_json::Value::as_str)
                == Some(value.as_str())
        });
        if !unchanged {
            self.sync
                .set_local_preference(
                    APP_ICON_PREF_KEY.to_owned(),
                    serde_json::Value::String(value.clone()),
                    false,
                )
                .await
                .map_err(sync_error_data)?;
        }
        to_value(AppIconStateDto { value })
    }

    #[cfg(not(feature = "desktop"))]
    async fn platform_set_app_icon(
        &self,
        _params: fluxdown_protocol::app_icon::PlatformSetAppIconParams,
    ) -> Result<serde_json::Value, RpcErrorData> {
        Err(platform_error_data(PlatformError::Unsupported(
            "app icons require the desktop build",
        )))
    }

    /// 当前自定义应用图标母版缩放后的 PNG（设置页预览 / GPUI 贴图用）。
    #[cfg(feature = "desktop")]
    async fn platform_app_icon_image(
        &self,
        params: fluxdown_protocol::app_icon::PlatformAppIconImageParams,
    ) -> Result<serde_json::Value, RpcErrorData> {
        let store = crate::app_icon::IconStore::new(self.store.data_dir());
        platform_blocking(move || crate::app_icon::icon_image(&store, params.size))
            .await
            .and_then(to_value)
    }

    #[cfg(not(feature = "desktop"))]
    async fn platform_app_icon_image(
        &self,
        _params: fluxdown_protocol::app_icon::PlatformAppIconImageParams,
    ) -> Result<serde_json::Value, RpcErrorData> {
        Err(platform_error_data(PlatformError::Unsupported(
            "app icons require the desktop build",
        )))
    }

    /// 给桌面进程现有窗口补设当前图标（Windows / X11；其余平台无操作）。只做窗口这一步，
    /// 托盘 / 快捷方式 / 通知由偏好变化触发。
    #[cfg(feature = "desktop")]
    async fn platform_sync_window_icons(&self) -> Result<serde_json::Value, RpcErrorData> {
        use fluxdown_protocol::app_icon::{APP_ICON_PREF_KEY, AppIconChoice};

        let store = crate::app_icon::IconStore::new(self.store.data_dir());
        let stored = self
            .events
            .inspect(|snapshot| snapshot.preferences.values.get(APP_ICON_PREF_KEY).cloned());
        let choice = AppIconChoice::from_preference(stored.as_ref());
        platform_blocking(move || crate::app_icon::sync_window_icons(&store, &choice))
            .await
            .map(|()| serde_json::json!({ "ok": true }))
    }

    /// headless 构建没有桌面进程窗口可补设：与「其余平台无操作」一致，直接成功。
    #[cfg(not(feature = "desktop"))]
    #[allow(clippy::unused_async, reason = "与桌面构建的处理器签名一致")]
    async fn platform_sync_window_icons(&self) -> Result<serde_json::Value, RpcErrorData> {
        Ok(serde_json::json!({ "ok": true }))
    }

    async fn ui_connected(&self) {
        self.local.shell.ui_connected().await;
    }

    async fn ui_disconnected(&self, was_ready: bool) {
        self.local.shell.ui_disconnected(was_ready).await;
    }
}

fn required_string(params: &serde_json::Value, field: &str) -> Result<String, RpcErrorData> {
    params
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid_field(field))
}

fn required_i64(params: &serde_json::Value, field: &str) -> Result<i64, RpcErrorData> {
    params
        .get(field)
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| invalid_field(field))
}

fn pagination(params: &serde_json::Value) -> Result<(u32, u32), RpcErrorData> {
    let page = params
        .get("page")
        .map_or(Some(1), serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid_field("page"))?;
    let page_size = params
        .get("pageSize")
        .map_or(Some(20), serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| (1..=100).contains(value))
        .ok_or_else(|| invalid_field("pageSize"))?;
    Ok((page, page_size))
}

fn update_error(error: UpdateError) -> RpcErrorData {
    match error {
        UpdateError::InvalidChannel(_) => invalid_field("channel"),
        UpdateError::Http(_) | UpdateError::Status(_) => {
            RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
        }
        UpdateError::Client(_) | UpdateError::Decode(_) => {
            RpcErrorData::new(ApplicationErrorCode::Internal, false)
        }
        UpdateError::NoUpdate => RpcErrorData::new(ApplicationErrorCode::Conflict, false),
        UpdateError::Manual(_) => RpcErrorData::new(ApplicationErrorCode::Unsupported, false),
    }
}

/// 提示音试听失败 → RPC 错误：值非法 = `InvalidArgument(value)`；文件缺失 / 类型不支持 / 播放器失败
/// 都带 `soundUnplayable` 原因，供客户端给出可操作文案。
fn sound_preview_error(error: crate::sound::PreviewError) -> RpcErrorData {
    use crate::sound::{PlaybackError, PreviewError};
    match error {
        PreviewError::InvalidValue => invalid_field("value"),
        PreviewError::UnsupportedType(_) => {
            invalid_field("value").with_reason(fluxdown_protocol::ErrorReason::SoundUnplayable)
        }
        PreviewError::Playback(PlaybackError::Missing(_)) => {
            RpcErrorData::new(ApplicationErrorCode::NotFound, false)
                .with_reason(fluxdown_protocol::ErrorReason::SoundUnplayable)
        }
        PreviewError::Playback(PlaybackError::Unsupported) => {
            RpcErrorData::new(ApplicationErrorCode::Unsupported, false)
        }
        PreviewError::Playback(PlaybackError::Failed(_)) => {
            RpcErrorData::new(ApplicationErrorCode::Unavailable, false)
                .with_reason(fluxdown_protocol::ErrorReason::SoundUnplayable)
        }
    }
}

fn invalid_field(field: &str) -> RpcErrorData {
    RpcErrorData {
        code: ApplicationErrorCode::InvalidArgument,
        retryable: false,
        field: Some(field.to_owned()),
        revision: None,
        reason: None,
    }
}

/// 非任务字段的当前投影（会话、网关、云设备、远程任务……）：取头形态，不克隆任务表。
fn agent_snapshot(
    events: &AgentEventHub,
) -> Result<fluxdown_protocol::AgentSnapshot, RpcErrorData> {
    match events.wire_snapshot().body {
        fluxdown_protocol::SnapshotBody::Agent(snapshot) => Ok(*snapshot),
        fluxdown_protocol::SnapshotBody::Daemon(_) => {
            Err(RpcErrorData::new(ApplicationErrorCode::Internal, false))
        }
    }
}

fn cloud_devices_from_value(
    value: &serde_json::Value,
) -> Result<Vec<fluxdown_protocol::CloudDevice>, RpcErrorData> {
    let devices = value
        .get("devices")
        .or_else(|| value.get("value"))
        .unwrap_or(value)
        .clone();
    serde_json::from_value(devices).map_err(|error| internal_error("cloud device list", error))
}

fn capture_value<T: serde::Serialize>(
    result: Result<T, CaptureError>,
) -> Result<serde_json::Value, RpcErrorData> {
    match result {
        Ok(value) => {
            serde_json::to_value(value).map_err(|error| internal_error("capture result", error))
        }
        Err(CaptureError::Full) => Err(RpcErrorData::new(ApplicationErrorCode::Unavailable, true)),
        Err(CaptureError::NotFound) => {
            Err(RpcErrorData::new(ApplicationErrorCode::NotFound, false))
        }
        Err(CaptureError::Busy) => Err(RpcErrorData::new(ApplicationErrorCode::Conflict, true)),
        Err(CaptureError::InvalidGroup) => Err(RpcErrorData::new(
            ApplicationErrorCode::InvalidArgument,
            false,
        )),
        Err(CaptureError::Daemon(error)) => Err(error),
        Err(CaptureError::Json(_)) => Err(RpcErrorData::new(
            ApplicationErrorCode::InvalidArgument,
            false,
        )),
        Err(CaptureError::Platform(_)) => {
            Err(RpcErrorData::new(ApplicationErrorCode::Internal, false))
        }
    }
}

fn diagnostics_value<T>(result: Result<T, DiagnosticsError>) -> Result<T, RpcErrorData> {
    use crate::permission::PermissionError;
    use fluxdown_protocol::ErrorReason;

    if let Err(error) = &result {
        tracing::warn!(error = %error, "doctor action failed");
    }
    result.map_err(|error| match error {
        DiagnosticsError::InvalidAction(_) => {
            RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
        }
        DiagnosticsError::Platform(error) => platform_error_data(error),
        DiagnosticsError::Daemon(error) => error,
        DiagnosticsError::State(_)
        | DiagnosticsError::Io(_)
        | DiagnosticsError::Export(_)
        | DiagnosticsError::Notification(_)
        | DiagnosticsError::Permission(PermissionError::Failed(_) | PermissionError::TimedOut(_)) => {
            RpcErrorData::new(ApplicationErrorCode::Internal, false)
        }
        DiagnosticsError::Permission(PermissionError::Cancelled) => {
            RpcErrorData::new(ApplicationErrorCode::Cancelled, false)
                .with_reason(ErrorReason::ElevationCancelled)
        }
        DiagnosticsError::Permission(PermissionError::ElevationUnavailable(_)) => {
            RpcErrorData::new(ApplicationErrorCode::Unsupported, false)
                .with_reason(ErrorReason::ElevationUnavailable)
        }
        DiagnosticsError::Permission(PermissionError::RunningElevated) => {
            RpcErrorData::new(ApplicationErrorCode::Unsupported, false)
                .with_reason(ErrorReason::RunningElevated)
        }
        DiagnosticsError::Permission(PermissionError::NotApplicable(_)) => {
            RpcErrorData::new(ApplicationErrorCode::Unsupported, false)
                .with_reason(ErrorReason::RepairNotApplicable)
        }
        DiagnosticsError::RepairIncomplete(_) => {
            RpcErrorData::new(ApplicationErrorCode::Internal, false)
                .with_reason(ErrorReason::RepairIncomplete)
        }
    })
}

fn parse_params<T: serde::de::DeserializeOwned>(
    params: Option<serde_json::Value>,
) -> Result<T, RpcErrorData> {
    serde_json::from_value(params_or_empty(params)).map_err(|error| {
        tracing::debug!(error = %error, "rejected RPC params");
        RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
    })
}

/// 序列化失败 / 云端响应畸形是 agent 自身或云端的问题：记录根因，对外折叠成 `Internal`。
fn internal_error(context: &str, error: impl std::fmt::Display) -> RpcErrorData {
    tracing::error!(context, error = %error, "agent RPC internal error");
    RpcErrorData::new(ApplicationErrorCode::Internal, false)
}

fn to_value<T: serde::Serialize>(value: T) -> Result<serde_json::Value, RpcErrorData> {
    serde_json::to_value(value).map_err(|error| internal_error("serialize RPC result", error))
}

/// 在阻塞线程上执行同步 OS 集成调用。
async fn platform_blocking<T: Send + 'static>(
    action: impl FnOnce() -> Result<T, PlatformError> + Send + 'static,
) -> Result<T, RpcErrorData> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|error| internal_error("platform task join", error))?
        .map_err(platform_error_data)
}

/// 系统交来的链接 / 文件因关联已关闭而未建任务。
fn ignored_capture() -> serde_json::Value {
    serde_json::json!({ "ignored": true })
}

/// 应用系统集成变更后返回最新 `PlatformIntegrationDto`。
async fn platform_integration_apply(
    action: impl FnOnce() -> Result<(), PlatformError> + Send + 'static,
) -> Result<serde_json::Value, RpcErrorData> {
    platform_blocking(move || {
        action()?;
        Ok(crate::platform::integration_status())
    })
    .await
    .and_then(to_value)
}

fn platform_error_data(error: PlatformError) -> RpcErrorData {
    match error {
        PlatformError::InvalidScheme(_) => invalid_field("scheme"),
        PlatformError::Unsupported(_) => {
            RpcErrorData::new(ApplicationErrorCode::Unsupported, false)
        }
        PlatformError::NotFound(path) => {
            tracing::debug!(path = %path.display(), "platform target is missing");
            RpcErrorData::new(ApplicationErrorCode::NotFound, false)
        }
        error @ (PlatformError::Io(_) | PlatformError::Failed(_)) => {
            internal_error("platform action", error)
        }
    }
}

/// 读取待上传的本机文件；必须是存在的普通文件且不超过 daemon 请求体上限。
async fn read_upload_file(path: &Path) -> Result<Vec<u8>, RpcErrorData> {
    let metadata = tokio::fs::metadata(path).await.map_err(|error| {
        tracing::debug!(path = %path.display(), error = %error, "upload file is not readable");
        invalid_field("path")
    })?;
    if !metadata.is_file() || metadata.len() > BLOB_UPLOAD_LIMIT {
        return Err(invalid_field("path"));
    }
    tokio::fs::read(path)
        .await
        .map_err(|error| internal_error("read upload file", error))
}

fn blob_value(result: Result<String, BlobError>) -> Result<String, RpcErrorData> {
    result.map_err(|error| match error {
        BlobError::Status(401 | 403) => {
            RpcErrorData::new(ApplicationErrorCode::Unauthorized, false)
        }
        BlobError::Status(413) => invalid_field("path"),
        BlobError::Http(_) | BlobError::Status(_) => {
            RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
        }
        BlobError::Url(_) | BlobError::Decode => {
            RpcErrorData::new(ApplicationErrorCode::Internal, false)
        }
    })
}

/// 32 字节随机用户 token 的十六进制表示。
fn generate_user_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// 管理 API / MCP 端点强制鉴权（空 token → 403）：任一开关由关转开且当前用户 token
/// 为空时生成随机 token，避免开启即全部请求被拒。已有 token 一律保留；
/// 显式清空 token 时的开关联动关闭由 `gateway_patch` 处理。
pub(crate) fn ensure_forced_auth_token(
    state: &mut crate::state::AgentState,
    api_was_enabled: bool,
    mcp_was_enabled: bool,
) {
    let turned_on = (state.gateway.api_enabled && !api_was_enabled)
        || (state.gateway.mcp_enabled && !mcp_was_enabled);
    if turned_on && state.gateway_user_token.trim().is_empty() {
        state.gateway_user_token = generate_user_token();
        state.gateway.user_token_configured = true;
    }
}

/// 局域网暴露面必须有 token：局域网监听且接管 / aria2 兼容端点任一开启时，空 token 等于向
/// 同网段匿名开放（这两个端点空 token 不鉴权）。CORS 放开不在此列：等同 aria2 的
/// `--rpc-allow-origin-all`，用户可显式选择空 token 开启（提交仍弹确认框）。缺 token 时生成
/// 随机 token，返回是否发生了改动；已有 token 一律保留。server 模式的空密钥表示尚未完成
/// 首次设置，由兼容 API 自身拒绝，调用方不得对它调用本函数。
pub(crate) fn ensure_exposed_auth_token(state: &mut crate::state::AgentState) -> bool {
    let exposed = state.gateway.lan_enabled
        && (state.gateway.takeover_enabled || state.gateway.jsonrpc_enabled);
    if exposed && state.gateway_user_token.trim().is_empty() {
        state.gateway_user_token = generate_user_token();
        state.gateway.user_token_configured = true;
        return true;
    }
    false
}

fn remote_value<T: serde::Serialize>(
    result: Result<T, RemoteError>,
) -> Result<serde_json::Value, RpcErrorData> {
    result.map_err(remote_error_data).and_then(to_value)
}

fn remote_error_data(error: RemoteError) -> RpcErrorData {
    match error {
        RemoteError::Daemon(error) => error,
        RemoteError::Cloud(error) => cloud_error_data(error),
        RemoteError::NotFound(_) => RpcErrorData::new(ApplicationErrorCode::NotFound, false),
        RemoteError::TaskUnavailable(_) => RpcErrorData::new(ApplicationErrorCode::Conflict, false)
            .with_reason(fluxdown_protocol::ErrorReason::TaskStateConflict),
        RemoteError::InvalidArgument {
            field,
            reason,
            message,
        } => {
            tracing::debug!(field, %message, "remote RPC argument rejected");
            let mut data = invalid_field(field);
            data.reason = reason;
            data
        }
        RemoteError::InvalidAction(action) => {
            tracing::debug!(%action, "unknown remote task action");
            invalid_field("action")
        }
        RemoteError::Json(error) => internal_error("remote task response", error),
        RemoteError::State(error) => internal_error("agent state", error),
        RemoteError::Protocol(message) => internal_error("remote task protocol", message),
        RemoteError::Kicked => RpcErrorData::new(ApplicationErrorCode::Conflict, false)
            .with_reason(fluxdown_protocol::ErrorReason::OnlineDeviceLimit),
    }
}

fn params_or_empty(params: Option<serde_json::Value>) -> serde_json::Value {
    params.unwrap_or_else(|| serde_json::json!({}))
}

fn cloud_value<T: serde::Serialize>(
    result: Result<T, CloudError>,
) -> Result<serde_json::Value, RpcErrorData> {
    result.map_err(cloud_error_data).and_then(to_value)
}

/// 云端错误 → RPC 错误（`reason` 承载云端错误码的细分含义，契约 §3）。原始消息只进日志。
fn cloud_error_data(error: CloudError) -> RpcErrorData {
    tracing::debug!(
        status = ?error.status,
        code = ?error.code,
        message = %error.message,
        "FluxCloud request failed"
    );
    error.to_rpc_error()
}

fn sync_value(
    result: Result<(), crate::sync::SyncError>,
) -> Result<serde_json::Value, RpcErrorData> {
    result
        .map(|()| serde_json::json!({ "ok": true }))
        .map_err(sync_error_data)
}

fn sync_error_data(error: crate::sync::SyncError) -> RpcErrorData {
    use crate::sync::SyncError;
    match error {
        SyncError::Cloud(error) => cloud_error_data(error),
        SyncError::Daemon(error) => error,
        SyncError::InvalidValue(message) => {
            tracing::debug!(%message, "rejected synced preference value");
            RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
        }
        SyncError::UnknownKey(key) => {
            tracing::debug!(%key, "rejected a key outside the sync catalog");
            invalid_field("keys")
        }
        SyncError::Disabled => RpcErrorData::new(ApplicationErrorCode::Conflict, false),
        SyncError::AccountChanged => RpcErrorData::new(ApplicationErrorCode::Unavailable, true),
        SyncError::Protocol(message) => internal_error("config sync protocol", message),
        SyncError::State(error) => internal_error("agent state", error),
    }
}

#[derive(Clone)]
struct GatewayState {
    service: Arc<GatewayService>,
    bearer: Arc<str>,
    cancel: CancellationToken,
    /// server 模式：浏览器鉴权与访问密钥；桌面形态为 `None`。
    server: Option<Arc<ServerHandle>>,
}

/// 在同一 listener 合并兼容 API 与官方 `/rpc`；server 模式再合并初始化 / 文件面 / SPA。
#[allow(
    clippy::too_many_arguments,
    reason = "gateway composition explicitly carries the private endpoint publication directory"
)]
pub async fn serve(
    listener: TcpListener,
    service: Arc<GatewayService>,
    api_host: Arc<AgentApiHost>,
    api_config: fluxdown_api::server::ApiServerConfig,
    bearer: String,
    cancel: CancellationToken,
    server: Option<Arc<ServerHandle>>,
    endpoint_dir: Option<PathBuf>,
) -> Result<(), std::io::Error> {
    let state = GatewayState {
        service: service.clone(),
        bearer: Arc::from(bearer.as_str()),
        cancel: cancel.clone(),
        server: server.clone(),
    };
    let rpc = Router::new()
        .route("/rpc", get(rpc_upgrade))
        .with_state(state);
    let app = fluxdown_api::server::api_router(api_host, api_config).merge(rpc);
    // SPA fallback 只在 server 模式挂载，且 API / `/rpc` 路由优先。
    let app = match server {
        Some(server) => app.merge(crate::server_mode::router(server)),
        None => app,
    };
    service
        .listener_control
        .run(listener, app, bearer, cancel, endpoint_dir)
        .await
}

pub async fn load_or_create_bearer(
    data_dir: &Path,
    override_path: Option<&Path>,
) -> Result<String, std::io::Error> {
    let path = override_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| data_dir.join("agent.token"));
    if let Ok(value) = tokio::fs::read_to_string(&path).await
        && !value.trim().is_empty()
    {
        return Ok(value.trim().to_owned());
    }
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let parent = path.parent().unwrap_or(data_dir);
    tokio::fs::create_dir_all(parent).await?;
    if override_path.is_none() {
        crate::state::set_private_dir_permissions(parent).await?;
    }
    let temp = temporary_path(&path);
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .await?;
    file.write_all(token.as_bytes()).await?;
    file.write_all(b"\n").await?;
    file.sync_all().await?;
    drop(file);
    crate::state::set_private_file_permissions(&temp).await?;
    crate::state::apply_windows_acl(&temp)
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    tokio::fs::rename(temp, path).await?;
    Ok(token)
}

async fn rpc_upgrade(
    State(state): State<GatewayState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    // 凭据种类与「本机来源」都在建连时一次判定，整条连接沿用：桌面宿主的 `agent.token` 本就在
    // 本机；server 模式见 `ServerHandle::local_platform_permitted`（字面本机来源或显式 env 放行）。
    // 设备控制凭据（局域网配对的远程设备）两种模式下都不是本机来源。
    let (upgrade, session) = match state.server.as_deref() {
        Some(server) => {
            let credential = match server
                .authorize_rpc_from(peer.ip(), &headers, &state.bearer)
                .await
            {
                Ok(credential) => credential,
                Err(status) => return status.into_response(),
            };
            let allow_local_platform = !matches!(credential, RpcCredential::Device { .. })
                && server.local_platform_permitted(peer.ip(), &headers);
            // 浏览器经子协议携带密钥：必须回显 `fluxdown.rpc.v1`，否则浏览器会断开握手。
            (
                upgrade.protocols([crate::server_mode::RPC_SUBPROTOCOL]),
                SessionContext::new(allow_local_platform, credential),
            )
        }
        None => {
            let session = if authorized(&headers, &state.bearer) {
                SessionContext::new(true, RpcCredential::Agent)
            } else {
                match authorize_device(&state.service, &headers).await {
                    Some(credential) => SessionContext::new(false, credential),
                    None => return StatusCode::UNAUTHORIZED.into_response(),
                }
            };
            (upgrade, session)
        }
    };
    upgrade
        .on_upgrade(move |socket| run_socket(socket, state.service, state.cancel, session))
        .into_response()
}

/// 桌面模式：`Authorization: Bearer` 携带设备控制凭据，且互联服务校验通过（名册里该设备被授权
/// 控制本机、未被撤销、HMAC 一致）才接受。
async fn authorize_device(service: &GatewayService, headers: &HeaderMap) -> Option<RpcCredential> {
    let token = crate::server_mode::bearer_token(headers)?;
    let fingerprint = service
        .link
        .as_ref()?
        .verify_control_credential(token)
        .await?;
    Some(RpcCredential::Device {
        token: token.to_owned(),
        fingerprint,
    })
}

/// 连接循环只读套接字、受理请求，并把应答 / 控制帧放进有序出站通道；套接字写端由
/// [`run_writer`] 独占，事件经 [`EventPump`] 合并后由它写出（见 `event_pump` 模块）。
/// 因此慢客户端（套接字写慢）不会让广播接收停摆，也就不会因落后而 4009 断线重拉快照。
async fn run_socket(
    socket: WebSocket,
    service: Arc<GatewayService>,
    cancel: CancellationToken,
    session: SessionContext,
) {
    let mut ready = false;
    let mut ui_client = false;
    let mut ui_snapshot_ready = false;
    let (sink, mut incoming_frames) = socket.split();
    let (out, outbox) = tokio::sync::mpsc::channel::<Message>(OUTBOX_CAPACITY);
    let mut events = EventPump::new();
    let writer = tokio::spawn(run_writer(sink, outbox, events.reader()));
    // 请求按「通道」并发处理：同一通道内严格按到达顺序，慢的云端 RPC 不再阻塞事件转发
    // 与 daemon 命令。
    let mut lanes: Option<RequestLanes> = None;
    let mut responses: Option<tokio::sync::mpsc::Receiver<RpcResponse>> = None;
    // 撤销订阅：访问密钥被改 / 名册里设备记录变化之后重新校验本会话的凭据。
    let mut key_changes = matches!(session.auth.credential(), RpcCredential::AccessKey(_))
        .then(|| service.access_key_epoch.subscribe());
    let mut roster_changes = match (session.auth.credential(), &service.link) {
        (RpcCredential::Device { .. }, Some(link)) => Some(link.subscribe_roster()),
        _ => None,
    };
    // 订阅之后补一次校验：覆盖「升级时通过校验、订阅前凭据已被撤销」的窗口。
    if !service.session_still_valid(&session.auth).await {
        send_close(
            &out,
            CLOSE_CODE_CREDENTIAL_REVOKED,
            CLOSE_REASON_CREDENTIAL_REVOKED,
        )
        .await;
        finish_writer(out, writer).await;
        return;
    }
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                // 完全退出时客户端必须停止重连 / 重拉；仅 agent 退出（SIGTERM）时允许重拉；
                // headless 服务器为更新重启时浏览器保持重连（见 `Lifecycle::close_reason`）。
                send_close(&out, 1001, service.local.lifecycle.close_reason()).await;
                break;
            }
            // 写端已退出（写失败或 4009 已发出）：连接结束。
            () = out.closed() => break,
            incoming = incoming_frames.next() => {
                let Some(Ok(Message::Text(text))) = incoming else { break; };
                let request = match serde_json::from_str::<RpcRequest>(&text) {
                    Ok(request) => request,
                    Err(error) => {
                        let response = RpcResponse::parse_failure(error.to_string());
                        if send_json(&out, &response).await.is_err() { break; }
                        continue;
                    }
                };
                if !ready && request.method == method::SYSTEM_SHUTDOWN && request.validate().is_ok() {
                    // 握手前也受理：协议版本不兼容的新桌面程序靠它替换旧 agent。
                    let response = RpcResponse::success(request.id, serde_json::json!({ "ok": true }));
                    if send_json(&out, &response).await.is_err() {
                        tracing::debug!("gateway shutdown acknowledgement failed; quit not requested");
                        break;
                    }
                    service.local.lifecycle.request_quit();
                    continue;
                }
                if !ready {
                    let id = request.id.clone();
                    match validate_first_request(&request, ServiceRole::Agent) {
                        Ok(hello) => {
                            ready = true;
                            ui_client = declares_selection_ui(&hello);
                            if ui_client { service.ui_connected().await; }
                            let (receiver, session_lanes, response_rx) = service.open_session(&session);
                            events.attach(receiver, MAX_PENDING_FRAMES);
                            lanes = Some(session_lanes);
                            responses = Some(response_rx);
                            let result = match serde_json::to_value(service.session_hello(session.allow_local_platform)) {
                                Ok(result) => result,
                                Err(_) => break,
                            };
                            if send_json(&out, &RpcResponse::success(id, result)).await.is_err() { break; }
                        }
                        Err(data) => {
                            let response = RpcResponse::failure(id, RpcErrorObject::application("hello rejected", data));
                            if send_json(&out, &response).await.is_err() { break; }
                        }
                    }
                    continue;
                }
                // CaptureTasksStarted 是不进快照的瞬时事件。官方 UI 已订阅后，先成功应答
                // 首个快照，再补投冷启动意图；提前在 hello 时发布会被快照的 sequence 吞掉。
                if ui_client && !ui_snapshot_ready && request.method == method::SYSTEM_SNAPSHOT {
                    let response = service.call_with(&session, request).await;
                    let succeeded = matches!(&response, RpcResponse::Success(_));
                    if send_json(&out, &response).await.is_err() { break; }
                    if succeeded {
                        service.local.shell.ui_snapshot_ready();
                        ui_snapshot_ready = true;
                    }
                    continue;
                }
                if let Some(lanes) = &lanes
                    && let Some(rejected) = lanes.submit(request)
                    && send_json(&out, &rejected).await.is_err()
                {
                    break;
                }
            }
            response = receive_response(&mut responses), if responses.is_some() => {
                if let Some(response) = response
                    && send_json(&out, &response).await.is_err()
                {
                    break;
                }
            }
            () = wait_changed(&mut key_changes), if key_changes.is_some() => {
                if !service.session_still_valid(&session.auth).await {
                    send_close(&out, CLOSE_CODE_CREDENTIAL_REVOKED, CLOSE_REASON_CREDENTIAL_REVOKED).await;
                    break;
                }
            }
            () = wait_changed(&mut roster_changes), if roster_changes.is_some() => {
                if !service.session_still_valid(&session.auth).await {
                    send_close(&out, CLOSE_CODE_CREDENTIAL_REVOKED, CLOSE_REASON_CREDENTIAL_REVOKED).await;
                    break;
                }
            }
            completed = receive_lane_exit(&mut lanes), if lanes.is_some() => {
                match completed {
                    Some(Err(error)) => tracing::error!(error = %error, "gateway request lane panicked"),
                    Some(Ok(())) => tracing::warn!("gateway request lane stopped unexpectedly"),
                    None => tracing::warn!("gateway request lanes disappeared"),
                }
                break;
            }
        }
    }
    finish_writer(out, writer).await;
    drop(events);
    if ui_client {
        service.ui_disconnected(ui_snapshot_ready).await;
    }
    drop(responses);
    if let Some(lanes) = lanes {
        lanes.shutdown().await;
    }
}

/// 应答序列化后放进出站通道；写端已退出时返回 `Err`。
async fn send_json<T: serde::Serialize>(
    out: &tokio::sync::mpsc::Sender<Message>,
    value: &T,
) -> Result<(), ()> {
    let text = serde_json::to_string(value).map_err(|_| ())?;
    out.send(Message::Text(text.into())).await.map_err(|_| ())
}

async fn send_close(out: &tokio::sync::mpsc::Sender<Message>, code: u16, reason: &'static str) {
    let close = Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }));
    if out.send(close).await.is_err() {
        tracing::debug!(close_code = code, "gateway peer closed before close frame");
    }
}

async fn receive_response(
    receiver: &mut Option<tokio::sync::mpsc::Receiver<RpcResponse>>,
) -> Option<RpcResponse> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// 单个连接上待写回的响应队列容量。
const RESPONSE_QUEUE: usize = 256;
/// 每个通道待处理请求上限；超出立即以可重试的 `Unavailable` 拒绝，而不是阻塞整个连接。
const LANE_QUEUE: usize = 128;

/// 握手声明了 `client.selections` 的是官方 UI：计入 UI 在线数并接收选择请求。
fn declares_selection_ui(hello: &ClientHello) -> bool {
    hello
        .capabilities
        .iter()
        .any(|capability| capability == method::CAPABILITY_CLIENT_SELECTIONS)
}

impl GatewayService {
    /// 握手通过后的连接装配：事件订阅、请求通道与响应队列（WebSocket 与进程内连接共用）。
    fn open_session(
        self: &Arc<Self>,
        session: &SessionContext,
    ) -> (
        tokio::sync::broadcast::Receiver<EventFrame>,
        RequestLanes,
        tokio::sync::mpsc::Receiver<RpcResponse>,
    ) {
        let events = self.events.subscribe();
        let (response_tx, response_rx) = tokio::sync::mpsc::channel(RESPONSE_QUEUE);
        let lanes = RequestLanes::spawn(self, response_tx, session);
        (events, lanes, response_rx)
    }

    /// 会话凭据是否仍然有效：`agent.token` / 进程内连接恒有效；访问密钥会话要求所呈密钥仍等于当前
    /// 访问密钥；设备凭据会话要求互联服务按名册重新校验后仍指向同一台设备。
    async fn session_still_valid(&self, auth: &session_auth::SessionAuth) -> bool {
        match auth.credential() {
            RpcCredential::Agent => true,
            RpcCredential::AccessKey(key) => crate::server_mode::key_matches(&self.api_token, &key),
            RpcCredential::Device { token, fingerprint } => match &self.link {
                Some(link) => {
                    link.verify_control_credential(&token).await.as_deref()
                        == Some(fingerprint.as_str())
                }
                None => false,
            },
        }
    }

    /// 本连接的 `system.hello` 应答：在服务级能力之外，只有可在主机上打开 / 定位任务产物的
    /// 连接（本机来源且本构建能启动系统打开器）才带 `agent.openTaskFiles`，客户端据此决定
    /// 是否展示「打开文件 / 在文件夹中显示」；同为本机来源且本构建 / 会话有系统剪贴板时才带
    /// `agent.copyTaskFiles`（「复制文件」）。
    fn session_hello(&self, allow_local_platform: bool) -> ServiceHello {
        let mut hello = self.hello.clone();
        if allow_local_platform && crate::platform::LAUNCHES_PATHS {
            hello
                .capabilities
                .push(method::CAPABILITY_AGENT_OPEN_TASK_FILES.to_owned());
        }
        if allow_local_platform && crate::platform::copies_files_to_clipboard() {
            hello
                .capabilities
                .push(method::CAPABILITY_AGENT_COPY_TASK_FILES.to_owned());
        }
        hello
    }
}

/// 只供 agent 自身调用的 `daemon.*` 方法：UI Gateway 与远端 `/rpc` 的 `daemon.*` 透传一律拒绝。
///
/// - `daemon.bt.guard.cloud.apply`：参数是「agent 已验签」的云名单；放开它等于让任意客户端
///   绕过签名替换封禁名单。
/// - `daemon.bt.guard.evidence.drain`：弹出待上报证据；证据只能经 `bt_guard` 模块（遥测红线例外）
///   上报，任意客户端取走会让证据丢失，且泄露对端 IP 与 infohash。
/// - `daemon.plugin.installPrivate`：来源标记 `private` 只能由 agent 在校验过 FluxCloud 票据包的
///   sha256 之后写入；放开它等于让任意客户端把任意包标成「私有商店」来源。
/// - `daemon.plugin.revokePrivate`：撤回清单与授权范围来自 FluxCloud 私有目录；任意客户端可借它把已装
///   私有插件封锁为 `Revoked`（升级前不可再启用），或伪造 `scope` 解除既有封锁。
fn internal_daemon_method(method_name: &str) -> bool {
    matches!(
        method_name,
        method::DAEMON_BT_GUARD_CLOUD_APPLY
            | method::DAEMON_BT_GUARD_EVIDENCE_DRAIN
            | method::DAEMON_PLUGIN_INSTALL_PRIVATE
            | method::DAEMON_PLUGIN_REVOKE_PRIVATE
    )
}

/// 允许「本机来源」在 server 模式下调用的 `agent.platform.*` 子集。
///
/// 只放行「与本机文件系统交互、但无系统级副作用」的打开 / 定位 / 复制任务产物到剪贴板。**不含**
/// 自启 / 文件关联 / 协议注册 / `openPath` 之类可写注册表或任意路径的动作——那些仍
/// 一律拒绝，即使来源是本机。
fn local_platform_method_allowed(method: &str) -> bool {
    matches!(
        method,
        fluxdown_protocol::method::AGENT_PLATFORM_OPEN_TASK
            | fluxdown_protocol::method::AGENT_PLATFORM_REVEAL_TASK
            | fluxdown_protocol::method::AGENT_PLATFORM_COPY_TASK_FILES
    )
}

/// headless 宿主不提供桌面集成：打开路径、写注册表 / 关联、请求管理员授权、打开系统设置与
/// 测试通知的诊断动作，以及可写任意目标路径的日志导出。Web 的 Doctor 仍可用其余动作（刷新
/// tracker / ed2k 服务器、修复托管组件执行权限等），日志导出走 `/api/web/logs/export`。
fn server_mode_denies(request: &RpcRequest, local_log_export: bool) -> bool {
    match request.method.as_str() {
        method::AGENT_DIAGNOSTICS_EXPORT_LOGS => !local_log_export,
        // 提示音只在桌面宿主的本机扬声器播放。
        method::AGENT_SOUND_PREVIEW => true,
        method::AGENT_DIAGNOSTICS_REPAIR => request
            .params
            .as_ref()
            .and_then(|params| params.get("action"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|action| {
                matches!(
                    action,
                    crate::diagnostics::ACTION_OPEN_LOG_DIR
                        | crate::diagnostics::ACTION_REGISTER
                        | crate::diagnostics::ACTION_REREGISTER
                        | crate::diagnostics::ACTION_USE_THIS_INSTALL
                        | crate::diagnostics::ACTION_FIX_DIR_ACCESS
                        | crate::diagnostics::ACTION_ENABLE_AUTOSTART
                        | crate::diagnostics::ACTION_OPEN_SETTINGS
                        | crate::diagnostics::ACTION_TEST_NOTIFICATION
                        | crate::diagnostics::ACTION_FIX_COMSPEC
                )
            }),
        _ => false,
    }
}

/// 请求通道：同一通道内串行（保持命令顺序），不同通道并发。
#[derive(Clone, Copy)]
enum Lane {
    /// FluxCloud / 网络往返（登录、设备、同步、远程任务、账单、更新检查）。
    Cloud,
    /// 透传给 daemon 的下载命令与设置写入，同通道串行以保持命令顺序。
    Daemon,
    /// 透传给 daemon 的长耗时方法（组件安装、插件市场、连通性测试等，清单见
    /// `fluxdown_protocol::method::SLOW_DAEMON_METHODS`）：daemon 本就按请求并发处理，
    /// 这里同样并发下发，不堵住普通 daemon 命令，也不被彼此堵住。
    DaemonSlow,
    /// 其余本机操作。
    Local,
    /// 系统文件图标：列表首屏会一次发出一批，单独成道，不拖慢打开文件等本机操作。
    Icon,
    /// 局域网配对 / 诊断：含最长约 70s 的对端等待或外部探测，单独成道，不堵住心跳与本机设置操作。
    Slow,
    /// Doctor 修复：可能等待用户在系统授权对话框里操作数分钟，单独成道，不堵住配对与重新诊断。
    Repair,
}

fn lane_for(method_name: &str) -> Lane {
    if method_name == method::AGENT_REMOTE_RECONNECT {
        return Lane::Local;
    }
    if method_name.starts_with("daemon.") {
        return if fluxdown_protocol::method::is_slow_daemon_method(method_name) {
            Lane::DaemonSlow
        } else {
            Lane::Daemon
        };
    }
    // 私有包安装 = 云端目录 + 下载（分钟级）+ daemon 安装：与慢 daemon 调用同道并发下发，
    // 不占用串行的 Cloud 通道（登录 / 设备 / 同步等不能被它堵住）。目录 `list` 仍走 Cloud。
    if matches!(
        method_name,
        method::AGENT_CAPTURE_PREVIEW | method::AGENT_PRIVATE_PLUGIN_INSTALL
    ) {
        return Lane::DaemonSlow;
    }
    if method_name == method::AGENT_CAPTURE_CREATE_GROUP {
        return Lane::Daemon;
    }
    if matches!(
        method_name,
        fluxdown_protocol::method::AGENT_PLATFORM_FILE_ICON
            | fluxdown_protocol::method::AGENT_PLATFORM_APP_ICON_IMAGE
    ) {
        return Lane::Icon;
    }
    const CLOUD_PREFIXES: [&str; 11] = [
        "agent.auth.",
        "agent.profile.",
        "agent.device.",
        "agent.plan.",
        "agent.order.",
        "agent.referral.",
        "agent.pluginFavorite.",
        "agent.privatePlugin.",
        "agent.remote.",
        "agent.sync.",
        "agent.update.",
    ];
    if CLOUD_PREFIXES
        .iter()
        .any(|prefix| method_name.starts_with(prefix))
    {
        Lane::Cloud
    } else if method_name == fluxdown_protocol::method::AGENT_DIAGNOSTICS_REPAIR {
        Lane::Repair
    } else if method_name.starts_with("agent.link.")
        || method_name == method::AGENT_SOUND_PREVIEW
        || method_name.starts_with("agent.diagnostics.")
    {
        Lane::Slow
    } else {
        Lane::Local
    }
}

struct RequestLanes {
    cloud: tokio::sync::mpsc::Sender<RpcRequest>,
    daemon: tokio::sync::mpsc::Sender<RpcRequest>,
    daemon_slow: tokio::sync::mpsc::Sender<RpcRequest>,
    local: tokio::sync::mpsc::Sender<RpcRequest>,
    icon: tokio::sync::mpsc::Sender<RpcRequest>,
    slow: tokio::sync::mpsc::Sender<RpcRequest>,
    repair: tokio::sync::mpsc::Sender<RpcRequest>,
    tasks: tokio::task::JoinSet<()>,
}

/// 单条连接同时在途的 daemon 慢调用上限（daemon 侧每连接上限为 16，留出余量）。
const DAEMON_SLOW_CONCURRENCY: usize = 8;

impl RequestLanes {
    fn spawn(
        service: &Arc<GatewayService>,
        responses: tokio::sync::mpsc::Sender<RpcResponse>,
        session: &SessionContext,
    ) -> Self {
        let mut tasks = tokio::task::JoinSet::new();
        let mut start = || {
            let (sender, mut receiver) = tokio::sync::mpsc::channel::<RpcRequest>(LANE_QUEUE);
            let service = Arc::clone(service);
            let responses = responses.clone();
            let session = session.clone();
            tasks.spawn(async move {
                while let Some(request) = receiver.recv().await {
                    let response = service.call_with(&session, request).await;
                    if responses.send(response).await.is_err() {
                        break;
                    }
                }
            });
            sender
        };
        let cloud = start();
        let daemon = start();
        let local = start();
        let icon = start();
        let slow = start();
        let repair = start();
        let (daemon_slow, mut receiver) = tokio::sync::mpsc::channel::<RpcRequest>(LANE_QUEUE);
        let service = Arc::clone(service);
        let session = session.clone();
        tasks.spawn(async move {
            let mut calls = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    completed = calls.join_next(), if !calls.is_empty() => {
                        if let Some(Err(error)) = completed {
                            tracing::error!(error = %error, "concurrent gateway request panicked");
                            break;
                        }
                    }
                    request = receiver.recv(), if calls.len() < DAEMON_SLOW_CONCURRENCY => {
                        let Some(request) = request else { break; };
                        let service = Arc::clone(&service);
                        let responses = responses.clone();
                        let session = session.clone();
                        calls.spawn(async move {
                            let response = service.call_with(&session, request).await;
                            if responses.send(response).await.is_err() {
                                tracing::trace!("gateway connection closed before concurrent response");
                            }
                        });
                    }
                }
            }
            while let Some(completed) = calls.join_next().await {
                if let Err(error) = completed {
                    tracing::error!(error = %error, "concurrent gateway request failed during close");
                }
            }
        });
        Self {
            cloud,
            daemon,
            daemon_slow,
            local,
            icon,
            slow,
            repair,
            tasks,
        }
    }

    async fn shutdown(self) {
        let Self {
            cloud,
            daemon,
            daemon_slow,
            local,
            icon,
            slow,
            repair,
            mut tasks,
        } = self;
        drop((cloud, daemon, daemon_slow, local, icon, slow, repair));
        while let Some(completed) = tasks.join_next().await {
            if let Err(error) = completed {
                tracing::error!(error = %error, "gateway request lane failed during close");
            }
        }
    }

    /// 提交请求；队列已满 / 通道已终止时返回应立即发回的失败响应。
    fn submit(&self, request: RpcRequest) -> Option<RpcResponse> {
        let sender = match lane_for(&request.method) {
            Lane::Cloud => &self.cloud,
            Lane::Daemon => &self.daemon,
            Lane::DaemonSlow => &self.daemon_slow,
            Lane::Local => &self.local,
            Lane::Icon => &self.icon,
            Lane::Slow => &self.slow,
            Lane::Repair => &self.repair,
        };
        let id = request.id.clone();
        match sender.try_send(request) {
            Ok(()) => None,
            Err(error) => {
                tracing::warn!(error = %error, "RPC request lane rejected a request");
                Some(RpcResponse::failure(
                    id,
                    RpcErrorObject::application(
                        "agent RPC failed",
                        RpcErrorData::new(ApplicationErrorCode::Unavailable, true),
                    ),
                ))
            }
        }
    }
}

async fn receive_lane_exit(
    lanes: &mut Option<RequestLanes>,
) -> Option<Result<(), tokio::task::JoinError>> {
    match lanes {
        Some(lanes) => lanes.tasks.join_next().await,
        None => std::future::pending().await,
    }
}

fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    value
        .strip_prefix("Bearer ")
        .is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes()))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("agent.token");
    path.with_file_name(format!(".{name}.{}.tmp", Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::{HeaderMap, HeaderValue, header};
    use fluxdown_protocol::{
        AgentSnapshot, ApplicationErrorCode, RequestId, RpcRequest, RpcResponse,
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    use super::{
        GatewayService, GatewayShell, Lane, authorized, ensure_exposed_auth_token, lane_for,
        load_or_create_bearer,
    };
    #[tokio::test]
    async fn service_bearer_is_exact_stable_and_private() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_agent_token_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create token dir");
        let first = load_or_create_bearer(&dir, None)
            .await
            .expect("create token");
        let second = load_or_create_bearer(&dir, None)
            .await
            .expect("reload token");
        assert_eq!(first, second);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {first}")).expect("header"),
        );
        assert!(authorized(&headers, &first));
        assert!(!authorized(&headers, "different"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("agent.token"))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), %error, "bearer test cleanup failed");
        }
    }

    struct TestGateway {
        service: Arc<GatewayService>,
        state: Arc<tokio::sync::Mutex<crate::state::AgentState>>,
        store: Arc<crate::state::StateStore>,
        api_token: fluxdown_api::auth::TokenCell,
        dir: std::path::PathBuf,
        endpoint_dir: std::path::PathBuf,
        bound_ip: std::net::IpAddr,
        cancel: tokio_util::sync::CancellationToken,
        server_task: tokio::task::JoinHandle<Result<(), std::io::Error>>,
    }

    #[tokio::test]
    async fn independent_connections_discard_profile_and_dispatch_from_previous_account() {
        use axum::routing::{get, post};
        use serde_json::json;

        for profile in [true, false] {
            let started = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let delayed = {
                let started = started.clone();
                let release = release.clone();
                move || {
                    let started = started.clone();
                    let release = release.clone();
                    async move {
                        started.notify_one();
                        release.notified().await;
                        axum::Json(if profile {
                            json!({"id": "A", "email": "", "nickname": "", "plan": "free", "status": "active", "createdAt": "", "entitlements": {}})
                        } else {
                            json!({"task": {"id": "A-task"}})
                        })
                    }
                }
            };
            let app = axum::Router::new()
                .route("/api/v1/me", get(delayed.clone()))
                .route("/api/v1/tasks/dispatch", post(delayed))
                .route("/api/v1/auth/logout", post(|| async { axum::http::StatusCode::NO_CONTENT }))
                .route("/api/v1/auth/login", post(|axum::Json(body): axum::Json<serde_json::Value>| async move {
                    let account = body["account"].as_str().expect("account");
                    axum::Json(json!({
                        "accessToken": format!("access-{account}"),
                        "refreshToken": format!("refresh-{account}"),
                        "expiresIn": 3600,
                        "user": {"id": account, "email": "", "nickname": "", "plan": "free", "status": "active", "createdAt": ""},
                        "entitlements": {},
                        "device": {"id": "row", "deviceId": "device", "name": "", "createdAt": "", "lastSeenAt": ""}
                    }))
                }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let address = listener.local_addr().expect("address");
            let server =
                tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
            let gateway = TestGateway::new("account_response_epoch").await;
            gateway
                .service
                .cloud
                .set_endpoint(&format!("http://{address}"))
                .await
                .expect("endpoint");
            gateway
                .service
                .auth
                .login(&json!({"account": "A"}))
                .await
                .expect("login A");
            let response_epoch = gateway.service.cloud.request_epoch();
            let already_decoded_profile = {
                let state = gateway.state.lock().await;
                let session = state
                    .credentials
                    .as_ref()
                    .expect("A credentials")
                    .session
                    .as_ref()
                    .expect("A session");
                serde_json::to_value(fluxdown_protocol::CloudProfile {
                    user: session.user.clone(),
                    entitlements: session.entitlements.clone(),
                    current_plan: session.current_plan.clone(),
                    purchase_credit_minor: 0,
                    active_subscription: session.active_subscription.clone(),
                })
                .expect("decoded profile fixture")
            };
            // Two independent WS connections have independent lanes. Invoke their
            // shared service concurrently, without an artificial global lane lock.
            let old_connection = async {
                if profile {
                    gateway
                        .service
                        .profile_request(reqwest::Method::GET, "", None, true)
                        .await
                        .is_err()
                } else {
                    gateway
                        .service
                        .remote
                        .dispatch(
                            serde_json::from_value(json!({
                                "toDevice": "other", "url": "https://example.test/file"
                            }))
                            .expect("dispatch params"),
                        )
                        .await
                        .is_err()
                }
            };
            let new_connection = async {
                started.notified().await;
                gateway.service.auth.logout().await.expect("logout A");
                gateway
                    .service
                    .auth
                    .login(&json!({"account": "B"}))
                    .await
                    .expect("login B");
                release.notify_one();
            };
            let (rejected, ()) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(old_connection, new_connection)
            })
            .await
            .expect("independent connections complete");
            assert!(rejected);
            // Also exercise the final commit boundary: transport may already
            // have accepted A's response before B logs in. A transport-only
            // epoch check cannot protect this separate state-lock acquisition.
            let stale_error = gateway
                .service
                .cloud
                .persist_profile(response_epoch, already_decoded_profile)
                .await
                .expect_err("decoded A profile cannot commit to B");
            assert_eq!(stale_error.code.as_deref(), Some("session_changed"));
            let state = gateway.state.lock().await;
            assert_eq!(
                state
                    .credentials
                    .as_ref()
                    .expect("B credentials")
                    .session
                    .as_ref()
                    .expect("B session")
                    .user
                    .id,
                "B"
            );
            assert!(state.remote_tasks.is_empty());
            drop(state);
            let snapshot = super::agent_snapshot(&gateway.service.events).expect("snapshot");
            assert_eq!(snapshot.session.expect("projected B").user.id, "B");
            assert!(snapshot.remote_tasks.is_empty());
            assert!(
                gateway
                    .store
                    .load()
                    .await
                    .expect("reload")
                    .remote_tasks
                    .is_empty()
            );
            server.abort();
        }
    }

    impl TestGateway {
        async fn new(label: &str) -> Self {
            Self::new_mode(label, false).await
        }

        async fn new_mode(label: &str, server_mode: bool) -> Self {
            Self::new_server_mode(label, server_mode, false).await
        }

        async fn new_server_mode(
            label: &str,
            server_mode: bool,
            allow_local_platform: bool,
        ) -> Self {
            Self::new_bound_opts(label, server_mode, allow_local_platform, "127.0.0.1:0").await
        }

        async fn new_bound(label: &str, server_mode: bool, address: &str) -> Self {
            Self::new_bound_opts(label, server_mode, false, address).await
        }

        async fn new_bound_opts(
            label: &str,
            server_mode: bool,
            allow_local_platform: bool,
            address: &str,
        ) -> Self {
            Self::new_bound_link(label, server_mode, allow_local_platform, address, true).await
        }

        /// 不装配直连服务的网关（精简宿主）：不下发 `agent.deviceLink` 能力。
        async fn new_without_link(label: &str) -> Self {
            Self::new_bound_link(label, false, false, "127.0.0.1:0", false).await
        }

        async fn new_bound_link(
            label: &str,
            server_mode: bool,
            allow_local_platform: bool,
            address: &str,
            attach_link: bool,
        ) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fluxdown_agent_{label}_{}_{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let store = Arc::new(
                crate::state::StateStore::open(dir.clone())
                    .await
                    .expect("open agent test store"),
            );
            let listener = tokio::net::TcpListener::bind(address)
                .await
                .expect("bind real test gateway");
            let bound = listener.local_addr().expect("test gateway address");
            let mut initial = crate::state::AgentState::default();
            initial.gateway.port = bound.port();
            initial.gateway.lan_enabled = !bound.ip().is_loopback();
            initial.gateway.lan_active = !bound.ip().is_loopback();
            store.save(&initial).await.expect("save initial gateway");
            let endpoint_dir = dir.join("bearer-location");
            store
                .save_gateway_endpoint(&endpoint_dir, bound)
                .await
                .expect("publish initial endpoint");
            let events = crate::event_hub::AgentEventHub::new(AgentSnapshot {
                gateway: initial.gateway.clone(),
                ..AgentSnapshot::default()
            });
            let state = Arc::new(tokio::sync::Mutex::new(initial));
            let daemon = Arc::new(crate::daemon_client::DaemonClient::disconnected());
            let cloud_client = crate::cloud::CloudClient::new(
                "http://127.0.0.1:9".to_owned(),
                state.clone(),
                store.clone(),
            )
            .expect("build agent test cloud client")
            .with_events(events.clone());
            let cloud_api = crate::cloud::CloudApi::new(cloud_client.clone());
            let auth = Arc::new(crate::cloud::CloudAuthService::new(
                cloud_client,
                events.clone(),
            ));
            let sync = Arc::new(crate::sync::SyncService::new(
                cloud_api.clone(),
                daemon.clone(),
                events.clone(),
                state.clone(),
                store.clone(),
            ));
            let remote = Arc::new(crate::remote::RemoteTaskService::new(
                cloud_api.clone(),
                daemon.clone(),
                events.clone(),
                state.clone(),
                store.clone(),
            ));
            let notify = Arc::new(crate::cloud_notify::CloudNotifyService::new(
                cloud_api.clone(),
                events.clone(),
                state.clone(),
                store.clone(),
                tokio_util::sync::CancellationToken::new(),
            ));
            let shell = crate::shell::ShellState::new(
                crate::shell::TrayAvailability::Unavailable(
                    fluxdown_protocol::TrayUnavailableReason::NotBuilt,
                ),
                daemon.clone(),
                events.clone(),
            );
            let capture = Arc::new(crate::capture::CaptureService::new(
                daemon.clone(),
                events.clone(),
                shell.clone(),
            ));
            let local = GatewayShell {
                shell,
                power: Arc::new(crate::power::PowerService::new(events.clone())),
                lifecycle: Arc::new(crate::lifecycle::Lifecycle::new(
                    tokio_util::sync::CancellationToken::new(),
                    daemon.clone(),
                    Arc::new(crate::supervisor::DaemonSupervisor::new(Some(
                        "127.0.0.1:9".parse().expect("test daemon address"),
                    ))),
                    dir.clone(),
                )),
                notifier: Arc::new(crate::notification::Notifier::new(dir.clone())),
            };
            let daemon_config = crate::daemon_client::DaemonClientConfig::new(
                "ws://127.0.0.1:9/rpc",
                String::new(),
            );
            let blobs = Arc::new(
                crate::capture::DaemonBlobClient::new(&daemon_config).expect("blob client"),
            );
            let api_switches = Arc::new(fluxdown_api::server::ApiRuntimeSwitches::new(
                false, false, false, false, false,
            ));
            let api_token = fluxdown_api::auth::TokenCell::new("");
            let diagnostics = Arc::new(crate::diagnostics::DiagnosticsService::new(
                daemon.clone(),
                daemon_config.clone(),
                events.clone(),
                state.clone(),
                store.clone(),
                api_switches.clone(),
                api_token.clone(),
            ));
            let update = Arc::new(crate::update::UpdateService::unsupported(
                events.clone(),
                dir.join("update-data"),
                crate::egress::Egress::system_default(),
            ));
            let link = crate::link::LinkService::new(crate::link::LinkServiceParts {
                events: events.clone(),
                state: state.clone(),
                store: store.clone(),
                tasks: Arc::new(crate::link::DaemonTaskCreator::new(daemon.clone())),
                bound,
                server_mode,
                network: crate::link::LinkNetworkOptions {
                    mdns: false,
                    advertise: Vec::new(),
                },
            });
            let api_host = Arc::new(crate::api_host::AgentApiHost::new(
                daemon.clone(),
                events.clone(),
                capture.clone(),
                blobs.clone(),
                link.clone(),
                None,
            ));
            let mut api_config = fluxdown_api::server::ApiServerConfig::from_config_map(
                &std::collections::HashMap::new(),
                fluxdown_protocol::APP_VERSION,
            )
            .with_runtime_switches(api_switches.clone());
            api_config.token = api_token.clone();
            api_config.lan_enabled = !bound.ip().is_loopback();
            api_config.enforce_loopback_host = bound.ip().is_loopback();
            let mut gateway = GatewayService::new(
                daemon,
                events.clone(),
                auth,
                Arc::new(cloud_api),
                sync,
                remote,
                notify,
                capture,
                blobs.clone(),
                diagnostics.clone(),
                update,
                state.clone(),
                store.clone(),
                api_switches,
                api_token.clone(),
                local,
            )
            .with_server_mode(server_mode);
            if attach_link {
                gateway = gateway.with_link(link.clone());
            }
            let service = Arc::new(gateway);
            let cancel = tokio_util::sync::CancellationToken::new();
            let server_handle = if server_mode {
                let (_ready_tx, ready_rx) = tokio::sync::watch::channel(true);
                Some(Arc::new(
                    crate::server_mode::ServerHandle::new(crate::server_mode::ServerHandleParts {
                        token: api_token.clone(),
                        state: state.clone(),
                        store: store.clone(),
                        events: events.clone(),
                        ready: ready_rx,
                        blobs: blobs.clone(),
                        diagnostics: diagnostics.clone(),
                        daemon: daemon_config,
                        webroot: None,
                        demo: false,
                        allow_local_platform,
                        link: attach_link.then(|| link.clone()),
                    })
                    .expect("server handle"),
                ))
            } else {
                None
            };
            let server_task = tokio::spawn(super::serve(
                listener,
                service.clone(),
                api_host,
                api_config,
                "test-official-bearer".to_owned(),
                cancel.clone(),
                server_handle,
                Some(endpoint_dir.clone()),
            ));
            super::restart::probe(bound, "test-official-bearer")
                .await
                .expect("initial gateway readiness");
            Self {
                service,
                state,
                store,
                api_token,
                dir,
                endpoint_dir,
                bound_ip: bound.ip(),
                cancel,
                server_task,
            }
        }

        async fn call(&self, method_name: &str, params: serde_json::Value) -> RpcResponse {
            self.service
                .call_with(
                    &super::SessionContext::local(!self.service.server_mode),
                    RpcRequest::new(RequestId::Integer(1), method_name, Some(params)),
                )
                .await
        }

        async fn finish(self) {
            let Self {
                service,
                state,
                store,
                api_token: _,
                dir,
                endpoint_dir: _,
                bound_ip: _,
                cancel,
                server_task,
            } = self;
            cancel.cancel();
            server_task
                .await
                .expect("gateway owner join")
                .expect("gateway owner shutdown");
            drop(service);
            drop(state);
            drop(store);
            if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!(path = %dir.display(), %error, "gateway test cleanup failed");
            }
        }

        async fn patch_gateway(&self, params: serde_json::Value) -> serde_json::Value {
            let response = self
                .call(fluxdown_protocol::method::AGENT_GATEWAY_PATCH, params)
                .await;
            let RpcResponse::Success(success) = response else {
                panic!("gateway patch failed: {response:?}");
            };
            success.result
        }

        async fn user_token(&self) -> String {
            self.state.lock().await.gateway_user_token.clone()
        }

        async fn address(&self) -> std::net::SocketAddr {
            crate::state::gateway_client_address(std::net::SocketAddr::new(
                self.bound_ip,
                self.state.lock().await.gateway.port,
            ))
        }

        async fn endpoint_bytes(&self) -> Vec<u8> {
            tokio::fs::read(self.endpoint_dir.join("gateway-endpoint.json"))
                .await
                .expect("read published endpoint")
        }

        async fn assert_live(&self) {
            let address = self.address().await;
            super::restart::probe(address, "test-official-bearer")
                .await
                .expect("official API stays live");
            let response = reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("local HTTP client")
                .get(format!("http://{address}/ping"))
                .send()
                .await
                .expect("live /ping");
            assert_eq!(response.status(), axum::http::StatusCode::OK);
        }

        async fn assert_unchanged(
            &self,
            before: &fluxdown_protocol::GatewayStatusDto,
            endpoint: &[u8],
            sequence: u64,
        ) {
            self.assert_live().await;
            assert_eq!(&self.state.lock().await.gateway, before);
            assert_eq!(
                &self.store.load().await.expect("persisted rollback").gateway,
                before
            );
            assert_eq!(self.endpoint_bytes().await, endpoint);
            assert_eq!(self.user_token().await, "original-token");
            assert_eq!(&*self.api_token.get(), "original-token");
            assert_eq!(self.service.events.snapshot().sequence, sequence);
        }
    }

    #[tokio::test]
    async fn gateway_port_switch_preserves_wildcard_and_ipv6_listener_addresses() {
        for address in ["0.0.0.0:0", "[::1]:0"] {
            let reservation = match tokio::net::TcpListener::bind(address).await {
                Ok(listener) => listener,
                Err(error)
                    if address.starts_with('[')
                        && matches!(
                            error.kind(),
                            std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
                        ) =>
                {
                    continue;
                }
                Err(error) => panic!("reserve interface {address}: {error}"),
            };
            let port = reservation.local_addr().expect("interface port").port();
            let harness = TestGateway::new_bound("gateway_interface_rebind", false, address).await;
            drop(reservation);
            let previous = harness.address().await;
            harness
                .patch_gateway(serde_json::json!({ "port": port }))
                .await;
            harness.assert_live().await;
            assert!(
                tokio::net::TcpStream::connect(previous).await.is_err(),
                "old interface listener must close"
            );
            let endpoint: serde_json::Value =
                serde_json::from_slice(&harness.endpoint_bytes().await)
                    .expect("interface endpoint");
            assert_eq!(
                endpoint["rpcUrl"],
                format!("ws://{}/rpc", harness.address().await)
            );
            harness.finish().await;
        }
    }

    type TestSocket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn reserve_port() -> (tokio::net::TcpListener, u16) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve test port");
        let port = listener.local_addr().expect("reserved address").port();
        (listener, port)
    }

    async fn socket_response(socket: &mut TestSocket, id: i64) -> RpcResponse {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let message = socket
                    .next()
                    .await
                    .expect("RPC response frame")
                    .expect("RPC socket read");
                let value: serde_json::Value =
                    serde_json::from_str(message.to_text().expect("RPC text")).expect("RPC JSON");
                if value.get("method").and_then(serde_json::Value::as_str)
                    == Some(fluxdown_protocol::method::SERVICE_EVENT)
                {
                    continue;
                }
                let response: RpcResponse = serde_json::from_value(value).expect("RPC response");
                let response_id = match &response {
                    RpcResponse::Success(response) => Some(&response.id),
                    RpcResponse::Failure(response) => response.id.as_ref(),
                };
                assert_eq!(response_id, Some(&RequestId::Integer(id)));
                return response;
            }
        })
        .await
        .expect("bounded RPC response")
    }

    async fn socket_request(
        socket: &mut TestSocket,
        id: i64,
        method: &str,
        params: serde_json::Value,
    ) -> RpcResponse {
        let request = RpcRequest::new(RequestId::Integer(id), method, Some(params));
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&request)
                    .expect("serialize request")
                    .into(),
            ))
            .await
            .expect("send RPC request");
        socket_response(socket, id).await
    }

    #[tokio::test]
    async fn gateway_port_patch_rebinds_immediately_and_returns_over_the_old_socket() {
        let harness = TestGateway::new("gateway_port_rebind").await;
        let previous = harness.address().await;
        let (reserved, port) = reserve_port().await;
        drop(reserved);
        let mut request = format!("ws://{previous}/rpc")
            .into_client_request()
            .expect("old RPC URL");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer test-official-bearer"),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("old official socket");
        let hello = socket_request(
            &mut socket,
            1,
            fluxdown_protocol::method::SYSTEM_HELLO,
            serde_json::json!({
                "clientName": "port-regression", "clientVersion": fluxdown_protocol::APP_VERSION,
                "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
                "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
                "requestedRole": "agent", "capabilities": []
            }),
        )
        .await;
        assert!(matches!(hello, RpcResponse::Success(_)));
        let response = socket_request(
            &mut socket,
            2,
            fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
            serde_json::json!({ "port": port, "userToken": "new-token" }),
        )
        .await;
        let RpcResponse::Success(success) = response else {
            panic!("rebind must return over old WS: {response:?}");
        };
        assert_eq!(success.result["port"], port);
        harness.assert_live().await;
        assert!(
            tokio::net::TcpStream::connect(previous).await.is_err(),
            "old acceptor must already be closed"
        );
        let persisted = harness.store.load().await.expect("rebound persisted state");
        assert_eq!(persisted.gateway.port, port);
        assert_eq!(persisted.gateway_user_token, "new-token");
        assert_eq!(
            super::agent_snapshot(&harness.service.events)
                .expect("published snapshot")
                .gateway
                .port,
            port
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&harness.endpoint_bytes().await)
                .expect("endpoint JSON")["rpcUrl"],
            format!("ws://127.0.0.1:{port}/rpc")
        );
        let ping = socket_request(
            &mut socket,
            3,
            fluxdown_protocol::method::SYSTEM_PING,
            serde_json::json!({}),
        )
        .await;
        assert!(matches!(ping, RpcResponse::Success(response) if response.result["ok"] == true));
        socket.close(None).await.expect("close old official socket");
        harness.finish().await;
    }

    /// 慢消费者（握手后不读套接字）在进度风暴下不被 4009 断开：转发器及时接收广播并合并，
    /// 恢复读取后收到的帧链按 `judge_frame` 无缺口地追到最后一帧。
    #[tokio::test]
    async fn stalled_websocket_consumer_gets_a_coalesced_gapless_chain() {
        use fluxdown_protocol::{
            DaemonEvent, FrameVerdict, ServiceEvent, WsServerMsg, judge_frame,
        };

        let harness = TestGateway::new("gateway_storm").await;
        let address = harness.address().await;
        let mut request = format!("ws://{address}/rpc")
            .into_client_request()
            .expect("RPC URL");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer test-official-bearer"),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("official socket");
        let hello = socket_request(
            &mut socket,
            1,
            fluxdown_protocol::method::SYSTEM_HELLO,
            serde_json::json!({
                "clientName": "storm-test", "clientVersion": fluxdown_protocol::APP_VERSION,
                "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
                "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
                "requestedRole": "agent", "capabilities": []
            }),
        )
        .await;
        assert!(matches!(hello, RpcResponse::Success(_)));

        // 起点 = 订阅时的游标；之后一帧都不读，数据量远超套接字缓冲，写端必然被卡住。
        let start = harness.service.events.snapshot();
        let epoch = start.epoch.clone();
        let mut cursor = start.sequence;
        let mut last = cursor;
        for step in 0..80_000_usize {
            let progress = DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id: format!("task-{}", step % 50),
                status: 1,
                downloaded_bytes: i64::try_from(step).expect("step"),
                total_bytes: 1 << 40,
                speed: 1,
                file_name: String::new(),
                save_dir: String::new(),
                upload_speed: 0,
                url: String::new(),
                error_message: String::new(),
                uploaded_bytes: 0,
                seeding_status: 0,
                seeding_message: String::new(),
                seeding_time_secs: 0,
            });
            last = harness.service.events.apply_daemon_event(progress).sequence;
            if step % 64 == 0 {
                tokio::task::yield_now().await;
            }
        }

        let mut received = 0_usize;
        tokio::time::timeout(Duration::from_secs(60), async {
            while cursor < last {
                let message = socket
                    .next()
                    .await
                    .expect("event frame")
                    .expect("event socket read");
                let value: serde_json::Value =
                    serde_json::from_str(message.to_text().expect("event text"))
                        .expect("event JSON");
                assert_eq!(
                    value["method"],
                    fluxdown_protocol::method::SERVICE_EVENT,
                    "slow consumer was disconnected: {message:?}"
                );
                let frame: fluxdown_protocol::EventFrame =
                    serde_json::from_value(value["params"].clone()).expect("event frame");
                assert!(matches!(frame.event, ServiceEvent::Agent(_)));
                assert_eq!(
                    judge_frame(&epoch, cursor, &frame),
                    FrameVerdict::Accept,
                    "gap after sequence {cursor}: previous {:?}",
                    frame.previous
                );
                cursor = frame.sequence;
                received += 1;
            }
        })
        .await
        .expect("storm must drain");
        assert!(
            received < 80_000,
            "{received} frames: backlog must be coalesced"
        );
        socket.close(None).await.expect("close official socket");
        harness.finish().await;
    }

    /// 线上快照只带头，任务表经 `system.taskPage` 分页；整表替换的广播帧同样只是头 / 重载宣告，
    /// 任何一帧都不随任务数增长。
    #[tokio::test]
    async fn snapshot_is_a_header_and_tables_page_over_the_socket() {
        use fluxdown_protocol::{
            DaemonEvent, DaemonSnapshot, TABLE_INLINE_LIMIT, TaskDto, WsServerMsg,
        };

        fn task(index: usize) -> TaskDto {
            serde_json::from_value(serde_json::json!({
                "taskId": format!("t{index:05}"), "url": "https://example.com/f",
                "fileName": "f", "saveDir": "/tmp", "status": 2, "downloadedBytes": 0,
                "totalBytes": 0, "createdAt": "1"
            }))
            .expect("task fixture")
        }

        let harness = TestGateway::new("gateway_task_pages").await;
        let total = 2_500_usize;
        harness
            .service
            .events
            .replace_daemon_snapshot(DaemonSnapshot {
                tasks: (0..total).map(task).collect(),
                ..DaemonSnapshot::default()
            });
        let address = harness.address().await;
        let mut request = format!("ws://{address}/rpc")
            .into_client_request()
            .expect("RPC URL");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer test-official-bearer"),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("official socket");
        let hello = socket_request(
            &mut socket,
            1,
            fluxdown_protocol::method::SYSTEM_HELLO,
            serde_json::json!({
                "clientName": "paging-test", "clientVersion": fluxdown_protocol::APP_VERSION,
                "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
                "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
                "requestedRole": "agent", "capabilities": []
            }),
        )
        .await;
        assert!(matches!(hello, RpcResponse::Success(_)));

        let RpcResponse::Success(snapshot) = socket_request(
            &mut socket,
            2,
            fluxdown_protocol::method::SYSTEM_SNAPSHOT,
            serde_json::json!({}),
        )
        .await
        else {
            panic!("snapshot");
        };
        let daemon = &snapshot.result["body"]["snapshot"]["daemon"];
        assert_eq!(daemon["tasks"], serde_json::json!([]));
        assert_eq!(daemon["taskTotal"], total);
        assert!(
            snapshot.result.to_string().len() < 64 * 1024,
            "the snapshot never grows with the task count"
        );

        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        let mut id = 10;
        loop {
            let RpcResponse::Success(page) = socket_request(
                &mut socket,
                id,
                fluxdown_protocol::method::SYSTEM_TASK_PAGE,
                serde_json::json!({ "section": "tasks", "cursor": cursor, "limit": 1000 }),
            )
            .await
            else {
                panic!("task page");
            };
            id += 1;
            assert_eq!(page.result["epoch"], snapshot.result["epoch"]);
            assert_eq!(page.result["total"], total);
            let rows = page.result["tasks"].as_array().expect("rows");
            assert!(rows.len() <= 1000);
            seen.extend(
                rows.iter()
                    .map(|row| row["taskId"].as_str().expect("task id").to_owned()),
            );
            cursor = page.result["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen.len(), total);
        assert!(
            seen.windows(2).all(|pair| pair[0] < pair[1]),
            "ascending, no duplicates"
        );

        // 非法游标是参数错误，不是内部错误。
        let refused = socket_request(
            &mut socket,
            id,
            fluxdown_protocol::method::SYSTEM_TASK_PAGE,
            serde_json::json!({ "section": "queue", "cursor": "not-a-number" }),
        )
        .await;
        assert!(matches!(refused, RpcResponse::Failure(_)));

        // 超过内联上限的整表事件广播成重载宣告。
        harness
            .service
            .events
            .apply_daemon_event(DaemonEvent::Engine(WsServerMsg::TasksSnapshot {
                tasks: (0..=TABLE_INLINE_LIMIT).map(task).collect(),
            }));
        let notice = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let message = socket.next().await.expect("frame").expect("socket read");
                let value: serde_json::Value =
                    serde_json::from_str(message.to_text().expect("text")).expect("json");
                if value["method"] == fluxdown_protocol::method::SERVICE_EVENT {
                    return value["params"]["event"]["event"]["data"].clone();
                }
            }
        })
        .await
        .expect("reload notice");
        assert_eq!(notice["type"], "tablesReloading");
        assert_eq!(notice["data"]["tasks"], TABLE_INLINE_LIMIT + 1);
        socket.close(None).await.expect("close official socket");
        harness.finish().await;
    }

    #[tokio::test]
    async fn invalid_gateway_ports_reject_the_entire_patch() {
        let harness = TestGateway::new("gateway_port_invalid").await;
        harness
            .patch_gateway(serde_json::json!({ "userToken": "original-token" }))
            .await;
        let before = harness.state.lock().await.gateway.clone();
        let sequence = harness.service.events.snapshot().sequence;
        for port in [0, 1023] {
            let response = harness
                .call(
                    fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
                    serde_json::json!({
                        "port": port,
                        "apiEnabled": true,
                        "corsEnabled": true,
                        "userToken": "must-not-apply"
                    }),
                )
                .await;
            let RpcResponse::Failure(failure) = response else {
                panic!("invalid port {port} must reject the whole patch");
            };
            let data = failure.error.data.expect("port error data");
            assert_eq!(data.code, ApplicationErrorCode::InvalidArgument);
            assert_eq!(data.field.as_deref(), Some("port"));
            assert_eq!(harness.state.lock().await.gateway, before);
            assert_eq!(harness.user_token().await, "original-token");
            assert_eq!(&*harness.api_token.get(), "original-token");
            assert_eq!(harness.service.events.snapshot().sequence, sequence);
            let persisted = harness.store.load().await.expect("reload rejected patch");
            assert_eq!(persisted.gateway, before);
            assert_eq!(persisted.gateway_user_token, "original-token");
        }
        harness.finish().await;
    }

    #[tokio::test]
    async fn fixed_gateway_ports_reject_the_entire_patch() {
        for server_mode in [false, true] {
            let harness = TestGateway::new_mode("gateway_port_fixed", server_mode).await;
            harness
                .patch_gateway(serde_json::json!({ "userToken": "original-token1" }))
                .await;
            // 环境固定模式依据启动时发布的 port_editable；server 模式还独立防御。
            harness.state.lock().await.gateway.port_editable = server_mode;
            let before = harness.state.lock().await.gateway.clone();
            let sequence = harness.service.events.snapshot().sequence;
            let response = harness
                .call(
                    fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
                    serde_json::json!({
                        "port": 18080,
                        "jsonrpcEnabled": true,
                        "userToken": "MustNotApply123"
                    }),
                )
                .await;
            let RpcResponse::Failure(failure) = response else {
                panic!("fixed listener must reject a port patch");
            };
            let data = failure.error.data.expect("fixed port error data");
            assert_eq!(data.code, ApplicationErrorCode::InvalidArgument);
            assert_eq!(data.field.as_deref(), Some("port"));
            assert_eq!(harness.state.lock().await.gateway, before);
            assert_eq!(harness.user_token().await, "original-token1");
            assert_eq!(&*harness.api_token.get(), "original-token1");
            assert_eq!(harness.service.events.snapshot().sequence, sequence);
            harness.finish().await;
        }
    }

    #[tokio::test]
    async fn failed_gateway_port_save_rolls_back_without_publishing_success() {
        let harness = TestGateway::new("gateway_port_save_failure").await;
        harness
            .patch_gateway(serde_json::json!({ "userToken": "original-token" }))
            .await;
        let before = harness.state.lock().await.gateway.clone();
        let sequence = harness.service.events.snapshot().sequence;
        let endpoint = harness.endpoint_bytes().await;
        let (reserved, port) = reserve_port().await;
        drop(reserved);
        let path = harness.dir.join("agent-state.json");
        let backup = harness.dir.join("saved-state.json");
        tokio::fs::rename(&path, &backup)
            .await
            .expect("back up state");
        tokio::fs::create_dir(&path)
            .await
            .expect("block atomic rename");
        let response = harness
            .call(
                fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
                serde_json::json!({
                    "port": port,
                    "apiEnabled": true,
                    "userToken": "must-not-apply"
                }),
            )
            .await;
        let RpcResponse::Failure(failure) = response else {
            panic!("failed persistence must not return success");
        };
        assert_eq!(
            failure.error.data.expect("save error data").reason,
            Some(fluxdown_protocol::ErrorReason::GatewayRestartFailed)
        );
        assert_eq!(harness.state.lock().await.gateway, before);
        assert_eq!(harness.user_token().await, "original-token");
        assert_eq!(&*harness.api_token.get(), "original-token");
        assert_eq!(harness.service.events.snapshot().sequence, sequence);
        tokio::fs::remove_dir(&path)
            .await
            .expect("unblock state path");
        tokio::fs::rename(&backup, &path)
            .await
            .expect("restore saved state");
        let persisted = harness.store.load().await.expect("reload original state");
        assert_eq!(persisted.gateway, before);
        assert_eq!(persisted.gateway_user_token, "original-token");
        harness.assert_unchanged(&before, &endpoint, sequence).await;
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "failed replacement must close"
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn occupied_gateway_port_keeps_the_old_authenticated_service_and_token() {
        let harness = TestGateway::new("gateway_port_occupied").await;
        harness
            .patch_gateway(serde_json::json!({ "userToken": "original-token" }))
            .await;
        let before = harness.state.lock().await.gateway.clone();
        let endpoint = harness.endpoint_bytes().await;
        let sequence = harness.service.events.snapshot().sequence;
        let (occupied, port) = reserve_port().await;
        let response = harness.call(fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
            serde_json::json!({ "port": port, "apiEnabled": true, "userToken": "must-not-apply" })).await;
        let RpcResponse::Failure(failure) = response else {
            panic!("occupied port must fail");
        };
        assert_eq!(
            failure.error.data.expect("occupied port reason").reason,
            Some(fluxdown_protocol::ErrorReason::GatewayPortInUse)
        );
        harness.assert_unchanged(&before, &endpoint, sequence).await;
        drop(occupied);
        harness.finish().await;
    }

    #[tokio::test]
    async fn rejected_authenticated_readiness_rolls_back_the_replacement() {
        let harness = TestGateway::new("gateway_probe_failure").await;
        harness
            .patch_gateway(serde_json::json!({ "userToken": "original-token" }))
            .await;
        let before = harness.state.lock().await.gateway.clone();
        let endpoint = harness.endpoint_bytes().await;
        let sequence = harness.service.events.snapshot().sequence;
        let (reserved, port) = reserve_port().await;
        drop(reserved);
        harness.service.listener_control.reject_next_readiness();
        let response = harness.call(fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
            serde_json::json!({ "port": port, "corsEnabled": true, "userToken": "must-not-apply" })).await;
        let RpcResponse::Failure(failure) = response else {
            panic!("failed real authenticated probe must fail patch");
        };
        assert_eq!(
            failure.error.data.expect("readiness reason").reason,
            Some(fluxdown_protocol::ErrorReason::GatewayRestartFailed)
        );
        harness.assert_unchanged(&before, &endpoint, sequence).await;
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "rejected listener must close"
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn endpoint_publication_failure_preserves_the_running_gateway() {
        let harness = TestGateway::new("gateway_endpoint_failure").await;
        harness
            .patch_gateway(serde_json::json!({ "userToken": "original-token" }))
            .await;
        let before = harness.state.lock().await.gateway.clone();
        let endpoint = harness.endpoint_bytes().await;
        let sequence = harness.service.events.snapshot().sequence;
        let path = harness.endpoint_dir.join("gateway-endpoint.json");
        let backup = harness.endpoint_dir.join("saved-endpoint.json");
        tokio::fs::rename(&path, &backup)
            .await
            .expect("back up endpoint");
        tokio::fs::create_dir(&path)
            .await
            .expect("block endpoint publication");
        let (reserved, port) = reserve_port().await;
        drop(reserved);
        let response = harness.call(fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
            serde_json::json!({ "port": port, "apiEnabled": true, "userToken": "must-not-apply" })).await;
        tokio::fs::remove_dir(&path)
            .await
            .expect("unblock endpoint publication");
        tokio::fs::rename(&backup, &path)
            .await
            .expect("restore endpoint");
        let RpcResponse::Failure(failure) = response else {
            panic!("endpoint persistence failure must reject patch");
        };
        assert_eq!(
            failure.error.data.expect("endpoint failure reason").reason,
            Some(fluxdown_protocol::ErrorReason::GatewayRestartFailed)
        );
        harness.assert_unchanged(&before, &endpoint, sequence).await;
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "unpublished listener must close"
        );
        harness.finish().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn state_write_failure_restores_an_already_published_endpoint() {
        use std::os::unix::fs::PermissionsExt;
        let harness = TestGateway::new("gateway_transaction_rollback").await;
        harness
            .patch_gateway(serde_json::json!({ "userToken": "original-token" }))
            .await;
        let before = harness.state.lock().await.gateway.clone();
        let endpoint = harness.endpoint_bytes().await;
        let sequence = harness.service.events.snapshot().sequence;
        let permissions = tokio::fs::metadata(&harness.dir)
            .await
            .expect("state directory mode")
            .permissions();
        tokio::fs::set_permissions(&harness.dir, std::fs::Permissions::from_mode(0o500))
            .await
            .expect("deny state writes");
        // A root-run suite cannot induce this ACL failure; do not rely on root respecting chmod.
        let canary = harness.dir.join("permission-canary");
        match tokio::fs::write(&canary, b"").await {
            Ok(()) => {
                tokio::fs::remove_file(&canary)
                    .await
                    .expect("remove root permission canary");
                tokio::fs::set_permissions(&harness.dir, permissions)
                    .await
                    .expect("restore directory mode");
                harness.finish().await;
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(error) => {
                tokio::fs::set_permissions(&harness.dir, permissions)
                    .await
                    .expect("restore directory mode");
                panic!("unexpected write-denial setup error: {error}");
            }
        }
        let (reserved, port) = reserve_port().await;
        drop(reserved);
        let response = harness.call(fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
            serde_json::json!({ "port": port, "apiEnabled": true, "userToken": "must-not-apply" })).await;
        tokio::fs::set_permissions(&harness.dir, permissions)
            .await
            .expect("restore state write permissions");
        let RpcResponse::Failure(failure) = response else {
            panic!("state persistence failure must reject patch");
        };
        assert_eq!(
            failure.error.data.expect("state failure reason").reason,
            Some(fluxdown_protocol::ErrorReason::GatewayRestartFailed)
        );
        harness.assert_unchanged(&before, &endpoint, sequence).await;
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "rolled-back listener must close"
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn concurrent_port_and_token_patches_commit_in_order_without_mixing_state() {
        let harness = TestGateway::new("gateway_patch_serialization").await;
        let (reserved, port) = reserve_port().await;
        drop(reserved);
        let (port_response, token_response) = tokio::join!(
            harness.call(
                fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
                serde_json::json!({ "port": port, "userToken": "port-token" })
            ),
            harness.call(
                fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
                serde_json::json!({ "userToken": "final-token" })
            ),
        );
        assert!(matches!(port_response, RpcResponse::Success(_)));
        assert!(matches!(token_response, RpcResponse::Success(_)));
        harness.assert_live().await;
        let state = harness
            .store
            .load()
            .await
            .expect("serialized final gateway");
        assert_eq!(state.gateway.port, port);
        assert_eq!(state.gateway_user_token, "final-token");
        assert_eq!(&*harness.api_token.get(), "final-token");
        assert_eq!(
            super::agent_snapshot(&harness.service.events)
                .expect("final snapshot")
                .gateway
                .port,
            port
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn remote_reconnect_requires_login_and_only_acknowledges_the_request() {
        let harness = TestGateway::new("remote_reconnect").await;
        let method = fluxdown_protocol::method::AGENT_REMOTE_RECONNECT;
        let response = harness.call(method, serde_json::json!({})).await;
        assert!(matches!(response, RpcResponse::Failure(_)));
        harness.state.lock().await.credentials = Some(crate::state::CloudCredentials {
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            expires_at_unix: i64::MAX,
            session: None,
        });
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            harness.call(method, serde_json::json!({})),
        )
        .await
        .expect("immediate acknowledgement");
        let RpcResponse::Success(success) = response else {
            panic!("reconnect failed: {response:?}");
        };
        assert_eq!(success.result, serde_json::json!({"accepted": true}));
        let fluxdown_protocol::SnapshotBody::Agent(snapshot) =
            harness.service.events.snapshot().body
        else {
            panic!("agent snapshot");
        };
        assert_eq!(
            snapshot.cloud_connection.state,
            fluxdown_protocol::CloudConnectionState::Disconnected
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn gateway_patch_persists_lan_flag_and_regenerates_token() {
        let harness = TestGateway::new("gateway_patch").await;
        let response = harness
            .call(
                fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
                serde_json::json!({ "lanEnabled": true, "regenerateUserToken": true }),
            )
            .await;
        let RpcResponse::Success(success) = response else {
            panic!("gateway patch failed: {response:?}");
        };
        assert_eq!(success.result["lanEnabled"], serde_json::json!(true));
        assert_eq!(
            success.result["userTokenConfigured"],
            serde_json::json!(true)
        );
        assert!(success.result.get("userToken").is_none());

        let first_token = harness.state.lock().await.gateway_user_token.clone();
        assert_eq!(first_token.len(), 64);
        assert!(first_token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let persisted = harness.store.load().await.expect("reload state");
        assert!(persisted.gateway.lan_enabled);
        assert_eq!(persisted.gateway_user_token, first_token);

        let response = harness
            .call(
                fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
                serde_json::json!({ "regenerateUserToken": true, "userToken": "ignored" }),
            )
            .await;
        assert!(matches!(response, RpcResponse::Success(_)));
        let second_token = harness.state.lock().await.gateway_user_token.clone();
        assert_ne!(second_token, first_token);
        assert_ne!(second_token, "ignored");

        let response = harness
            .call(
                fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
                serde_json::json!({ "userToken": "", "lanEnabled": false }),
            )
            .await;
        let RpcResponse::Success(success) = response else {
            panic!("gateway patch failed: {response:?}");
        };
        assert_eq!(
            success.result["userTokenConfigured"],
            serde_json::json!(false)
        );
        assert_eq!(success.result["lanEnabled"], serde_json::json!(false));
        harness.finish().await;
    }

    #[tokio::test]
    async fn enabling_forced_auth_switch_fills_only_a_missing_token() {
        let harness = TestGateway::new("gateway_forced_auth").await;

        let result = harness
            .patch_gateway(serde_json::json!({ "apiEnabled": true }))
            .await;
        assert_eq!(result["userTokenConfigured"], serde_json::json!(true));
        assert!(result.get("userToken").is_none());
        let generated = harness.user_token().await;
        assert_eq!(generated.len(), 64);
        assert!(generated.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(&*harness.api_token.get(), generated);
        let persisted = harness.store.load().await.expect("reload state");
        assert_eq!(persisted.gateway_user_token, generated);

        // 已有 token：开启另一个强制鉴权开关不得覆盖。
        harness
            .patch_gateway(serde_json::json!({ "mcpEnabled": true }))
            .await;
        assert_eq!(harness.user_token().await, generated);

        // 开关已开时显式清空 token：管理 API 与 MCP 随之关闭，不回填 token。
        let result = harness
            .patch_gateway(serde_json::json!({ "userToken": "" }))
            .await;
        assert_eq!(result["userTokenConfigured"], serde_json::json!(false));
        assert_eq!(result["apiEnabled"], serde_json::json!(false));
        assert_eq!(result["mcpEnabled"], serde_json::json!(false));
        assert!(harness.user_token().await.is_empty());
        assert!(harness.api_token.is_empty());
        let persisted = harness.store.load().await.expect("reload state");
        assert!(!persisted.gateway.api_enabled);
        assert!(!persisted.gateway.mcp_enabled);

        // 管理 API 由关转开且 token 为空 → 重新生成。
        harness
            .patch_gateway(serde_json::json!({ "apiEnabled": true }))
            .await;
        let regenerated = harness.user_token().await;
        assert_eq!(regenerated.len(), 64);
        assert_ne!(regenerated, generated);

        // 同一请求里显式给出的 token 优先。
        harness
            .patch_gateway(serde_json::json!({ "apiEnabled": false, "mcpEnabled": false }))
            .await;
        harness
            .patch_gateway(serde_json::json!({ "apiEnabled": true, "userToken": "custom-token" }))
            .await;
        assert_eq!(harness.user_token().await, "custom-token");
        assert_eq!(&*harness.api_token.get(), "custom-token");

        // 非强制鉴权开关不生成 token。
        harness
            .patch_gateway(serde_json::json!({ "apiEnabled": false, "userToken": "" }))
            .await;
        let result = harness
            .patch_gateway(serde_json::json!({ "takeoverEnabled": true, "jsonrpcEnabled": true }))
            .await;
        assert_eq!(result["userTokenConfigured"], serde_json::json!(false));
        assert!(harness.user_token().await.is_empty());
        harness.finish().await;
    }

    #[tokio::test]
    async fn diagnostics_enable_service_fills_missing_token() {
        let harness = TestGateway::new("diagnostics_enable_service").await;
        let response = harness
            .call(
                fluxdown_protocol::method::AGENT_DIAGNOSTICS_REPAIR,
                serde_json::json!({ "action": crate::diagnostics::ACTION_ENABLE_SERVICE }),
            )
            .await;
        assert!(matches!(response, RpcResponse::Success(_)), "{response:?}");
        let state = harness.state.lock().await.clone();
        assert!(state.gateway.api_enabled);
        assert!(state.gateway.user_token_configured);
        assert_eq!(state.gateway_user_token.len(), 64);
        assert_eq!(&*harness.api_token.get(), state.gateway_user_token);
        let persisted = harness.store.load().await.expect("reload state");
        assert_eq!(persisted.gateway_user_token, state.gateway_user_token);
        harness.finish().await;
    }

    #[tokio::test]
    async fn exposing_compat_endpoints_to_lan_requires_a_token_but_cors_does_not() {
        let harness = TestGateway::new("gateway_exposed_token").await;

        // 只开局域网、兼容端点全关：不必有 token。
        let result = harness
            .patch_gateway(serde_json::json!({
                "takeoverEnabled": false,
                "jsonrpcEnabled": false,
                "lanEnabled": true
            }))
            .await;
        assert_eq!(result["userTokenConfigured"], serde_json::json!(false));
        assert!(harness.user_token().await.is_empty());

        // 兼容端点在局域网下开启：补 token 并同步到运行期。
        let result = harness
            .patch_gateway(serde_json::json!({ "jsonrpcEnabled": true }))
            .await;
        assert_eq!(result["userTokenConfigured"], serde_json::json!(true));
        let generated = harness.user_token().await;
        assert_eq!(generated.len(), 64);
        assert_eq!(&*harness.api_token.get(), generated);
        let persisted = harness.store.load().await.expect("reload state");
        assert_eq!(persisted.gateway_user_token, generated);

        // 暴露面仍在时显式清空 token：重新补一个，不会回到匿名开放。
        harness
            .patch_gateway(serde_json::json!({ "userToken": "" }))
            .await;
        let refilled = harness.user_token().await;
        assert_eq!(refilled.len(), 64);
        assert_ne!(refilled, generated);

        // 同一请求关掉暴露面并清空 token：尊重清空。
        let result = harness
            .patch_gateway(serde_json::json!({ "lanEnabled": false, "userToken": "" }))
            .await;
        assert_eq!(result["userTokenConfigured"], serde_json::json!(false));
        assert!(harness.user_token().await.is_empty());

        // CORS 放开不强制 token：允许空 token 开启。
        let result = harness
            .patch_gateway(serde_json::json!({ "corsEnabled": true }))
            .await;
        assert_eq!(result["userTokenConfigured"], serde_json::json!(false));
        assert!(harness.user_token().await.is_empty());
        harness.finish().await;
    }

    #[test]
    fn exposed_token_is_only_generated_for_exposed_compat_endpoints() {
        let mut state = crate::state::AgentState::default();
        state.gateway.lan_enabled = true;
        state.gateway.takeover_enabled = false;
        state.gateway.jsonrpc_enabled = false;
        assert!(!ensure_exposed_auth_token(&mut state));
        state.gateway.takeover_enabled = true;
        assert!(ensure_exposed_auth_token(&mut state));
        let token = state.gateway_user_token.clone();
        assert_eq!(token.len(), 64);
        assert!(state.gateway.user_token_configured);
        assert!(!ensure_exposed_auth_token(&mut state));
        assert_eq!(state.gateway_user_token, token);
    }

    #[tokio::test]
    async fn file_backed_methods_reject_missing_or_non_regular_paths() {
        let harness = TestGateway::new("file_paths").await;
        for method_name in [
            fluxdown_protocol::method::AGENT_PLUGIN_INSTALL_FILE,
            fluxdown_protocol::method::AGENT_CAPTURE_SUBMIT_TORRENT_FILE,
        ] {
            for path in [
                harness.dir.join("missing.bin").display().to_string(),
                harness.dir.display().to_string(),
            ] {
                let response = harness
                    .call(method_name, serde_json::json!({ "path": path }))
                    .await;
                let RpcResponse::Failure(failure) = response else {
                    panic!("{method_name} accepted {path}");
                };
                let data = failure.error.data.expect("error data");
                assert_eq!(data.code, ApplicationErrorCode::InvalidArgument);
                assert_eq!(data.field.as_deref(), Some("path"));
            }
        }
        harness.finish().await;
    }

    #[tokio::test]
    async fn every_canonical_agent_method_reaches_a_real_dispatch_branch() {
        let harness = TestGateway::new("dispatch").await;
        // An accepting loopback server avoids platform-dependent refused-connect latency.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind cloud fixture");
        let address = listener.local_addr().expect("cloud fixture address");
        let cloud_base = format!("http://{address}");
        let shutdown = tokio_util::sync::CancellationToken::new();
        let stop = shutdown.clone();
        let cloud_server = tokio::spawn(async move {
            axum::serve(listener, axum::Router::new())
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .expect("serve cloud fixture");
        });
        harness
            .service
            .cloud
            .set_endpoint(&cloud_base)
            .await
            .expect("set cloud fixture endpoint");
        let service = &harness.service;

        for (index, method_name) in fluxdown_protocol::method::ALL_METHODS
            .iter()
            .copied()
            .filter(|name| name.starts_with("agent."))
            .enumerate()
        {
            // 版本检查会真的访问官方站点；用非法渠道让它在触网前返回 InvalidArgument。
            let params = if method_name == fluxdown_protocol::method::AGENT_UPDATE_CHECK {
                serde_json::json!({ "channel": "offline-test" })
            } else if method_name == fluxdown_protocol::method::AGENT_CLOUD_ENDPOINT_SET {
                serde_json::json!({ "baseUrl": &cloud_base })
            } else {
                serde_json::json!({})
            };
            let response = tokio::time::timeout(
                Duration::from_secs(2),
                service.call_with(
                    &super::SessionContext::local(!service.server_mode),
                    RpcRequest::new(
                        RequestId::Integer(i64::try_from(index).unwrap_or(i64::MAX)),
                        method_name,
                        Some(params),
                    ),
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("{method_name} dispatch timed out"));
            if let RpcResponse::Failure(failure) = response
                && failure.error.data.as_ref().map(|data| data.code)
                    == Some(ApplicationErrorCode::Unsupported)
            {
                panic!("{method_name} fell through agent dispatch");
            }
        }
        harness.finish().await;
        shutdown.cancel();
        cloud_server.await.expect("join cloud fixture");
    }

    /// `daemon.bt.guard.cloud.apply` 携带「agent 已验签」的云名单、`daemon.bt.guard.evidence.drain` 弹出
    /// 待上报证据、`daemon.plugin.{installPrivate,revokePrivate}` 只接受 agent 校验过的 FluxCloud 私有包 /
    /// 撤回清单：它们只有 agent 自己的模块调用，经 Gateway 的任何会话（本机 UI、访问密钥、
    /// 配对设备）一律拒绝；其余 `daemon.bt.guard.*` 照常透传给 daemon。
    #[tokio::test]
    async fn bt_guard_internal_daemon_methods_are_denied_to_every_session_kind() {
        use super::session_auth::RpcCredential;
        use fluxdown_protocol::method;

        let internal = [
            method::DAEMON_BT_GUARD_CLOUD_APPLY,
            method::DAEMON_BT_GUARD_EVIDENCE_DRAIN,
            method::DAEMON_PLUGIN_INSTALL_PRIVATE,
            method::DAEMON_PLUGIN_REVOKE_PRIVATE,
        ];
        let forwarded = [
            method::DAEMON_BT_GUARD_STATE,
            method::DAEMON_BT_GUARD_ENTRIES_LIST,
            method::DAEMON_BT_GUARD_ENTRIES_ADD,
            method::DAEMON_BT_GUARD_ENTRIES_REMOVE,
            method::DAEMON_BT_GUARD_EVENTS_LIST,
            method::DAEMON_BT_GUARD_EXPORT,
            method::DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
        ];
        for server_mode in [false, true] {
            let harness = TestGateway::new_mode("bt_guard_internal", server_mode).await;
            let sessions = [
                ("local agent", super::SessionContext::local(true)),
                (
                    "access key",
                    super::SessionContext::new(false, RpcCredential::AccessKey("key".to_owned())),
                ),
                (
                    "paired device",
                    super::SessionContext::new(
                        false,
                        RpcCredential::Device {
                            token: "token".to_owned(),
                            fingerprint: "fingerprint".to_owned(),
                        },
                    ),
                ),
            ];
            for (label, session) in &sessions {
                for method_name in internal.iter().chain(forwarded.iter()).copied() {
                    let response = harness
                        .service
                        .call_with(
                            session,
                            RpcRequest::new(
                                RequestId::Integer(1),
                                method_name,
                                Some(serde_json::json!({})),
                            ),
                        )
                        .await;
                    let RpcResponse::Failure(failure) = response else {
                        panic!("{method_name} must fail against the disconnected test daemon");
                    };
                    let data = failure.error.data.expect("application error data");
                    let expected = if internal.contains(&method_name) {
                        // 网关直接拒绝，根本不会触达 daemon。
                        ApplicationErrorCode::Unsupported
                    } else {
                        // 透传给（未连接的）daemon：失败原因是 daemon 不可用，而不是被网关拒绝。
                        ApplicationErrorCode::Unavailable
                    };
                    assert_eq!(
                        data.code, expected,
                        "{method_name} for {label} (server mode: {server_mode})"
                    );
                }
            }
            harness.finish().await;
        }
    }

    #[tokio::test]
    async fn server_mode_rejects_host_diagnostics_and_keeps_web_repairs() {
        let harness = TestGateway::new_mode("server_diagnostics", true).await;
        let repair = fluxdown_protocol::method::AGENT_DIAGNOSTICS_REPAIR;
        let target = harness.dir.join("chosen.zip");
        tokio::fs::write(&target, b"existing archive")
            .await
            .expect("create protected archive");
        let mut denied_repairs = vec![
            serde_json::json!({
                "action": crate::diagnostics::ACTION_OPEN_LOG_DIR,
                "target": harness.dir.display().to_string(),
            }),
            serde_json::json!({ "action": crate::diagnostics::ACTION_REREGISTER }),
            serde_json::json!({ "action": crate::diagnostics::ACTION_USE_THIS_INSTALL }),
            serde_json::json!({ "action": crate::diagnostics::ACTION_FIX_COMSPEC }),
        ];
        for association in ["fluxdown", "magnet", "ed2k", "torrent"] {
            denied_repairs.push(serde_json::json!({
                "action": crate::diagnostics::ACTION_REGISTER,
                "target": association,
            }));
        }
        for params in denied_repairs {
            let response = harness.call(repair, params.clone()).await;
            let RpcResponse::Failure(failure) = response else {
                panic!("host repair must be rejected: {params}");
            };
            assert_eq!(
                failure.error.data.map(|data| (data.code, data.retryable)),
                Some((ApplicationErrorCode::Unsupported, false)),
                "{params}"
            );
        }
        for params in [
            serde_json::json!({ "targetPath": target.display().to_string() }),
            serde_json::json!({}),
        ] {
            let response = harness
                .call(
                    fluxdown_protocol::method::AGENT_DIAGNOSTICS_EXPORT_LOGS,
                    params,
                )
                .await;
            let RpcResponse::Failure(failure) = response else {
                panic!("host log export must be rejected");
            };
            assert_eq!(
                failure.error.data.map(|data| (data.code, data.retryable)),
                Some((ApplicationErrorCode::Unsupported, false))
            );
        }
        assert_eq!(
            tokio::fs::read(&target)
                .await
                .expect("read protected archive"),
            b"existing archive"
        );
        for action in [
            crate::diagnostics::ACTION_REFRESH_TRACKERS,
            crate::diagnostics::ACTION_REFRESH_ED2K_SERVERS,
        ] {
            let response = harness
                .call(repair, serde_json::json!({ "action": action }))
                .await;
            let RpcResponse::Failure(failure) = response else {
                panic!("disconnected daemon must report unavailable");
            };
            assert_eq!(
                failure.error.data.map(|data| (data.code, data.retryable)),
                Some((ApplicationErrorCode::Unavailable, true)),
                "{action}"
            );
        }
        let enabled = harness
            .call(
                repair,
                serde_json::json!({ "action": crate::diagnostics::ACTION_ENABLE_SERVICE }),
            )
            .await;
        assert!(matches!(enabled, RpcResponse::Success(_)), "{enabled:?}");
        assert!(
            harness
                .store
                .load()
                .await
                .expect("reload state")
                .gateway
                .api_enabled
        );
        harness.finish().await;
    }

    #[tokio::test]
    async fn desktop_diagnostics_rejects_non_directory_open_targets() {
        let harness = TestGateway::new("desktop_diagnostics").await;
        let logs = harness.dir.join("logs");
        let nested = logs.join("nested");
        tokio::fs::create_dir_all(&nested)
            .await
            .expect("create log directories");
        let file = logs.join("downloaded-program");
        tokio::fs::write(&file, b"program")
            .await
            .expect("create file in logs");
        for target in [file, nested, std::env::temp_dir(), logs.join("missing")] {
            let response = harness
                .call(
                    fluxdown_protocol::method::AGENT_DIAGNOSTICS_REPAIR,
                    serde_json::json!({
                        "action": crate::diagnostics::ACTION_OPEN_LOG_DIR,
                        "target": target.display().to_string(),
                    }),
                )
                .await;
            let RpcResponse::Failure(failure) = response else {
                panic!(
                    "non-log-directory target must be rejected: {}",
                    target.display()
                );
            };
            assert_eq!(
                failure.error.data.map(|data| data.code),
                Some(ApplicationErrorCode::Internal),
                "{}",
                target.display()
            );
        }
        harness.finish().await;
    }

    #[tokio::test]
    async fn hello_advertises_the_device_link_capability_only_with_a_link_service() {
        let device_link = fluxdown_protocol::method::CAPABILITY_AGENT_DEVICE_LINK;
        let with_link = TestGateway::new("hello-with-link").await;
        assert!(
            with_link
                .service
                .hello
                .capabilities
                .iter()
                .any(|capability| capability == device_link)
        );
        with_link.finish().await;

        // 没有装配直连服务（精简宿主）：不宣告能力，客户端据此隐藏直连入口；RPC 也明确报不支持。
        let without_link = TestGateway::new_without_link("hello-without-link").await;
        let capabilities = &without_link.service.hello.capabilities;
        assert!(
            !capabilities
                .iter()
                .any(|capability| capability == device_link)
        );
        assert!(capabilities.iter().any(|capability| {
            capability == fluxdown_protocol::method::CAPABILITY_AGENT_GATEWAY
        }));
        let response = without_link
            .call(
                fluxdown_protocol::method::AGENT_LINK_PAIRING_CODE,
                serde_json::json!({}),
            )
            .await;
        let RpcResponse::Failure(failure) = response else {
            panic!("link RPC must fail without a link service: {response:?}");
        };
        assert_eq!(
            failure.error.data.map(|data| data.code),
            Some(ApplicationErrorCode::Unsupported)
        );
        without_link.finish().await;
    }

    #[tokio::test]
    async fn gateway_status_distinguishes_the_configured_lan_flag_from_the_actual_binding() {
        // 只监听回环：持久化的 `lanEnabled` 已开（下次启动才生效），但本次运行并不可被局域网访问。
        let loopback = TestGateway::new("lan-active-loopback").await;
        let status = loopback
            .patch_gateway(serde_json::json!({ "lanEnabled": true }))
            .await;
        assert_eq!(status["lanEnabled"], true);
        assert_eq!(status["lanActive"], false);
        loopback.finish().await;

        // 实际监听非回环地址：`lanActive` 为真，且随网关事件 / 状态一起下发。
        let lan = TestGateway::new_bound("lan-active-lan", false, "0.0.0.0:0").await;
        let status = lan
            .patch_gateway(serde_json::json!({ "corsEnabled": false }))
            .await;
        assert_eq!(status["lanActive"], true);
        let published = lan
            .service
            .events
            .inspect(|snapshot| snapshot.gateway.clone());
        assert!(published.lan_active);
        lan.finish().await;
    }

    #[test]
    fn slow_network_methods_do_not_share_the_local_lane() {
        assert!(matches!(lane_for("agent.link.pairFinish"), Lane::Slow));
        assert!(matches!(
            lane_for(fluxdown_protocol::method::AGENT_LINK_CONTROL_CREDENTIAL),
            Lane::Slow
        ));
        assert!(matches!(lane_for("agent.diagnostics.run"), Lane::Slow));
        assert!(matches!(lane_for("system.ping"), Lane::Local));
        assert!(matches!(
            lane_for(fluxdown_protocol::method::AGENT_SOUND_PREVIEW),
            Lane::Slow
        ));
        assert!(matches!(lane_for("agent.remote.reconnect"), Lane::Local));
        for name in [
            fluxdown_protocol::method::AGENT_PLUGIN_FAVORITE_LIST,
            fluxdown_protocol::method::AGENT_PLUGIN_FAVORITE_SET,
            fluxdown_protocol::method::AGENT_PRIVATE_PLUGIN_LIST,
        ] {
            assert!(matches!(lane_for(name), Lane::Cloud), "{name}");
        }
        // 分钟级的私有包安装不占用串行的 Cloud 通道。
        assert!(matches!(
            lane_for(fluxdown_protocol::method::AGENT_PRIVATE_PLUGIN_INSTALL),
            Lane::DaemonSlow
        ));
    }

    #[tokio::test]
    async fn reconnect_lane_bypasses_an_inflight_cloud_request() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let app = axum::Router::new().route(
            "/api/v1/tasks/remote",
            axum::routing::get({
                let entered = entered.clone();
                let release = release.clone();
                move || {
                    let entered = entered.clone();
                    let release = release.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        axum::Json(serde_json::json!({"tasks":[]}))
                    }
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let harness = TestGateway::new("reconnect_lane").await;
        harness
            .service
            .cloud
            .set_endpoint(&format!("http://{address}"))
            .await
            .expect("endpoint");
        harness.state.lock().await.credentials = Some(crate::state::CloudCredentials {
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            expires_at_unix: i64::MAX,
            session: None,
        });
        let service = Arc::new(harness.service);
        let (responses, mut receiver) = tokio::sync::mpsc::channel(4);
        let lanes = super::RequestLanes::spawn(
            &service,
            responses,
            &super::SessionContext::local(!service.server_mode),
        );
        assert!(
            lanes
                .submit(RpcRequest::new(
                    RequestId::Integer(1),
                    "agent.remote.list",
                    None,
                ))
                .is_none()
        );
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .expect("cloud request blocked");
        assert!(
            lanes
                .submit(RpcRequest::new(
                    RequestId::Integer(2),
                    "agent.remote.reconnect",
                    None,
                ))
                .is_none()
        );
        let response = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("reconnect is not queued behind cloud")
            .expect("response");
        let RpcResponse::Success(response) = response else {
            panic!("reconnect failed")
        };
        assert_eq!(response.id, RequestId::Integer(2));
        assert_eq!(response.result, serde_json::json!({"accepted":true}));
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("cloud unblocked")
            .expect("cloud response");
        drop(lanes);
        drop(service);
        server.abort();
        assert!(
            server
                .await
                .expect_err("stopped mock server")
                .is_cancelled()
        );
        drop(harness.state);
        drop(harness.store);
        tokio::fs::remove_dir_all(harness.dir)
            .await
            .expect("cleanup");
    }

    #[test]
    fn slow_daemon_methods_leave_the_ordered_daemon_lane() {
        for name in fluxdown_protocol::method::SLOW_DAEMON_METHODS {
            assert!(matches!(lane_for(name), Lane::DaemonSlow), "{name}");
        }
        for name in [
            fluxdown_protocol::method::DAEMON_TASK_PAUSE,
            fluxdown_protocol::method::DAEMON_TASK_RESUME,
            fluxdown_protocol::method::DAEMON_TASK_CREATE,
            fluxdown_protocol::method::DAEMON_TASK_CREATE_MANY,
            fluxdown_protocol::method::DAEMON_CONFIG_PATCH,
        ] {
            assert!(matches!(lane_for(name), Lane::Daemon), "{name}");
        }
    }

    #[tokio::test]
    async fn server_mode_disables_platform_methods_and_guards_the_access_key() {
        let harness = TestGateway::new_mode("server_mode", true).await;

        for method_name in [
            fluxdown_protocol::method::AGENT_PLATFORM_OPEN_TASK,
            fluxdown_protocol::method::AGENT_PLATFORM_INTEGRATION_GET,
            fluxdown_protocol::method::AGENT_PLATFORM_SET_AUTOSTART,
            fluxdown_protocol::method::AGENT_SOUND_PREVIEW,
        ] {
            let response = harness.call(method_name, serde_json::json!({})).await;
            let RpcResponse::Failure(failure) = response else {
                panic!("{method_name} must be rejected in server mode");
            };
            assert_eq!(
                failure.error.data.map(|data| data.code),
                Some(ApplicationErrorCode::Unsupported),
                "{method_name}"
            );
        }

        // 清空 / 不合规的密钥会重新打开匿名 setup 或让 Web 登录页拒绝自己：一律拒绝。
        for token in ["", "short", "letters-only-key"] {
            let response = harness
                .call(
                    fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
                    serde_json::json!({ "userToken": token }),
                )
                .await;
            assert!(
                matches!(response, RpcResponse::Failure(_)),
                "{token:?} must be rejected"
            );
        }
        assert_eq!(harness.user_token().await, "");

        let accepted = harness
            .patch_gateway(serde_json::json!({ "userToken": "flux2026abc" }))
            .await;
        assert_eq!(accepted["userTokenConfigured"], true);
        assert_eq!(harness.user_token().await, "flux2026abc");
        harness.finish().await;
    }

    #[tokio::test]
    async fn server_mode_lanes_gate_task_file_actions_by_connection_origin() {
        let harness = TestGateway::new_mode("server_mode_local_platform", true).await;
        let method = |name: &str, id: i64| RpcRequest::new(RequestId::Integer(id), name, None);

        for local in [true, false] {
            let (responses, mut receiver) = tokio::sync::mpsc::channel(8);
            let lanes = super::RequestLanes::spawn(
                &harness.service,
                responses,
                &super::SessionContext::local(local),
            );
            let requests = [
                fluxdown_protocol::method::AGENT_PLATFORM_OPEN_TASK,
                fluxdown_protocol::method::AGENT_PLATFORM_REVEAL_TASK,
                fluxdown_protocol::method::AGENT_PLATFORM_COPY_TASK_FILES,
                fluxdown_protocol::method::AGENT_PLATFORM_OPEN_PATH,
                fluxdown_protocol::method::AGENT_PLATFORM_INTEGRATION_GET,
                fluxdown_protocol::method::AGENT_PLATFORM_SET_AUTOSTART,
            ];
            for (id, name) in (1..).zip(requests) {
                assert!(lanes.submit(method(name, id)).is_none(), "{name}");
            }
            let mut codes = std::collections::HashMap::new();
            for _ in requests {
                let response = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                    .await
                    .expect("platform response")
                    .expect("lane open");
                let RpcResponse::Failure(failure) = response else {
                    panic!("platform call without params must fail: {response:?}");
                };
                codes.insert(
                    failure.id.expect("response id"),
                    failure.error.data.map(|data| data.code),
                );
            }
            // 本机来源：打开 / 定位 / 复制任务产物进入参数校验（缺 taskId / taskIds）；远程来源一律 Unsupported。
            let task_files = if local {
                ApplicationErrorCode::InvalidArgument
            } else {
                ApplicationErrorCode::Unsupported
            };
            for (id, name) in (1..).zip(requests) {
                let expected = if id <= 3 {
                    task_files
                } else {
                    ApplicationErrorCode::Unsupported
                };
                assert_eq!(
                    codes.get(&RequestId::Integer(id)).copied().flatten(),
                    Some(expected),
                    "{name} local={local}"
                );
            }
            lanes.shutdown().await;
        }
        let remote_hello = harness.service.session_hello(false);
        for capability in [
            fluxdown_protocol::method::CAPABILITY_AGENT_OPEN_TASK_FILES,
            fluxdown_protocol::method::CAPABILITY_AGENT_COPY_TASK_FILES,
        ] {
            assert!(
                !remote_hello
                    .capabilities
                    .iter()
                    .any(|item| item == capability),
                "remote connections must not advertise {capability}"
            );
        }
        harness.finish().await;
    }

    #[tokio::test]
    async fn desktop_hello_advertises_task_file_actions_per_connection() {
        let harness = TestGateway::new("desktop_task_files_hello").await;
        let mut request = format!("ws://{}/rpc", harness.address().await)
            .into_client_request()
            .expect("RPC URL");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer test-official-bearer"),
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("official socket");
        let hello = socket_request(
            &mut socket,
            1,
            fluxdown_protocol::method::SYSTEM_HELLO,
            serde_json::json!({
                "clientName": "task-files", "clientVersion": fluxdown_protocol::APP_VERSION,
                "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
                "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
                "requestedRole": "agent", "capabilities": []
            }),
        )
        .await;
        let RpcResponse::Success(hello) = hello else {
            panic!("hello failed: {hello:?}");
        };
        let advertises = |name: &str| {
            hello.result["capabilities"]
                .as_array()
                .expect("capabilities")
                .iter()
                .any(|capability| capability == name)
        };
        assert_eq!(
            advertises(fluxdown_protocol::method::CAPABILITY_AGENT_OPEN_TASK_FILES),
            crate::platform::LAUNCHES_PATHS
        );
        assert_eq!(
            advertises(fluxdown_protocol::method::CAPABILITY_AGENT_COPY_TASK_FILES),
            crate::platform::copies_files_to_clipboard()
        );
        assert!(
            !harness
                .service
                .hello
                .capabilities
                .iter()
                .any(|capability| capability
                    == fluxdown_protocol::method::CAPABILITY_AGENT_OPEN_TASK_FILES),
            "the shared service hello stays connection-independent"
        );
        socket.close(None).await.expect("close socket");
        harness.finish().await;
    }

    #[tokio::test]
    async fn server_mode_hello_advertises_task_file_actions_by_request_host() {
        let harness = TestGateway::new_mode("server_task_files_host", true).await;
        let addr = harness.address().await;

        for (host_hdr, expect_local) in [("example.com", false), ("localhost", true)] {
            let mut request = format!("ws://{addr}/rpc")
                .into_client_request()
                .expect("RPC URL");
            request.headers_mut().insert(
                header::AUTHORIZATION,
                HeaderValue::from_static("Bearer test-official-bearer"),
            );
            request
                .headers_mut()
                .insert(header::HOST, HeaderValue::from_static(host_hdr));
            let (mut socket, _) = tokio_tungstenite::connect_async(request)
                .await
                .expect("socket connect");
            let hello = socket_request(
                &mut socket,
                1,
                fluxdown_protocol::method::SYSTEM_HELLO,
                serde_json::json!({
                    "clientName": "server-host-test",
                    "clientVersion": fluxdown_protocol::APP_VERSION,
                    "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
                    "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
                    "requestedRole": "agent",
                    "capabilities": []
                }),
            )
            .await;
            let RpcResponse::Success(hello) = hello else {
                panic!("hello failed for {host_hdr}: {hello:?}");
            };
            let advertised = hello.result["capabilities"]
                .as_array()
                .expect("capabilities array")
                .iter()
                .any(|capability| {
                    capability == fluxdown_protocol::method::CAPABILITY_AGENT_OPEN_TASK_FILES
                });
            // 在 LAUNCHES_PATHS 为 false 的平台（如移动端/无桌面 runner），系统不支持打开外部路径，
            // 故能力恒不下发；仅当支持路径打开且属于本机 Host 时才宣告。
            let expected = expect_local && crate::platform::LAUNCHES_PATHS;
            assert_eq!(
                advertised, expected,
                "host {host_hdr} must have open task capability={expected}"
            );
            socket.close(None).await.expect("close socket");
        }

        harness.finish().await;
    }

    #[tokio::test]
    async fn server_mode_hello_advertises_task_file_actions_when_flag_enabled() {
        let harness = TestGateway::new_server_mode("server_task_files_flag", true, true).await;
        let addr = harness.address().await;

        let mut request = format!("ws://{addr}/rpc")
            .into_client_request()
            .expect("RPC URL");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer test-official-bearer"),
        );
        request
            .headers_mut()
            .insert(header::HOST, HeaderValue::from_static("example.com"));
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("socket connect");
        let hello = socket_request(
            &mut socket,
            1,
            fluxdown_protocol::method::SYSTEM_HELLO,
            serde_json::json!({
                "clientName": "server-flag-test",
                "clientVersion": fluxdown_protocol::APP_VERSION,
                "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
                "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
                "requestedRole": "agent",
                "capabilities": []
            }),
        )
        .await;
        let RpcResponse::Success(hello) = hello else {
            panic!("hello failed: {hello:?}");
        };
        let advertised = hello.result["capabilities"]
            .as_array()
            .expect("capabilities array")
            .iter()
            .any(|capability| {
                capability == fluxdown_protocol::method::CAPABILITY_AGENT_OPEN_TASK_FILES
            });
        // 当显式开启 allow_local_platform 时，即使 Host 是公网域名，只要平台支持 LAUNCHES_PATHS，
        // 也会宣告 CAPABILITY_AGENT_OPEN_TASK_FILES。
        let expected = crate::platform::LAUNCHES_PATHS;
        assert_eq!(
            advertised, expected,
            "when allow_local_platform is enabled, public host must have open task capability={expected}"
        );
        socket.close(None).await.expect("close socket");
        harness.finish().await;
    }

    // ── 局域网配对即授权：设备控制凭据端到端 ────────────────────────────────

    /// 手机：独立状态目录与身份、只出站的互联服务（与嵌入式移动宿主的装配一致）。
    struct Phone {
        link: Arc<crate::link::LinkService>,
        state: Arc<tokio::sync::Mutex<crate::state::AgentState>>,
        store: Arc<crate::state::StateStore>,
        dir: std::path::PathBuf,
    }

    impl Drop for Phone {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.dir) {
                tracing::warn!(path = %self.dir.display(), %error, "phone test cleanup failed");
            }
        }
    }

    impl Phone {
        async fn start(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fluxdown_agent_phone_{label}_{}_{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let store = Arc::new(
                crate::state::StateStore::open(dir.clone())
                    .await
                    .expect("open phone store"),
            );
            let state = Arc::new(tokio::sync::Mutex::new(crate::state::AgentState {
                device_name: label.to_owned(),
                ..crate::state::AgentState::default()
            }));
            let events = crate::event_hub::AgentEventHub::new(AgentSnapshot::default());
            let daemon = Arc::new(crate::daemon_client::DaemonClient::disconnected());
            let link = crate::link::LinkService::new(crate::link::LinkServiceParts {
                events,
                state: state.clone(),
                store: store.clone(),
                tasks: Arc::new(crate::link::DaemonTaskCreator::new(daemon)),
                bound: "127.0.0.1:0".parse().expect("phone bound address"),
                server_mode: false,
                network: crate::link::LinkNetworkOptions {
                    mdns: false,
                    advertise: Vec::new(),
                },
            });
            link.start().await.expect("start phone link");
            Self {
                link,
                state,
                store,
                dir,
            }
        }

        fn fingerprint(&self) -> String {
            self.link
                .api_ping_info()
                .expect("phone link ready")
                .fingerprint
        }

        /// 手机名册里主机的链路密钥（手工拼凭据用）。
        async fn link_secret_of(&self, host_fp: &str) -> Vec<u8> {
            use fluxdown_link::LinkStorage;
            crate::link::AgentLinkStorage::new(self.state.clone(), self.store.clone())
                .get(host_fp)
                .await
                .expect("read phone roster")
                .expect("host in the phone roster")
                .link_secret
        }
    }

    async fn start_host_link(host: &TestGateway) -> Arc<crate::link::LinkService> {
        let link = host.service.link.clone().expect("host link attached");
        link.start().await.expect("start host link");
        link
    }

    fn fingerprint_of(link: &crate::link::LinkService) -> String {
        link.api_ping_info().expect("link ready").fingerprint
    }

    async fn wait_until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
        for _ in 0..250 {
            if let Some(value) = probe() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// 主机展示配对码 → 手机 `pairBegin` → 主机批准 → 手机 `pairFinish`。
    async fn pair_phone(
        host: &TestGateway,
        host_link: &Arc<crate::link::LinkService>,
        phone: &Phone,
        request_control: bool,
    ) {
        let code = host_link.pairing_code().await.expect("pairing code");
        let address = host.address().await.to_string();
        let begin = phone
            .link
            .pair_begin(&address, &code.code, request_control)
            .await
            .expect("pairBegin");
        let request = wait_until("incoming pairing request", || {
            host.service
                .events
                .inspect(|snapshot| snapshot.link_pairing_requests.first().cloned())
        })
        .await;
        assert_eq!(request.requests_control, request_control);
        let finisher = {
            let link = phone.link.clone();
            let token = begin.token.clone();
            tokio::spawn(async move { link.pair_finish(&token, true).await })
        };
        host_link
            .approve(&request.session_id, true)
            .expect("host approves");
        finisher
            .await
            .expect("pairFinish task")
            .expect("pairFinish")
            .expect("paired");
        let phone_fp = phone.fingerprint();
        wait_until("host roster entry", || {
            host.service.events.inspect(|snapshot| {
                snapshot
                    .linked_devices
                    .iter()
                    .any(|device| device.fingerprint == phone_fp)
                    .then_some(())
            })
        })
        .await;
    }

    async fn connect_rpc(
        address: std::net::SocketAddr,
        bearer: &str,
    ) -> Result<TestSocket, tokio_tungstenite::tungstenite::Error> {
        let mut request = format!("ws://{address}/rpc")
            .into_client_request()
            .expect("RPC URL");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {bearer}")).expect("bearer header"),
        );
        tokio_tungstenite::connect_async(request)
            .await
            .map(|(socket, _)| socket)
    }

    fn rejected_with(error: &tokio_tungstenite::tungstenite::Error, status: u16) -> bool {
        matches!(
            error,
            tokio_tungstenite::tungstenite::Error::Http(response)
                if response.status().as_u16() == status
        )
    }

    /// `system.hello` 并返回应答里的能力表。
    async fn rpc_hello(socket: &mut TestSocket, id: i64) -> Vec<String> {
        let response = socket_request(
            socket,
            id,
            fluxdown_protocol::method::SYSTEM_HELLO,
            serde_json::json!({
                "clientName": "device-credential-test",
                "clientVersion": fluxdown_protocol::APP_VERSION,
                "minProtocolVersion": fluxdown_protocol::MIN_PROTOCOL_VERSION,
                "maxProtocolVersion": fluxdown_protocol::PROTOCOL_VERSION,
                "requestedRole": "agent",
                "capabilities": []
            }),
        )
        .await;
        let RpcResponse::Success(hello) = response else {
            panic!("hello failed: {response:?}");
        };
        hello.result["capabilities"]
            .as_array()
            .expect("capabilities array")
            .iter()
            .filter_map(|capability| capability.as_str().map(str::to_owned))
            .collect()
    }

    async fn assert_rpc_alive(socket: &mut TestSocket, id: i64) {
        let response = socket_request(
            socket,
            id,
            fluxdown_protocol::method::SYSTEM_PING,
            serde_json::json!({}),
        )
        .await;
        assert!(matches!(response, RpcResponse::Success(_)), "{response:?}");
    }

    /// 服务端以「凭据已撤销」关闭连接：关闭码 1008、原因 `credential-revoked`。
    async fn expect_revoked_close(socket: &mut TestSocket) {
        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match socket.next().await {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(frame))) => {
                        return frame;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return None,
                }
            }
        })
        .await
        .expect("the session must be closed after the revocation");
        let frame = closed.expect("a close frame with the revocation reason");
        assert_eq!(u16::from(frame.code), 1008);
        assert_eq!(&*frame.reason, "credential-revoked");
    }

    async fn web_status(address: std::net::SocketAddr, bearer: Option<&str>) -> u16 {
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client");
        let mut request = client.get(format!("http://{address}/api/web/logs/export"));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        request
            .send()
            .await
            .expect("web route response")
            .status()
            .as_u16()
    }

    #[tokio::test]
    async fn lan_pairing_grants_a_device_session_that_ends_when_the_host_unpairs() {
        let host = TestGateway::new_server_mode("pair_control_e2e", true, false).await;
        let host_link = start_host_link(&host).await;
        host.patch_gateway(serde_json::json!({ "userToken": "flux2026abc" }))
            .await;
        let phone = Phone::start("phone").await;
        let address = host.address().await;

        pair_phone(&host, &host_link, &phone, true).await;
        let credential = phone
            .link
            .control_credential(&fingerprint_of(&host_link))
            .await
            .expect("controlCredential")
            .token;

        // 设备凭据建 /rpc 会话并拿到快照；设备会话不是本机来源，不下发打开文件的能力。
        let mut device = connect_rpc(address, &credential)
            .await
            .expect("the device credential opens /rpc");
        let capabilities = rpc_hello(&mut device, 1).await;
        assert!(
            !capabilities.iter().any(|capability| capability
                == fluxdown_protocol::method::CAPABILITY_AGENT_OPEN_TASK_FILES)
        );
        let snapshot = socket_request(
            &mut device,
            2,
            fluxdown_protocol::method::SYSTEM_SNAPSHOT,
            serde_json::json!({}),
        )
        .await;
        assert!(matches!(snapshot, RpcResponse::Success(_)), "{snapshot:?}");
        // 同一凭据也能用于 `/api/web/*` 文件面（与 /rpc 同一校验）；没有凭据仍是 401。
        assert_ne!(web_status(address, Some(&credential)).await, 401);
        assert_eq!(web_status(address, None).await, 401);
        // 对照：访问密钥会话不受名册变化影响。
        let mut keyed = connect_rpc(address, "flux2026abc")
            .await
            .expect("the access key opens /rpc");
        rpc_hello(&mut keyed, 1).await;

        // 主机侧解除配对：已建立的设备会话被断开，重连与文件面都是 401。
        host_link
            .remove(&phone.fingerprint())
            .await
            .expect("unpair on the host");
        expect_revoked_close(&mut device).await;
        let error = connect_rpc(address, &credential)
            .await
            .expect_err("a revoked device must not reconnect");
        assert!(rejected_with(&error, 401), "{error:?}");
        assert_eq!(web_status(address, Some(&credential)).await, 401);
        assert_rpc_alive(&mut keyed, 2).await;

        host.finish().await;
    }

    #[tokio::test]
    async fn repairing_without_control_cuts_the_old_device_session() {
        let host = TestGateway::new_server_mode("pair_control_repair", true, false).await;
        let host_link = start_host_link(&host).await;
        let phone = Phone::start("phone").await;
        let address = host.address().await;

        pair_phone(&host, &host_link, &phone, true).await;
        let credential = phone
            .link
            .control_credential(&fingerprint_of(&host_link))
            .await
            .expect("controlCredential")
            .token;
        let mut device = connect_rpc(address, &credential)
            .await
            .expect("device session");
        rpc_hello(&mut device, 1).await;

        // 同一手机重新配对且这次没有请求控制：授权随新记录消失（link_secret 也变了）。
        pair_phone(&host, &host_link, &phone, false).await;
        expect_revoked_close(&mut device).await;
        let error = connect_rpc(address, &credential)
            .await
            .expect_err("the old credential is dead");
        assert!(rejected_with(&error, 401), "{error:?}");
        assert!(matches!(
            phone
                .link
                .control_credential(&fingerprint_of(&host_link))
                .await,
            Err(crate::link::LinkOpError::Link(
                fluxdown_link::LinkError::NotPaired
            ))
        ));

        host.finish().await;
    }

    #[tokio::test]
    async fn plain_pairing_yields_no_usable_rpc_credential() {
        let host = TestGateway::new_server_mode("pair_plain", true, false).await;
        let host_link = start_host_link(&host).await;
        let phone = Phone::start("phone").await;
        let address = host.address().await;
        let host_fp = fingerprint_of(&host_link);

        pair_phone(&host, &host_link, &phone, false).await;
        // 手机没有可签发的凭据；就算它用共享的链路密钥手工拼一个，主机也不认（未授权控制）。
        assert!(matches!(
            phone.link.control_credential(&host_fp).await,
            Err(crate::link::LinkOpError::Link(
                fluxdown_link::LinkError::NotPaired
            ))
        ));
        let hand_made = fluxdown_link::crypto::control_credential(
            &phone.link_secret_of(&host_fp).await,
            &phone.fingerprint(),
            &host_fp,
        )
        .expect("hand-made credential");
        let error = connect_rpc(address, &hand_made)
            .await
            .expect_err("an ungranted device must be rejected");
        assert!(rejected_with(&error, 401), "{error:?}");
        assert_eq!(web_status(address, Some(&hand_made)).await, 401);
        // 格式错误 / 凭空编造的凭据同样是 401。
        for forged in ["fdl1.nope.nope", "not-a-credential"] {
            let error = connect_rpc(address, forged).await.expect_err("forged");
            assert!(rejected_with(&error, 401), "{forged}: {error:?}");
        }

        host.finish().await;
    }

    #[tokio::test]
    async fn desktop_host_accepts_device_credentials_but_denies_platform_methods() {
        let host = TestGateway::new("pair_control_desktop").await;
        let host_link = start_host_link(&host).await;
        let phone = Phone::start("phone").await;
        let address = host.address().await;

        pair_phone(&host, &host_link, &phone, true).await;
        let credential = phone
            .link
            .control_credential(&fingerprint_of(&host_link))
            .await
            .expect("controlCredential")
            .token;

        let mut device = connect_rpc(address, &credential)
            .await
            .expect("desktop mode accepts the device credential");
        let capabilities = rpc_hello(&mut device, 1).await;
        assert!(
            !capabilities.iter().any(|capability| capability
                == fluxdown_protocol::method::CAPABILITY_AGENT_OPEN_TASK_FILES)
        );
        let denied = socket_request(
            &mut device,
            2,
            fluxdown_protocol::method::AGENT_PLATFORM_OPEN_TASK,
            serde_json::json!({}),
        )
        .await;
        let RpcResponse::Failure(denied) = denied else {
            panic!("platform methods must be denied for device sessions");
        };
        assert_eq!(
            denied.error.data.map(|data| data.code),
            Some(ApplicationErrorCode::Unsupported)
        );
        // 本机 `agent.token` 会话照常进入参数校验。
        let mut local = connect_rpc(address, "test-official-bearer")
            .await
            .expect("agent token session");
        rpc_hello(&mut local, 1).await;
        let accepted = socket_request(
            &mut local,
            2,
            fluxdown_protocol::method::AGENT_PLATFORM_OPEN_TASK,
            serde_json::json!({}),
        )
        .await;
        let RpcResponse::Failure(accepted) = accepted else {
            panic!("an empty openTask call must fail validation");
        };
        assert_eq!(
            accepted.error.data.map(|data| data.code),
            Some(ApplicationErrorCode::InvalidArgument)
        );

        // 解除配对：设备会话断开且不能重连；`agent.token` 会话不受影响。
        host_link
            .remove(&phone.fingerprint())
            .await
            .expect("unpair on the host");
        expect_revoked_close(&mut device).await;
        let error = connect_rpc(address, &credential)
            .await
            .expect_err("revoked");
        assert!(rejected_with(&error, 401), "{error:?}");
        assert_rpc_alive(&mut local, 3).await;

        host.finish().await;
    }

    #[tokio::test]
    async fn changing_the_access_key_disconnects_old_key_sessions_except_the_changer() {
        let host = TestGateway::new_server_mode("access_key_revocation", true, false).await;
        host.patch_gateway(serde_json::json!({ "userToken": "flux2026abc" }))
            .await;
        let address = host.address().await;

        let mut other = connect_rpc(address, "flux2026abc")
            .await
            .expect("other tab");
        rpc_hello(&mut other, 1).await;
        let mut changer = connect_rpc(address, "flux2026abc")
            .await
            .expect("changer tab");
        rpc_hello(&mut changer, 1).await;
        let mut service = connect_rpc(address, "test-official-bearer")
            .await
            .expect("agent token session");
        rpc_hello(&mut service, 1).await;

        // 改密钥：用旧密钥认证的其它会话断开，发起者（Web 设置页）和 agent.token 会话不受影响。
        let patched = socket_request(
            &mut changer,
            2,
            fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
            serde_json::json!({ "userToken": "rotated2026key" }),
        )
        .await;
        assert!(matches!(patched, RpcResponse::Success(_)), "{patched:?}");
        expect_revoked_close(&mut other).await;
        assert_rpc_alive(&mut changer, 3).await;
        assert_rpc_alive(&mut service, 2).await;
        let error = connect_rpc(address, "flux2026abc")
            .await
            .expect_err("old key");
        assert!(rejected_with(&error, 401), "{error:?}");
        let mut rotated = connect_rpc(address, "rotated2026key")
            .await
            .expect("new key");
        rpc_hello(&mut rotated, 1).await;

        // 重新生成：发起者记住的值再次跟随，其它新密钥会话断开。
        let regenerated = socket_request(
            &mut changer,
            4,
            fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
            serde_json::json!({ "regenerateUserToken": true }),
        )
        .await;
        assert!(
            matches!(regenerated, RpcResponse::Success(_)),
            "{regenerated:?}"
        );
        expect_revoked_close(&mut rotated).await;
        assert_rpc_alive(&mut changer, 5).await;
        let new_key = host.user_token().await;
        assert_ne!(new_key, "rotated2026key");
        let mut fresh = connect_rpc(address, &new_key)
            .await
            .expect("regenerated key");
        rpc_hello(&mut fresh, 1).await;

        // 设置同一个密钥不是改动：任何会话都不被踢。
        let unchanged = socket_request(
            &mut changer,
            6,
            fluxdown_protocol::method::AGENT_GATEWAY_PATCH,
            serde_json::json!({ "userToken": new_key }),
        )
        .await;
        assert!(
            matches!(unchanged, RpcResponse::Success(_)),
            "{unchanged:?}"
        );
        assert_rpc_alive(&mut fresh, 2).await;

        host.finish().await;
    }
}
