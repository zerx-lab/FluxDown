//! 扩展路由：WS 实时推送 / 配置读写 / 队列 CRUD / 文件取回 / 目录列举 /
//! 代理测试 / token 管理 / 运行状态 / 合并版 OpenAPI + Scalar 文档。
//!
//! 鉴权模型：
//! - 常规扩展端点 → `route_layer` 统一套用管理 token 门禁
//!   （复用 [`fluxdown_api::auth::check_management_auth`]）。
//! - `GET /api/v1/ws`、`GET /api/v1/tasks/{id}/file` → 浏览器无法设自定义
//!   header，改用 `?token=` 查询参数在 handler 内常量时间比较。
//! - `openapi.json` / `docs` → 无鉴权（纯接口描述，不含数据）。

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use fluxdown_api::auth::{check_management_auth, constant_time_eq};
use fluxdown_api::service::ApiError;
use fluxdown_api::types::{QueueDto, TaskDto};
use fluxdown_engine::components::{
    ffmpeg_status, install_ffmpeg, install_ytdlp, list_versions, list_ytdlp_versions,
    uninstall_ffmpeg, uninstall_ytdlp, ytdlp_status,
};
use fluxdown_engine::db::Db;
use fluxdown_engine::downloader::build_client;
use fluxdown_engine::log_info;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::selection::HostSelection;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::io::ReaderStream;
use utoipa::OpenApi;

use crate::actor::ActorCmd;
use crate::config::default_save_dir;
use crate::wire::{
    ComponentFfmpegStatus, ComponentVersions, ComponentYtdlpStatus, CreateQueueRequest, FsEntry,
    FsListResponse, InstallFfmpegRequest, LogFileDto, LogsResponse, MoveQueueRequest,
    ProxyTestRequest, ProxyTestResponse, QueueScheduleRequest, ReorderQueueRequest, StatsResponse,
    TokenResponse, TrackerSubRefreshResponse, UpdateQueueRequest, WsClientMsg, WsServerMsg,
};
use crate::ws_hub::WsHub;

/// 扩展路由共享状态。
#[derive(Clone)]
pub struct ServerState {
    pub db: Db,
    pub cmd_tx: mpsc::Sender<ActorCmd>,
    pub hub: Arc<WsHub>,
    pub selector: Arc<dyn HostSelection>,
    /// 管理 token（启动时读定；`token/regenerate` 后重启生效）。
    pub token: String,
    pub version: String,
    /// 演示模式：`Some(url)` 时仅允许下载该 URL（`FLUXDOWN_DEMO_URL`）。
    pub demo_url: Option<String>,
    /// 解析后的数据目录（与 `engine.data_dir` 一致），供组件 API
    /// （ffmpeg 探测/安装）直接调用，无需经 actor。
    pub data_dir: PathBuf,
    /// ffmpeg 托管安装互斥标志：安装/更新期间为 `true`，防止并发重复安装。
    pub ffmpeg_installing: Arc<AtomicBool>,
    /// yt-dlp 托管安装互斥标志：安装/更新期间为 `true`，防止并发重复安装。
    pub ytdlp_installing: Arc<AtomicBool>,
}

impl ServerState {
    /// 发送 actor 命令并等待回执；actor 断开 → 503。
    async fn send_cmd<T>(
        &self,
        make: impl FnOnce(tokio::sync::oneshot::Sender<T>) -> ActorCmd,
    ) -> Result<T, ApiError> {
        let (ack, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(make(ack))
            .await
            .map_err(|_| ApiError::Unavailable)?;
        rx.await.map_err(|_| ApiError::Unavailable)
    }

    /// 当前默认保存目录（config 表实时值，回退平台默认）。
    async fn current_save_dir(&self) -> String {
        let dir = self
            .db
            .get_config("default_save_dir")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        if dir.trim().is_empty() {
            default_save_dir()
        } else {
            dir
        }
    }
}

/// 组装全部扩展路由（含鉴权中间件），与 `fluxdown_api::server::api_router`
/// `merge` 后使用（两侧路径不重叠、同路径不同方法自动合并）。
pub fn extra_router(state: ServerState) -> Router {
    let protected = Router::new()
        .route(paths::CONFIG, get(get_config).put(put_config))
        .route(paths::QUEUES, post(create_queue))
        .route(paths::QUEUE, put(update_queue).delete(delete_queue))
        .route(paths::QUEUE_START, post(start_queue))
        .route(paths::QUEUE_STOP, post(stop_queue))
        .route(paths::QUEUE_SCHEDULE, put(set_queue_schedule))
        .route(paths::QUEUE_ORDER, put(reorder_queue))
        .route(paths::TASK_QUEUE, put(move_task_queue))
        .route(paths::TASK_BOOST, put(boost_task))
        .route(paths::FS_LIST, get(fs_list))
        .route(paths::PROXY_TEST, post(proxy_test))
        .route(paths::TOKEN_REGENERATE, post(token_regenerate))
        .route(paths::STATS, get(stats))
        .route(paths::LOGS, get(logs_info))
        .route(paths::BT_TRACKER_SUB_REFRESH, post(bt_tracker_sub_refresh))
        .route(paths::COMPONENT_FFMPEG, get(component_ffmpeg_status))
        .route(
            paths::COMPONENT_FFMPEG_VERSIONS,
            get(component_ffmpeg_versions),
        )
        .route(
            paths::COMPONENT_FFMPEG_INSTALL,
            post(component_ffmpeg_install),
        )
        .route(
            paths::COMPONENT_FFMPEG_UNINSTALL,
            post(component_ffmpeg_uninstall),
        )
        .route(paths::COMPONENT_YTDLP, get(component_ytdlp_status))
        .route(
            paths::COMPONENT_YTDLP_VERSIONS,
            get(component_ytdlp_versions),
        )
        .route(
            paths::COMPONENT_YTDLP_INSTALL,
            post(component_ytdlp_install),
        )
        .route(
            paths::COMPONENT_YTDLP_UNINSTALL,
            post(component_ytdlp_uninstall),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let open = Router::new()
        .route(paths::WS, get(ws_handler))
        .route(paths::TASK_FILE, get(task_file))
        .route(paths::LOGS_EXPORT, get(logs_export))
        .route(paths::OPENAPI, get(openapi_spec))
        .route(paths::DOCS, get(scalar_docs));

    protected.merge(open).with_state(state)
}

/// 扩展端点路径常量（OpenAPI 注解与路由注册共用）。
pub mod paths {
    pub const WS: &str = "/api/v1/ws";
    pub const CONFIG: &str = "/api/v1/config";
    pub const QUEUES: &str = "/api/v1/queues";
    pub const QUEUE: &str = "/api/v1/queues/{id}";
    pub const QUEUE_START: &str = "/api/v1/queues/{id}/start";
    pub const QUEUE_STOP: &str = "/api/v1/queues/{id}/stop";
    pub const QUEUE_SCHEDULE: &str = "/api/v1/queues/{id}/schedule";
    pub const QUEUE_ORDER: &str = "/api/v1/queues/{id}/order";
    pub const TASK_QUEUE: &str = "/api/v1/tasks/{id}/queue";
    pub const TASK_BOOST: &str = "/api/v1/tasks/{id}/boost";
    pub const TASK_FILE: &str = "/api/v1/tasks/{id}/file";
    pub const FS_LIST: &str = "/api/v1/fs/list";
    pub const PROXY_TEST: &str = "/api/v1/proxy/test";
    pub const TOKEN_REGENERATE: &str = "/api/v1/token/regenerate";
    pub const STATS: &str = "/api/v1/stats";
    pub const LOGS: &str = "/api/v1/logs";
    pub const LOGS_EXPORT: &str = "/api/v1/logs/export";
    pub const COMPONENT_FFMPEG: &str = "/api/v1/components/ffmpeg";
    pub const COMPONENT_FFMPEG_VERSIONS: &str = "/api/v1/components/ffmpeg/versions";
    pub const COMPONENT_FFMPEG_INSTALL: &str = "/api/v1/components/ffmpeg/install";
    pub const COMPONENT_FFMPEG_UNINSTALL: &str = "/api/v1/components/ffmpeg/uninstall";
    pub const COMPONENT_YTDLP: &str = "/api/v1/components/ytdlp";
    pub const COMPONENT_YTDLP_VERSIONS: &str = "/api/v1/components/ytdlp/versions";
    pub const COMPONENT_YTDLP_INSTALL: &str = "/api/v1/components/ytdlp/install";
    pub const COMPONENT_YTDLP_UNINSTALL: &str = "/api/v1/components/ytdlp/uninstall";
    pub const OPENAPI: &str = "/api/v1/openapi.json";
    pub const DOCS: &str = "/api/v1/docs";
    pub const BT_TRACKER_SUB_REFRESH: &str = "/api/v1/bt/tracker-sub/refresh";
}

/// 统一 JSON 错误体（与 `fluxdown_api` 的 `ResultMessage` 形态一致）。
fn error_response(status: StatusCode, message: &str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({ "success": false, "message": message })),
    )
        .into_response()
}

/// 管理 token 门禁中间件（除 `/ws`、`/file`、文档外的扩展端点）。
async fn require_auth(
    State(state): State<ServerState>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    if let Err((code, msg)) = check_management_auth(req.headers(), &state.token) {
        return error_response(
            StatusCode::from_u16(code).unwrap_or(StatusCode::UNAUTHORIZED),
            msg,
        );
    }
    next.run(req).await
}

#[derive(Debug, Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: String,
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

/// 实时事件推送 + HLS/BT 选择往返。`?token=` 鉴权（浏览器 WS 无法设 header），
/// 校验失败升级后立即以 1008 (Policy Violation) 关闭。
#[utoipa::path(get, path = "/api/v1/ws", tag = "server",
    params(("token" = String, Query, description = "管理 token")),
    responses((status = 101, description = "升级为 WebSocket；服务端推送 WsServerMsg，接收 WsClientMsg"))
)]
async fn ws_handler(
    State(state): State<ServerState>,
    Query(q): Query<TokenQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let authorized = !state.token.is_empty() && constant_time_eq(&q.token, &state.token);
    ws.on_upgrade(move |socket| handle_socket(socket, state, authorized))
}

async fn handle_socket(mut socket: WebSocket, state: ServerState, authorized: bool) {
    if !authorized {
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: 1008,
                reason: "invalid token".into(),
            })))
            .await;
        return;
    }

    // 连接建立即发送全量快照，客户端无需先发起 REST 轮询。
    if let Ok(tasks) = state.db.load_all_tasks().await {
        let msg = WsServerMsg::TasksSnapshot {
            tasks: tasks.into_iter().map(TaskDto::from).collect(),
        };
        if send_msg(&mut socket, &msg).await.is_err() {
            return;
        }
    }
    if let Ok(queues) = state.db.load_all_queues().await {
        let msg = WsServerMsg::QueuesChanged {
            queues: queues.into_iter().map(QueueDto::from).collect(),
        };
        if send_msg(&mut socket, &msg).await.is_err() {
            return;
        }
    }

    let mut events = state.hub.events.subscribe();
    loop {
        tokio::select! {
            broadcast = events.recv() => {
                match broadcast {
                    Ok(json) => {
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    // 慢消费者滞后：跳过丢失的消息继续（进度类消息可容忍丢帧）。
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        log_info!("[ws] client lagged, skipped {} messages", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                let Message::Text(text) = msg else { continue };
                match serde_json::from_str::<WsClientMsg>(&text) {
                    Ok(WsClientMsg::Ping {}) => {
                        if send_msg(&mut socket, &WsServerMsg::Pong {}).await.is_err() {
                            break;
                        }
                    }
                    Ok(WsClientMsg::HlsSelection { task_id, selected_index }) => {
                        state.selector.provide_hls_selection(&task_id, selected_index);
                    }
                    Ok(WsClientMsg::BtSelection { task_id, selected_indices }) => {
                        state.selector.provide_bt_selection(&task_id, selected_indices);
                    }
                    Ok(WsClientMsg::SelectVariant { task_id, selected_index }) => {
                        state.selector.provide_variant_selection(&task_id, selected_index);
                    }
                    Err(e) => log_info!("[ws] bad client message: {}", e),
                }
            }
        }
    }
}

async fn send_msg(socket: &mut WebSocket, msg: &WsServerMsg) -> Result<(), ()> {
    let json = serde_json::to_string(msg).map_err(|_| ())?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|_| ())
}

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 读取全部配置键值。读取前先经 actor 把内存中的 CDN 遥测样本刷进
/// `cdn_pending_reports`（对齐 hub 的 RequestConfig 处理点），保证 Web 面板
/// 众包上报能读到最新样本。
#[utoipa::path(get, path = "/api/v1/config", tag = "server",
    responses((status = 200, body = HashMap<String, String>)),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn get_config(State(state): State<ServerState>) -> Result<Response, ApiError> {
    state
        .send_cmd(|ack| ActorCmd::FlushCdnReports { ack })
        .await?;
    let map = state
        .db
        .get_all_config()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(axum::Json(map).into_response())
}

/// 批量写入配置键值并 live-apply 到引擎（`local_server_*` 键重启生效）。
#[utoipa::path(put, path = "/api/v1/config", tag = "server",
    request_body = HashMap<String, String>,
    responses((status = 200, description = "已持久化并应用")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn put_config(
    State(state): State<ServerState>,
    axum::Json(entries): axum::Json<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let keys: Vec<String> = entries.keys().cloned().collect();
    for (key, value) in &entries {
        state
            .db
            .set_config(key, value)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    state
        .send_cmd(|ack| ActorCmd::ApplyConfig { keys, ack })
        .await?;
    // 订阅地址变更 / 重新启用订阅 → 后台立即刷新一次（镜像桌面 apply_config_key）。
    let should_refresh = entries.contains_key("bt_tracker_sub_urls")
        || entries
            .get("bt_tracker_sub_enabled")
            .is_some_and(|v| v == "true");
    if should_refresh {
        let db = state.db.clone();
        let cmd_tx = state.cmd_tx.clone();
        tokio::spawn(async move {
            crate::actor::refresh_tracker_sub(&db, &cmd_tx).await;
        });
    }
    Ok(axum::Json(serde_json::json!({ "success": true, "message": "applied" })).into_response())
}

/// 立即刷新 BT Tracker 订阅：同步拉取全部订阅源、去重、写回缓存并失效当前
/// BT 会话，回执抓取结果。网络耗时约 20s/源。
#[utoipa::path(post, path = "/api/v1/bt/tracker-sub/refresh", tag = "server",
    responses((status = 200, description = "刷新完成", body = TrackerSubRefreshResponse)),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn bt_tracker_sub_refresh(State(state): State<ServerState>) -> Result<Response, ApiError> {
    let outcome = crate::actor::refresh_tracker_sub(&state.db, &state.cmd_tx).await;
    let updated_at = state
        .db
        .get_config("bt_tracker_sub_updated_at")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let resp = TrackerSubRefreshResponse {
        success: outcome.is_success(),
        tracker_count: outcome.trackers.len() as i64,
        ok_sources: outcome.ok_sources as i64,
        total_sources: outcome.total_sources as i64,
        updated_at,
        error: outcome.error,
    };
    Ok(axum::Json(resp).into_response())
}

// ---------------------------------------------------------------------------
// 队列 CRUD / 任务队列操作
// ---------------------------------------------------------------------------

/// 创建命名队列。
#[utoipa::path(post, path = "/api/v1/queues", tag = "server",
    request_body = CreateQueueRequest,
    responses((status = 200, description = "已创建")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn create_queue(
    State(state): State<ServerState>,
    axum::Json(req): axum::Json<CreateQueueRequest>,
) -> Result<Response, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("queue name is required".into()));
    }
    state
        .send_cmd(|ack| ActorCmd::CreateQueue {
            name: req.name,
            speed_limit_kbps: req.speed_limit_kbps,
            max_concurrent: req.max_concurrent,
            default_save_dir: req.default_save_dir,
            default_segments: req.default_segments,
            default_user_agent: req.default_user_agent,
            ack,
        })
        .await?;
    Ok(ok_response())
}

/// 更新命名队列。
#[utoipa::path(put, path = "/api/v1/queues/{id}", tag = "server",
    params(("id" = String, Path, description = "队列 ID")),
    request_body = UpdateQueueRequest,
    responses((status = 200, description = "已更新")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn update_queue(
    State(state): State<ServerState>,
    Path(id): Path<String>,
    axum::Json(req): axum::Json<UpdateQueueRequest>,
) -> Result<Response, ApiError> {
    state
        .send_cmd(|ack| ActorCmd::UpdateQueue {
            queue_id: id,
            name: req.name,
            speed_limit_kbps: req.speed_limit_kbps,
            max_concurrent: req.max_concurrent,
            default_save_dir: req.default_save_dir,
            default_segments: req.default_segments,
            default_user_agent: req.default_user_agent,
            ack,
        })
        .await?;
    Ok(ok_response())
}

/// 删除命名队列（队列内任务移入默认队列）。
#[utoipa::path(delete, path = "/api/v1/queues/{id}", tag = "server",
    params(("id" = String, Path, description = "队列 ID")),
    responses((status = 200, description = "已删除")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn delete_queue(
    State(state): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    state
        .send_cmd(|ack| ActorCmd::DeleteQueue { queue_id: id, ack })
        .await?;
    Ok(ok_response())
}

/// 启动队列：置运行态并按队列内顺序恢复其中所有待下载任务。
#[utoipa::path(post, path = "/api/v1/queues/{id}/start", tag = "server",
    params(("id" = String, Path, description = "队列 ID")),
    responses((status = 200, description = "已启动")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn start_queue(
    State(state): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    state
        .send_cmd(|ack| ActorCmd::StartQueue { queue_id: id, ack })
        .await?;
    Ok(ok_response())
}

/// 停止队列：置停止态并暂停其中所有排队/活跃任务。
#[utoipa::path(post, path = "/api/v1/queues/{id}/stop", tag = "server",
    params(("id" = String, Path, description = "队列 ID")),
    responses((status = 200, description = "已停止")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn stop_queue(
    State(state): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    state
        .send_cmd(|ack| ActorCmd::StopQueue { queue_id: id, ack })
        .await?;
    Ok(ok_response())
}

/// 更新队列的每日定时启停计划。
#[utoipa::path(put, path = "/api/v1/queues/{id}/schedule", tag = "server",
    params(("id" = String, Path, description = "队列 ID")),
    request_body = QueueScheduleRequest,
    responses((status = 200, description = "已更新")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn set_queue_schedule(
    State(state): State<ServerState>,
    Path(id): Path<String>,
    axum::Json(req): axum::Json<QueueScheduleRequest>,
) -> Result<Response, ApiError> {
    state
        .send_cmd(|ack| ActorCmd::SetQueueSchedule {
            queue_id: id,
            enabled: req.enabled,
            start_time: req.start_time,
            stop_time: req.stop_time,
            days: req.days,
            ack,
        })
        .await?;
    Ok(ok_response())
}

/// 持久化队列内任务顺序（1..N 写入 queueOrder，队列启动按此顺序恢复）。
#[utoipa::path(put, path = "/api/v1/queues/{id}/order", tag = "server",
    params(("id" = String, Path, description = "队列 ID")),
    request_body = ReorderQueueRequest,
    responses((status = 200, description = "已排序")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn reorder_queue(
    State(state): State<ServerState>,
    Path(id): Path<String>,
    axum::Json(req): axum::Json<ReorderQueueRequest>,
) -> Result<Response, ApiError> {
    state
        .send_cmd(|ack| ActorCmd::ReorderQueue {
            queue_id: id,
            task_ids: req.task_ids,
            ack,
        })
        .await?;
    Ok(ok_response())
}

/// 移动任务到指定队列（空 queueId = 默认队列）。
#[utoipa::path(put, path = "/api/v1/tasks/{id}/queue", tag = "server",
    params(("id" = String, Path, description = "任务 ID")),
    request_body = MoveQueueRequest,
    responses((status = 200, description = "已移动")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn move_task_queue(
    State(state): State<ServerState>,
    Path(id): Path<String>,
    axum::Json(req): axum::Json<MoveQueueRequest>,
) -> Result<Response, ApiError> {
    state
        .send_cmd(|ack| ActorCmd::MoveToQueue {
            task_id: id,
            queue_id: req.queue_id,
            ack,
        })
        .await?;
    Ok(ok_response())
}

/// Boost 优先下载该任务（暂停其他任务以释放带宽）。
#[utoipa::path(put, path = "/api/v1/tasks/{id}/boost", tag = "server",
    params(("id" = String, Path, description = "任务 ID")),
    responses((status = 200, description = "已 Boost")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn boost_task(
    State(state): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    state
        .send_cmd(|ack| ActorCmd::Boost { task_id: id, ack })
        .await?;
    Ok(ok_response())
}

fn ok_response() -> Response {
    axum::Json(serde_json::json!({ "success": true, "message": "ok" })).into_response()
}

// ---------------------------------------------------------------------------
// 已完成文件取回（浏览器「保存到本地」）
// ---------------------------------------------------------------------------

/// 流式取回已完成任务的文件。`?token=` 鉴权（浏览器导航下载无法设 header）。
#[utoipa::path(get, path = "/api/v1/tasks/{id}/file", tag = "server",
    params(
        ("id" = String, Path, description = "任务 ID"),
        ("token" = String, Query, description = "管理 token")
    ),
    responses(
        (status = 200, description = "文件字节流（attachment）"),
        (status = 400, description = "任务未完成"),
        (status = 404, description = "任务或文件不存在")
    )
)]
async fn task_file(
    State(state): State<ServerState>,
    Path(id): Path<String>,
    Query(q): Query<TokenQuery>,
) -> Response {
    if state.token.is_empty() || !constant_time_eq(&q.token, &state.token) {
        return error_response(StatusCode::UNAUTHORIZED, "invalid or missing token");
    }
    let task = match state.db.load_task_by_id(&id).await {
        Ok(Some(t)) => t,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "task not found"),
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    if task.status != 3 {
        return error_response(StatusCode::BAD_REQUEST, "task is not completed");
    }
    let path = PathBuf::from(&task.save_dir).join(&task.file_name);
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return error_response(StatusCode::NOT_FOUND, "file not found on disk"),
    };
    let len = file.metadata().await.ok().map(|m| m.len());
    let stream = ReaderStream::new(file);
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    if let Some(len) = len
        && let Ok(v) = header::HeaderValue::from_str(&len.to_string())
    {
        headers.insert(header::CONTENT_LENGTH, v);
    }
    if let Ok(v) = header::HeaderValue::from_str(&content_disposition(&task.file_name)) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    response
}

/// 构造 `Content-Disposition: attachment`。非 ASCII 文件名走 RFC 5987
/// `filename*`（UTF-8 百分号编码），另给纯 ASCII 兜底 `filename`。
fn content_disposition(file_name: &str) -> String {
    let ascii_fallback: String = file_name
        .chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && c != '\\' && !c.is_ascii_control() {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded = percent_encode_rfc5987(file_name);
    format!("attachment; filename=\"{ascii_fallback}\"; filename*=UTF-8''{encoded}")
}

/// RFC 5987 attr-char 之外的字节全部百分号编码。
fn percent_encode_rfc5987(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric()
            || matches!(
                c,
                '!' | '#' | '$' | '&' | '+' | '-' | '.' | '^' | '_' | '`' | '|' | '~'
            )
        {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 日志（目录路径展示 + zip 导出，NAS 远程运维用）
// ---------------------------------------------------------------------------

/// 日志目录路径与文件清单。前端「关于」页展示路径 + 提供导出入口。
#[utoipa::path(get, path = "/api/v1/logs", tag = "server",
    responses((status = 200, body = LogsResponse)),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn logs_info() -> Result<Response, ApiError> {
    let dir = fluxdown_engine::logger::log_dir().display().to_string();
    let files = fluxdown_engine::logger::list_log_files()
        .into_iter()
        .map(|m| LogFileDto {
            name: m.name,
            size: m.size as i64,
        })
        .collect();
    Ok(axum::Json(LogsResponse { dir, files }).into_response())
}

/// 导出全部日志为 zip 下载。`?token=` 鉴权（浏览器导航下载无法设 header，
/// 与 `/tasks/{id}/file` 一致）。
#[utoipa::path(get, path = "/api/v1/logs/export", tag = "server",
    params(("token" = String, Query, description = "管理 token")),
    responses(
        (status = 200, description = "日志压缩包（attachment: fluxdown_logs.zip）"),
        (status = 401, description = "token 无效或缺失")
    )
)]
async fn logs_export(State(state): State<ServerState>, Query(q): Query<TokenQuery>) -> Response {
    if state.token.is_empty() || !constant_time_eq(&q.token, &state.token) {
        return error_response(StatusCode::UNAUTHORIZED, "invalid or missing token");
    }
    let bytes = match fluxdown_engine::logger::export_logs_zip() {
        Ok(b) => b,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/zip"),
    );
    if let Ok(v) = header::HeaderValue::from_str(&content_disposition("fluxdown_logs.zip")) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    response
}

// ---------------------------------------------------------------------------
// 目录列举（服务器端保存目录选择器）
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FsQuery {
    #[serde(default)]
    path: String,
}

/// 列举服务器上某目录的子目录（仅目录，不含文件）。空 path = 默认保存目录。
#[utoipa::path(get, path = "/api/v1/fs/list", tag = "server",
    params(("path" = Option<String>, Query, description = "要列举的目录；空 = 默认保存目录")),
    responses((status = 200, body = FsListResponse)),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn fs_list(
    State(state): State<ServerState>,
    Query(q): Query<FsQuery>,
) -> Result<Response, ApiError> {
    let base = if q.path.trim().is_empty() {
        state.current_save_dir().await
    } else {
        q.path
    };
    let base_path = FsPath::new(&base);
    let parent = base_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned());
    let mut dirs = Vec::new();
    // 群晖/QNAP 等套件以受限用户运行，浏览到授权目录之外会 EACCES。
    // 此时不整体失败（否则选择器卡死显示「目录读取失败」，无法后退换路径），
    // 而是返回空子目录列表 + 可用的 parent，让用户仍能沿面包屑/上级继续导航。
    if let Ok(mut rd) = tokio::fs::read_dir(base_path).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // 隐藏目录（.git 等）不进选择器。
            if name.starts_with('.') {
                continue;
            }
            dirs.push(FsEntry {
                path: entry.path().to_string_lossy().into_owned(),
                name,
            });
        }
    }
    dirs.sort_by_key(|a| a.name.to_lowercase());
    Ok(axum::Json(FsListResponse {
        path: base,
        parent,
        dirs,
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// 代理测试 / token / 运行状态
// ---------------------------------------------------------------------------

/// 测试代理连通性，返回延迟（毫秒）。
#[utoipa::path(post, path = "/api/v1/proxy/test", tag = "server",
    request_body = ProxyTestRequest,
    responses(
        (status = 200, body = ProxyTestResponse),
        (status = 400, description = "连接失败，message 带原因")
    ),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn proxy_test(
    State(state): State<ServerState>,
    axum::Json(req): axum::Json<ProxyTestRequest>,
) -> Result<Response, ApiError> {
    let result = state
        .send_cmd(|ack| ActorCmd::TestProxy {
            proxy_type: req.proxy_type,
            host: req.host,
            port: req.port,
            username: req.username,
            password: req.password,
            ack,
        })
        .await?;
    match result {
        Ok(latency_ms) => Ok(axum::Json(ProxyTestResponse { latency_ms }).into_response()),
        Err(e) => Err(ApiError::BadRequest(e)),
    }
}

/// 重新生成管理 token 并持久化。**重启服务器后生效**（当前会话沿用旧 token）。
#[utoipa::path(post, path = "/api/v1/token/regenerate", tag = "server",
    responses((status = 200, body = TokenResponse)),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn token_regenerate(State(state): State<ServerState>) -> Result<Response, ApiError> {
    let token = format!("fxd_{}", uuid::Uuid::new_v4().simple());
    state
        .db
        .set_config("local_server_token", &token)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(axum::Json(TokenResponse {
        token,
        note: "restart the server for the new token to take effect".into(),
    })
    .into_response())
}

/// 服务器运行状态（磁盘剩余 / WS 连接数 / 版本 / 演示模式）。
#[utoipa::path(get, path = "/api/v1/stats", tag = "server",
    responses((status = 200, body = StatsResponse)),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn stats(State(state): State<ServerState>) -> Result<Response, ApiError> {
    let save_dir = state.current_save_dir().await;
    let disk_free_bytes = fs2::available_space(FsPath::new(&save_dir)).ok();
    Ok(axum::Json(StatsResponse {
        disk_free_bytes,
        save_dir,
        server_version: state.version.clone(),
        ws_clients: state.hub.events.receiver_count(),
        demo_mode: state.demo_url.is_some(),
        demo_url: state.demo_url.clone().unwrap_or_default(),
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// 组件（v1 仅 ffmpeg）—— 不走 actor，直接持 Db + data_dir 调引擎 components API。
// ---------------------------------------------------------------------------

/// ffmpeg 组件状态探测（生效路径来源 / 版本 / 托管版本 / 系统路径）。
#[utoipa::path(get, path = "/api/v1/components/ffmpeg", tag = "server",
    responses((status = 200, body = ComponentFfmpegStatus)),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn component_ffmpeg_status(State(state): State<ServerState>) -> Result<Response, ApiError> {
    let status = ffmpeg_status(&state.db, &state.data_dir).await;
    Ok(axum::Json(ComponentFfmpegStatus::from(status)).into_response())
}

/// 列出当前平台可安装的 ffmpeg 稳定版本（降序；数据来自 BtbN/FFmpeg-Builds latest Release）。
#[utoipa::path(get, path = "/api/v1/components/ffmpeg/versions", tag = "server",
    responses(
        (status = 200, body = ComponentVersions),
        (status = 500, description = "拉取版本列表失败（网络错误或当前平台无托管构建）")
    ),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn component_ffmpeg_versions(
    State(_state): State<ServerState>,
) -> Result<Response, ApiError> {
    let client =
        build_client(&ProxyConfig::default(), "").map_err(|e| ApiError::Internal(e.to_string()))?;
    let versions = list_versions(&client)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(axum::Json(ComponentVersions::from(versions)).into_response())
}

/// 安装/更新托管 ffmpeg：立即返回 202，实际下载在后台执行——进度经 WS
/// `componentProgress` 推送，完成/失败经 `componentResult` 推送。安装期间
/// 重复请求返回 400（`ffmpeg_installing` 互斥标志去重）。
#[utoipa::path(post, path = "/api/v1/components/ffmpeg/install", tag = "server",
    request_body = InstallFfmpegRequest,
    responses(
        (status = 202, description = "已开始安装，经 WS 推送进度/结果"),
        (status = 400, description = "已有安装任务在进行中")
    ),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn component_ffmpeg_install(
    State(state): State<ServerState>,
    axum::Json(req): axum::Json<InstallFfmpegRequest>,
) -> Result<Response, ApiError> {
    if state.ffmpeg_installing.swap(true, Ordering::SeqCst) {
        return Err(ApiError::BadRequest(
            "ffmpeg install already in progress".to_string(),
        ));
    }
    let db = state.db.clone();
    let data_dir = state.data_dir.clone();
    let hub = state.hub.clone();
    let flag = state.ffmpeg_installing.clone();
    tokio::spawn(async move {
        let outcome: Result<(), String> = async {
            let client = build_client(&ProxyConfig::default(), "").map_err(|e| e.to_string())?;
            let hub_progress = hub.clone();
            let progress = move |downloaded: u64, total: u64| {
                hub_progress.broadcast(&WsServerMsg::ComponentProgress {
                    component: "ffmpeg".to_string(),
                    downloaded_bytes: downloaded as i64,
                    total_bytes: total as i64,
                });
            };
            install_ffmpeg(&db, &data_dir, &client, req.version.as_deref(), &progress)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        .await;
        match outcome {
            Ok(()) => hub.broadcast(&WsServerMsg::ComponentResult {
                component: "ffmpeg".to_string(),
                ok: true,
                message: "installed".to_string(),
            }),
            Err(message) => {
                log_info!("[components] ffmpeg install failed: {}", message);
                hub.broadcast(&WsServerMsg::ComponentResult {
                    component: "ffmpeg".to_string(),
                    ok: false,
                    message,
                });
            }
        }
        flag.store(false, Ordering::SeqCst);
    });
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(serde_json::json!({ "success": true, "message": "installing" })),
    )
        .into_response())
}

/// 卸载托管安装（删除数据目录 `bin/ffmpeg[.exe]` 与版本记录；手动/系统路径不受影响）。
#[utoipa::path(post, path = "/api/v1/components/ffmpeg/uninstall", tag = "server",
    responses((status = 200, description = "已卸载")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn component_ffmpeg_uninstall(
    State(state): State<ServerState>,
) -> Result<Response, ApiError> {
    uninstall_ffmpeg(&state.db, &state.data_dir)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(ok_response())
}

// ---------------------------------------------------------------------------
// 组件（yt-dlp）—— 不走 actor，直接持 Db + data_dir 调引擎 components API。
// ---------------------------------------------------------------------------

/// yt-dlp 组件状态探测（生效路径来源 / 版本 / 托管版本 / 系统路径）。
#[utoipa::path(get, path = "/api/v1/components/ytdlp", tag = "server",
    responses((status = 200, body = ComponentYtdlpStatus)),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn component_ytdlp_status(State(state): State<ServerState>) -> Result<Response, ApiError> {
    let status = ytdlp_status(&state.db, &state.data_dir).await;
    Ok(axum::Json(ComponentYtdlpStatus::from(status)).into_response())
}

/// 列出当前平台可安装的 yt-dlp 稳定版本（降序；数据来自 yt-dlp/yt-dlp latest Release）。
#[utoipa::path(get, path = "/api/v1/components/ytdlp/versions", tag = "server",
    responses(
        (status = 200, body = ComponentVersions),
        (status = 500, description = "拉取版本列表失败（网络错误或当前平台无托管构建）")
    ),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn component_ytdlp_versions(State(_state): State<ServerState>) -> Result<Response, ApiError> {
    let client =
        build_client(&ProxyConfig::default(), "").map_err(|e| ApiError::Internal(e.to_string()))?;
    let versions = list_ytdlp_versions(&client)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(axum::Json(ComponentVersions::from(versions)).into_response())
}

/// 安装/更新托管 yt-dlp：立即返回 202，实际下载在后台执行——进度经 WS
/// `componentProgress` 推送，完成/失败经 `componentResult` 推送。安装期间
/// 重复请求返回 400（`ytdlp_installing` 互斥标志去重）。
#[utoipa::path(post, path = "/api/v1/components/ytdlp/install", tag = "server",
    request_body = InstallFfmpegRequest,
    responses(
        (status = 202, description = "已开始安装，经 WS 推送进度/结果"),
        (status = 400, description = "已有安装任务在进行中")
    ),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn component_ytdlp_install(
    State(state): State<ServerState>,
    axum::Json(req): axum::Json<InstallFfmpegRequest>,
) -> Result<Response, ApiError> {
    if state.ytdlp_installing.swap(true, Ordering::SeqCst) {
        return Err(ApiError::BadRequest(
            "ytdlp install already in progress".to_string(),
        ));
    }
    let db = state.db.clone();
    let data_dir = state.data_dir.clone();
    let hub = state.hub.clone();
    let flag = state.ytdlp_installing.clone();
    tokio::spawn(async move {
        let outcome: Result<(), String> = async {
            let client = build_client(&ProxyConfig::default(), "").map_err(|e| e.to_string())?;
            let hub_progress = hub.clone();
            let progress = move |downloaded: u64, total: u64| {
                hub_progress.broadcast(&WsServerMsg::ComponentProgress {
                    component: "ytdlp".to_string(),
                    downloaded_bytes: downloaded as i64,
                    total_bytes: total as i64,
                });
            };
            install_ytdlp(&db, &data_dir, &client, req.version.as_deref(), &progress)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        .await;
        match outcome {
            Ok(()) => hub.broadcast(&WsServerMsg::ComponentResult {
                component: "ytdlp".to_string(),
                ok: true,
                message: "installed".to_string(),
            }),
            Err(message) => {
                log_info!("[components] ytdlp install failed: {}", message);
                hub.broadcast(&WsServerMsg::ComponentResult {
                    component: "ytdlp".to_string(),
                    ok: false,
                    message,
                });
            }
        }
        flag.store(false, Ordering::SeqCst);
    });
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(serde_json::json!({ "success": true, "message": "installing" })),
    )
        .into_response())
}

/// 卸载托管安装（删除数据目录 `bin/yt-dlp[.exe]` 与版本记录；手动/系统路径不受影响）。
#[utoipa::path(post, path = "/api/v1/components/ytdlp/uninstall", tag = "server",
    responses((status = 200, description = "已卸载")),
    security(("bearer_token" = []), ("api_key" = []))
)]
async fn component_ytdlp_uninstall(State(state): State<ServerState>) -> Result<Response, ApiError> {
    uninstall_ytdlp(&state.db, &state.data_dir)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(ok_response())
}

// ---------------------------------------------------------------------------
// OpenAPI（核心 + 扩展合并）与 Scalar 文档
// ---------------------------------------------------------------------------

/// 本 crate 扩展端点的 OpenAPI 文档（与 `fluxdown_api::openapi::ApiDoc` 合并）。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "FluxDown Server API",
        description = "FluxDown headless 服务器 HTTP API：核心任务管理（复用桌面 API 契约）\
            + 服务器扩展（WebSocket 实时推送 / 配置 / 队列 CRUD / 文件取回 / 目录列举 / 代理测试）。",
        version = crate::SERVER_VERSION,
    ),
    paths(
        ws_handler,
        get_config,
        put_config,
        create_queue,
        update_queue,
        delete_queue,
        start_queue,
        stop_queue,
        set_queue_schedule,
        reorder_queue,
        move_task_queue,
        boost_task,
        task_file,
        fs_list,
        proxy_test,
        token_regenerate,
        stats,
        logs_info,
        logs_export,
        bt_tracker_sub_refresh,
        component_ffmpeg_status,
        component_ffmpeg_versions,
        component_ffmpeg_install,
        component_ffmpeg_uninstall,
        component_ytdlp_status,
        component_ytdlp_versions,
        component_ytdlp_install,
        component_ytdlp_uninstall,
    ),
    components(schemas(
        crate::wire::WsServerMsg,
        crate::wire::WsClientMsg,
        crate::wire::SegmentDetailDto,
        crate::wire::QueuePositionDto,
        crate::wire::HlsQualityOptionDto,
        crate::wire::ResolveVariantOptionDto,
        crate::wire::BtFileDto,
        crate::wire::CreateQueueRequest,
        crate::wire::MoveQueueRequest,
        crate::wire::QueueScheduleRequest,
        crate::wire::ReorderQueueRequest,
        crate::wire::ProxyTestRequest,
        crate::wire::ProxyTestResponse,
        crate::wire::FsEntry,
        crate::wire::FsListResponse,
        crate::wire::StatsResponse,
        crate::wire::LogsResponse,
        crate::wire::LogFileDto,
        crate::wire::TokenResponse,
        crate::wire::ComponentFfmpegStatus,
        crate::wire::ComponentYtdlpStatus,
        crate::wire::ComponentVersions,
        crate::wire::InstallFfmpegRequest,
        crate::wire::TrackerSubRefreshResponse,
    )),
    tags((name = "server", description = "headless 服务器扩展端点"))
)]
struct ServerApiDoc;

/// 合并版规范 JSON（`OnceLock` 缓存；self 冲突时胜，当前两侧路径不相交）。
fn merged_openapi_json() -> &'static str {
    static SPEC: OnceLock<String> = OnceLock::new();
    SPEC.get_or_init(|| {
        let merged = ServerApiDoc::openapi().merge_from(fluxdown_api::openapi::ApiDoc::openapi());
        merged
            .to_pretty_json()
            .unwrap_or_else(|e| format!("{{\"error\":\"openapi serialize failed: {e}\"}}"))
    })
}

/// OpenAPI 3.1 规范（合并核心 + 扩展）。无鉴权：纯接口描述。
async fn openapi_spec() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        merged_openapi_json(),
    )
        .into_response()
}

/// Scalar API 文档页（CDN 加载，渲染 `/api/v1/openapi.json`）。
async fn scalar_docs() -> Html<&'static str> {
    Html(SCALAR_HTML)
}

/// 镜像 `website/src/pages/api-docs.astro` 的 Scalar 嵌入片段。
const SCALAR_HTML: &str = r#"<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>FluxDown Server API Docs</title>
    <style>html, body, #app { height: 100%; margin: 0; }</style>
  </head>
  <body>
    <div id="app"></div>
    <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference"></script>
    <script>
      Scalar.createApiReference('#app', {
        url: '/api/v1/openapi.json',
        theme: 'none',
        layout: 'modern',
      });
    </script>
  </body>
</html>
"#;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_ascii_filename_is_not_percent_encoded() {
        let header = content_disposition("report-final_v2.pdf");
        assert_eq!(
            header,
            "attachment; filename=\"report-final_v2.pdf\"; filename*=UTF-8''report-final_v2.pdf"
        );
    }

    #[test]
    fn content_disposition_non_ascii_filename_encodes_filename_star_and_ascii_fallback() {
        let header = content_disposition("测试文件.mp4");
        // ASCII fallback: every non-ASCII char replaced 1:1 with '_', ASCII
        // suffix kept verbatim -- browsers without RFC 5987 support still
        // get a syntactically valid (if illegible) filename.
        assert!(
            header.contains("filename=\"____.mp4\""),
            "unexpected ascii fallback in {header:?}"
        );
        // filename* carries the real UTF-8 bytes, percent-encoded.
        let expected_star = format!(
            "filename*=UTF-8''{}.mp4",
            "测试文件"
                .bytes()
                .map(|b| format!("%{b:02X}"))
                .collect::<String>()
        );
        assert!(
            header.contains(&expected_star),
            "expected {expected_star:?} in {header:?}"
        );
    }

    #[test]
    fn content_disposition_quote_and_backslash_do_not_break_header_syntax() {
        let header = content_disposition("weird\"na\\me.txt");
        // The whole header must never contain a raw backslash: percent-
        // encoding in filename* and the ASCII-fallback substitution both
        // avoid it, so any raw '\\' means one leaked through unescaped.
        assert!(
            !header.contains('\\'),
            "raw backslash leaked into header: {header:?}"
        );
        // The `filename="..."` quoted-string token must have exactly its
        // two delimiting quotes -- a leaked raw quote from the original
        // name would prematurely terminate the token and corrupt every
        // header field that follows it.
        assert_eq!(
            header.matches('"').count(),
            2,
            "raw quote leaked into header, corrupting quoted-string syntax: {header:?}"
        );
        // The RFC 5987 filename* form percent-encodes both characters
        // instead of leaving them raw.
        assert!(
            header.contains("%22"),
            "quote must be percent-encoded: {header:?}"
        );
        assert!(
            header.contains("%5C"),
            "backslash must be percent-encoded: {header:?}"
        );
    }

    #[test]
    fn percent_encode_rfc5987_keeps_unreserved_chars_and_encodes_the_rest() {
        assert_eq!(percent_encode_rfc5987("abcXYZ019"), "abcXYZ019");
        assert_eq!(percent_encode_rfc5987("a b"), "a%20b");
        assert_eq!(percent_encode_rfc5987("100%"), "100%25");
    }
}
