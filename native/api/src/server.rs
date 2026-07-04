//! axum HTTP 服务器 —— FluxDown 本机 API 服务。
//!
//! 一个端口、一个服务器，三组按配置独立启停的路由：
//!
//! | 路由组 | 端点 | 开关 | 鉴权 |
//! |---|---|---|---|
//! | 探活 | `GET /ping` | 总开关 | 无 |
//! | 脚本接管 | `POST /download`、`/download/batch` | `takeover_enabled` | `X-FluxDown-Client` 头 + 可选 token |
//! | aria2 兼容 | `POST /jsonrpc` | `jsonrpc_enabled` | 可选 token（头或 `token:xxx`） |
//! | 管理 API | `/api/v1/*` | `management_enabled` | **强制** token（Bearer 或头） |
//!
//! 服务器只依赖 [`ApiHost`] trait，宿主形态（桌面 App / headless server）无关。
//! 安全模型详见 [`crate::auth`] 模块文档。

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post, put};
use fluxdown_engine::log_info;
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::auth::{check_management_auth, check_takeover_auth, header_token_ok};
use crate::jsonrpc::handle_jsonrpc;
use crate::mcp::handle_mcp;
use crate::routes;
use crate::service::{ApiError, ApiHost};
use crate::takeover::parse_batch;
use crate::types::{CreateTaskRequest, CreatedTask, DownloadRequest};

/// 请求体大小上限：4 MB（足够容纳批量 URL 列表）。
const MAX_BODY_SIZE: usize = 4 * 1024 * 1024;
/// 端口重绑重试次数（热重启时旧 listener 释放存在竞态窗口）。
const BIND_RETRIES: u32 = 20;
/// 每次重绑重试间隔。
const BIND_RETRY_DELAY: Duration = Duration::from_millis(100);

/// API 服务器配置，从 DB config 表加载。
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use fluxdown_api::server::ApiServerConfig;
///
/// let cfg = ApiServerConfig::from_config_map(&HashMap::new(), "1.0.0");
/// assert!(cfg.enabled);            // 默认启用
/// assert_eq!(cfg.port, 17800);
/// assert!(cfg.token.is_empty());
/// assert!(cfg.takeover_enabled);   // 接管默认开
/// assert!(cfg.jsonrpc_enabled);    // aria2 兼容默认开
/// assert!(!cfg.management_enabled); // 管理 API 默认关
/// ```
#[derive(Debug, Clone)]
pub struct ApiServerConfig {
    /// 总开关（`local_server_enabled`，默认 true）。
    pub enabled: bool,
    /// 监听端口（`local_server_port`，默认 17800；永远只绑 127.0.0.1）。
    pub port: u16,
    /// 鉴权 token（`local_server_token`，空 = 接管/aria2 不鉴权，管理 API 拒绝）。
    pub token: String,
    /// 脚本接管子开关（`local_server_takeover_enabled`，默认 true）。
    pub takeover_enabled: bool,
    /// aria2 兼容子开关（`local_server_jsonrpc_enabled`，默认 true）。
    pub jsonrpc_enabled: bool,
    /// 管理 API 子开关（`local_server_api_enabled`，默认 false）。
    pub management_enabled: bool,
    /// MCP 端点子开关（`local_server_mcp_enabled`，默认 false）。
    /// 与管理 API 共用 token 鉴权（Bearer / X-FluxDown-Token）。
    pub mcp_enabled: bool,
    /// 宿主应用版本号（`/ping`、`/api/v1/info` 返回）。
    pub app_version: String,
}

impl ApiServerConfig {
    /// 从 config 表键值对构建配置。缺失键取默认值。
    #[must_use]
    pub fn from_config_map(map: &HashMap<String, String>, app_version: &str) -> Self {
        let flag = |key: &str, default: bool| -> bool {
            map.get(key).map(|v| v == "true").unwrap_or(default)
        };
        Self {
            enabled: flag("local_server_enabled", true),
            port: map
                .get("local_server_port")
                .and_then(|v| v.parse().ok())
                .unwrap_or(17800),
            token: map.get("local_server_token").cloned().unwrap_or_default(),
            takeover_enabled: flag("local_server_takeover_enabled", true),
            jsonrpc_enabled: flag("local_server_jsonrpc_enabled", true),
            management_enabled: flag("local_server_api_enabled", false),
            mcp_enabled: flag("local_server_mcp_enabled", false),
            app_version: app_version.to_string(),
        }
    }

    /// 监听地址。永远只绑本机回环，杜绝外网/局域网暴露。
    #[must_use]
    pub fn bind_addr(&self) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, self.port))
    }
}

/// 运行中 API 服务器的句柄。[`shutdown`](Self::shutdown) 触发优雅停机，
/// 用于配置变更后的热重启。
#[derive(Debug)]
pub struct ApiServerHandle {
    cancel: CancellationToken,
}

impl ApiServerHandle {
    /// 触发优雅停机（幂等）。
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

/// 启动 API 服务器（后台 tokio 任务），立即返回句柄。
///
/// - `config.enabled == false` → 不监听，返回的句柄无操作。
/// - 绑定失败（端口被占用）→ 重试 [`BIND_RETRIES`] 次后放弃，仅本特性不可用，
///   不影响宿主主功能。
pub fn spawn_api_server(host: Arc<dyn ApiHost>, config: ApiServerConfig) -> ApiServerHandle {
    let cancel = CancellationToken::new();
    let handle = ApiServerHandle {
        cancel: cancel.clone(),
    };
    if !config.enabled {
        log_info!("[api-server] disabled by config");
        return handle;
    }

    let addr = config.bind_addr();
    tokio::spawn(async move {
        // 热重启场景：旧 listener 释放与新绑定之间存在竞态窗口，重试消化。
        let mut listener = None;
        for attempt in 0..BIND_RETRIES {
            match TcpListener::bind(addr).await {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(e) if attempt + 1 == BIND_RETRIES => {
                    log_info!("[api-server] failed to bind {}: {}", addr, e);
                }
                Err(_) => tokio::time::sleep(BIND_RETRY_DELAY).await,
            }
        }
        let Some(listener) = listener else {
            return;
        };
        log_info!(
            "[api-server] listening on http://{} (takeover={}, jsonrpc={}, management={})",
            addr,
            config.takeover_enabled,
            config.jsonrpc_enabled,
            config.management_enabled
        );
        serve_on(listener, host, config, cancel).await;
    });
    handle
}

/// 在已绑定的 listener 上跑完整服务（抽出以便集成测试注入 `127.0.0.1:0`
/// 临时端口）。`cancel` 触发优雅停机后返回。
pub(crate) async fn serve_on(
    listener: TcpListener,
    host: Arc<dyn ApiHost>,
    config: ApiServerConfig,
    cancel: CancellationToken,
) {
    let app = build_router(AppState {
        host,
        config: Arc::new(config),
    });
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(cancel.cancelled_owned())
        .await;
    if let Err(e) = served {
        log_info!("[api-server] serve error: {}", e);
    } else {
        log_info!("[api-server] stopped");
    }
}

// ---------------------------------------------------------------------------
// 路由与状态
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct AppState {
    host: Arc<dyn ApiHost>,
    config: Arc<ApiServerConfig>,
}

/// 核心路由集（按配置开关注册）：探活 / 脚本接管 / aria2 兼容 / 管理 CRUD。
/// **不含** `API_OPENAPI` 路由与 fallback —— 由调用方决定（桌面
/// `build_router` 补齐两者；headless 服务器自带合并版 openapi 与 SPA
/// fallback，见 [`api_router`]）。
fn register_core(state: AppState) -> Router<AppState> {
    let mut router = Router::new().route(routes::PING, get(ping));

    if state.config.takeover_enabled {
        router = router
            .route(routes::DOWNLOAD, post(takeover_download))
            .route(routes::DOWNLOAD_BATCH, post(takeover_download_batch));
    }
    if state.config.jsonrpc_enabled {
        router = router.route(routes::JSONRPC, post(jsonrpc));
    }
    if state.config.mcp_enabled {
        router = router.route(routes::MCP, post(mcp));
    }
    if state.config.management_enabled {
        router = router
            .route(routes::API_INFO, get(api_info))
            .route(routes::API_TASKS, get(api_list_tasks).post(api_create_task))
            // 注意：静态段 `/tasks/pause` 与参数段 `/tasks/{id}` 并存，
            // axum(matchit) 静态路由优先，无冲突。
            .route(routes::API_TASKS_PAUSE, put(api_pause_all))
            .route(routes::API_TASKS_CONTINUE, put(api_continue_all))
            .route(routes::API_TASK, get(api_get_task).delete(api_delete_task))
            .route(routes::API_TASK_PAUSE, put(api_pause_task))
            .route(routes::API_TASK_CONTINUE, put(api_continue_task))
            .route(routes::API_QUEUES, get(api_list_queues));
    }

    router
}

/// 按配置组装桌面 App 完整路由：核心路由 + OpenAPI 规范 + 404 fallback。
/// 关闭的路由组不注册（对应端点回 404，与旧行为一致）。
fn build_router(state: AppState) -> Router {
    let mut router = register_core(state.clone());
    if state.config.management_enabled {
        // OpenAPI 规范文档（无鉴权——纯接口描述，不含任何用户数据）。
        router = router.route(routes::API_OPENAPI, get(openapi_spec));
    }
    router
        .fallback(unknown_endpoint)
        .layer(middleware::from_fn(options_preflight))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .with_state(state)
}

/// 供其他宿主（headless 服务器）复用的核心路由集。
///
/// 与桌面 [`spawn_api_server`] 的差异：**不含** `/api/v1/openapi.json`
/// 与 404 fallback，调用方 `merge` 自己的扩展路由、提供合并版 OpenAPI
/// 与 SPA fallback，不会与本函数产生路由冲突。已附带 OPTIONS 预检拒绝
/// 与请求体大小限制两层中间件（与桌面行为一致）。
pub fn api_router(host: Arc<dyn ApiHost>, config: ApiServerConfig) -> Router {
    let state = AppState {
        host,
        config: Arc::new(config),
    };
    register_core(state.clone())
        .layer(middleware::from_fn(options_preflight))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .with_state(state)
}

/// OPTIONS 预检统一回 204（**故意不带** `Access-Control-Allow-Origin`，
/// 使恶意网页的跨域预检失败 —— 见 [`crate::auth`] 安全模型第 2 条）。
async fn options_preflight(req: axum::extract::Request, next: Next) -> Response {
    if req.method() == Method::OPTIONS {
        return (
            StatusCode::NO_CONTENT,
            [(header::ALLOW, "GET, POST, PUT, DELETE, OPTIONS")],
        )
            .into_response();
    }
    next.run(req).await
}

async fn unknown_endpoint() -> Response {
    result_response(StatusCode::NOT_FOUND, false, "unknown endpoint")
}

/// `{"success":bool,"message":...}` 形态响应（接管端点与错误统一格式）。
fn result_response(status: StatusCode, success: bool, message: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "success": success, "message": message })),
    )
        .into_response()
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        result_response(status, false, &self.to_string())
    }
}

// ---------------------------------------------------------------------------
// 探活
// ---------------------------------------------------------------------------

/// 探活。返回应用名、版本号与 `pong`。无鉴权。
#[utoipa::path(get, path = "/ping", tag = "system",
    responses((status = 200, description = "应用存活，返回 app/version/message"))
)]
pub(crate) async fn ping(State(state): State<AppState>) -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "success": true,
            "app": "FluxDown",
            "version": state.config.app_version,
            "message": "pong",
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// 脚本接管端点
// ---------------------------------------------------------------------------

/// 提交单个外部下载请求。用 `Bytes` 而非 `Json` 提取：油猴脚本可能不带
/// `application/json` Content-Type（保留旧实现的宽容行为）。
#[utoipa::path(post, path = "/download", tag = "takeover",
    request_body = DownloadRequest,
    responses(
        (status = 200, description = "已受理，进入快速下载确认流程", body = crate::types::ResultMessage),
        (status = 400, description = "载荷非法或缺少 url", body = crate::types::ResultMessage),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
        (status = 403, description = "缺少 X-FluxDown-Client 头", body = crate::types::ResultMessage),
    ),
    security(("tokenHeader" = []))
)]
pub(crate) async fn takeover_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err((code, msg)) = check_takeover_auth(&headers, &state.config.token) {
        return result_response(status_from(code), false, msg);
    }
    let dl: DownloadRequest = match serde_json::from_slice(&body) {
        Ok(dl) => dl,
        Err(e) => {
            return result_response(
                StatusCode::BAD_REQUEST,
                false,
                &format!("invalid download payload: {e}"),
            );
        }
    };
    submit_external(&state, dl).await
}

/// 批量提交外部下载请求。支持 `{"urls":[...]}` 与 `{"items":[{...}]}` 两种形态，
/// 合并为单次确认。鉴权与 `/download` 相同。
#[utoipa::path(post, path = "/download/batch", tag = "takeover",
    responses(
        (status = 200, description = "已受理", body = crate::types::ResultMessage),
        (status = 400, description = "载荷非法", body = crate::types::ResultMessage),
    ),
    security(("tokenHeader" = []))
)]
pub(crate) async fn takeover_download_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err((code, msg)) = check_takeover_auth(&headers, &state.config.token) {
        return result_response(status_from(code), false, msg);
    }
    match parse_batch(&body) {
        Ok(dl) => {
            let count = dl.url.split('\n').filter(|s| !s.trim().is_empty()).count();
            log_info!("[api-server] /download/batch {} urls", count);
            submit_external(&state, dl).await
        }
        Err(e) => result_response(StatusCode::BAD_REQUEST, false, &e),
    }
}

async fn submit_external(state: &AppState, dl: DownloadRequest) -> Response {
    if dl.url.trim().is_empty() {
        return result_response(StatusCode::BAD_REQUEST, false, "url is required");
    }
    log_info!("[api-server] external download url={}", dl.url);
    match state.host.submit_external(dl).await {
        Ok(()) => result_response(StatusCode::OK, true, "download accepted"),
        Err(e) => e.into_response(),
    }
}

fn status_from(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

// ---------------------------------------------------------------------------
// aria2 JSON-RPC 兼容端点
// ---------------------------------------------------------------------------

/// aria2 客户端约定：HTTP 层始终 200，错误在 JSON-RPC envelope 内表达。
#[utoipa::path(post, path = "/jsonrpc", tag = "aria2",
    responses((status = 200, description = "JSON-RPC 响应（错误在 envelope 内表达）。支持方法：aria2.addUri / aria2.getVersion / aria2.getGlobalStat / system.multicall / system.listMethods；token 可经 X-FluxDown-Token 头或 params[0]=\"token:xxx\" 传递")),
    security(("tokenHeader" = []))
)]
pub(crate) async fn jsonrpc(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let token_ok = header_token_ok(&headers, &state.config.token);
    let resp = handle_jsonrpc(state.host.as_ref(), &state.config.token, token_ok, &body).await;
    ([(header::CACHE_CONTROL, "no-store")], Json(resp)).into_response()
}

// ---------------------------------------------------------------------------
// MCP 兼容端点
// ---------------------------------------------------------------------------

/// MCP（Model Context Protocol）端点。强制 token 鉴权（Bearer /
/// X-FluxDown-Token，复用管理 API 门禁）。请求返回 `200 application/json`
/// JSON-RPC 响应；通知（无 `id`）返回 `202 Accepted` 空体。
#[utoipa::path(post, path = "/mcp", tag = "mcp",
    responses(
        (status = 200, description = "JSON-RPC 响应（initialize / tools/list / tools/call / ping）"),
        (status = 202, description = "通知已接受（无响应体）"),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
        (status = 403, description = "服务端未配置 token", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err((code, msg)) = check_management_auth(&headers, &state.config.token) {
        return result_response(status_from(code), false, msg);
    }
    match handle_mcp(state.host.as_ref(), &state.config.app_version, &body).await {
        Some(resp) => ([(header::CACHE_CONTROL, "no-store")], Json(resp)).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

// ---------------------------------------------------------------------------
// 管理 API（/api/v1）
// ---------------------------------------------------------------------------

/// 管理 API 统一鉴权入口。`Err` 装箱：`Response` 体积大，避免撑大每个
/// handler 的返回路径（clippy::result_large_err）。
fn guard(state: &AppState, headers: &HeaderMap) -> Result<(), Box<Response>> {
    check_management_auth(headers, &state.config.token)
        .map_err(|(code, msg)| Box::new(result_response(status_from(code), false, msg)))
}

/// 应用信息（名称与版本号）。
#[utoipa::path(get, path = "/api/v1/info", tag = "management",
    responses(
        (status = 200, description = "应用信息", body = crate::types::ApiInfo),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
        (status = 403, description = "服务端未配置 token", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn api_info(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = guard(&state, &headers) {
        return *resp;
    }
    Json(crate::types::ApiInfo {
        name: "FluxDown".to_string(),
        version: state.config.app_version.clone(),
    })
    .into_response()
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct TaskListQuery {
    /// 按状态过滤：0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
    status: Option<i32>,
}

/// 列出全部任务，可按状态过滤。
#[utoipa::path(get, path = "/api/v1/tasks", tag = "management",
    params(TaskListQuery),
    responses(
        (status = 200, description = "任务列表", body = Vec<crate::types::TaskDto>),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn api_list_tasks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TaskListQuery>,
) -> Response {
    if let Err(resp) = guard(&state, &headers) {
        return *resp;
    }
    match state.host.list_tasks().await {
        Ok(mut tasks) => {
            if let Some(status) = q.status {
                tasks.retain(|t| t.status == status);
            }
            Json(tasks).into_response()
        }
        Err(e) => e.into_response(),
    }
}

/// 直接创建下载任务（不弹确认框），返回新任务 ID。
#[utoipa::path(post, path = "/api/v1/tasks", tag = "management",
    request_body = CreateTaskRequest,
    responses(
        (status = 200, description = "创建成功", body = crate::types::CreatedTask),
        (status = 400, description = "载荷非法或缺少 url", body = crate::types::ResultMessage),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
        (status = 503, description = "应用关闭中", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn api_create_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(resp) = guard(&state, &headers) {
        return *resp;
    }
    let req: CreateTaskRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return result_response(
                StatusCode::BAD_REQUEST,
                false,
                &format!("invalid create payload: {e}"),
            );
        }
    };
    if req.url.trim().is_empty() {
        return result_response(StatusCode::BAD_REQUEST, false, "url is required");
    }
    match state.host.create_task(req).await {
        Ok(task_id) => Json(CreatedTask { task_id }).into_response(),
        Err(e) => e.into_response(),
    }
}

/// 按 ID 查询单个任务。
#[utoipa::path(get, path = "/api/v1/tasks/{id}", tag = "management",
    params(("id" = String, Path, description = "任务 ID（UUID）")),
    responses(
        (status = 200, description = "任务信息", body = crate::types::TaskDto),
        (status = 404, description = "任务不存在", body = crate::types::ResultMessage),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn api_get_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = guard(&state, &headers) {
        return *resp;
    }
    match state.host.get_task(&id).await {
        Ok(Some(task)) => Json(task).into_response(),
        Ok(None) => ApiError::NotFound.into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeleteTaskQuery {
    /// true = 同时删除磁盘文件。默认 false（仅删记录）。
    #[serde(default)]
    delete_files: bool,
}

/// 删除任务，可选同时删除磁盘文件。
#[utoipa::path(delete, path = "/api/v1/tasks/{id}", tag = "management",
    params(("id" = String, Path, description = "任务 ID（UUID）"), DeleteTaskQuery),
    responses(
        (status = 200, description = "已删除", body = crate::types::ResultMessage),
        (status = 404, description = "任务不存在", body = crate::types::ResultMessage),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn api_delete_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<DeleteTaskQuery>,
) -> Response {
    if let Err(resp) = guard(&state, &headers) {
        return *resp;
    }
    ack(state.host.delete_task(&id, q.delete_files).await)
}

/// 暂停单个任务。
#[utoipa::path(put, path = "/api/v1/tasks/{id}/pause", tag = "management",
    params(("id" = String, Path, description = "任务 ID（UUID）")),
    responses(
        (status = 200, description = "已暂停", body = crate::types::ResultMessage),
        (status = 404, description = "任务不存在", body = crate::types::ResultMessage),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn api_pause_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = guard(&state, &headers) {
        return *resp;
    }
    ack(state.host.pause_task(&id).await)
}

/// 恢复单个任务。
#[utoipa::path(put, path = "/api/v1/tasks/{id}/continue", tag = "management",
    params(("id" = String, Path, description = "任务 ID（UUID）")),
    responses(
        (status = 200, description = "已恢复", body = crate::types::ResultMessage),
        (status = 404, description = "任务不存在", body = crate::types::ResultMessage),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn api_continue_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = guard(&state, &headers) {
        return *resp;
    }
    ack(state.host.continue_task(&id).await)
}

/// 暂停全部活跃任务（pending / downloading / preparing）。
#[utoipa::path(put, path = "/api/v1/tasks/pause", tag = "management",
    responses(
        (status = 200, description = "已全部暂停", body = crate::types::ResultMessage),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn api_pause_all(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = guard(&state, &headers) {
        return *resp;
    }
    ack(state.host.pause_all().await)
}

/// 恢复全部已暂停任务。
#[utoipa::path(put, path = "/api/v1/tasks/continue", tag = "management",
    responses(
        (status = 200, description = "已全部恢复", body = crate::types::ResultMessage),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn api_continue_all(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = guard(&state, &headers) {
        return *resp;
    }
    ack(state.host.continue_all().await)
}

/// 列出全部命名队列。
#[utoipa::path(get, path = "/api/v1/queues", tag = "management",
    responses(
        (status = 200, description = "队列列表", body = Vec<crate::types::QueueDto>),
        (status = 401, description = "token 无效", body = crate::types::ResultMessage),
    ),
    security(("bearerAuth" = []), ("tokenHeader" = []))
)]
pub(crate) async fn api_list_queues(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = guard(&state, &headers) {
        return *resp;
    }
    match state.host.list_queues().await {
        Ok(queues) => Json(queues).into_response(),
        Err(e) => e.into_response(),
    }
}

/// 无返回值操作的统一应答。
fn ack(result: Result<(), ApiError>) -> Response {
    match result {
        Ok(()) => result_response(StatusCode::OK, true, "ok"),
        Err(e) => e.into_response(),
    }
}

/// OpenAPI 3.1 规范（JSON）。无鉴权：纯接口描述，不含任何用户数据。
async fn openapi_spec() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::openapi::openapi_json(),
    )
        .into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn from_config_map_reads_all_keys_including_new_subswitches() {
        let mut map = HashMap::new();
        map.insert("local_server_enabled".to_string(), "false".to_string());
        map.insert("local_server_port".to_string(), "9999".to_string());
        map.insert("local_server_token".to_string(), "secret".to_string());
        map.insert(
            "local_server_takeover_enabled".to_string(),
            "false".to_string(),
        );
        map.insert(
            "local_server_jsonrpc_enabled".to_string(),
            "false".to_string(),
        );
        map.insert("local_server_api_enabled".to_string(), "true".to_string());

        let cfg = ApiServerConfig::from_config_map(&map, "2.3.4");

        assert!(!cfg.enabled);
        assert_eq!(cfg.port, 9999);
        assert_eq!(cfg.token, "secret");
        assert!(!cfg.takeover_enabled);
        assert!(!cfg.jsonrpc_enabled);
        assert!(cfg.management_enabled);
        assert_eq!(cfg.app_version, "2.3.4");
    }

    #[test]
    fn bind_addr_is_always_loopback_regardless_of_port() {
        let mut map = HashMap::new();
        map.insert("local_server_port".to_string(), "12345".to_string());
        let cfg = ApiServerConfig::from_config_map(&map, "1.0.0");
        let addr = cfg.bind_addr();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), 12345);
    }
}
