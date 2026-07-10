//! Named Pipe server for browser extension communication via Native Messaging.
//!
//! Architecture:
//!   - FluxDown main process creates a Named Pipe server at `\\.\pipe\fluxdown`.
//!   - The NMH relay binary (`fluxdown_nmh.exe`) connects to this pipe.
//!   - Messages use a 4-byte LE length prefix + JSON payload.
//!
//! Message protocol (mirrors the no-launch action set in
//! `native/nmh/src/main.rs` and the extension background script's
//! popup-facing runtime messages built on top of these):
//!   - `{"action":"ping","msg_id":N}`     → `{"success":true,"message":"pong","msg_id":N}`
//!   - `{"action":"download","msg_id":N, ...}` → `{"success":true,"message":"download accepted","msg_id":N}`
//!   - `{"action":"tasks","msg_id":N}` → `{"success":true,"msg_id":N,"tasks":[TaskBrief, ...]}`
//!     (every non-completed task + the 10 most recently completed, newest first)
//!   - `{"action":"task_op","msg_id":N,"op":"pause"|"resume"|"remove","taskId":"..."}`
//!     → `{"success":true,"msg_id":N}` / `{"success":false,"message":"...","msg_id":N}`
//!   - `{"action":"open_file","msg_id":N,"taskId":"..."}` /
//!     `{"action":"reveal_file","msg_id":N,"taskId":"..."}` → only allowed for
//!     `status == 3` (completed) tasks; rejects with a `message` otherwise
//!     (unknown task / not completed / file missing on disk).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use fluxdown_api::service::{ApiHost, LiveSpeed};
use fluxdown_api::types::{DownloadRequest, TaskDto};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::logger::{log_error, log_info};

/// Named Pipe path for the NMH relay to connect to.
#[cfg(windows)]
const PIPE_NAME: &str = r"\\.\pipe\fluxdown";

/// Maximum message size: 1 MB.
const MAX_MESSAGE_SIZE: u32 = 1024 * 1024;

/// Incoming pipe message with action routing.
#[derive(Debug, Deserialize)]
struct PipeMessage {
    action: String,
    #[serde(default)]
    msg_id: u64,
    #[serde(flatten)]
    payload: serde_json::Value,
}

/// JSON response sent back via the pipe.
#[derive(Debug, Serialize)]
struct PipeResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    msg_id: u64,
    /// Only set on a successful `"tasks"` response.
    #[serde(skip_serializing_if = "Option::is_none")]
    tasks: Option<Vec<TaskBrief>>,
}

impl PipeResponse {
    /// Successful response carrying a human-readable status `message`.
    fn ok(msg_id: u64, message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: Some(message.into()),
            msg_id,
            tasks: None,
        }
    }

    /// Failure response carrying the reason in `message`.
    fn err(msg_id: u64, message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: Some(message.into()),
            msg_id,
            tasks: None,
        }
    }

    /// Successful `"tasks"` response.
    fn with_tasks(msg_id: u64, tasks: Vec<TaskBrief>) -> Self {
        Self {
            success: true,
            message: None,
            msg_id,
            tasks: Some(tasks),
        }
    }
}

/// `taskId`-only payload shared by `task_op`/`open_file`/`reveal_file`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskIdPayload {
    task_id: String,
}

/// `{"op":"pause"|"resume"|"remove","taskId":"..."}` payload for `task_op`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskOpPayload {
    op: String,
    task_id: String,
}

/// Minimal task snapshot for the extension popup's task panel (`"tasks"`
/// action). Field shape is the stable wire contract shared with the
/// extension background script — keep in sync with its TaskBrief type.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskBrief {
    task_id: String,
    file_name: String,
    /// 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
    status: i32,
    downloaded_bytes: i64,
    total_bytes: i64,
    /// Download rate in bytes/sec; 0 when no live sample is available
    /// (idle/paused/just-finished tasks).
    speed: i64,
    error_message: String,
    created_at: String,
}

/// Task status code for "completed" (see [`TaskBrief::status`] doc).
const STATUS_COMPLETED: i32 = 3;

/// Max recently-completed tasks kept in the `"tasks"` response payload.
const MAX_COMPLETED_TASKS: usize = 10;

/// Build the `"tasks"` action payload: every non-completed task plus the
/// [`MAX_COMPLETED_TASKS`] most recently completed ones (by `created_at`
/// descending), each carrying its live speed merged in from `speeds`
/// (no entry ⇒ 0 B/s).
///
/// Pure so the filter/sort semantics are unit-testable without a running
/// pipe server or database.
fn select_task_briefs(tasks: Vec<TaskDto>, speeds: &HashMap<String, LiveSpeed>) -> Vec<TaskBrief> {
    let (mut completed, active): (Vec<TaskDto>, Vec<TaskDto>) = tasks
        .into_iter()
        .partition(|t| t.status == STATUS_COMPLETED);
    completed.sort_by_key(|t| std::cmp::Reverse(t.created_at.parse::<i64>().unwrap_or(0)));
    completed.truncate(MAX_COMPLETED_TASKS);

    active
        .into_iter()
        .chain(completed)
        .map(|t| {
            let speed = speeds.get(&t.task_id).map(|s| s.download_bps).unwrap_or(0);
            TaskBrief {
                task_id: t.task_id,
                file_name: t.file_name,
                status: t.status,
                downloaded_bytes: t.downloaded_bytes,
                total_bytes: t.total_bytes,
                speed,
                error_message: t.error_message,
                created_at: t.created_at,
            }
        })
        .collect()
}

/// Resolve `taskId` → absolute file path for `open_file`/`reveal_file`. Only
/// `status == 3` (completed) tasks are eligible; returns a pre-built failure
/// [`PipeResponse`] otherwise (unknown task, not completed yet, or the file
/// missing on disk).
async fn resolve_completed_task_path(
    msg_id: u64,
    task_id: &str,
    api_host: &Arc<dyn ApiHost>,
) -> Result<std::path::PathBuf, PipeResponse> {
    let task = match api_host.get_task(task_id).await {
        Ok(Some(t)) => t,
        Ok(None) => return Err(PipeResponse::err(msg_id, "task not found")),
        Err(e) => return Err(PipeResponse::err(msg_id, e.to_string())),
    };
    if task.status != STATUS_COMPLETED {
        return Err(PipeResponse::err(msg_id, "task is not completed"));
    }
    let path = Path::new(&task.save_dir).join(&task.file_name);
    if !path.exists() {
        return Err(PipeResponse::err(
            msg_id,
            format!("file not found: {}", path.display()),
        ));
    }
    Ok(path)
}

/// `{"action":"tasks"}` handler.
async fn handle_tasks(msg_id: u64, api_host: &Arc<dyn ApiHost>) -> PipeResponse {
    let tasks = match api_host.list_tasks().await {
        Ok(t) => t,
        Err(e) => return PipeResponse::err(msg_id, format!("list_tasks failed: {e}")),
    };
    let speeds = api_host.live_speeds().await.unwrap_or_default();
    PipeResponse::with_tasks(msg_id, select_task_briefs(tasks, &speeds))
}

/// `{"action":"task_op"}` handler: `pause`/`resume`/`remove` (remove never
/// deletes files — matches the popup's "clear entry" semantics).
async fn handle_task_op(
    msg_id: u64,
    payload: serde_json::Value,
    api_host: &Arc<dyn ApiHost>,
) -> PipeResponse {
    let req: TaskOpPayload = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return PipeResponse::err(msg_id, format!("invalid task_op payload: {e}")),
    };
    let (result, ok_message) = match req.op.as_str() {
        "pause" => (api_host.pause_task(&req.task_id).await, "paused"),
        "resume" => (api_host.continue_task(&req.task_id).await, "resumed"),
        "remove" => (api_host.delete_task(&req.task_id, false).await, "removed"),
        other => return PipeResponse::err(msg_id, format!("unknown task_op: {other}")),
    };
    match result {
        Ok(()) => PipeResponse::ok(msg_id, ok_message),
        Err(e) => PipeResponse::err(msg_id, e.to_string()),
    }
}

/// `{"action":"open_file"}` handler: opens the completed task's file with
/// the OS default application.
async fn handle_open_file(
    msg_id: u64,
    payload: serde_json::Value,
    api_host: &Arc<dyn ApiHost>,
    log_tag: &str,
) -> PipeResponse {
    let req: TaskIdPayload = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return PipeResponse::err(msg_id, format!("invalid payload: {e}")),
    };
    let path = match resolve_completed_task_path(msg_id, &req.task_id, api_host).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let path_str = path.to_string_lossy().into_owned();
    if let Err(e) =
        tokio::task::spawn_blocking(move || crate::reveal_file::open_file(&path_str)).await
    {
        log_error!("[{}] open_file task panicked: {}", log_tag, e);
        return PipeResponse::err(msg_id, "failed to open file");
    }
    PipeResponse::ok(msg_id, "ok")
}

/// `{"action":"reveal_file"}` handler: reveals the completed task's file in
/// the native file manager, honoring the user's `reveal_file_cmd` template.
async fn handle_reveal_file(
    msg_id: u64,
    payload: serde_json::Value,
    api_host: &Arc<dyn ApiHost>,
    log_tag: &str,
) -> PipeResponse {
    let req: TaskIdPayload = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return PipeResponse::err(msg_id, format!("invalid payload: {e}")),
    };
    let path = match resolve_completed_task_path(msg_id, &req.task_id, api_host).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let path_str = path.to_string_lossy().into_owned();
    // 与 `download_actor.rs` 的 `RevealFile` 信号分支一致:空模板 = 平台默认。
    let cfg = api_host.get_config().await.unwrap_or_default();
    let tpl = cfg.get("reveal_file_cmd").cloned().unwrap_or_default();
    if let Err(e) =
        tokio::task::spawn_blocking(move || crate::reveal_file::reveal(&path_str, &tpl)).await
    {
        log_error!("[{}] reveal_file task panicked: {}", log_tag, e);
        return PipeResponse::err(msg_id, "failed to reveal file");
    }
    PipeResponse::ok(msg_id, "ok")
}

/// Dispatch one parsed [`PipeMessage`] to its action handler. Shared between
/// the Windows Named Pipe and Unix Domain Socket transports (`mod server`
/// below, per-platform) so action semantics never drift between platforms —
/// only the framed I/O differs.
async fn dispatch_action(
    msg: PipeMessage,
    dl_tx: &mpsc::Sender<DownloadRequest>,
    api_host: &Arc<dyn ApiHost>,
    log_tag: &str,
) -> PipeResponse {
    match msg.action.as_str() {
        "ping" => {
            log_info!("[{}] ping (msg_id={})", log_tag, msg.msg_id);
            PipeResponse::ok(msg.msg_id, "pong")
        }
        "download" => match serde_json::from_value::<DownloadRequest>(msg.payload) {
            Ok(download_req) => {
                log_info!(
                    "[{}] download request (msg_id={}): url={}",
                    log_tag,
                    msg.msg_id,
                    download_req.url
                );
                let _ = dl_tx.send(download_req).await;
                PipeResponse::ok(msg.msg_id, "download accepted")
            }
            Err(e) => {
                log_info!(
                    "[{}] download parse error (msg_id={}): {}",
                    log_tag,
                    msg.msg_id,
                    e
                );
                PipeResponse::err(msg.msg_id, format!("invalid download payload: {}", e))
            }
        },
        "tasks" => {
            log_info!("[{}] tasks (msg_id={})", log_tag, msg.msg_id);
            handle_tasks(msg.msg_id, api_host).await
        }
        "task_op" => {
            log_info!("[{}] task_op (msg_id={})", log_tag, msg.msg_id);
            handle_task_op(msg.msg_id, msg.payload, api_host).await
        }
        "open_file" => {
            log_info!("[{}] open_file (msg_id={})", log_tag, msg.msg_id);
            handle_open_file(msg.msg_id, msg.payload, api_host, log_tag).await
        }
        "reveal_file" => {
            log_info!("[{}] reveal_file (msg_id={})", log_tag, msg.msg_id);
            handle_reveal_file(msg.msg_id, msg.payload, api_host, log_tag).await
        }
        other => {
            log_info!(
                "[{}] unknown action '{}' (msg_id={})",
                log_tag,
                other,
                msg.msg_id
            );
            PipeResponse::err(msg.msg_id, format!("unknown action: {}", other))
        }
    }
}

// ---------------------------------------------------------------------------
// Named Pipe server (Windows)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod server {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ServerOptions;
    use tokio::sync::mpsc;

    use super::{
        ApiHost, DownloadRequest, MAX_MESSAGE_SIZE, PIPE_NAME, PipeMessage, PipeResponse,
        dispatch_action,
    };
    use crate::logger::log_info;

    /// Read a 4-byte LE length-prefixed message from the pipe.
    async fn read_framed_message(
        pipe: &mut tokio::net::windows::named_pipe::NamedPipeServer,
    ) -> Result<Vec<u8>, std::io::Error> {
        let mut len_buf = [0u8; 4];
        pipe.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf);
        if len == 0 || len > MAX_MESSAGE_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid message length: {}", len),
            ));
        }
        let mut buf = vec![0u8; len as usize];
        pipe.read_exact(&mut buf).await?;
        Ok(buf)
    }

    /// Write a 4-byte LE length-prefixed message to the pipe.
    async fn write_framed_message(
        pipe: &mut tokio::net::windows::named_pipe::NamedPipeServer,
        data: &[u8],
    ) -> Result<(), std::io::Error> {
        let len = data.len() as u32;
        pipe.write_all(&len.to_le_bytes()).await?;
        pipe.write_all(data).await?;
        pipe.flush().await?;
        Ok(())
    }

    /// Handle a single pipe client connection.
    async fn handle_pipe_client(
        mut pipe: tokio::net::windows::named_pipe::NamedPipeServer,
        tx: mpsc::Sender<DownloadRequest>,
        api_host: Arc<dyn ApiHost>,
    ) {
        loop {
            let raw = match read_framed_message(&mut pipe).await {
                Ok(data) => data,
                Err(e) => {
                    log_info!("[nmh-pipe] read error: {}", e);
                    break;
                }
            };

            let msg: PipeMessage = match serde_json::from_slice(&raw) {
                Ok(m) => m,
                Err(e) => {
                    log_info!("[nmh-pipe] JSON parse error: {}", e);
                    let resp = PipeResponse::err(0, format!("invalid JSON: {}", e));
                    if let Ok(json) = serde_json::to_vec(&resp)
                        && write_framed_message(&mut pipe, &json).await.is_err()
                    {
                        break;
                    }
                    continue;
                }
            };

            let response = dispatch_action(msg, &tx, &api_host, "nmh-pipe").await;

            if let Ok(json) = serde_json::to_vec(&response)
                && write_framed_message(&mut pipe, &json).await.is_err()
            {
                break;
            }
        }
    }

    /// Windows pipe security: grant Everyone read/write and stamp a Low
    /// mandatory integrity label so a Medium-IL `fluxdown_nmh.exe` (spawned by
    /// the browser) can connect even when FluxDown runs elevated (High IL).
    /// Without it the pipe inherits the creator's High IL and the no-write-up
    /// rule silently rejects the relay — browser interception dies until the
    /// app next runs unelevated.
    mod pipe_security {
        use std::ffi::c_void;
        use std::io;

        #[repr(C)]
        struct SecurityAttributes {
            n_length: u32,
            lp_security_descriptor: *mut c_void,
            b_inherit_handle: i32,
        }

        #[link(name = "advapi32")]
        unsafe extern "system" {
            fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
                string_security_descriptor: *const u16,
                string_sddl_revision: u32,
                security_descriptor: *mut *mut c_void,
                security_descriptor_size: *mut u32,
            ) -> i32;
        }

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn LocalFree(hmem: *mut c_void) -> *mut c_void;
        }

        /// Owns the security descriptor allocated by the SDDL conversion plus
        /// the `SECURITY_ATTRIBUTES` pointing at it; frees it on drop. Never
        /// held across an `.await` (built and dropped inside the synchronous
        /// `create_instance`), so the raw pointer needs no `Send`.
        pub struct PipeSecurity {
            attrs: SecurityAttributes,
        }

        impl PipeSecurity {
            /// Build the descriptor, or error if the SDDL conversion fails.
            pub fn new() -> io::Result<Self> {
                // D: Everyone (WD) generic read+write. S: Low mandatory label,
                // no-write-up (equal/higher IL subjects may still write).
                let sddl: Vec<u16> = "D:(A;;GRGW;;;WD)S:(ML;;NW;;;LW)"
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                let mut psd: *mut c_void = std::ptr::null_mut();
                // SAFETY: `sddl` is a valid NUL-terminated UTF-16 string; on
                // success the call allocates `psd`, freed in `Drop`. Revision
                // 1 == SDDL_REVISION_1.
                let ok = unsafe {
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        sddl.as_ptr(),
                        1,
                        &mut psd,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Self {
                    attrs: SecurityAttributes {
                        n_length: std::mem::size_of::<SecurityAttributes>() as u32,
                        lp_security_descriptor: psd,
                        b_inherit_handle: 0,
                    },
                })
            }

            /// Pointer to the `SECURITY_ATTRIBUTES`, valid while `self` lives.
            pub fn as_ptr(&self) -> *mut c_void {
                (&raw const self.attrs).cast::<c_void>().cast_mut()
            }
        }

        impl Drop for PipeSecurity {
            fn drop(&mut self) {
                if !self.attrs.lp_security_descriptor.is_null() {
                    // SAFETY: descriptor was allocated by the SDDL conversion.
                    unsafe { LocalFree(self.attrs.lp_security_descriptor) };
                }
            }
        }
    }

    /// Create one pipe server instance with a hardened security descriptor
    /// (Everyone R/W + Low integrity label) so a Medium-IL NMH relay can
    /// connect even when FluxDown runs elevated. Falls back to default security
    /// if the descriptor cannot be built, never breaking the unelevated path.
    fn create_instance(
        first: bool,
    ) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        let mut options = ServerOptions::new();
        options.first_pipe_instance(first);
        match pipe_security::PipeSecurity::new() {
            // SAFETY: `sec` and its descriptor outlive this create call, which
            // copies the SECURITY_ATTRIBUTES into the pipe synchronously.
            Ok(sec) => unsafe {
                options.create_with_security_attributes_raw(PIPE_NAME, sec.as_ptr())
            },
            Err(e) => {
                log_info!("[nmh-pipe] pipe security unavailable, using default: {}", e);
                options.create(PIPE_NAME)
            }
        }
    }

    /// Spawn the Named Pipe server, feeding download requests into `tx` and
    /// answering `tasks`/`task_op`/`open_file`/`reveal_file` via `api_host`.
    /// The receiving end of `tx` is owned by `download_actor` and shared with
    /// the local HTTP takeover server so both transports converge on one
    /// channel.
    pub fn spawn_listener_with(tx: mpsc::Sender<DownloadRequest>, api_host: Arc<dyn ApiHost>) {
        tokio::spawn(async move {
            log_info!("[nmh-pipe] starting Named Pipe server at {}", PIPE_NAME);

            // Create the first server instance before entering the loop.
            let mut server = match create_instance(true) {
                Ok(s) => s,
                Err(e) => {
                    log_info!("[nmh-pipe] failed to create pipe server: {}", e);
                    return;
                }
            };

            loop {
                // Wait for a client to connect.
                if let Err(e) = server.connect().await {
                    log_info!("[nmh-pipe] connect error: {}", e);
                    // Brief pause before retrying.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }

                log_info!("[nmh-pipe] client connected");

                // Create the next server instance to accept the next client
                // while we handle the current one.
                let next_server = match create_instance(false) {
                    Ok(s) => s,
                    Err(e) => {
                        log_info!("[nmh-pipe] failed to create next pipe instance: {}", e);
                        // Can't accept more clients, but handle the current one.
                        let tx_clone = tx.clone();
                        let api_host_clone = api_host.clone();
                        tokio::spawn(handle_pipe_client(server, tx_clone, api_host_clone));
                        // Exit the accept loop — single client mode until restart.
                        break;
                    }
                };

                // Hand off the connected server to a task.
                let connected = std::mem::replace(&mut server, next_server);
                let tx_clone = tx.clone();
                let api_host_clone = api_host.clone();
                tokio::spawn(handle_pipe_client(connected, tx_clone, api_host_clone));
            }
        });
    }
}

// Non-Windows: Unix Domain Socket server.
#[cfg(not(windows))]
mod server {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;
    use tokio::sync::mpsc;

    use super::{
        ApiHost, DownloadRequest, MAX_MESSAGE_SIZE, PipeMessage, PipeResponse, dispatch_action,
    };
    use crate::logger::log_info;

    /// Returns the current user's home directory.
    ///
    /// Prefers `$HOME` but falls back to the passwd database via `getpwuid_r`
    /// so that the correct path is returned even when the process is launched
    /// by a system service (launchd on macOS) that may not set `$HOME`.
    #[cfg(target_os = "macos")]
    fn home_dir() -> Option<std::path::PathBuf> {
        if let Ok(h) = std::env::var("HOME") {
            if !h.is_empty() {
                return Some(std::path::PathBuf::from(h));
            }
        }
        use std::ffi::CStr;
        let uid = unsafe { libc::getuid() };
        let buf_size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
        let buf_size = if buf_size > 0 {
            buf_size as usize
        } else {
            1024
        };
        let mut buf = vec![0i8; buf_size];
        let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let ret = unsafe {
            libc::getpwuid_r(
                uid,
                pwd.as_mut_ptr(),
                buf.as_mut_ptr(),
                buf_size,
                &mut result,
            )
        };
        if ret == 0 && !result.is_null() {
            let pwd = unsafe { pwd.assume_init() };
            if !pwd.pw_dir.is_null() {
                let cstr = unsafe { CStr::from_ptr(pwd.pw_dir) };
                if let Ok(s) = cstr.to_str() {
                    if !s.is_empty() {
                        return Some(std::path::PathBuf::from(s));
                    }
                }
            }
        }
        None
    }

    /// Returns the Unix socket path for the NMH relay to connect to.
    ///
    /// - macOS: `~/Library/Application Support/fluxdown/fluxdown.sock`
    ///   (avoids /tmp sandbox isolation and $TMPDIR per-app randomisation;
    ///    uses getpwuid fallback so launchd-launched NMH also finds it)
    /// - Linux:  `~/.local/share/fluxdown/fluxdown.sock`
    ///   (avoids $XDG_RUNTIME_DIR sandbox remapping inside Flatpak/Snap;
    ///    ~/.local/share/ is bind-mounted into the sandbox so both the host
    ///    app and the browser-spawned NMH see the same path)
    /// - Other Unix: `$XDG_RUNTIME_DIR/fluxdown.sock` → `/tmp/fluxdown.sock`
    pub fn socket_path() -> std::path::PathBuf {
        #[cfg(target_os = "macos")]
        {
            if let Some(home) = home_dir() {
                let dir = home
                    .join("Library")
                    .join("Application Support")
                    .join("fluxdown");
                let _ = std::fs::create_dir_all(&dir);
                return dir.join("fluxdown.sock");
            }
        }
        // Linux: use ~/.local/share/fluxdown/fluxdown.sock
        // This path is accessible from both the host (app process) and Flatpak/Snap
        // sandboxes (which bind-mount ~/.local/share/ into the sandbox), unlike
        // $XDG_RUNTIME_DIR which gets remapped to a sandbox-private path inside
        // Flatpak, causing the app and NMH to see different socket paths.
        #[cfg(target_os = "linux")]
        {
            if let Ok(home) = std::env::var("HOME") {
                if !home.is_empty() {
                    let dir = std::path::Path::new(&home)
                        .join(".local")
                        .join("share")
                        .join("fluxdown");
                    let _ = std::fs::create_dir_all(&dir);
                    return dir.join("fluxdown.sock");
                }
            }
        }
        // Fallback for any other Unix-like OS
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            std::path::Path::new(&dir).join("fluxdown.sock")
        } else {
            std::path::Path::new("/tmp").join("fluxdown.sock")
        }
    }

    async fn read_framed_message(
        stream: &mut tokio::net::UnixStream,
    ) -> Result<Vec<u8>, std::io::Error> {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf);
        if len == 0 || len > MAX_MESSAGE_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid message length: {}", len),
            ));
        }
        let mut buf = vec![0u8; len as usize];
        stream.read_exact(&mut buf).await?;
        Ok(buf)
    }

    async fn write_framed_message(
        stream: &mut tokio::net::UnixStream,
        data: &[u8],
    ) -> Result<(), std::io::Error> {
        let len = data.len() as u32;
        stream.write_all(&len.to_le_bytes()).await?;
        stream.write_all(data).await?;
        stream.flush().await?;
        Ok(())
    }

    async fn handle_client(
        mut stream: tokio::net::UnixStream,
        tx: mpsc::Sender<DownloadRequest>,
        api_host: Arc<dyn ApiHost>,
    ) {
        loop {
            let raw = match read_framed_message(&mut stream).await {
                Ok(data) => data,
                Err(e) => {
                    log_info!("[nmh-uds] read error: {}", e);
                    break;
                }
            };

            let msg: PipeMessage = match serde_json::from_slice(&raw) {
                Ok(m) => m,
                Err(e) => {
                    log_info!("[nmh-uds] JSON parse error: {}", e);
                    let resp = PipeResponse::err(0, format!("invalid JSON: {}", e));
                    if let Ok(json) = serde_json::to_vec(&resp)
                        && write_framed_message(&mut stream, &json).await.is_err()
                    {
                        break;
                    }
                    continue;
                }
            };

            let response = dispatch_action(msg, &tx, &api_host, "nmh-uds").await;

            if let Ok(json) = serde_json::to_vec(&response)
                && write_framed_message(&mut stream, &json).await.is_err()
            {
                break;
            }
        }
    }

    pub fn spawn_listener_with(tx: mpsc::Sender<DownloadRequest>, api_host: Arc<dyn ApiHost>) {
        let sock_path = socket_path();

        tokio::spawn(async move {
            // Remove stale socket file left by a previous run.
            let _ = std::fs::remove_file(&sock_path);

            let listener = match UnixListener::bind(&sock_path) {
                Ok(l) => {
                    log_info!(
                        "[nmh-uds] Unix socket server started at {}",
                        sock_path.display()
                    );
                    l
                }
                Err(e) => {
                    log_info!("[nmh-uds] failed to bind Unix socket: {}", e);
                    return;
                }
            };

            // Restrict socket to owner-only so other local users cannot connect.
            if let Err(e) =
                std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))
            {
                log_info!("[nmh-uds] failed to set socket permissions: {}", e);
            }

            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        log_info!("[nmh-uds] client connected");
                        let tx_clone = tx.clone();
                        let api_host_clone = api_host.clone();
                        tokio::spawn(handle_client(stream, tx_clone, api_host_clone));
                    }
                    Err(e) => {
                        log_info!("[nmh-uds] accept error: {}", e);
                    }
                }
            }
        });
    }
}

/// Spawn the Native Messaging listener, feeding requests into the provided
/// sender. Used by `download_actor` so that both the NMH transport and the
/// local HTTP takeover server can push `DownloadRequest`s into one shared
/// channel — the actor's `native_msg_rx` branch then handles both uniformly.
/// `api_host` answers the `tasks`/`task_op`/`open_file`/`reveal_file`
/// actions (task panel queries/controls), sharing the same instance the
/// local HTTP API server uses.
pub fn spawn_native_messaging_listener_with(
    tx: mpsc::Sender<DownloadRequest>,
    api_host: Arc<dyn ApiHost>,
) {
    server::spawn_listener_with(tx, api_host);
}

/// Spawn the Native Messaging listener and return a fresh receiver.
///
/// Convenience wrapper for callers that don't need to share the channel.
/// Ping requests are handled internally (immediate pong response).
#[allow(dead_code)]
pub fn spawn_native_messaging_listener(
    api_host: Arc<dyn ApiHost>,
) -> mpsc::Receiver<DownloadRequest> {
    let (tx, rx) = mpsc::channel::<DownloadRequest>(64);
    server::spawn_listener_with(tx, api_host);
    rx
}

// wire 类型（DownloadRequest/RequestBody）的反序列化测试随类型迁移至
// fluxdown_api crate（native/api/src/types.rs 的所有者测试）。

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn dto(task_id: &str, status: i32, created_at: &str) -> TaskDto {
        TaskDto {
            task_id: task_id.to_string(),
            url: "https://example.com/f".to_string(),
            file_name: format!("{task_id}.bin"),
            save_dir: "/tmp".to_string(),
            status,
            downloaded_bytes: 0,
            total_bytes: 100,
            error_message: String::new(),
            created_at: created_at.to_string(),
            proxy_url: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            file_missing: false,
        }
    }

    #[test]
    fn select_task_briefs_keeps_all_non_completed() {
        let tasks = vec![dto("a", 0, "100"), dto("b", 1, "200"), dto("c", 2, "300")];
        let briefs = select_task_briefs(tasks, &HashMap::new());
        let ids: Vec<&str> = briefs.iter().map(|t| t.task_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn select_task_briefs_caps_completed_at_ten_newest_first() {
        let mut tasks = Vec::new();
        for i in 0..15 {
            tasks.push(dto(&format!("t{i}"), STATUS_COMPLETED, &i.to_string()));
        }
        let briefs = select_task_briefs(tasks, &HashMap::new());
        assert_eq!(briefs.len(), MAX_COMPLETED_TASKS);
        // Newest (largest created_at) first: t14, t13, ..., t5.
        let ids: Vec<&str> = briefs.iter().map(|t| t.task_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["t14", "t13", "t12", "t11", "t10", "t9", "t8", "t7", "t6", "t5"]
        );
    }

    #[test]
    fn select_task_briefs_mixes_active_and_recent_completed() {
        let tasks = vec![
            dto("active1", 1, "1"),
            dto("done_old", STATUS_COMPLETED, "1"),
            dto("done_new", STATUS_COMPLETED, "2"),
        ];
        let briefs = select_task_briefs(tasks, &HashMap::new());
        let ids: Vec<&str> = briefs.iter().map(|t| t.task_id.as_str()).collect();
        assert_eq!(ids, vec!["active1", "done_new", "done_old"]);
    }

    #[test]
    fn select_task_briefs_merges_live_speed_defaulting_to_zero() {
        let tasks = vec![dto("a", 1, "1"), dto("b", 1, "2")];
        let mut speeds = HashMap::new();
        speeds.insert(
            "a".to_string(),
            LiveSpeed {
                download_bps: 4096,
                upload_bps: 0,
            },
        );
        let briefs = select_task_briefs(tasks, &speeds);
        let by_id = |id: &str| briefs.iter().find(|t| t.task_id == id).unwrap();
        assert_eq!(by_id("a").speed, 4096);
        assert_eq!(by_id("b").speed, 0);
    }
}
