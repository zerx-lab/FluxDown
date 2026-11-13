//! loopback-only daemon HTTP 与鉴权 WebSocket 传输。

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use axum::extract::{Path as AxumPath, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use fluxdown_protocol::{CLOSE_REASON_SERVICE_QUIT, RpcResponse};
use futures_util::StreamExt;
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::blob_store::BlobKind;
use crate::event_pump::{
    EventPump, MAX_PENDING_FRAMES, OUTBOX_CAPACITY, finish_writer, run_writer,
};
use crate::rpc::{DaemonAuth, MAX_SLOW_CALLS, RpcSession, SessionReply, SlowCalls, Step};
use crate::service::DaemonService;

const REQUEST_BODY_LIMIT: usize = 4 * 1024 * 1024;
/// 插件包 blob 上限：与插件包体积上限（引擎 `MAX_FXPLUG_BYTES` / FluxCloud 私有包上限）一致。
const PLUGIN_BLOB_LIMIT: usize = 10 * 1024 * 1024;
/// WebSocket 单条消息上限：握手前任何对端都能发帧，不能沿用库默认的 64 MiB。
const WS_MESSAGE_LIMIT: usize = 16 * 1024 * 1024;
/// 握手帧只有几百字节；认证前超过该长度的帧直接断开。
const PRE_AUTH_FRAME_LIMIT: usize = 4 * 1024;
/// 握手必须在该时限内完成，未认证连接不得长期占着名额。
const AUTH_DEADLINE: Duration = Duration::from_secs(10);
/// 同时处于握手阶段（尚未认证）的连接上限。
const MAX_PENDING_AUTH: usize = 16;

#[derive(Clone)]
struct HttpState {
    service: Arc<DaemonService>,
    auth: Arc<DaemonAuth>,
    cancel: CancellationToken,
    /// 由 `system.shutdown` 触发的退出：各连接以 [`CLOSE_REASON_SERVICE_QUIT`] 关闭。
    quit_requested: Arc<AtomicBool>,
    pending_auth: Arc<Semaphore>,
}

/// 启动 daemon HTTP 服务直到取消或 listener 失败。
///
/// `token` 是 agent 与 daemon 共享的长期密钥：新版 agent 只用它做 WebSocket 握手证明、
/// HTTP 端点用已握手会话派生出的凭据；旧版 agent 的静态 Bearer 仍被接受（见 [`DaemonAuth`]）。
pub async fn serve(
    listener: TcpListener,
    service: Arc<DaemonService>,
    token: String,
    cancel: CancellationToken,
) -> Result<(), std::io::Error> {
    let state = HttpState {
        service,
        auth: Arc::new(DaemonAuth::new(token)),
        cancel: cancel.clone(),
        quit_requested: Arc::new(AtomicBool::new(false)),
        pending_auth: Arc::new(Semaphore::new(MAX_PENDING_AUTH)),
    };
    let app = Router::new()
        .route("/rpc", get(rpc_upgrade))
        .route("/files/tasks/{task_id}", get(download_task_file))
        .route("/blobs/torrents", post(upload_torrent))
        .route(
            "/blobs/plugins",
            post(upload_plugin).layer(axum::extract::DefaultBodyLimit::max(PLUGIN_BLOB_LIMIT)),
        )
        .route("/exports/{export_id}", get(download_export))
        .route("/plugin-images/{session}/{id}", get(download_plugin_image))
        .layer(axum::extract::DefaultBodyLimit::max(REQUEST_BODY_LIMIT))
        .with_state(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(cancel.cancelled_owned())
        .await
}

/// 加载或创建 daemon token 文件。
pub async fn load_or_create_token(
    data_dir: &Path,
    override_path: Option<&Path>,
) -> Result<String, std::io::Error> {
    let path = override_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| data_dir.join("daemon.token"));
    if let Ok(existing) = tokio::fs::read_to_string(&path).await {
        let token = existing.trim();
        if !token.is_empty() {
            // 便携模式下旧版本创建的 token 可能仍继承宽松 ACL，每次启动重新收紧。
            if let Err(error) = set_private_file_permissions(&path).await {
                tracing::warn!(error = %error, "failed to restrict daemon token permissions");
            }
            return Ok(token.to_owned());
        }
    }
    let parent = path.parent().unwrap_or(data_dir);
    tokio::fs::create_dir_all(parent).await?;
    set_private_dir_permissions(parent).await?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let temp = temporary_path(&path);
    let mut options = tokio::fs::OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = options.open(&temp).await?;
    use tokio::io::AsyncWriteExt;
    file.write_all(token.as_bytes()).await?;
    file.write_all(b"\n").await?;
    file.sync_all().await?;
    drop(file);
    set_private_file_permissions(&temp).await?;
    tokio::fs::rename(&temp, &path).await?;
    Ok(token)
}

/// 升级头不带凭据时，身份证明在升级后的 `system.auth.*` 握手里完成，握手通过之前连接只能走
/// 握手帧。仍在升级头带静态 Bearer 的旧版 agent 只要 token 有效就直接视为已认证（无效 401）。
async fn rpc_upgrade(
    State(state): State<HttpState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    // 浏览器页面发起的 WebSocket 必带 Origin，agent 客户端从不发送：拒绝跨站页面占握手名额。
    if headers.contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (permit, bearer_authenticated) = if headers.contains_key(header::AUTHORIZATION) {
        if !bearer_token(&headers).is_some_and(|presented| state.auth.accepts_token(presented)) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        (None, true)
    } else {
        let Ok(permit) = state.pending_auth.clone().try_acquire_owned() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        (Some(permit), false)
    };
    upgrade
        .max_message_size(WS_MESSAGE_LIMIT)
        .on_upgrade(move |socket| run_socket(socket, state, permit, bearer_authenticated))
        .into_response()
}

async fn upload_torrent(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    upload_blob(
        &state,
        &headers,
        BlobKind::Torrent,
        REQUEST_BODY_LIMIT,
        body,
    )
    .await
}

async fn upload_plugin(State(state): State<HttpState>, headers: HeaderMap, body: Body) -> Response {
    upload_blob(&state, &headers, BlobKind::Plugin, PLUGIN_BLOB_LIMIT, body).await
}

/// 先鉴权再读取请求体：未授权请求不会让 daemon 缓冲最多 `limit` 字节。
async fn upload_blob(
    state: &HttpState,
    headers: &HeaderMap,
    kind: BlobKind,
    limit: usize,
    body: Body,
) -> Response {
    if !authorized(headers, &state.auth) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let body = match axum::body::to_bytes(body, limit).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    match state.service.blobs().put(kind, &body).await {
        Ok(blob_id) => axum::Json(serde_json::json!({ "blobId": blob_id })).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": format!("{error:#}") })),
        )
            .into_response(),
    }
}

async fn download_task_file(
    State(state): State<HttpState>,
    headers: HeaderMap,
    AxumPath(task_id): AxumPath<String>,
) -> Response {
    if !authorized(&headers, &state.auth) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(task) = state.service.task(&task_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if task.status != 3 {
        return StatusCode::CONFLICT.into_response();
    }
    let path = PathBuf::from(&task.save_dir).join(&task.file_name);
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let metadata = match file.metadata().await {
        Ok(metadata) => metadata,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    // 多文件 BT 任务落盘为目录；对目录 open 在 Unix 上会成功，但读取必然失败，
    // 需在发出 200 头之前明确拒绝。
    if !metadata.is_file() {
        return StatusCode::CONFLICT.into_response();
    }
    let length = metadata.len();
    let disposition = attachment_disposition(&task.file_name);
    match Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, length)
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(Body::from_stream(ReaderStream::new(file)))
    {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn download_plugin_image(
    State(state): State<HttpState>,
    headers: HeaderMap,
    AxumPath((session, id)): AxumPath<(String, String)>,
) -> Response {
    if !authorized(&headers, &state.auth) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(bytes) = state.service.plugin_image(&session, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Keep the registry's Arc alive while streaming, without copying the image.
    let length = bytes.len();
    (
        [
            (header::CONTENT_TYPE, "image/png".to_owned()),
            (header::CONTENT_LENGTH, length.to_string()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        Body::from(axum::body::Bytes::from_owner(bytes)),
    )
        .into_response()
}

/// `attachment` 头：ASCII 回退 `filename` + RFC 5987 `filename*`（UTF-8 百分号编码），
/// 浏览器优先取后者，非 ASCII 文件名不再乱码。
fn attachment_disposition(file_name: &str) -> String {
    let fallback: String = file_name
        .chars()
        .map(|c| {
            if (c.is_ascii_graphic() && !matches!(c, '"' | '\\' | '%')) || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(file_name.len());
    for byte in file_name.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            )
        {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

async fn download_export(
    State(state): State<HttpState>,
    headers: HeaderMap,
    AxumPath(export_id): AxumPath<String>,
) -> Response {
    if !authorized(&headers, &state.auth) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let bytes = match state.service.blobs().read(&export_id, BlobKind::Logs).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    if state
        .service
        .blobs()
        .consume(&export_id, BlobKind::Logs)
        .await
        .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    (StatusCode::OK, bytes).into_response()
}

/// 连接循环只读套接字、受理 RPC，并把应答 / 控制帧放进有序出站通道；套接字写端由
/// [`run_writer`] 独占，事件经 [`EventPump`] 合并后由它写出（见 `event_pump` 模块）。因此
/// 等待 actor/DB 或套接字写慢时广播接收都不停。普通调用保持 FIFO，慢调用沿用有界后台
/// 执行。不能把普通 create 改为慢方法，否则 agent 的独立通道会破坏 create/pause 等命令的
/// 串行顺序。
async fn run_socket(
    socket: WebSocket,
    state: HttpState,
    mut auth_permit: Option<OwnedSemaphorePermit>,
    bearer_authenticated: bool,
) {
    let HttpState {
        service,
        cancel,
        quit_requested,
        auth,
        ..
    } = state;
    let mut session = RpcSession::new(service.clone(), auth, bearer_authenticated);
    // 握手名额：认证通过即释放；期间受 `AUTH_DEADLINE` 约束。带有效静态 Bearer 升级的
    // 旧版 agent 没有名额，也不受握手时限约束。
    let auth_deadline = tokio::time::sleep(AUTH_DEADLINE);
    tokio::pin!(auth_deadline);
    let (sink, mut incoming_frames) = socket.split();
    let (out, outbox) = mpsc::channel::<Message>(OUTBOX_CAPACITY);
    let mut events = EventPump::new();
    let writer = tokio::spawn(run_writer(sink, outbox, events.reader()));
    let mut slow = SlowCalls::new(MAX_SLOW_CALLS);
    let mut ordered = std::pin::pin!(None);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let reason = if quit_requested.load(Ordering::Acquire) {
                    CLOSE_REASON_SERVICE_QUIT
                } else {
                    "daemon-shutdown"
                };
                send_close(&out, 1001, reason).await;
                break;
            }
            _ = &mut auth_deadline, if auth_permit.is_some() => {
                send_close(&out, 1008, "auth-timeout").await;
                break;
            }
            // 写端已退出（写失败或 4009 已发出）：连接结束。
            () = out.closed() => break,
            incoming = incoming_frames.next(), if ordered.as_ref().get_ref().is_none() => {
                let Some(Ok(message)) = incoming else { break; };
                match message {
                    Message::Text(text) => {
                        if !session.is_authenticated() && text.len() > PRE_AUTH_FRAME_LIMIT {
                            send_close(&out, 1009, "frame too large before authentication").await;
                            break;
                        }
                        let reply = match session.accept(&text) {
                            Step::Reply(reply) => reply,
                            Step::Call(call) if call.is_slow() => {
                                match slow.spawn(call.id(), call.run()) {
                                    Ok(()) => continue,
                                    Err(rejection) => SessionReply::reply(*rejection),
                                }
                            }
                            Step::Call(call) => {
                                ordered.as_mut().set(Some(call.run()));
                                continue;
                            }
                        };
                        if reply.became_ready {
                            events.attach(service.events().subscribe(), MAX_PENDING_FRAMES);
                        }
                        if session.is_authenticated() {
                            auth_permit = None;
                        }
                        if send_json(&out, &reply.response).await.is_err() { break; }
                        if reply.shutdown_requested {
                            quit_requested.store(true, Ordering::Release);
                            cancel.cancel();
                        }
                        if reply.close {
                            send_close(&out, 1008, "unauthorized").await;
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(payload) => {
                        if out.send(Message::Pong(payload)).await.is_err() { break; }
                    }
                    Message::Pong(_) => {}
                    Message::Binary(_) => {
                        send_close(&out, 1003, "text frames required").await;
                        break;
                    }
                }
            }
            response = receive_ordered(ordered.as_mut()), if ordered.as_ref().get_ref().is_some() => {
                ordered.as_mut().set(None);
                if send_json(&out, &response).await.is_err() { break; }
            }
            finished = slow.next(), if !slow.is_empty() => {
                let Some(response) = finished else { continue; };
                if send_json(&out, &response).await.is_err() { break; }
            }
        }
    }
    // 先中止在途调用，再冲刷出站消息（含关闭帧），最后释放连接所有权的选择订阅。
    ordered.as_mut().set(None);
    drop(slow);
    finish_writer(out, writer).await;
    drop(events);
    session.disconnect();
}

/// 应答序列化后放进出站通道；写端已退出时返回 `Err`。
async fn send_json<T: serde::Serialize>(out: &mpsc::Sender<Message>, value: &T) -> Result<(), ()> {
    let json = serde_json::to_string(value).map_err(|_| ())?;
    out.send(Message::Text(json.into())).await.map_err(|_| ())
}

async fn send_close(out: &mpsc::Sender<Message>, code: u16, reason: &'static str) {
    let close = Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }));
    if out.send(close).await.is_err() {
        // 对端可能已经断开；关闭帧 best effort，不影响本地关闭。
        tracing::debug!(
            close_code = code,
            "daemon WebSocket close frame was not delivered"
        );
    }
}

async fn receive_ordered<F: Future<Output = RpcResponse>>(
    call: Pin<&mut Option<F>>,
) -> RpcResponse {
    match call.as_pin_mut() {
        Some(call) => call.await,
        None => std::future::pending().await,
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// HTTP 端点的 `Authorization: Bearer`：某个存活会话的 HTTP 凭据，或旧版 agent 的静态 token。
fn authorized(headers: &HeaderMap, auth: &DaemonAuth) -> bool {
    bearer_token(headers).is_some_and(|credential| auth.authorizes_http(credential))
}

pub(crate) fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("daemon.token");
    path.with_file_name(format!(".{name}.{}.tmp", Uuid::new_v4()))
}

#[cfg(unix)]
async fn set_private_dir_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await
}

#[cfg(not(unix))]
async fn set_private_dir_permissions(path: &Path) -> Result<(), std::io::Error> {
    crate::private_fs::restrict_to_current_user(path, true).await
}

#[cfg(unix)]
pub(crate) async fn set_private_file_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await
}

#[cfg(not(unix))]
pub(crate) async fn set_private_file_permissions(path: &Path) -> Result<(), std::io::Error> {
    crate::private_fs::restrict_to_current_user(path, false).await
}

#[cfg(test)]
mod dispatch_tests;

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::{attachment_disposition, authorized};
    use crate::rpc::DaemonAuth;

    #[test]
    fn attachment_disposition_carries_utf8_name_and_safe_ascii_fallback() {
        assert_eq!(
            attachment_disposition("报告 v1.zip"),
            "attachment; filename=\"__ v1.zip\"; filename*=UTF-8''%E6%8A%A5%E5%91%8A%20v1.zip"
        );
        let quoted = attachment_disposition("a\"b\r\n%.bin");
        assert!(quoted.starts_with("attachment; filename=\"a_b___.bin\""));
        assert!(quoted.ends_with("filename*=UTF-8''a%22b%0D%0A%25.bin"));
    }

    #[test]
    fn http_endpoints_accept_live_session_credentials_and_the_legacy_static_token() {
        let auth = DaemonAuth::new("long-lived-token");
        auth.register_http("connection", "session-credential".to_owned());
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, &auth));
        for (value, accepted) in [
            ("Bearer session-credential", true),
            // 旧版 agent 仍发送静态 token：daemon 作为校验方继续接受。
            ("Bearer long-lived-token", true),
            ("Bearer wrong", false),
            ("session-credential", false),
            ("Basic long-lived-token", false),
        ] {
            headers.insert(header::AUTHORIZATION, HeaderValue::from_static(value));
            assert_eq!(authorized(&headers, &auth), accepted, "{value}");
        }
    }
}
