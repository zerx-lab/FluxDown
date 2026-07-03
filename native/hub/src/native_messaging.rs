//! Named Pipe server for browser extension communication via Native Messaging.
//!
//! Architecture:
//!   - FluxDown main process creates a Named Pipe server at `\\.\pipe\fluxdown`.
//!   - The NMH relay binary (`fluxdown_nmh.exe`) connects to this pipe.
//!   - Messages use a 4-byte LE length prefix + JSON payload.
//!
//! Message protocol:
//!   - `{"action":"ping","msg_id":N}`     → `{"success":true,"message":"pong","msg_id":N}`
//!   - `{"action":"download","msg_id":N, ...}` → `{"success":true,"message":"download accepted","msg_id":N}`

use fluxdown_api::types::DownloadRequest;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

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
}

// ---------------------------------------------------------------------------
// Named Pipe server (Windows)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod server {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ServerOptions;
    use tokio::sync::mpsc;

    use super::{DownloadRequest, MAX_MESSAGE_SIZE, PIPE_NAME, PipeMessage, PipeResponse};
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
                    let resp = PipeResponse {
                        success: false,
                        message: Some(format!("invalid JSON: {}", e)),
                        msg_id: 0,
                    };
                    if let Ok(json) = serde_json::to_vec(&resp)
                        && write_framed_message(&mut pipe, &json).await.is_err()
                    {
                        break;
                    }
                    continue;
                }
            };

            let response = match msg.action.as_str() {
                "ping" => {
                    log_info!("[nmh-pipe] ping (msg_id={})", msg.msg_id);
                    PipeResponse {
                        success: true,
                        message: Some("pong".to_string()),
                        msg_id: msg.msg_id,
                    }
                }
                "download" => match serde_json::from_value::<DownloadRequest>(msg.payload) {
                    Ok(download_req) => {
                        log_info!(
                            "[nmh-pipe] download request (msg_id={}): url={}",
                            msg.msg_id,
                            download_req.url
                        );
                        let _ = tx.send(download_req).await;
                        PipeResponse {
                            success: true,
                            message: Some("download accepted".to_string()),
                            msg_id: msg.msg_id,
                        }
                    }
                    Err(e) => {
                        log_info!(
                            "[nmh-pipe] download parse error (msg_id={}): {}",
                            msg.msg_id,
                            e
                        );
                        PipeResponse {
                            success: false,
                            message: Some(format!("invalid download payload: {}", e)),
                            msg_id: msg.msg_id,
                        }
                    }
                },
                other => {
                    log_info!(
                        "[nmh-pipe] unknown action '{}' (msg_id={})",
                        other,
                        msg.msg_id
                    );
                    PipeResponse {
                        success: false,
                        message: Some(format!("unknown action: {}", other)),
                        msg_id: msg.msg_id,
                    }
                }
            };

            if let Ok(json) = serde_json::to_vec(&response)
                && write_framed_message(&mut pipe, &json).await.is_err()
            {
                break;
            }
        }
    }

    /// Spawn the Named Pipe server, feeding download requests into `tx`.
    /// The receiving end is owned by `download_actor` and shared with the
    /// local HTTP takeover server so both transports converge on one channel.
    pub fn spawn_listener_with(tx: mpsc::Sender<DownloadRequest>) {
        tokio::spawn(async move {
            log_info!("[nmh-pipe] starting Named Pipe server at {}", PIPE_NAME);

            // Create the first server instance before entering the loop.
            let mut server = match ServerOptions::new()
                .first_pipe_instance(true)
                .create(PIPE_NAME)
            {
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
                let next_server = match ServerOptions::new().create(PIPE_NAME) {
                    Ok(s) => s,
                    Err(e) => {
                        log_info!("[nmh-pipe] failed to create next pipe instance: {}", e);
                        // Can't accept more clients, but handle the current one.
                        let tx_clone = tx.clone();
                        tokio::spawn(handle_pipe_client(server, tx_clone));
                        // Exit the accept loop — single client mode until restart.
                        break;
                    }
                };

                // Hand off the connected server to a task.
                let connected = std::mem::replace(&mut server, next_server);
                let tx_clone = tx.clone();
                tokio::spawn(handle_pipe_client(connected, tx_clone));
            }
        });
    }
}

// Non-Windows: Unix Domain Socket server.
#[cfg(not(windows))]
mod server {
    use std::os::unix::fs::PermissionsExt;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;
    use tokio::sync::mpsc;

    use super::{DownloadRequest, MAX_MESSAGE_SIZE, PipeMessage, PipeResponse};
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

    async fn handle_client(mut stream: tokio::net::UnixStream, tx: mpsc::Sender<DownloadRequest>) {
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
                    let resp = PipeResponse {
                        success: false,
                        message: Some(format!("invalid JSON: {}", e)),
                        msg_id: 0,
                    };
                    if let Ok(json) = serde_json::to_vec(&resp)
                        && write_framed_message(&mut stream, &json).await.is_err()
                    {
                        break;
                    }
                    continue;
                }
            };

            let response = match msg.action.as_str() {
                "ping" => {
                    log_info!("[nmh-uds] ping (msg_id={})", msg.msg_id);
                    PipeResponse {
                        success: true,
                        message: Some("pong".to_string()),
                        msg_id: msg.msg_id,
                    }
                }
                "download" => match serde_json::from_value::<DownloadRequest>(msg.payload) {
                    Ok(download_req) => {
                        log_info!(
                            "[nmh-uds] download request (msg_id={}): url={}",
                            msg.msg_id,
                            download_req.url
                        );
                        let _ = tx.send(download_req).await;
                        PipeResponse {
                            success: true,
                            message: Some("download accepted".to_string()),
                            msg_id: msg.msg_id,
                        }
                    }
                    Err(e) => {
                        log_info!(
                            "[nmh-uds] download parse error (msg_id={}): {}",
                            msg.msg_id,
                            e
                        );
                        PipeResponse {
                            success: false,
                            message: Some(format!("invalid download payload: {}", e)),
                            msg_id: msg.msg_id,
                        }
                    }
                },
                other => {
                    log_info!(
                        "[nmh-uds] unknown action '{}' (msg_id={})",
                        other,
                        msg.msg_id
                    );
                    PipeResponse {
                        success: false,
                        message: Some(format!("unknown action: {}", other)),
                        msg_id: msg.msg_id,
                    }
                }
            };

            if let Ok(json) = serde_json::to_vec(&response)
                && write_framed_message(&mut stream, &json).await.is_err()
            {
                break;
            }
        }
    }

    pub fn spawn_listener_with(tx: mpsc::Sender<DownloadRequest>) {
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
                        tokio::spawn(handle_client(stream, tx_clone));
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
pub fn spawn_native_messaging_listener_with(tx: mpsc::Sender<DownloadRequest>) {
    server::spawn_listener_with(tx);
}

/// Spawn the Native Messaging listener and return a fresh receiver.
///
/// Convenience wrapper for callers that don't need to share the channel.
/// Ping requests are handled internally (immediate pong response).
#[allow(dead_code)]
pub fn spawn_native_messaging_listener() -> mpsc::Receiver<DownloadRequest> {
    let (tx, rx) = mpsc::channel::<DownloadRequest>(64);
    server::spawn_listener_with(tx);
    rx
}

// wire 类型（DownloadRequest/RequestBody）的反序列化测试随类型迁移至
// fluxdown_api crate（native/api/src/types.rs 的所有者测试）。
