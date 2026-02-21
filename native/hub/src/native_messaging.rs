//! HTTP server for browser extension communication.
//!
//! Architecture:
//!   - FluxDown main process starts a minimal HTTP server on localhost:19527.
//!   - The browser extension communicates directly via `fetch()` HTTP requests.
//!   - No Native Messaging relay or registry registration needed.
//!
//! Endpoints:
//!   - `POST /download`  — Submit a download request (JSON body).
//!   - `GET  /ping`      — Health check (returns `{"success":true,"message":"pong"}`).
//!   - `OPTIONS /*`      — CORS preflight (allows browser extension origin).

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// HTTP port for the browser extension bridge.
pub const LISTEN_PORT: u16 = 19527;

// ---------------------------------------------------------------------------
// Message types matching the browser extension protocol
// ---------------------------------------------------------------------------

/// Download request payload from the browser extension.
#[derive(Debug, Clone, Deserialize)]
pub struct DownloadRequest {
    pub url: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub referrer: String,
    #[serde(default)]
    pub cookies: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub headers: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "fileSize")]
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(rename = "mimeType")]
    #[serde(default)]
    pub mime_type: Option<String>,
}

/// JSON response sent back to the browser extension.
#[derive(Debug, Serialize)]
pub struct ApiResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "taskId")]
    pub task_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Minimal HTTP helpers
// ---------------------------------------------------------------------------

/// Parsed HTTP request (only the fields we care about).
struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

/// Read all available data from the stream (up to a limit) and parse the HTTP request.
async fn parse_http_request(
    stream: &mut tokio::net::TcpStream,
) -> Result<HttpRequest, String> {
    // Read up to 1MB from the stream. Browser extension requests are tiny.
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total = 0usize;

    // Read until we have the full headers (terminated by \r\n\r\n).
    loop {
        if total >= buf.len() {
            return Err("request too large".to_string());
        }
        let n = stream
            .read(&mut buf[total..])
            .await
            .map_err(|e| format!("read error: {}", e))?;
        if n == 0 {
            if total == 0 {
                return Err("empty request".to_string());
            }
            break;
        }
        total += n;

        // Check if we have the full headers.
        if let Some(header_end) = find_header_end(&buf[..total]) {
            // Parse Content-Length to know if we need more body data.
            let header_str = String::from_utf8_lossy(&buf[..header_end]);
            let content_length = parse_content_length(&header_str);
            let body_start = header_end + 4; // skip \r\n\r\n
            let body_received = total.saturating_sub(body_start);

            if body_received >= content_length {
                // We have everything.
                break;
            }

            // Need to read more body data.
            let needed = content_length - body_received;
            let target = total + needed;
            if target > buf.len() {
                return Err("request body too large".to_string());
            }
            while total < target {
                let n = stream
                    .read(&mut buf[total..target])
                    .await
                    .map_err(|e| format!("read body error: {}", e))?;
                if n == 0 {
                    break;
                }
                total += n;
            }
            break;
        }
    }

    let data = &buf[..total];

    // Find header/body boundary.
    let header_end = find_header_end(data).ok_or("incomplete headers")?;
    let header_str = String::from_utf8_lossy(&data[..header_end]);
    let body_start = header_end + 4;

    // Parse request line.
    let first_line = header_str.lines().next().ok_or("no request line")?;
    let parts: Vec<&str> = first_line.split(' ').collect();
    if parts.len() < 2 {
        return Err("malformed request line".to_string());
    }
    let method = parts[0].to_uppercase();
    let path = parts[1].to_string();

    // Extract body.
    let body = if body_start < total {
        data[body_start..total].to_vec()
    } else {
        Vec::new()
    };

    Ok(HttpRequest { method, path, body })
}

/// Find the position of `\r\n\r\n` in the buffer (returns index of first `\r`).
fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4)
        .position(|w| w == b"\r\n\r\n")
}

/// Parse Content-Length from raw header string. Returns 0 if not found.
fn parse_content_length(headers: &str) -> usize {
    for line in headers.lines() {
        let lower = line.to_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:")
            && let Ok(len) = value.trim().parse::<usize>()
        {
            return len;
        }
    }
    0
}

/// CORS headers to allow browser extension requests.
const CORS_HEADERS: &str = "\
Access-Control-Allow-Origin: *\r\n\
Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
Access-Control-Allow-Headers: Content-Type\r\n\
Access-Control-Max-Age: 86400";

/// Send an HTTP response.
async fn send_http_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    status_text: &str,
    body: &[u8],
) {
    let response = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         {}\r\n\
         Connection: close\r\n\
         \r\n",
        status,
        status_text,
        body.len(),
        CORS_HEADERS,
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
}

/// Send an HTTP JSON response with a given status.
async fn send_json_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    status_text: &str,
    resp: &ApiResponse,
) {
    if let Ok(json) = serde_json::to_vec(resp) {
        send_http_response(stream, status, status_text, &json).await;
    }
}

// ---------------------------------------------------------------------------
// HTTP server
// ---------------------------------------------------------------------------

/// Handle a single HTTP connection: parse the request and dispatch to a route.
async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    addr: std::net::SocketAddr,
    tx: mpsc::Sender<DownloadRequest>,
) {
    let req = match parse_http_request(&mut stream).await {
        Ok(r) => r,
        Err(e) => {
            rinf::debug_print!("[http-bridge] parse error from {}: {}", addr, e);
            send_json_response(
                &mut stream,
                400,
                "Bad Request",
                &ApiResponse {
                    success: false,
                    message: Some(format!("parse error: {}", e)),
                    task_id: None,
                },
            )
            .await;
            return;
        }
    };

    // Route: OPTIONS (CORS preflight)
    if req.method == "OPTIONS" {
        send_http_response(&mut stream, 204, "No Content", b"").await;
        return;
    }

    // Route: GET /ping
    if req.method == "GET" && req.path == "/ping" {
        rinf::debug_print!("[http-bridge] ping from {}", addr);
        send_json_response(
            &mut stream,
            200,
            "OK",
            &ApiResponse {
                success: true,
                message: Some("pong".to_string()),
                task_id: None,
            },
        )
        .await;
        return;
    }

    // Route: POST /download
    if req.method == "POST" && req.path == "/download" {
        match serde_json::from_slice::<DownloadRequest>(&req.body) {
            Ok(download_req) => {
                rinf::debug_print!(
                    "[http-bridge] download request from {}: url={}",
                    addr,
                    download_req.url
                );
                // Respond immediately so the extension doesn't timeout.
                send_json_response(
                    &mut stream,
                    200,
                    "OK",
                    &ApiResponse {
                        success: true,
                        message: Some("download accepted".to_string()),
                        task_id: None,
                    },
                )
                .await;
                // Forward to the download actor.
                let _ = tx.send(download_req).await;
            }
            Err(e) => {
                rinf::debug_print!("[http-bridge] JSON parse error: {}", e);
                send_json_response(
                    &mut stream,
                    400,
                    "Bad Request",
                    &ApiResponse {
                        success: false,
                        message: Some(format!("invalid JSON: {}", e)),
                        task_id: None,
                    },
                )
                .await;
            }
        }
        return;
    }

    // Route: not found
    send_json_response(
        &mut stream,
        404,
        "Not Found",
        &ApiResponse {
            success: false,
            message: Some(format!("unknown route: {} {}", req.method, req.path)),
            task_id: None,
        },
    )
    .await;
}

/// Accept loop for a single listener — forwards each connection to `handle_connection`.
async fn run_accept_loop(listener: TcpListener, tx: mpsc::Sender<DownloadRequest>) {
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                rinf::debug_print!("[http-bridge] accept error: {}", e);
                continue;
            }
        };
        tokio::spawn(handle_connection(stream, addr, tx.clone()));
    }
}

/// Spawn the HTTP server that listens for incoming browser extension requests.
///
/// Returns a receiver that yields `DownloadRequest` items whenever the
/// browser extension sends a download request via HTTP POST.
/// Ping requests are handled internally (immediate pong response).
///
/// Binds both `127.0.0.1:LISTEN_PORT` (IPv4) and `[::1]:LISTEN_PORT` (IPv6)
/// so that `localhost` works regardless of whether the browser resolves it
/// to an IPv4 or IPv6 address.  IPv6 binding is best-effort: if the OS has
/// IPv6 disabled the error is logged and the server continues on IPv4 only.
pub fn spawn_native_messaging_listener() -> mpsc::Receiver<DownloadRequest> {
    let (tx, rx) = mpsc::channel::<DownloadRequest>(64);

    tokio::spawn(async move {
        let mut bound = false;

        // IPv4 loopback — primary
        match TcpListener::bind(("127.0.0.1", LISTEN_PORT)).await {
            Ok(l) => {
                rinf::debug_print!(
                    "[http-bridge] IPv4 listening on http://127.0.0.1:{}",
                    LISTEN_PORT
                );
                tokio::spawn(run_accept_loop(l, tx.clone()));
                bound = true;
            }
            Err(e) => {
                rinf::debug_print!(
                    "[http-bridge] IPv4 bind failed on port {}: {}",
                    LISTEN_PORT,
                    e
                );
            }
        }

        // IPv6 loopback — optional (Firefox may resolve localhost → ::1)
        match TcpListener::bind(("::1", LISTEN_PORT)).await {
            Ok(l) => {
                rinf::debug_print!(
                    "[http-bridge] IPv6 listening on http://[::1]:{}",
                    LISTEN_PORT
                );
                tokio::spawn(run_accept_loop(l, tx.clone()));
                bound = true;
            }
            Err(e) => {
                rinf::debug_print!(
                    "[http-bridge] IPv6 not available (ok if disabled): {}",
                    e
                );
            }
        }

        if !bound {
            rinf::debug_print!(
                "[http-bridge] no address could be bound on port {}",
                LISTEN_PORT
            );
        }
    });

    rx
}
