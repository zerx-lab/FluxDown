use std::error::Error as StdError;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use reqwest::header::HeaderValue;
use thiserror::Error;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::logger::log_info;
use crate::speed_limiter::SpeedLimiter;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum DownloadError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("db error: {0}")]
    Db(#[from] crate::db::DbError),
    #[error("cancelled")]
    Cancelled,
    #[error("checksum mismatch: {0}")]
    ChecksumMismatch(String),
    /// Server does not honour `Range` requests — returned the enclosed HTTP
    /// status (e.g. `200 OK`) instead of `206 Partial Content`.
    /// Multi-segment assembly is impossible; the caller should fall back to
    /// single-stream mode.
    #[error("server does not support Range requests (returned {0} instead of 206 Partial Content)")]
    RangeNotSupported(String),
    #[error("{0}")]
    Other(String),
}

/// 检测下载错误是否为服务器主动拒绝（403 Forbidden / 429 Too Many Requests）。
///
/// 这类错误通常意味着服务器限制了并发连接数，多段下载的额外连接被拒绝。
/// 与网络超时、连接重置等瞬时错误不同，重试这类错误毫无意义——应当立即
/// 通知 coordinator 进行降级处理。
pub(crate) fn is_server_rejection(e: &DownloadError) -> bool {
    match e {
        DownloadError::Request(req_err) => {
            if let Some(status) = req_err.status() {
                matches!(status.as_u16(), 403 | 429)
            } else {
                false
            }
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct FileInfo {
    pub file_name: String,
    pub total_bytes: i64,
    pub supports_range: bool,
    /// MIME content type from the server (e.g. "text/html", "application/octet-stream").
    /// Empty when the probe phase was skipped (hint_file_size > 0).
    pub content_type: String,
    /// ETag header value from the server (e.g. `"abc123"` or `W/"abc123"`).
    /// Used by multi-segment downloads to verify all connections fetch the same
    /// file version.  Empty when the server did not provide an ETag.
    pub etag: String,
    /// Last-Modified header value from the server (RFC 7232 §2.2).
    /// Used together with `etag` for file-identity verification across segments.
    /// Empty when the server did not provide Last-Modified.
    pub last_modified: String,
    /// `true` when the server's probe response included a `Content-Encoding`
    /// other than `identity` (e.g. gzip, br, deflate).  Because reqwest is
    /// built WITHOUT gzip/brotli/deflate Cargo features, the compressed bytes
    /// would be written raw to disk, corrupting the file.  Callers should
    /// treat this as a warning and avoid multi-segment downloads.
    #[allow(dead_code)]
    pub content_encoding_compressed: bool,
}

pub struct ProgressUpdate {
    pub task_id: String,
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    pub status: i32,
    pub error_message: String,
    /// Non-empty only on initial status=1 update (resolved file name).
    pub file_name: String,
    /// Per-segment progress info (for IDM-style visualization).
    /// `None` for single-thread downloads; `Some(vec)` for multi-segment.
    pub segment_details: Option<Vec<SegmentProgressInfo>>,
}

/// Snapshot of a single segment's progress, sent from downloader to progress_reporter.
#[derive(Clone)]
pub struct SegmentProgressInfo {
    pub index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
}

pub struct DownloadParams {
    pub task_id: String,
    pub url: String,
    pub save_dir: String,
    pub file_name: String,
    pub segment_count: i32,
    /// When `true`, skip file-name dedup — the file on disk belongs to *this*
    /// task and should be reused, not treated as a naming collision.
    pub is_resume: bool,
    pub db: Db,
    pub client: Client,
    pub progress_tx: mpsc::Sender<ProgressUpdate>,
    pub cancel_token: CancellationToken,
    /// Global speed limiter — shared across all concurrent downloads.
    pub speed_limiter: SpeedLimiter,
    /// Browser cookies for authenticated downloads (e.g. GitHub private repos).
    /// Format: "name1=val1; name2=val2"
    pub cookies: String,
    /// HTTP Referer header value captured by the browser extension.
    /// Empty = do not send Referer (manually added downloads).
    pub referrer: String,
    /// File size hint from the browser extension (bytes). 0 = unknown.
    /// When > 0, the probe phase (HEAD + Range:0-0) is skipped entirely.
    /// This prevents one-time CDN URLs from being "consumed" by probe requests.
    pub hint_file_size: i64,
    /// Proxy configuration — used by FTP downloader for SOCKS/HTTP CONNECT tunneling.
    /// HTTP downloads use the proxy via the `client` field (already configured).
    pub proxy_config: crate::proxy_config::ProxyConfig,
    /// HLS quality selection: when set, the HLS downloader sends quality
    /// options to Dart via signal and awaits the chosen variant index on this
    /// channel.  `None` for non-HLS downloads (HTTP/FTP/BT).
    pub hls_quality_rx: Option<tokio::sync::oneshot::Receiver<i32>>,
    /// Checksum spec for post-download integrity verification.
    /// Format: "algo=hexhash", e.g. "sha-256=abc123..." or "md5=d41d8c...".
    /// Empty = skip verification.
    pub checksum: String,
    /// 浏览器扩展捕获的额外 HTTP 请求头（如 Authorization）。
    /// 在发起 HTTP 请求时附加到请求头中。
    pub extra_headers: std::collections::HashMap<String, String>,
    /// `DownloadManager::reserved_temp_paths` 在 `do_start_task` 同步段的快照。
    ///
    /// 传入 `dedup_filename` 以防止批量下载时多个并发任务选出相同文件名：
    /// 每个任务在检查文件名冲突时，除磁盘现有文件外，还会排除此快照中
    /// 已被兄弟任务预订的临时路径（`.fdownloading`）。
    ///
    /// 对于 resume 任务（`is_resume = true`），此字段无意义（dedup 被跳过）；
    /// 对于 BT 任务，此字段为空集合（BT 有自己的文件名管理机制）。
    pub reserved_filenames_snapshot: std::collections::HashSet<std::path::PathBuf>,
}

/// 将浏览器扩展捕获的额外 HTTP 头应用到请求构建器上。
///
/// 使用 `req.headers(map)` 而非逐个 `req.header()`，确保**覆盖**语义：
/// 当 extra_headers 中包含 User-Agent、Accept 等已由 reqwest Client
/// 默认设置的头时，浏览器的真实值会替代默认值，而不是追加产生重复头。
/// 这是 IDM/NDM 的核心策略——原样复制浏览器的请求头。
///
/// 无效的 header name 或 value 会被静默跳过。
///
/// **Defense-in-depth filtering**: Even though the browser extension already
/// strips dangerous headers on the TypeScript side, we filter them again here
/// at the Rust boundary.  This protects against:
///   - A buggy or outdated extension version that forgets to filter,
///   - Manual API callers that bypass the extension entirely,
///   - Future protocol changes that add new dangerous headers.
///
/// Filtered headers:
///   - `accept-encoding` / `content-encoding` — reqwest has NO gzip/br/deflate
///     Cargo features enabled; forwarding these causes the server to send
///     compressed bytes that are written raw to disk → file corruption.
///   - `transfer-encoding` — hop-by-hop header; must not be forwarded.
///   - `host` — must match the actual request target, not the browser's.
///   - `content-length` — meaningless on a GET; can confuse intermediaries.
///   - `connection` — hop-by-hop header managed by the HTTP stack.
pub(crate) fn apply_extra_headers(
    req: reqwest::RequestBuilder,
    extra_headers: &std::collections::HashMap<String, String>,
) -> reqwest::RequestBuilder {
    if extra_headers.is_empty() {
        return req;
    }

    /// Headers that must never be forwarded from the browser extension.
    /// Compared case-insensitively via `HeaderName` (which lowercases).
    const BLOCKED_HEADERS: &[&str] = &[
        "accept-encoding",
        "content-encoding",
        "transfer-encoding",
        "host",
        "content-length",
        "connection",
    ];

    let mut map = reqwest::header::HeaderMap::with_capacity(extra_headers.len());
    for (name, value) in extra_headers {
        if let (Ok(header_name), Ok(header_value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            if BLOCKED_HEADERS
                .iter()
                .any(|&blocked| header_name.as_str() == blocked)
            {
                log_info!(
                    "[extra-headers] filtered dangerous header: {}",
                    header_name.as_str()
                );
                continue;
            }
            map.insert(header_name, header_value);
        }
    }
    // req.headers(map) 内部用 insert 逐个替换同名头，
    // 确保浏览器的真实 User-Agent 等值覆盖 build_client 设的默认值。
    req.headers(map)
}

/// Content-Encoding types that the server may apply to response bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentEncoding {
    Gzip,
    Brotli,
    Deflate,
    Zstd,
}

/// Detect the `Content-Encoding` from response headers.
///
/// Returns `Some(encoding)` when the server applied compression (gzip, br,
/// deflate, zstd).  Returns `None` when the header is absent, empty, or
/// `identity` (i.e. the body is uncompressed).
///
/// Unknown encodings are mapped to `None` — callers that need strict
/// validation should check the raw header separately.
pub fn detect_content_encoding(headers: &reqwest::header::HeaderMap) -> Option<ContentEncoding> {
    let ce = headers.get(reqwest::header::CONTENT_ENCODING)?;
    let value = ce.to_str().unwrap_or("");
    // HTTP allows comma-separated encodings (e.g. "gzip, identity").
    // Take the first non-identity encoding as the dominant one.
    for part in value.split(',') {
        let lower = part.trim().to_ascii_lowercase();
        match lower.as_str() {
            "gzip" | "x-gzip" => return Some(ContentEncoding::Gzip),
            "br" | "brotli" => return Some(ContentEncoding::Brotli),
            "deflate" => return Some(ContentEncoding::Deflate),
            "zstd" => return Some(ContentEncoding::Zstd),
            _ => continue, // "identity", "", "compress", unknown
        }
    }
    None
}

/// Wrap a response byte stream with transparent decompression if the server
/// returned a compressed `Content-Encoding`.  For `identity` or missing
/// encoding, returns the original stream unchanged.
///
/// This is the core fix for file corruption: instead of writing raw gzip
/// bytes to disk, we decompress on-the-fly and write the original file
/// content.
///
/// The output stream uses `std::io::Error` because `reqwest::Error` is opaque
/// and cannot be constructed from an `io::Error`.  Callers should convert via
/// `DownloadError::Io` when consuming chunks.
pub fn maybe_decompress_stream(
    stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>>
    + Unpin
    + Send
    + 'static,
    encoding: Option<ContentEncoding>,
) -> Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Unpin + Send> {
    // Map the incoming reqwest::Error stream to io::Error so every branch
    // has a uniform error type.
    let io_stream = stream.map(|result| result.map_err(std::io::Error::other));

    let Some(enc) = encoding else {
        return Box::new(io_stream);
    };

    let reader = tokio_util::io::StreamReader::new(io_stream);

    // Wrap with the appropriate decompressor and convert back to a stream.
    match enc {
        ContentEncoding::Gzip => {
            let decoder = async_compression::tokio::bufread::GzipDecoder::new(reader);
            Box::new(tokio_util::io::ReaderStream::new(decoder))
        }
        ContentEncoding::Brotli => {
            let decoder = async_compression::tokio::bufread::BrotliDecoder::new(reader);
            Box::new(tokio_util::io::ReaderStream::new(decoder))
        }
        ContentEncoding::Deflate => {
            let decoder = async_compression::tokio::bufread::DeflateDecoder::new(reader);
            Box::new(tokio_util::io::ReaderStream::new(decoder))
        }
        ContentEncoding::Zstd => {
            let decoder = async_compression::tokio::bufread::ZstdDecoder::new(reader);
            Box::new(tokio_util::io::ReaderStream::new(decoder))
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP client builder (shared config)
// ---------------------------------------------------------------------------

/// Default User-Agent for HTTP requests.
///
/// Uses a neutral download-manager identifier instead of a browser UA.
///
/// **Why not Chrome UA?**  Cloudflare's Bot Management compares the TLS
/// fingerprint (JA3/JA4) against the declared User-Agent.  rustls produces a
/// JA3 fingerprint that does not match Chrome's.  When a non-browser TLS
/// fingerprint is paired with a Chrome UA, Cloudflare flags the request as
/// bot traffic and returns 403/404 — this breaks downloads from any CDN
/// behind Cloudflare (e.g. JetBrains' `download-cdn.clf.jetbrains.com.cn`).
///
/// When the browser extension captures a download it passes the real browser
/// UA via `extra_headers`.  That UA is applied on the first attempt; if the
/// server returns 4xx we automatically retry *without* the browser UA so that
/// Cloudflare-protected CDNs also work (see [`resolve_file_info`]).
const DEFAULT_UA: &str = "FluxDown/1.0";

/// Build a properly configured HTTP client that mirrors Chrome's capabilities.
///
/// When `proxy_config` specifies a proxy, it is injected into the client builder.
/// - `ProxyMode::None`   → explicit `no_proxy()` to disable env-var proxies
/// - `ProxyMode::System`  → auto-detect from Windows registry / environment
/// - `ProxyMode::Manual`  → user-specified proxy URL (HTTP/HTTPS/SOCKS4/SOCKS5)
///
/// When `user_agent` is non-empty, it overrides the built-in Chrome UA.
pub fn build_client(
    proxy_config: &crate::proxy_config::ProxyConfig,
    user_agent: &str,
) -> Result<Client, DownloadError> {
    use crate::proxy_config::{ProxyMode, detect_system_proxy};

    let ua = if user_agent.is_empty() {
        DEFAULT_UA
    } else {
        user_agent
    };
    let mut builder = Client::builder()
        .user_agent(ua)
        // TLS — 跳过证书验证（过期、自签名、hostname 不匹配等）。
        // 下载管理器与浏览器行为保持一致：浏览器允许用户忽略证书错误继续下载，
        // 且企业内网邮箱等场景常见 hostname mismatch，严格验证会导致下载失败。
        // 类似 curl -k / aria2 --check-certificate=false。
        .danger_accept_invalid_certs(true)
        // NOTE: This setting also applies to MITM proxy scenarios.
        // A malicious proxy could intercept HTTPS traffic undetected.
        // Users operating in sensitive environments should be aware of this trade-off.
        // A future improvement would be to add a "strict TLS" toggle in Settings.
        // HTTP version — force HTTP/1.1 for download manager use cases:
        //  1. Range requests are reliable and well-tested on HTTP/1.1.
        //  2. Multi-segment downloads use separate TCP connections; HTTP/2
        //     multiplexing would force all segments onto one connection.
        //  3. Some servers advertise h2 via ALPN but have buggy HTTP/2
        //     implementations that close connections mid-response.
        .http1_only()
        // TCP tuning — disable Nagle's algorithm to eliminate up to 200 ms
        // latency on small writes (Range request headers, TLS handshake
        // messages).  All high-performance download managers (IDM, aria2)
        // set this.  Safe for bulk transfers because BufWriter already
        // coalesces writes into 256 KB chunks before hitting the socket.
        .tcp_nodelay(true)
        // TCP Keep-Alive — 60s 间隔比系统默认（通常 >2min）更激进，
        // 确保 NAT/防火墙不会因空闲超时而断开长时间下载的连接。
        // reqwest 底层设置 TCP_KEEPIDLE=60s（首次探测前等待时间）。
        .tcp_keepalive(Duration::from_secs(60))
        // Redirects — follow up to 30 hops like Chrome
        .redirect(reqwest::redirect::Policy::limited(30))
        // Timeouts — 15 s is sufficient for initial TCP+TLS handshake;
        // the stall detector (CHUNK_STALL_TIMEOUT) handles mid-transfer
        // hangs separately.  Shorter timeout lets failed segments retry
        // faster instead of blocking a worker for 30 s.
        .connect_timeout(Duration::from_secs(15))
        // No global timeout — downloads can be very long
        // Connection pool — keep enough idle connections to cover all
        // segments of a typical multi-segment download.  16 matches
        // MAX_SEGMENTS on a 4-core machine (cpu_cores * 4) and avoids
        // expensive TCP+TLS re-handshakes when workers finish one segment
        // and immediately start the next.  90 s idle timeout tolerates
        // brief pauses / UI interaction without discarding warm connections.
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(16)
        // Cookies — needed for session-based downloads (Google Drive, etc.).
        // reqwest follows RFC 6265: cookies are scoped to their domain.
        .cookie_store(true)
        // Do NOT enable auto-decompression (.gzip/.brotli/.deflate).
        // A download manager must receive raw bytes so that:
        //  1. Content-Length matches the actual bytes written to disk.
        //  2. Range-based multi-segment downloads use correct byte offsets.
        //  3. The integrity check (file size vs Content-Length) works reliably.
        //
        // The gzip/brotli/deflate Cargo features are intentionally NOT enabled
        // to keep the binary small and avoid accidental decompression.
        // We explicitly set `Accept-Encoding: identity` so the server never
        // sends compressed content and Content-Length always equals raw bytes.
        .default_headers({
            let mut h = reqwest::header::HeaderMap::new();
            h.insert(
                reqwest::header::ACCEPT_ENCODING,
                HeaderValue::from_static("identity"),
            );
            h
        });

    // --- Proxy injection ---
    match proxy_config.mode {
        ProxyMode::None => {
            // Explicitly disable proxy so env vars (HTTP_PROXY etc.) are ignored.
            builder = builder.no_proxy();
        }
        ProxyMode::System => {
            // Read Windows registry / env vars for system proxy.
            match detect_system_proxy() {
                Ok(Some(sys_proxy)) => {
                    if let Some(url) = sys_proxy.to_proxy_url() {
                        log_info!(
                            "[build_client] system proxy detected (url redacted for security)"
                        );
                        match reqwest::Proxy::all(&url) {
                            Ok(mut proxy) => {
                                if !sys_proxy.username.is_empty() {
                                    proxy =
                                        proxy.basic_auth(&sys_proxy.username, &sys_proxy.password);
                                }
                                if !sys_proxy.no_proxy_list.is_empty() {
                                    proxy = proxy.no_proxy(reqwest::NoProxy::from_string(
                                        &sys_proxy.no_proxy_list,
                                    ));
                                }
                                builder = builder.proxy(proxy);
                            }
                            Err(e) => {
                                log_info!("[build_client] failed to parse system proxy URL: {}", e);
                            }
                        }
                    } else {
                        log_info!("[build_client] system proxy enabled but no URL resolved");
                    }
                }
                Ok(None) => {
                    log_info!("[build_client] system proxy: not configured");
                }
                Err(e) => {
                    log_info!("[build_client] system proxy detection error: {}", e);
                }
            }
        }
        ProxyMode::Manual => {
            if let Some(url) = proxy_config.to_proxy_url() {
                log_info!("[build_client] manual proxy configured");
                match reqwest::Proxy::all(&url) {
                    Ok(mut proxy) => {
                        if !proxy_config.username.is_empty() {
                            proxy =
                                proxy.basic_auth(&proxy_config.username, &proxy_config.password);
                        }
                        if !proxy_config.no_proxy_list.is_empty() {
                            proxy = proxy.no_proxy(reqwest::NoProxy::from_string(
                                &proxy_config.no_proxy_list,
                            ));
                        }
                        builder = builder.proxy(proxy);
                    }
                    Err(e) => {
                        log_info!("[build_client] failed to create proxy from URL: {}", e);
                    }
                }
            } else {
                log_info!("[build_client] manual proxy: incomplete config, using direct");
                builder = builder.no_proxy();
            }
        }
    }

    let client = builder.build()?;
    Ok(client)
}

// ---------------------------------------------------------------------------
// Resolve file info (HEAD probe → GET fallback)
// ---------------------------------------------------------------------------

/// Timeout for the probe requests (HEAD / GET Range:0-0).
/// 15 seconds is sufficient for most servers; the retry mechanism handles
/// transient failures without making users wait excessively.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum retries for the probe phase (HEAD + GET).
///
/// 3 attempts total:
///   1. Original headers (incl. browser UA from extension, if any)
///   2. Normal retry (same headers, covers DNS/TLS cold-start)
///   3. **UA-downgrade retry** — strips browser UA from extra_headers so that
///      the request uses the neutral `DEFAULT_UA`.  This handles Cloudflare
///      Bot Management which rejects requests where the TLS fingerprint
///      (rustls ≠ Chrome) contradicts a Chrome User-Agent header.
const PROBE_MAX_RETRIES: u32 = 3;

/// Base delay for probe retries (used with exponential backoff).
const PROBE_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

/// Resolve file info with automatic retry on transient failures.
///
/// On Windows, the very first HTTPS request from a new process can fail due to
/// DNS resolver cold-start, rustls TLS session initialisation, or firewall
/// first-connection inspection.  Retrying transparently hides this from users.
pub async fn resolve_file_info(
    client: &Client,
    url: &str,
    cookies: &str,
    referrer: &str,
    extra_headers: &std::collections::HashMap<String, String>,
) -> Result<FileInfo, DownloadError> {
    // Prepare a fallback header map that strips browser-like User-Agent.
    // On the last attempt we use this to avoid Cloudflare JA3-vs-UA mismatch.
    let headers_without_browser_ua: std::collections::HashMap<String, String> = extra_headers
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("user-agent"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let has_browser_ua = extra_headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("user-agent"));

    let mut last_err = None;
    for attempt in 0..PROBE_MAX_RETRIES {
        // Last attempt: if extra_headers carried a browser UA, drop it so
        // the request falls back to DEFAULT_UA ("FluxDown/1.0").  This
        // avoids Cloudflare's TLS-fingerprint-vs-UA bot detection.
        let use_downgraded_ua = has_browser_ua && attempt + 1 == PROBE_MAX_RETRIES;
        let hdrs = if use_downgraded_ua {
            if attempt == 0 {
                // Should not happen with PROBE_MAX_RETRIES >= 2, but guard anyway.
                extra_headers
            } else {
                log_info!(
                    "[resolve] retry {}/{}: stripping browser UA to avoid bot detection",
                    attempt + 1,
                    PROBE_MAX_RETRIES
                );
                &headers_without_browser_ua
            }
        } else {
            extra_headers
        };

        match resolve_file_info_once(client, url, cookies, referrer, hdrs).await {
            Ok(info) => return Ok(info),
            Err(e) => {
                log_info!(
                    "[resolve] probe attempt {}/{} failed: {}",
                    attempt + 1,
                    PROBE_MAX_RETRIES,
                    e
                );
                last_err = Some(e);
                if attempt + 1 < PROBE_MAX_RETRIES {
                    let delay = PROBE_RETRY_BASE_DELAY * 2u32.saturating_pow(attempt);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| DownloadError::Other("probe failed after retries".to_string())))
}

/// Walk the std::error::Error source chain and return a " → cause1 → cause2" suffix string.
/// Returns an empty string when there is no source, so it can be appended directly to a message.
fn format_error_chain(mut src: Option<&dyn StdError>) -> String {
    let mut s = String::new();
    while let Some(cause) = src {
        s.push_str(" → ");
        s.push_str(&cause.to_string());
        src = cause.source();
    }
    s
}

async fn resolve_file_info_once(
    client: &Client,
    url: &str,
    cookies: &str,
    referrer: &str,
    extra_headers: &std::collections::HashMap<String, String>,
) -> Result<FileInfo, DownloadError> {
    // --- Concurrent HEAD + GET probe ----------------------------------------
    // Fire both HEAD and GET Range:0-0 in parallel.  HEAD is faster when it
    // works, but many servers/CDNs omit Content-Disposition on HEAD.  By
    // running both concurrently we avoid the serial HEAD→GET penalty.
    //
    // IMPORTANT for Content-Encoding handling:
    // Many CDNs (Cloudflare, Akamai) add Content-Encoding: gzip to HEAD and
    // full-GET responses but **omit** it from 206 Partial Content responses.
    // This is correct per HTTP semantics: Range requests operate on the
    // *original* (identity) representation, not the compressed one.
    //
    // We therefore check Content-Encoding on the GET Range:0-0 response
    // **separately** from the merged headers.  If GET returned 206 without
    // Content-Encoding, Range requests are safe for multi-segment downloads
    // even when HEAD advertised compression.

    let head_fut = {
        let mut req = client.head(url).timeout(PROBE_TIMEOUT);
        if !cookies.is_empty() {
            req = req.header("Cookie", cookies);
        }
        if !referrer.is_empty() {
            req = req.header(reqwest::header::REFERER, referrer);
        }
        req = apply_extra_headers(req, extra_headers);
        req.send()
    };

    let get_fut = {
        let mut req = client
            .get(url)
            .header("Range", "bytes=0-0")
            .timeout(PROBE_TIMEOUT);
        if !cookies.is_empty() {
            req = req.header("Cookie", cookies);
        }
        if !referrer.is_empty() {
            req = req.header(reqwest::header::REFERER, referrer);
        }
        req = apply_extra_headers(req, extra_headers);
        req.send()
    };

    let (head_result, get_result) = tokio::join!(head_fut, get_fut);

    // Extract HEAD response (if successful)
    let head_data = match head_result {
        Ok(r) if r.status().is_success() => {
            let u = r.url().clone();
            let h = r.headers().clone();
            Some((h, u))
        }
        Ok(r) => {
            log_info!(
                "[resolve] HEAD failed: status={}, url={}, cookies_len={}",
                r.status(),
                r.url(),
                cookies.len()
            );
            None
        }
        Err(e) => {
            log_info!(
                "[resolve] HEAD network error: {}{}, cookies_len={}",
                e,
                format_error_chain(e.source()),
                cookies.len()
            );
            None
        }
    };

    // Extract GET response (if successful)
    let get_data = match get_result {
        Ok(r) if r.status().is_success() => {
            let u = r.url().clone();
            let h = r.headers().clone();
            let got_206 = r.status() == reqwest::StatusCode::PARTIAL_CONTENT;
            // Check Content-Encoding on the GET Range:0-0 response BEFORE
            // merging with HEAD.  This tells us whether Range responses
            // carry compression — the key signal for multi-segment safety.
            let get_range_compressed = got_206 && detect_content_encoding(&h).is_some();
            drop(r); // release connection immediately
            Some((h, u, got_206, get_range_compressed))
        }
        Ok(r) => {
            log_info!(
                "[resolve] GET failed: status={}, url={}, cookies_len={}",
                r.status(),
                r.url(),
                cookies.len()
            );
            None
        }
        Err(e) => {
            log_info!(
                "[resolve] GET network error: {}{}, cookies_len={}",
                e,
                format_error_chain(e.source()),
                cookies.len()
            );
            None
        }
    };

    // Track whether the GET Range:0-0 response itself carried compression.
    // false = either GET didn't succeed, returned 200 (not 206), or returned
    //         206 without Content-Encoding → Range requests are safe.
    // true  = GET returned 206 WITH Content-Encoding → rare but must disable
    //         multi-segment to avoid corrupt byte-range splicing.
    let range_response_compressed = get_data
        .as_ref()
        .is_some_and(|(_, _, _, compressed)| *compressed);

    // Merge results: HEAD as base, GET to fill in missing data.
    let (mut headers, mut final_url) = match (&head_data, &get_data) {
        (Some((hh, hu)), _) => (hh.clone(), hu.clone()),
        (None, Some((gh, gu, _, _))) => (gh.clone(), gu.clone()),
        (None, None) => {
            return Err(DownloadError::Other(
                "both HEAD and GET probes failed".to_string(),
            ));
        }
    };

    // If HEAD succeeded but lacks Content-Disposition, merge from GET.
    if head_data.is_some()
        && let Some((get_headers, get_url, got_206, _)) = &get_data
    {
        if !headers.contains_key(reqwest::header::CONTENT_DISPOSITION)
            && let Some(cd) = get_headers.get(reqwest::header::CONTENT_DISPOSITION)
        {
            headers.insert(reqwest::header::CONTENT_DISPOSITION, cd.clone());
        }
        if let Some(ct) = get_headers.get(reqwest::header::CONTENT_TYPE) {
            headers.insert(reqwest::header::CONTENT_TYPE, ct.clone());
        }
        // Prefer GET's final URL (may differ after redirect)
        final_url = get_url.clone();
        // If GET gave us 206, copy Content-Range for accurate file size
        if *got_206 && let Some(cr) = get_headers.get("content-range") {
            headers.insert(
                reqwest::header::HeaderName::from_static("content-range"),
                cr.clone(),
            );
        }
    }

    // --- Phase 3: Parse metadata from merged headers ------------------------
    // A 206 response from GET proves range support even without Accept-Ranges header.
    let got_206_from_get = get_data.as_ref().is_some_and(|(_, _, got, _)| *got);
    let mut supports_range = got_206_from_get
        || headers
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v != "none");

    let total_bytes = if let Some(cr) = headers.get("content-range") {
        // e.g. "bytes 0-0/12345"
        cr.to_str()
            .ok()
            .and_then(|v| v.rsplit('/').next())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    } else {
        headers
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    };

    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let file_name = extract_filename(&headers, final_url.as_str());
    log_info!(
        "[resolve] url={} → name={}, size={}, range={}, ct={}",
        url,
        file_name,
        total_bytes,
        supports_range,
        content_type
    );

    let etag = headers
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let last_modified = headers
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // --- Content-Encoding handling -------------------------------------------
    //
    // The merged `headers` may carry Content-Encoding from the HEAD response.
    // However, this does NOT mean Range responses are also compressed.
    //
    // HTTP semantics (RFC 9110 §8.8.3): Range requests operate on the
    // "selected representation" which is typically the **identity** encoding.
    // Most CDNs (Cloudflare, Akamai, AWS CloudFront) correctly:
    //   - HEAD / full GET → Content-Encoding: gzip (if Accept-Encoding allows)
    //   - GET Range:bytes=X-Y → 206 with NO Content-Encoding (raw bytes)
    //
    // We use the GET Range:0-0 probe result (`range_response_compressed`) as
    // the authoritative signal for multi-segment safety:
    //
    //   GET 206 WITHOUT Content-Encoding → Range returns raw bytes → safe
    //   GET 206 WITH    Content-Encoding → rare; server compresses Range
    //                                      responses too → NOT safe
    //   Only HEAD available (GET failed)  → conservative; use HEAD's signal
    //
    // When Range responses ARE compressed, we disable multi-segment and let
    // `download_single` decompress the full-GET stream on-the-fly.
    //
    // When Range responses are NOT compressed (the common case), multi-segment
    // can proceed normally even if HEAD showed Content-Encoding.

    // Did *any* probe response (HEAD or GET) indicate compression?
    let content_encoding_compressed = detect_content_encoding(&headers).is_some();

    // Should we disable Range support due to compression?
    // Only if the GET Range:0-0 *itself* returned compressed content,
    // OR if we have no GET data and must rely on HEAD alone.
    let got_get_206 = get_data.as_ref().is_some_and(|(_, _, got, _)| *got);
    let disable_range_for_compression = if got_get_206 {
        // We have a 206 response — use its Content-Encoding as ground truth.
        range_response_compressed
    } else {
        // No 206 available (GET failed or returned 200) — fall back to the
        // merged headers (conservative: if HEAD says compressed, disable).
        content_encoding_compressed
    };

    if disable_range_for_compression {
        log_info!(
            "[resolve] WARNING: Range response itself carries Content-Encoding: {:?} — \
             byte ranges are invalid on compressed streams; disabling multi-segment",
            headers
                .get(reqwest::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?")
        );
        supports_range = false;
    } else if content_encoding_compressed {
        // HEAD indicated compression but the GET 206 did NOT — Range requests
        // return raw (identity) bytes.  Multi-segment is safe.  The HEAD's
        // Content-Length may be the compressed size though — if we got a
        // Content-Range from the 206, that already gave us the real file size.
        log_info!(
            "[resolve] HEAD indicated Content-Encoding: {:?} but GET Range:0-0 \
             returned 206 without compression — Range requests use identity \
             encoding; multi-segment is safe (total_bytes={}, range={})",
            headers
                .get(reqwest::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?"),
            total_bytes,
            supports_range
        );
    }

    Ok(FileInfo {
        file_name,
        total_bytes,
        supports_range,
        content_type,
        etag,
        last_modified,
        content_encoding_compressed,
    })
}

// ---------------------------------------------------------------------------
// File-name extraction
// ---------------------------------------------------------------------------

/// MIME type → common extension mapping for when there is no filename.
fn mime_to_ext(content_type: &str) -> Option<&'static str> {
    let ct = content_type.split(';').next().unwrap_or("").trim();
    match ct {
        "application/pdf" => Some("pdf"),
        "application/zip" => Some("zip"),
        "application/x-gzip" | "application/gzip" => Some("gz"),
        "application/x-tar" => Some("tar"),
        "application/x-bzip2" => Some("bz2"),
        "application/x-xz" => Some("xz"),
        "application/x-7z-compressed" => Some("7z"),
        "application/x-rar-compressed" | "application/vnd.rar" => Some("rar"),
        "application/json" => Some("json"),
        "application/xml" | "text/xml" => Some("xml"),
        "application/javascript" | "text/javascript" => Some("js"),
        "application/wasm" => Some("wasm"),
        "application/octet-stream" => None, // generic binary
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        "application/msword" => Some("doc"),
        "application/vnd.ms-excel" => Some("xls"),
        "application/vnd.ms-powerpoint" => Some("ppt"),
        "application/x-iso9660-image" => Some("iso"),
        "application/x-msdownload" | "application/x-dosexec" => Some("exe"),
        "application/vnd.android.package-archive" => Some("apk"),
        "application/java-archive" => Some("jar"),
        "application/x-shockwave-flash" => Some("swf"),
        "application/x-debian-package" => Some("deb"),
        "application/x-rpm" => Some("rpm"),
        "application/x-msi" => Some("msi"),
        "application/vnd.apple.installer+xml" => Some("pkg"),
        "text/html" => Some("html"),
        "text/css" => Some("css"),
        "text/csv" => Some("csv"),
        "text/plain" => Some("txt"),
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/svg+xml" => Some("svg"),
        "image/bmp" => Some("bmp"),
        "image/x-icon" | "image/vnd.microsoft.icon" => Some("ico"),
        "image/tiff" => Some("tiff"),
        "image/avif" => Some("avif"),
        "audio/mpeg" => Some("mp3"),
        "audio/ogg" => Some("ogg"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        "audio/flac" => Some("flac"),
        "audio/aac" => Some("aac"),
        "audio/mp4" | "audio/x-m4a" => Some("m4a"),
        "audio/webm" => Some("weba"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        "video/x-matroska" => Some("mkv"),
        "video/x-msvideo" => Some("avi"),
        "video/quicktime" => Some("mov"),
        "video/x-flv" => Some("flv"),
        "video/mp2t" => Some("ts"),
        "video/3gpp" => Some("3gp"),
        "font/woff" => Some("woff"),
        "font/woff2" => Some("woff2"),
        "font/ttf" | "application/x-font-ttf" => Some("ttf"),
        "font/otf" => Some("otf"),
        _ => None,
    }
}

pub(crate) fn extract_filename(headers: &reqwest::header::HeaderMap, url: &str) -> String {
    // 1. Try Content-Disposition: attachment; filename="xxx"
    if let Some(name) = extract_from_content_disposition(headers) {
        return name;
    }

    // 2. Try URL path (after removing query & fragment)
    if let Some(name) = extract_from_url(url) {
        return name;
    }

    // 3. Try Content-Type → build "download.ext"
    if let Some(ct) = headers.get(reqwest::header::CONTENT_TYPE)
        && let Ok(ct_str) = ct.to_str()
        && let Some(ext) = mime_to_ext(ct_str)
    {
        return format!("download.{}", ext);
    }

    "download".to_string()
}

fn extract_from_content_disposition(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let disposition = headers.get(reqwest::header::CONTENT_DISPOSITION)?;
    // Use from_utf8 instead of to_str(): the http crate's to_str() rejects any byte > 0x7E,
    // but some servers (e.g. z-lib CDN) embed raw UTF-8 characters (Chinese, Japanese, etc.)
    // directly in the filename="" parameter.  Those bytes are valid UTF-8 even though they
    // are not ASCII, so from_utf8 succeeds where to_str would silently return None.
    let value = std::str::from_utf8(disposition.as_bytes()).ok()?;

    // Prefer filename*= (RFC 5987 / RFC 6266) over filename=
    for part in value.split(';') {
        let trimmed = part.trim();
        if let Some(name) = trimmed.strip_prefix("filename*=") {
            // Format: charset'language'percent-encoded-name
            // e.g. UTF-8''My%20File.pdf
            let name = name.trim();
            if let Some(encoded) = name.split('\'').nth(2)
                && let Ok(decoded) = urlencoding_decode(encoded)
            {
                let decoded = decoded.trim();
                if !decoded.is_empty() {
                    return Some(sanitize_filename(decoded));
                }
            }
        }
    }

    for part in value.split(';') {
        let trimmed = part.trim();
        if let Some(name) = trimmed.strip_prefix("filename=") {
            let name = name.trim_matches(|c| c == '"' || c == '\'' || c == ' ');
            if !name.is_empty() {
                // Heuristic: some servers (e.g. Chinese cloud storage OBS/S3)
                // percent-encode the filename= value instead of using the
                // RFC 5987 filename*= syntax.  When the raw value contains
                // percent-encoded sequences, try URL-decoding it so that
                // `%E6%B0%B8%E7%94%9F.mp4` becomes `永生.mp4`.
                if name.contains('%')
                    && let Ok(decoded) = urlencoding_decode(name)
                {
                    let decoded = decoded.trim();
                    if !decoded.is_empty() && decoded != name {
                        return Some(sanitize_filename(decoded));
                    }
                }
                return Some(sanitize_filename(name));
            }
        }
    }

    None
}

pub fn extract_from_url(url: &str) -> Option<String> {
    // Strip query and fragment
    let path = url.split('?').next().unwrap_or(url);
    let path = path.split('#').next().unwrap_or(path);
    let segment = path.rsplit('/').next()?;
    let decoded = urlencoding_decode(segment).unwrap_or_else(|_| segment.to_string());
    let decoded = decoded.trim();
    if decoded.is_empty() || decoded == "/" {
        return None;
    }
    Some(sanitize_filename(decoded))
}

/// Remove or replace characters that are illegal in file names on Windows/macOS/Linux.
pub fn sanitize_filename(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let s = s.trim_matches(|c: char| c == '.' || c == ' ');
    if s.is_empty() {
        "download".to_string()
    } else {
        s.to_string()
    }
}

fn urlencoding_decode(s: &str) -> Result<String, String> {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                result.push(byte);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            result.push(b' ');
            i += 1;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Dedup file name: "file.txt" → "file (1).txt" etc.
// ---------------------------------------------------------------------------

/// Deduplicate a filename so it does not collide with any existing file in
/// `dir` **nor** with any in-flight download that has already reserved the
/// same temporary path.
///
/// # Parameters
/// - `dir`      – target save directory.
/// - `name`     – desired filename (e.g. `"video.mp4"`).
/// - `reserved` – snapshot of `DownloadManager::reserved_temp_paths`;
///   contains the `.fdownloading` paths that concurrent tasks have already
///   claimed.  Pass an empty set when the caller has no reserved paths to
///   check (e.g. resume tasks, which skip dedup entirely).
///
/// # Why `reserved` is needed
/// `dedup_filename` is called from inside a spawned tokio task, well after
/// the manager's synchronous section has finished.  Multiple tasks spawned
/// in the same batch can all enter `dedup_filename` concurrently; each sees
/// the same on-disk state (no `.fdownloading` file yet) and all independently
/// choose the same filename.  They then race to write the same temp file,
/// causing the last writer to silently overwrite the earlier ones.
///
/// By consulting `reserved` — a snapshot taken **before** spawning, in the
/// manager's synchronous section — each task can see which names its siblings
/// have already claimed and avoid them.
pub async fn dedup_filename(
    dir: &Path,
    name: &str,
    reserved: &std::collections::HashSet<std::path::PathBuf>,
) -> String {
    use std::ffi::OsStr;

    // Phase 1: fast probe — most of the time there is no conflict.
    let candidate = dir.join(name);
    let temp_candidate = PathBuf::from(format!("{}{}", candidate.display(), TEMP_EXT));
    // Also check the in-flight reservation set BEFORE the async disk probes
    // so that two tasks starting simultaneously both see each other's claim.
    if !reserved.contains(&temp_candidate)
        && !tokio::fs::try_exists(&candidate).await.unwrap_or(false)
        && !tokio::fs::try_exists(&temp_candidate)
            .await
            .unwrap_or(false)
    {
        return name.to_string();
    }

    // Phase 2: conflict detected — scan directory into memory to avoid
    // up to 19998 filesystem calls in the dedup loop.
    let existing = {
        let mut set = std::collections::HashSet::new();
        if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                set.insert(entry.file_name()); // OsString: handles non-UTF-8
            }
        }
        set
    };

    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let ext = Path::new(name).extension().and_then(|s| s.to_str());

    for i in 1..=9999 {
        let new_name = if let Some(ext) = ext {
            format!("{} ({}).{}", stem, i, ext)
        } else {
            format!("{} ({})", stem, i)
        };
        let temp_name = format!("{}{}", new_name, TEMP_EXT);
        let temp_path = dir.join(&temp_name);
        // Check both the final/in-progress disk files AND the in-flight set.
        if !reserved.contains(&temp_path)
            && !existing.contains(OsStr::new(&new_name))
            && !existing.contains(OsStr::new(&temp_name))
        {
            return new_name;
        }
    }
    name.to_string()
}

/// Temporary file extension used during download (like Chrome's `.crdownload`).
/// The file is renamed to the final name only after all data is verified.
pub const TEMP_EXT: &str = ".fdownloading";

/// Buffer size for `BufWriter` wrapping file I/O during downloads.
/// 256 KB reduces the frequency of syscalls compared to the default 8 KB,
/// significantly improving throughput especially with many concurrent segments.
pub const BUF_WRITER_CAPACITY: usize = 256 * 1024;

/// Interval (in seconds) between DB persistence of download progress.
/// Balances crash-recovery granularity (max ~3 s of re-download) against
/// SQLite Mutex contention (reduces writes from ~80/s to ~5/s with 16 segments).
pub const DB_SAVE_INTERVAL_SECS: u64 = 3;

/// 单个 chunk 的读取超时（stall detection）。如果超过此时间没有收到任何数据，
/// 视为连接停滞，返回错误触发 retry 或让用户感知到真实状态。
/// 与 segment_coordinator 中的同名常量保持一致。
const CHUNK_STALL_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run_download(params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let result = run_download_inner(&params).await;

    match result {
        Ok(total) => {
            log_info!(
                "[download] task {} completed, total={} bytes",
                task_id_log,
                total
            );
            let _ = params.db.update_task_status(&params.task_id, 3, "").await;
            let _ = params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: total,
                    total_bytes: total,
                    status: 3,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: None,
                })
                .await;
        }
        Err(DownloadError::Cancelled) => {
            log_info!("[download] task {} cancelled", task_id_log);
            // pause / cancel already handled upstream — nothing to do
        }
        Err(e) => {
            let msg = e.to_string();
            // 追加完整错误链（root cause），方便排查网络/TLS 等底层错误。
            // msg 已包含 reqwest 顶层描述，这里从 source 的 source 开始
            // 避免重复打印同一层信息。
            let chain = if let Some(src) = StdError::source(&e) {
                format_error_chain(src.source())
            } else {
                String::new()
            };
            let full_msg = format!("{}{}", msg, chain);
            // checksum 失败时所有字节均已下载完毕（只是校验未通过），需特殊处理进度。
            let is_checksum_fail = matches!(e, DownloadError::ChecksumMismatch(_));
            log_info!("[download] task {} error: {}", task_id_log, full_msg);
            let _ = params
                .db
                .update_task_status(&params.task_id, 4, &full_msg)
                .await;

            // Preserve actual progress from DB so the UI doesn't jump back to 0%.
            let (dl, total) = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => {
                    // checksum 失败 → 字节已全部下载，进度应显示 100%。
                    // 其他错误 → 保留 DB 中实际已下载量，防止 UI 回跳至 0%。
                    let dl = if is_checksum_fail {
                        t.total_bytes
                    } else {
                        t.downloaded_bytes
                    };
                    (dl, t.total_bytes)
                }
                other => {
                    log_info!(
                        "[download] task {} warning: failed to read progress from DB: {:?}",
                        task_id_log,
                        other.err()
                    );
                    (0, 0)
                }
            };
            let _ = params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: dl,
                    total_bytes: total,
                    status: 4,
                    error_message: full_msg,
                    file_name: String::new(),
                    segment_details: None,
                })
                .await;
        }
    }
}

/// Verify that a file at `path` matches the checksum in `spec`.
///
/// `spec` format: `"algo=hexhash"`, e.g. `"sha-256=abc123..."` or `"md5=d41d8c..."`.
/// Supported algorithms: `sha-256`/`sha256`, `sha-512`/`sha512`, `sha-1`/`sha1`, `md5`.
/// Returns `Ok(())` if the digest matches, or `Err(DownloadError::ChecksumMismatch)` if not.
async fn verify_checksum(path: &Path, spec: &str) -> Result<(), DownloadError> {
    let sep = spec.find('=').ok_or_else(|| {
        DownloadError::Other(format!(
            "invalid checksum format (expected algo=hash): {}",
            spec
        ))
    })?;
    let algo_raw = spec[..sep].trim().to_lowercase();
    let expected_hex = spec[sep + 1..].trim().to_lowercase();

    // Normalize algorithm aliases to a canonical key.
    let algo = match algo_raw.as_str() {
        "sha-256" | "sha256" => "sha256",
        "sha-512" | "sha512" => "sha512",
        "sha-1" | "sha1" => "sha1",
        "md5" => "md5",
        other => {
            return Err(DownloadError::Other(format!(
                "unsupported checksum algorithm: {}",
                other
            )));
        }
    };

    let path_owned = path.to_path_buf();
    let algo_owned = algo.to_string();

    let actual_hex = tokio::task::spawn_blocking(move || -> Result<String, DownloadError> {
        use std::io::Read;
        let mut file = std::fs::File::open(&path_owned)?;
        let mut buf = vec![0u8; 1024 * 1024]; // 1 MiB read buffer
        match algo_owned.as_str() {
            "sha256" => {
                use sha2::Digest;
                let mut h = sha2::Sha256::new();
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                Ok(hex::encode(h.finalize()))
            }
            "sha512" => {
                use sha2::Digest;
                let mut h = sha2::Sha512::new();
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                Ok(hex::encode(h.finalize()))
            }
            "sha1" => {
                use sha1::Digest;
                let mut h = sha1::Sha1::new();
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                Ok(hex::encode(h.finalize()))
            }
            "md5" => {
                use md5::Digest;
                let mut h = md5::Md5::new();
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                Ok(hex::encode(h.finalize()))
            }
            _ => Err(DownloadError::Other("unreachable algo branch".to_string())),
        }
    })
    .await
    .map_err(|e| DownloadError::Other(format!("checksum thread panicked: {}", e)))??;

    if actual_hex != expected_hex {
        return Err(DownloadError::ChecksumMismatch(format!(
            "expected {}, got {}",
            expected_hex, actual_hex
        )));
    }
    Ok(())
}

/// Run the segment advisor to dynamically compute optimal segment count.
/// Updates `tasks.segments` in DB so that subsequent resumes skip the probe.
async fn compute_segments_with_advisor(p: &DownloadParams, info: &FileInfo) -> i32 {
    use crate::segment_advisor::{
        AdvisorInput, advise_static, advise_with_bandwidth, probe_bandwidth,
    };
    let advisor_input = AdvisorInput {
        total_bytes: info.total_bytes,
        supports_range: info.supports_range,
    };

    // Phase 1: static recommendation (file size + CPU cores).
    let static_advice = advise_static(&advisor_input);
    log_info!(
        "[download] task {} static advice: segments={}, reason={}",
        p.task_id,
        static_advice.segments,
        static_advice.reason
    );

    let result = if static_advice.segments > 1 {
        // Phase 2: bandwidth probe to refine the recommendation.
        //
        // Skip the probe when the task was started via a browser-extension
        // hint (hint_file_size > 0).  Those URLs may be one-time signed CDN
        // tokens; an extra Range request would consume the token and break the
        // actual download.  Static advice (file size + CPU cores) is a good
        // enough estimate in this case.
        if p.hint_file_size > 0 {
            log_info!(
                "[download] task {} hint mode: skipping bandwidth probe, using static advice (segments={})",
                p.task_id,
                static_advice.segments
            );
            static_advice.segments
        } else {
            match probe_bandwidth(
                &p.client,
                &p.url,
                info.supports_range,
                &p.cancel_token,
                &p.cookies,
                &p.referrer,
                &p.extra_headers,
            )
            .await
            {
                Some(bw) => {
                    let bw_advice = advise_with_bandwidth(&advisor_input, bw);
                    log_info!(
                        "[download] task {} bandwidth probe: {:.1} KB/s → segments={}, reason={}",
                        p.task_id,
                        bw / 1024.0,
                        bw_advice.segments,
                        bw_advice.reason
                    );
                    bw_advice.segments
                }
                None => {
                    log_info!(
                        "[download] task {} bandwidth probe failed/cancelled, using static advice",
                        p.task_id
                    );
                    static_advice.segments
                }
            }
        }
    } else {
        static_advice.segments
    };

    // Persist to DB so resume_task can skip the advisor.
    // If this write fails, the advisor will re-run on resume — acceptable.
    if let Err(e) = p.db.update_task_segments(&p.task_id, result).await {
        log_info!(
            "[download] task {} failed to persist segment count to DB: {}",
            p.task_id,
            e
        );
    }

    result
}

async fn run_download_inner(p: &DownloadParams) -> Result<i64, DownloadError> {
    log_info!("[download] task {} starting, url={}", p.task_id, p.url);

    // Transition to status=5 (preparing) — probing server, resolving file info
    let _ = p.db.update_task_status(&p.task_id, 5, "").await;
    let _ = p
        .progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: 0,
            total_bytes: 0,
            status: 5,
            error_message: String::new(),
            file_name: p.file_name.clone(),
            segment_details: None,
        })
        .await;

    let client = &p.client;

    // When the browser extension provides a file size hint, skip the probe
    // phase (HEAD + GET Range:0-0) entirely.  One-time CDN URLs (e.g.
    // Lanzou, ctbpsp.com signed URLs) treat every HTTP request as a download
    // attempt.  The probe would "consume" the URL token, leaving the actual
    // download to receive an HTML error page instead of the real file.
    //
    // hint_file_size semantics:
    //   > 0  — known file size from browser extension, skip probe
    //   -1   — size unknown but confirmed downloadable (webRequest sniffed),
    //          skip probe to preserve one-time tokens
    //    0   — no hint, run normal probe
    let info = if p.hint_file_size != 0 {
        let name = if p.file_name.is_empty() {
            // Hint mode skips the HEAD probe entirely, so we have no response
            // headers to extract the filename from.  Try the URL path first;
            // if that also yields nothing (e.g. "/download?token=abc") fall
            // back to "download" so we never end up with an empty dest_path
            // that would point at the save directory itself.
            extract_from_url(&p.url).unwrap_or_else(|| "download".to_string())
        } else {
            p.file_name.clone()
        };
        // When hint is -1 (unknown size), use 0 as total_bytes so the
        // downloader treats it as unknown-length and reads until EOF.
        let effective_size = if p.hint_file_size > 0 {
            p.hint_file_size
        } else {
            0
        };
        log_info!(
            "[download] task {} using hint: name={}, size={} (probe skipped, hint={})",
            p.task_id,
            name,
            effective_size,
            p.hint_file_size
        );
        FileInfo {
            file_name: name,
            total_bytes: effective_size,
            // Optimistically assume Range support for auto (0) and explicit
            // multi-segment (> 1) requests.  Most servers that expose a
            // Content-Length also honour Range headers.  The bandwidth probe
            // is intentionally skipped for hint-mode tasks (see below) so no
            // extra HTTP request is made that could consume a one-time CDN
            // token (e.g. Lanzou cloud signed URLs).
            // Only assume Range support when we have a real file size;
            // unknown-size downloads (-1 hint) fall back to single-stream.
            supports_range: p.hint_file_size > 0 && p.segment_count != 1,
            content_type: String::new(),
            // Hint mode skips the probe, so no ETag/Last-Modified available.
            etag: String::new(),
            last_modified: String::new(),
            // Hint mode skips the probe — no Content-Encoding info.
            content_encoding_compressed: false,
        }
    } else {
        log_info!("[download] task {} resolving file info...", p.task_id);
        let info =
            resolve_file_info(client, &p.url, &p.cookies, &p.referrer, &p.extra_headers).await?;
        log_info!(
            "[download] task {} resolved: name={}, size={}, range={}",
            p.task_id,
            info.file_name,
            info.total_bytes,
            info.supports_range
        );
        info
    };

    // Safety net: if the server returned HTML but the user expects a binary file,
    // the URL is likely a redirect/landing page (e.g. Lanzou CDN transit page).
    // Abort early instead of saving an HTML file with a wrong extension.
    if !info.content_type.is_empty() {
        let ct_lower = info.content_type.to_ascii_lowercase();
        let mime = ct_lower.split(';').next().unwrap_or("").trim();
        if mime == "text/html" || mime == "application/xhtml+xml" {
            let expected = if p.file_name.is_empty() {
                &info.file_name
            } else {
                &p.file_name
            };
            let looks_like_html = expected.ends_with(".html")
                || expected.ends_with(".htm")
                || expected.ends_with(".xhtml");
            if !looks_like_html {
                return Err(DownloadError::Other(format!(
                    "server returned HTML page (Content-Type: {}) instead of the expected file — \
                     the URL may be a redirect/transit page",
                    mime
                )));
            }
        }
    }

    // 早期取消检查：probe 完成后、创建文件之前检测 pause/delete，
    // 防止已取消的任务仍然在磁盘上创建临时文件。
    if p.cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    let auto_name = if p.file_name.is_empty() {
        info.file_name.clone()
    } else {
        p.file_name.clone()
    };

    let save_dir = PathBuf::from(&p.save_dir);

    // Safety net: if filename is still empty after all resolution attempts,
    // abort early with a clear error instead of silently using save_dir as
    // the destination path (which would cause an OS-level write error or
    // corrupt the directory).
    if auto_name.is_empty() {
        return Err(DownloadError::Other(
            "could not determine a file name for this download — \
             please retry and specify a file name manually"
                .to_string(),
        ));
    }

    // When resuming, the file on disk belongs to *this* task — skip dedup.
    // For new downloads, dedup to avoid overwriting unrelated files.
    // Pass the reserved_filenames_snapshot so that sibling tasks started in
    // the same batch are also excluded from the candidate set, preventing the
    // TOCTOU race where two concurrent tasks both see "no conflict" and choose
    // the same filename, then overwrite each other's .fdownloading temp file.
    let actual_name = if p.is_resume {
        auto_name.clone()
    } else {
        dedup_filename(&save_dir, &auto_name, &p.reserved_filenames_snapshot).await
    };

    // For resume tasks we must NOT blindly overwrite total_bytes with the
    // freshly-probed value.  CDN servers frequently return a slightly different
    // Content-Length on each request (transfer-encoding overhead, dynamic header
    // injection, signed-URL padding, …).  Even a 1-byte difference would cause
    // download_multi_segment to conclude "file changed → delete all segments →
    // restart from zero".
    //
    // update_task_file_info_resume() applies a tolerance threshold: only
    // updates total_bytes when the delta exceeds 1 % of the stored size (or
    // 1 MiB, whichever is smaller).  It returns the *effective* total_bytes
    // that callers must use so everything (segments, progress bar, size checks)
    // is consistent with a single source of truth.
    //
    // For new downloads we still use the plain overwrite (update_task_file_info)
    // because there is no prior state to protect.
    let mut effective_total_bytes = if p.is_resume {
        let (effective, updated) =
            p.db.update_task_file_info_resume(&p.task_id, &actual_name, info.total_bytes)
                .await?;
        if updated {
            log_info!(
                "[download] task {} resume: total_bytes updated {} → {} (genuine size change)",
                p.task_id,
                info.total_bytes, // probed value that was accepted
                effective
            );
        } else {
            log_info!(
                "[download] task {} resume: preserving stored total_bytes={} (probe={}, delta within tolerance)",
                p.task_id,
                effective,
                info.total_bytes
            );
        }
        effective
    } else {
        p.db.update_task_file_info(&p.task_id, &actual_name, info.total_bytes)
            .await?;
        info.total_bytes
    };

    // When resuming, also determine whether the server actually supports Range
    // requests.  The probe result is authoritative for new downloads, but for
    // resumes the probe may return supports_range=false for servers that only
    // advertise Accept-Ranges on the real GET (not HEAD).  If we have existing
    // segment rows in the DB the server clearly supported Range previously, so
    // trust that history and keep multi-segment mode.
    let effective_supports_range = if p.is_resume && !info.supports_range {
        let existing_segs = p.db.load_segments(&p.task_id).await.unwrap_or_default();
        if !existing_segs.is_empty() {
            log_info!(
                "[download] task {} resume: probe says no Range support but {} segment(s) exist in DB — \
                 trusting prior Range capability",
                p.task_id,
                existing_segs.len()
            );
            true
        } else {
            false
        }
    } else {
        info.supports_range
    };

    // 二次取消检查：缩小 DB 已更新但文件尚未创建的竞争窗口。
    if p.cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    let _ = p.db.update_task_status(&p.task_id, 1, "").await;

    // Immediately notify Dart: status=1 with resolved file name & total size.
    // For resume tasks, send persisted downloaded bytes as baseline so speed
    // smoothing doesn't treat resumed bytes as a fresh in-interval delta.
    let initial_downloaded = if p.is_resume {
        p.db.load_task_by_id(&p.task_id)
            .await
            .ok()
            .flatten()
            .map(|t| t.downloaded_bytes.max(0))
            .unwrap_or(0)
    } else {
        0
    };

    let _ = p
        .progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: initial_downloaded,
            total_bytes: effective_total_bytes,
            status: 1,
            error_message: String::new(),
            file_name: actual_name.clone(),
            segment_details: None,
        })
        .await;

    let mut dest_path = save_dir.join(&actual_name);
    // Chrome-style: write to a temporary file during download, rename on success.
    let temp_path = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));

    // Dynamic segment calculation when user chose "auto" (segment_count <= 0).
    let segments = if p.segment_count <= 0 {
        // When resuming, check if DB already has segment rows from a previous
        // run.  If so, reuse that count — avoids a redundant bandwidth probe
        // and guarantees segment definitions stay consistent with what's on disk.
        if p.is_resume {
            let existing = p.db.load_segments(&p.task_id).await.unwrap_or_default();
            if !existing.is_empty() {
                let n = existing.len() as i32;
                log_info!(
                    "[download] task {} resume: reusing {} existing segment(s) from DB",
                    p.task_id,
                    n
                );
                n
            } else {
                // Segment rows were lost (e.g. crash between tasks.segments
                // update and insert_segments).  Fall through to advisor.
                compute_segments_with_advisor(p, &info).await
            }
        } else {
            compute_segments_with_advisor(p, &info).await
        }
    } else {
        p.segment_count
    };

    // Use multi-segment only when the server supports Range,
    // file is > 1 MB, and we asked for more than 1 segment.
    let use_segments =
        effective_supports_range && effective_total_bytes > 1_048_576 && segments > 1;

    log_info!(
        "[download] task {} mode={}, segments={}, temp={}, dest={}",
        p.task_id,
        if use_segments {
            "multi-segment"
        } else {
            "single"
        },
        segments,
        temp_path.display(),
        dest_path.display()
    );

    // Tracks whether we actually used multi-segment for the integrity check
    // below.  Flipped to false when the server doesn't support Range requests
    // and we auto-fall back to single-stream within this attempt.
    let mut actual_use_segments = use_segments;
    let single_result: Option<SingleDownloadResult> = if use_segments {
        match download_multi_segment(
            &p.task_id,
            &p.url,
            &temp_path,
            effective_total_bytes,
            segments,
            client,
            &p.db,
            &p.progress_tx,
            &p.cancel_token,
            &p.speed_limiter,
            &p.cookies,
            &p.referrer,
            &p.extra_headers,
            &info.etag,
            &info.last_modified,
        )
        .await
        {
            Ok(()) => None,
            Err(DownloadError::RangeNotSupported(status)) => {
                // The server ignored the Range header (returned `status` instead
                // of 206).  Observed with FnOS NAS `multiple-download?token=…`
                // endpoints and some other local servers that accept Range
                // syntax but always reply 200 + full content.
                //
                // Auto-fall back to single-stream so the download succeeds on
                // this attempt without requiring a manual retry.  The host was
                // already recorded in the single-conn cache by do_segment, so
                // future tasks for the same host start single-stream from the
                // very beginning.
                log_info!(
                    "[download] task {} Range not supported (server returned {}); \
                     auto-falling back to single-stream",
                    p.task_id,
                    status
                );
                actual_use_segments = false;
                // Remove stale segment rows that belong to the failed
                // multi-segment attempt; they would confuse a future resume.
                let _ = p.db.delete_segments(&p.task_id).await;
                // Remove the pre-allocated temp file.  Workers may have written
                // data at incorrect offsets (full-file content at each segment's
                // start position), so the file content is corrupt.  Starting
                // clean is the only safe option.
                let _ = tokio::fs::remove_file(&temp_path).await;
                let result = download_single(
                    &p.task_id,
                    &p.url,
                    &temp_path,
                    effective_total_bytes,
                    false, // server doesn't support Range — never attempt it
                    client,
                    &p.db,
                    &p.progress_tx,
                    &p.cancel_token,
                    &p.speed_limiter,
                    &p.cookies,
                    &p.referrer,
                    &p.extra_headers,
                )
                .await?;
                Some(result)
            }
            Err(e) => return Err(e),
        }
    } else {
        let result = download_single(
            &p.task_id,
            &p.url,
            &temp_path,
            effective_total_bytes,
            effective_supports_range,
            client,
            &p.db,
            &p.progress_tx,
            &p.cancel_token,
            &p.speed_limiter,
            &p.cookies,
            &p.referrer,
            &p.extra_headers,
        )
        .await?;
        Some(result)
    };

    // Integrity check — verify download completeness.
    if effective_total_bytes > 0 {
        if actual_use_segments {
            // Multi-segment: file is pre-allocated via set_len() so metadata
            // size always == total_bytes.  Check actual progress from DB instead.
            let segs = p.db.load_segments(&p.task_id).await?;
            let seg_total: i64 = segs.iter().map(|s| s.downloaded_bytes).sum();
            if seg_total < effective_total_bytes {
                return Err(DownloadError::Other(format!(
                    "segment integrity failed: expected {} bytes, segments downloaded {} bytes",
                    effective_total_bytes, seg_total
                )));
            }
            // Also verify actual file size on disk (guards against external
            // file deletion/truncation between download and this check).
            let file_len = tokio::fs::metadata(&temp_path)
                .await
                .map(|m| m.len() as i64)
                .unwrap_or(0);
            if file_len < effective_total_bytes {
                return Err(DownloadError::Other(format!(
                    "file integrity failed: disk size={} bytes, expected {} bytes",
                    file_len, effective_total_bytes
                )));
            }
        } else {
            // Single-thread: no pre-allocation, file size == downloaded bytes.
            let meta = tokio::fs::metadata(&temp_path).await?;
            let file_len = meta.len() as i64;
            if file_len != effective_total_bytes {
                // The stream ended normally — check whether the response had
                // its own Content-Length that matches the actual file.  This
                // handles servers (e.g. CNKI) where the browser-extension hint
                // size differs from what the server actually delivers to
                // FluxDown's own request (dynamic tokens, re-generated PDFs,
                // slight header drift, etc.).
                let resp_cl = single_result
                    .as_ref()
                    .map(|r| r.response_content_length)
                    .unwrap_or(-1);
                if resp_cl > 0 && file_len == resp_cl {
                    log_info!(
                        "[download] task {} size drift accepted: hint={} bytes, \
                         response content-length={}, file={} (stream completed normally)",
                        p.task_id,
                        effective_total_bytes,
                        resp_cl,
                        file_len
                    );
                    // Update DB so the stored total_bytes reflects reality.
                    let _ = p.db.update_task_total_bytes(&p.task_id, file_len).await;
                    effective_total_bytes = file_len;
                } else if resp_cl <= 0 && file_len > 0 {
                    // Server didn't send Content-Length (chunked transfer) but
                    // the stream ended cleanly.  Trust the actual file.
                    log_info!(
                        "[download] task {} no response content-length, trusting \
                         actual file size: hint={}, file={} (stream completed normally)",
                        p.task_id,
                        effective_total_bytes,
                        file_len
                    );
                    let _ = p.db.update_task_total_bytes(&p.task_id, file_len).await;
                    effective_total_bytes = file_len;
                } else {
                    return Err(DownloadError::Other(format!(
                        "size mismatch: expected {} bytes, got {} bytes \
                         (response content-length={})",
                        effective_total_bytes, file_len, resp_cl
                    )));
                }
            }
        }
    }

    // Determine the actual downloaded size.  When the server didn't report
    // Content-Length (total_bytes == 0), read the real file size from disk so
    // that the completion signal carries accurate byte counts.
    let actual_total = if effective_total_bytes > 0 {
        effective_total_bytes // may have been corrected by size-drift logic above
    } else {
        match tokio::fs::metadata(&temp_path).await {
            Ok(m) => m.len() as i64,
            Err(e) => {
                log_info!(
                    "[download] task {} warning: cannot read temp file size: {}",
                    p.task_id,
                    e
                );
                0
            }
        }
    };

    // If a better file name was discovered during download (e.g. the actual
    // GET response carried a richer Content-Disposition than the initial probe
    // hint), download_single / do_segment will have already written it to the
    // DB via update_task_file_name.  Re-read it here and redirect dest_path so
    // the final rename lands on the correct file name.
    //
    // Without this step the task's in-memory / Dart-side name (updated via
    // ProgressUpdate) diverges from the on-disk file name, which stays at the
    // original hint-based name forever.
    {
        let db_file_name =
            p.db.load_task_by_id(&p.task_id)
                .await
                .ok()
                .flatten()
                .map(|t| t.file_name)
                .unwrap_or_default();

        if !db_file_name.is_empty() && db_file_name != actual_name {
            // Dedup in case the better name collides with an existing file that
            // was created between probe and now.
            let deduped =
                dedup_filename(&save_dir, &db_file_name, &p.reserved_filenames_snapshot).await;
            if deduped != db_file_name {
                // Dedup changed the name — keep DB and reality in sync.
                let _ = p.db.update_task_file_name(&p.task_id, &deduped).await;
            }
            log_info!(
                "[download] task {} better name applied: {} → {}",
                p.task_id,
                actual_name,
                deduped
            );
            dest_path = save_dir.join(&deduped);
        }
    }

    // Checksum verification — runs after size integrity check, before rename.
    if !p.checksum.is_empty() {
        log_info!(
            "[download] task {} verifying checksum: {}",
            p.task_id,
            p.checksum
        );
        verify_checksum(&temp_path, &p.checksum).await?;
        log_info!("[download] task {} checksum ok", p.task_id);
    }

    // All data verified — rename temp file to final destination.
    // This is the atomic moment the file "appears" as complete.
    tokio::fs::rename(&temp_path, &dest_path)
        .await
        .map_err(|e| {
            DownloadError::Other(format!(
                "failed to rename {} → {}: {}",
                temp_path.display(),
                dest_path.display(),
                e
            ))
        })?;

    log_info!(
        "[download] task {} renamed {} → {}",
        p.task_id,
        temp_path.display(),
        dest_path.display()
    );

    Ok(actual_total)
}

// ---------------------------------------------------------------------------
// Single-thread download (with resume support)
// ---------------------------------------------------------------------------

/// Result of a single-thread download, carrying response metadata for the
/// caller's integrity check.
struct SingleDownloadResult {
    /// The `Content-Length` header value from the server's actual response.
    /// -1 when the header was absent (e.g. chunked transfer).
    response_content_length: i64,
}

#[allow(clippy::too_many_arguments)]
async fn download_single(
    task_id: &str,
    url: &str,
    dest: &Path,
    total_bytes: i64,
    supports_range: bool,
    client: &Client,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    cookies: &str,
    referrer: &str,
    extra_headers: &std::collections::HashMap<String, String>,
) -> Result<SingleDownloadResult, DownloadError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Check if there's an existing partial file we can resume
    let existing_len = match tokio::fs::metadata(dest).await {
        Ok(m) => m.len() as i64,
        Err(_) => 0,
    };

    // Attempt a Range resume when:
    //   • The caller says the server supports Range (from probe or from DB history), AND
    //   • We have a non-empty partial file on disk, AND
    //   • The partial file is smaller than the known total (or total is unknown)
    let want_resume =
        supports_range && existing_len > 0 && (total_bytes == 0 || existing_len < total_bytes);

    let mut downloaded: i64;
    let mut file;

    let mut req = client.get(url);
    if !cookies.is_empty() {
        req = req.header("Cookie", cookies);
    }
    if !referrer.is_empty() {
        req = req.header(reqwest::header::REFERER, referrer);
    }
    req = apply_extra_headers(req, extra_headers);
    if want_resume {
        req = req.header("Range", format!("bytes={}-", existing_len));
    }

    let resp = req.send().await?.error_for_status()?;

    // Detect compressed responses — we now decompress on-the-fly instead of
    // rejecting.  When decompression is active, total_bytes from the probe is
    // the *compressed* size, not the decompressed size, so we must treat it
    // as unknown for progress reporting and skip the final size integrity check.
    let encoding = detect_content_encoding(resp.headers());
    if encoding.is_some() {
        log_info!(
            "[download-single] task {} server returned Content-Encoding: {:?} — \
             decompressing on-the-fly",
            task_id,
            encoding
        );
    }

    // Verify the server actually honoured the Range request.
    // Some servers (or CDN edge nodes) silently ignore Range and return 200 OK
    // with the full file.  If we appended to the partial file in that case we
    // would produce a corrupt result.  Detect this and fall back to a clean
    // full download.
    //
    // HTTP 206 Partial Content  → server honoured Range → safe to append
    // HTTP 200 OK               → server ignored Range  → must restart from 0
    // Any other 2xx             → treat as non-resumable for safety
    let actual_resume = want_resume && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;

    if want_resume && !actual_resume {
        log_info!(
            "[download-single] task {} server returned {} instead of 206; \
             falling back to full download (existing_len={} discarded)",
            task_id,
            resp.status(),
            existing_len
        );
    }

    // Capture the response's own Content-Length before consuming the body.
    // For resumed downloads (206), this is the *remaining* length, not total.
    // For full downloads (200), this is the complete file size.
    let response_content_length: i64 = if actual_resume {
        // For 206 responses, the full size is existing_len + Content-Length.
        resp.content_length()
            .map(|cl| existing_len + cl as i64)
            .unwrap_or(-1)
    } else {
        resp.content_length().map(|cl| cl as i64).unwrap_or(-1)
    };

    if actual_resume {
        downloaded = existing_len;
        let mut raw_file = OpenOptions::new().write(true).open(dest).await?;
        raw_file.seek(std::io::SeekFrom::End(0)).await?;
        file = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, raw_file);
    } else {
        downloaded = 0;
        file = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, File::create(dest).await?);
        // Reset DB progress so the UI doesn't show stale values
        let _ = db.update_task_progress(task_id, 0).await;
    }

    // Try extracting a better filename from the actual download response.
    // This is the ultimate fallback — the real GET may have Content-Disposition
    // even when the probe HEAD/GET-Range:0-0 didn't.
    let resp_name = extract_filename(resp.headers(), resp.url().as_str());
    if !resp_name.is_empty()
        && resp_name != "download"
        && resp
            .headers()
            .contains_key(reqwest::header::CONTENT_DISPOSITION)
    {
        log_info!(
            "[download-single] got better name from response: {}",
            resp_name
        );
        // Persist immediately so run_download_inner can redirect dest_path
        // to this name before the final .fdownloading → real-name rename.
        let _ = db.update_task_file_name(task_id, &resp_name).await;
        let _ = progress_tx
            .send(ProgressUpdate {
                task_id: task_id.to_string(),
                downloaded_bytes: downloaded,
                total_bytes,
                status: 1,
                error_message: String::new(),
                file_name: resp_name,
                segment_details: Some(vec![SegmentProgressInfo {
                    index: 0,
                    start_byte: 0,
                    end_byte: if total_bytes > 0 { total_bytes - 1 } else { 0 },
                    downloaded_bytes: downloaded,
                }]),
            })
            .await;
    }

    // Wrap with decompression if needed.  The stream now yields
    // Result<Bytes, io::Error> regardless of whether decompression is active.
    let raw_stream = resp.bytes_stream();
    let mut stream = maybe_decompress_stream(raw_stream, encoding);

    // When decompression is active, the probe's total_bytes is the *compressed*
    // size — the actual decompressed bytes written to disk will differ.
    // Treat size as unknown so progress reports don't show wrong percentages
    // and the final integrity check is skipped.
    let total_bytes = if encoding.is_some() { 0 } else { total_bytes };

    let mut last_report = std::time::Instant::now();
    let mut last_db_save = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                file.flush().await?;
                let _ = db.update_task_progress(task_id, downloaded).await;
                return Err(DownloadError::Cancelled);
            }
            result = tokio::time::timeout(CHUNK_STALL_TIMEOUT, stream.next()) => {
                // Unwrap the timeout layer first.  If no chunk arrived within
                // CHUNK_STALL_TIMEOUT the TCP connection is likely dead — flush
                // partial progress and bubble up an error.  For single-thread
                // downloads the task will enter error state; the user can resume
                // and a fresh Range request will pick up from saved progress.
                let chunk = match result {
                    Ok(c) => c,
                    Err(_) => {
                        file.flush().await?;
                        let _ = db.update_task_progress(task_id, downloaded).await;
                        return Err(DownloadError::Other(format!(
                            "download stalled: no data received for {}s",
                            CHUNK_STALL_TIMEOUT.as_secs()
                        )));
                    }
                };
                match chunk {
                    Some(Ok(bytes)) => {
                        // --- Speed limiter: write in sub-chunks as tokens allow ---
                        let mut offset = 0usize;
                        let chunk_len = bytes.len();
                        while offset < chunk_len {
                            let remaining = (chunk_len - offset) as u64;
                            let allowed = speed_limiter.consume(remaining).await;
                            let end = offset + allowed as usize;
                            file.write_all(&bytes[offset..end]).await?;
                            offset = end;
                        }
                        let len = chunk_len as i64;
                        downloaded += len;

                        // Progress report to Dart — every 200ms for smooth UI.
                        if last_report.elapsed().as_millis() >= 200 {
                            let _ = progress_tx
                                .send(ProgressUpdate {
                                    task_id: task_id.to_string(),
                                    downloaded_bytes: downloaded,
                                    total_bytes,
                                    status: 1,
                                    error_message: String::new(),
                                    file_name: String::new(),
                                    segment_details: Some(vec![SegmentProgressInfo {
                                        index: 0,
                                        start_byte: 0,
                                        end_byte: if total_bytes > 0 { total_bytes - 1 } else { 0 },
                                        downloaded_bytes: downloaded,
                                    }]),
                                })
                                .await;
                            last_report = std::time::Instant::now();
                        }

                        // DB persistence — periodic save for crash recovery.
                        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                            let _ = db.update_task_progress(task_id, downloaded).await;
                            last_db_save = std::time::Instant::now();
                        }
                    }
                    Some(Err(e)) => {
                        file.flush().await?;
                        let _ = db.update_task_progress(task_id, downloaded).await;
                        return Err(DownloadError::Io(e));
                    }
                    None => break,
                }
            }
        }
    }

    file.flush().await?;
    let _ = db.update_task_progress(task_id, downloaded).await;
    Ok(SingleDownloadResult {
        response_content_length,
    })
}

// ---------------------------------------------------------------------------
// Multi-segment download (delegates to SegmentCoordinator)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn download_multi_segment(
    task_id: &str,
    url: &str,
    dest: &Path,
    total_bytes: i64,
    segment_count: i32,
    client: &Client,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    cookies: &str,
    referrer: &str,
    extra_headers: &std::collections::HashMap<String, String>,
    etag: &str,
    last_modified: &str,
) -> Result<(), DownloadError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // NOTE: total_bytes arriving here is already the *effective* value returned
    // by update_task_file_info_resume — it is consistent with the stored segment
    // boundaries (small CDN drift has been filtered out).  The coordinator's own
    // effective_total_bytes logic (db_total vs probe) provides a second layer of
    // protection.  The pre-check below is therefore intentionally removed: it
    // compared the raw probed total_bytes against segment end_byte, which caused
    // false positives (CDN rounding) that silently wiped all progress.
    //
    // The coordinator itself handles the two genuine cases:
    //   • db_total <= probe_total  → trust DB segments, correct tasks.total_bytes
    //   • db_total >  probe_total  → file genuinely shrank, rebuild segments

    // Delegate to the IDM-style dynamic segment coordinator.
    crate::segment_coordinator::run_coordinated_download(
        task_id,
        url,
        dest,
        total_bytes,
        segment_count,
        client,
        db,
        progress_tx,
        cancel_token,
        speed_limiter,
        cookies,
        referrer,
        extra_headers,
        etag,
        last_modified,
    )
    .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        PROBE_MAX_RETRIES, PROBE_RETRY_BASE_DELAY, PROBE_TIMEOUT, TEMP_EXT, dedup_filename,
        extract_filename, extract_from_content_disposition, extract_from_url, mime_to_ext,
        sanitize_filename, urlencoding_decode,
    };
    use std::time::Duration;

    // -----------------------------------------------------------------------
    // sanitize_filename
    // -----------------------------------------------------------------------

    #[test]
    fn sanitize_replaces_illegal_chars() {
        assert_eq!(sanitize_filename("file<1>:2.txt"), "file_1__2.txt");
    }

    #[test]
    fn sanitize_replaces_all_special_chars() {
        assert_eq!(
            sanitize_filename(r#"a<b>c:d"e/f\g|h?i*j"#),
            "a_b_c_d_e_f_g_h_i_j"
        );
    }

    #[test]
    fn sanitize_strips_leading_trailing_dots_and_spaces() {
        assert_eq!(sanitize_filename("...file..."), "file");
        assert_eq!(sanitize_filename("  file  "), "file");
        assert_eq!(sanitize_filename("..file.."), "file");
    }

    #[test]
    fn sanitize_empty_and_only_dots() {
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename("..."), "download");
        assert_eq!(sanitize_filename("   "), "download");
    }

    #[test]
    fn sanitize_control_characters() {
        assert_eq!(sanitize_filename("file\x00name\x1F.txt"), "file_name_.txt");
    }

    #[test]
    fn sanitize_preserves_unicode() {
        assert_eq!(sanitize_filename("文件下载.zip"), "文件下载.zip");
        assert_eq!(sanitize_filename("ファイル.tar.gz"), "ファイル.tar.gz");
    }

    // -----------------------------------------------------------------------
    // extract_from_url
    // -----------------------------------------------------------------------

    #[test]
    fn extract_from_url_basic() {
        let name = extract_from_url("https://example.com/path/file.zip");
        assert_eq!(name.as_deref(), Some("file.zip"));
    }

    #[test]
    fn extract_from_url_strips_query_and_fragment() {
        let name = extract_from_url("https://example.com/file.zip?v=1&token=abc#section");
        assert_eq!(name.as_deref(), Some("file.zip"));
    }

    #[test]
    fn extract_from_url_encoded_filename() {
        let name = extract_from_url("https://example.com/My%20File%20(1).pdf");
        assert_eq!(name.as_deref(), Some("My File (1).pdf"));
    }

    #[test]
    fn extract_from_url_trailing_slash_returns_none() {
        let name = extract_from_url("https://example.com/path/");
        assert!(
            name.is_none(),
            "trailing slash should return None, got: {name:?}"
        );
    }

    #[test]
    fn extract_from_url_no_path() {
        let name = extract_from_url("https://example.com");
        // The last segment is "example.com" — should extract it
        assert!(name.is_some());
    }

    #[test]
    fn extract_from_url_chinese_filename() {
        let name = extract_from_url("https://example.com/%E4%B8%8B%E8%BD%BD.exe");
        assert_eq!(name.as_deref(), Some("下载.exe"));
    }

    // -----------------------------------------------------------------------
    // urlencoding_decode
    // -----------------------------------------------------------------------

    #[test]
    fn urlencoding_decode_basic() {
        assert_eq!(
            urlencoding_decode("hello%20world").unwrap_or_default(),
            "hello world"
        );
    }

    #[test]
    fn urlencoding_decode_plus_to_space() {
        assert_eq!(
            urlencoding_decode("hello+world").unwrap_or_default(),
            "hello world"
        );
    }

    #[test]
    fn urlencoding_decode_invalid_utf8_returns_error() {
        // 0x80 alone is not valid UTF-8
        let result = urlencoding_decode("%80");
        assert!(result.is_err(), "invalid UTF-8 should return Err");
    }

    #[test]
    fn urlencoding_decode_partial_percent() {
        // "%" at end should pass through
        let result = urlencoding_decode("test%").unwrap_or_default();
        assert_eq!(result, "test%");
    }

    // -----------------------------------------------------------------------
    // extract_from_content_disposition (private, tested via extract_filename)
    // -----------------------------------------------------------------------

    fn make_headers_with_cd(value: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str(value) {
            headers.insert(reqwest::header::CONTENT_DISPOSITION, v);
        }
        headers
    }

    #[test]
    fn content_disposition_quoted_filename() {
        let headers = make_headers_with_cd("attachment; filename=\"my_file.zip\"");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("my_file.zip"));
    }

    #[test]
    fn content_disposition_unquoted_filename() {
        let headers = make_headers_with_cd("attachment; filename=simple.txt");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("simple.txt"));
    }

    #[test]
    fn content_disposition_rfc5987_filename_star() {
        let headers = make_headers_with_cd("attachment; filename*=UTF-8''%E6%96%87%E4%BB%B6.pdf");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("文件.pdf"));
    }

    #[test]
    fn content_disposition_filename_star_overrides_plain() {
        let headers = make_headers_with_cd(
            "attachment; filename=\"fallback.txt\"; filename*=UTF-8''preferred.txt",
        );
        let name = extract_from_content_disposition(&headers);
        // filename* should take precedence
        assert_eq!(name.as_deref(), Some("preferred.txt"));
    }

    #[test]
    fn content_disposition_empty_filename() {
        let headers = make_headers_with_cd("attachment; filename=\"\"");
        let name = extract_from_content_disposition(&headers);
        assert!(name.is_none(), "empty filename should return None");
    }

    #[test]
    fn content_disposition_no_filename_param() {
        let headers = make_headers_with_cd("inline");
        let name = extract_from_content_disposition(&headers);
        assert!(name.is_none());
    }

    #[test]
    fn content_disposition_percent_encoded_filename_unquoted() {
        // Chinese cloud storage (OBS/S3) often sends percent-encoded filename=
        // instead of using the RFC 5987 filename*= syntax.
        let headers = make_headers_with_cd(
            "attachment;filename=%E6%B0%B8%E7%94%9F%E6%88%98%E5%A3%AB.Sisu.2022265.mp4",
        );
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("永生战士.Sisu.2022265.mp4"));
    }

    #[test]
    fn content_disposition_percent_encoded_filename_quoted() {
        let headers = make_headers_with_cd(
            "attachment; filename=\"%E6%B0%B8%E7%94%9F%E6%88%98%E5%A3%AB.Sisu.2022265.mp4\"",
        );
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("永生战士.Sisu.2022265.mp4"));
    }

    #[test]
    fn content_disposition_plain_ascii_with_percent_literal() {
        // A filename like "50%.txt" should NOT be mangled by the heuristic
        // because urlencoding_decode("50%.txt") will fail or leave it unchanged.
        let headers = make_headers_with_cd("attachment; filename=\"50%.txt\"");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("50%.txt"));
    }

    #[test]
    fn content_disposition_percent_encoded_spaces() {
        let headers = make_headers_with_cd("attachment; filename=My%20Great%20File.pdf");
        let name = extract_from_content_disposition(&headers);
        assert_eq!(name.as_deref(), Some("My Great File.pdf"));
    }

    // -----------------------------------------------------------------------
    // extract_filename (integration of all strategies)
    // -----------------------------------------------------------------------

    #[test]
    fn extract_filename_prefers_content_disposition() {
        let headers = make_headers_with_cd("attachment; filename=\"from_header.zip\"");
        let name = extract_filename(&headers, "https://example.com/from_url.tar.gz");
        assert_eq!(name, "from_header.zip");
    }

    #[test]
    fn extract_filename_falls_back_to_url() {
        let headers = reqwest::header::HeaderMap::new();
        let name = extract_filename(&headers, "https://example.com/from_url.tar.gz");
        assert_eq!(name, "from_url.tar.gz");
    }

    #[test]
    fn extract_filename_falls_back_to_mime() {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str("application/pdf") {
            headers.insert(reqwest::header::CONTENT_TYPE, v);
        }
        let name = extract_filename(&headers, "https://example.com/");
        assert_eq!(name, "download.pdf");
    }

    #[test]
    fn extract_filename_ultimate_fallback() {
        let headers = reqwest::header::HeaderMap::new();
        let name = extract_filename(&headers, "https://example.com/");
        assert_eq!(name, "download");
    }

    // -----------------------------------------------------------------------
    // mime_to_ext
    // -----------------------------------------------------------------------

    #[test]
    fn mime_to_ext_common_types() {
        assert_eq!(mime_to_ext("application/pdf"), Some("pdf"));
        assert_eq!(mime_to_ext("application/zip"), Some("zip"));
        assert_eq!(mime_to_ext("video/mp4"), Some("mp4"));
        assert_eq!(mime_to_ext("image/jpeg"), Some("jpg"));
    }

    #[test]
    fn mime_to_ext_with_charset_parameter() {
        // MIME type often comes with ";charset=utf-8"
        assert_eq!(mime_to_ext("text/html; charset=utf-8"), Some("html"));
    }

    #[test]
    fn mime_to_ext_unknown_type() {
        assert_eq!(mime_to_ext("application/x-unknown-format"), None);
    }

    // -----------------------------------------------------------------------
    // Bug #4: PROBE_TIMEOUT configuration — document current problematic values
    // -----------------------------------------------------------------------

    #[test]
    fn http_probe_timeout_is_reasonable() {
        // 3 attempts: original → normal retry → UA-downgrade retry.
        // HEAD+GET run concurrently (max 15s per attempt, not 30s).
        assert_eq!(PROBE_TIMEOUT, Duration::from_secs(15));
        assert_eq!(PROBE_MAX_RETRIES, 3);
        assert_eq!(PROBE_RETRY_BASE_DELAY, Duration::from_secs(1));

        // Worst case: 3 attempts × 15s + delays (1s + 2s) = 48s
        let worst_per_attempt = PROBE_TIMEOUT; // HEAD+GET concurrent
        let delay_sum = PROBE_RETRY_BASE_DELAY + PROBE_RETRY_BASE_DELAY * 2; // 1s + 2s
        let worst_total = worst_per_attempt * PROBE_MAX_RETRIES + delay_sum;
        assert!(
            worst_total <= Duration::from_secs(60),
            "worst-case probe time {worst_total:?} should be <= 60s"
        );
    }

    // -----------------------------------------------------------------------
    // Bug #5: HEAD and GET are serial — measure by counting sequential phases
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_file_info_merges_head_and_get_results() {
        // After fix: HEAD+GET run concurrently via tokio::join!.
        // The merge logic still applies:
        // - If HEAD has Content-Disposition, use it.
        // - If HEAD lacks Content-Disposition, merge from GET.
        // Verify the merge condition logic is correct:
        let headers = reqwest::header::HeaderMap::new();
        let has_cd = headers.contains_key(reqwest::header::CONTENT_DISPOSITION);
        assert!(
            !has_cd,
            "empty headers should not have Content-Disposition — GET data will be merged"
        );

        // With Content-Disposition present, no merge needed
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str("attachment; filename=\"test.zip\"") {
            headers.insert(reqwest::header::CONTENT_DISPOSITION, v);
        }
        let has_cd = headers.contains_key(reqwest::header::CONTENT_DISPOSITION);
        assert!(
            has_cd,
            "Content-Disposition present — no need to merge from GET"
        );
    }

    // -----------------------------------------------------------------------
    // dedup_filename
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn dedup_filename_no_conflict() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_no_conflict");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // Clean up any leftover
        let _ = tokio::fs::remove_file(dir.join("test.txt")).await;
        let _ = tokio::fs::remove_file(dir.join(format!("test.txt{TEMP_EXT}"))).await;

        let result = dedup_filename(&dir, "test.txt", &std::collections::HashSet::new()).await;
        assert_eq!(result, "test.txt");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_with_conflict() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_conflict");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // Create conflicting file
        tokio::fs::write(dir.join("test.txt"), b"")
            .await
            .unwrap_or(());

        let result = dedup_filename(&dir, "test.txt", &std::collections::HashSet::new()).await;
        assert_eq!(result, "test (1).txt");

        // Clean up
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_temp_file_conflict() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_temp");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // Create a .fdownloading temp file — should also be considered a conflict
        tokio::fs::write(dir.join(format!("test.txt{TEMP_EXT}")), b"")
            .await
            .unwrap_or(());

        let result = dedup_filename(&dir, "test.txt", &std::collections::HashSet::new()).await;
        assert_eq!(result, "test (1).txt");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_no_extension() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_noext");
        let _ = tokio::fs::create_dir_all(&dir).await;
        tokio::fs::write(dir.join("README"), b"")
            .await
            .unwrap_or(());

        let result = dedup_filename(&dir, "README", &std::collections::HashSet::new()).await;
        assert_eq!(result, "README (1)");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // -----------------------------------------------------------------------
    // dedup_filename: reserved set prevents TOCTOU races in batch downloads
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn dedup_filename_reserved_set_avoids_collision() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_reserved");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // No file exists on disk, but the temp path is already reserved
        // by a sibling task (simulating a batch download in progress).
        let reserved_temp = dir.join(format!("video.mp4{TEMP_EXT}"));
        let mut reserved = std::collections::HashSet::new();
        reserved.insert(reserved_temp.clone());

        // Should NOT return "video.mp4" because its .fdownloading path is reserved.
        let result = dedup_filename(&dir, "video.mp4", &reserved).await;
        assert_eq!(result, "video (1).mp4");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_reserved_set_phase2_collision() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_reserved_p2");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // video.mp4 exists on disk AND video (1).mp4.fdownloading is reserved.
        tokio::fs::write(dir.join("video.mp4"), b"")
            .await
            .unwrap_or(());
        let reserved_temp1 = dir.join(format!("video (1).mp4{TEMP_EXT}"));
        let mut reserved = std::collections::HashSet::new();
        reserved.insert(reserved_temp1);

        // "video.mp4" conflicts (on disk), "video (1).mp4" conflicts (reserved),
        // so should fall through to "video (2).mp4".
        let result = dedup_filename(&dir, "video.mp4", &reserved).await;
        assert_eq!(result, "video (2).mp4");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // -----------------------------------------------------------------------
    // Bug: HeaderValue::to_str() rejects raw UTF-8 bytes (non-ASCII)
    // z-lib CDN sends Content-Disposition with unencoded Chinese chars in
    // filename="", causing the current disposition.to_str().ok()? to silently
    // return None and lose the filename entirely.
    // Fix: use std::str::from_utf8(hv.as_bytes()) which accepts any valid UTF-8.
    // -----------------------------------------------------------------------

    #[test]
    fn header_value_to_str_fails_for_raw_utf8_chinese() {
        // z-lib CDN sends:  filename="三体 (刘慈欣).epub"  as raw UTF-8 bytes
        // 三体  = \xe4\xb8\x89\xe4\xbd\x93
        // 刘慈欣 = \xe5\x88\x98\xe6\x85\x88\xe6\xac\xa3
        let raw: &[u8] = b"attachment; filename=\"\xe4\xb8\x89\xe4\xbd\x93 (\xe5\x88\x98\xe6\x85\x88\xe6\xac\xa3).epub\"; filename*=UTF-8''%E4%B8%89%E4%BD%93%20(%E5%88%98%E6%85%88%E6%AC%A3).epub";

        // reqwest / http crate accepts arbitrary bytes in HeaderValue::from_bytes.
        let hv = reqwest::header::HeaderValue::from_bytes(raw)
            .expect("HeaderValue::from_bytes must accept arbitrary bytes");

        // to_str() requires every byte to be visible ASCII (0x20-0x7E).
        // Chinese UTF-8 bytes are > 0x7E, so this MUST return Err.
        let to_str_result = hv.to_str();
        assert!(
            to_str_result.is_err(),
            "to_str() should fail for headers containing raw non-ASCII UTF-8 bytes"
        );

        // std::str::from_utf8 only requires valid UTF-8, so it MUST succeed.
        let from_utf8_result = std::str::from_utf8(hv.as_bytes());
        assert!(
            from_utf8_result.is_ok(),
            "from_utf8() should succeed for valid UTF-8 bytes; got Err({:?})",
            from_utf8_result.err()
        );

        let value = from_utf8_result.unwrap();
        // The decoded string must contain the Chinese characters.
        assert!(
            value.contains('\u{4e09}'), // first char of 三
            "decoded header must contain Chinese chars from 三体; got: {value:?}"
        );
        assert!(
            value.contains("filename*="),
            "decoded header must still contain filename*= parameter; got: {value:?}"
        );
        // Prove the raw bytes really are non-ASCII (>0x7E)
        assert!(
            raw.iter().any(|&b| b > 0x7e),
            "test data must contain non-ASCII bytes"
        );
    }

    #[test]
    fn content_disposition_raw_utf8_chinese_filename_extracted_correctly() {
        // Regression test for z-lib CDN: server sends raw UTF-8 bytes in filename="".
        // Before the fix (to_str) this returned None and callers fell back to the URL,
        // producing garbage like "redirection" or a hash string as the task name.
        // After the fix (from_utf8) the correct Chinese filename is extracted via
        // the filename*= parameter (RFC 5987 percent-encoding).
        let raw: &[u8] = b"attachment; filename=\"\xe4\xb8\x89\xe4\xbd\x93 (\xe5\x88\x98\xe6\x85\x88\xe6\xac\xa3).epub\"; filename*=UTF-8''%E4%B8%89%E4%BD%93%20(%E5%88%98%E6%85%88%E6%AC%A3).epub";

        let mut headers = reqwest::header::HeaderMap::new();
        let hv = reqwest::header::HeaderValue::from_bytes(raw)
            .expect("HeaderValue::from_bytes must accept arbitrary bytes");
        headers.insert(reqwest::header::CONTENT_DISPOSITION, hv);

        let name = extract_from_content_disposition(&headers);
        // filename*= (RFC 5987) takes priority and decodes to the correct Chinese name.
        assert_eq!(
            name.as_deref(),
            Some("三体 (刘慈欣).epub"),
            "raw UTF-8 bytes in filename= must not prevent filename*= from being parsed"
        );
    }

    // -----------------------------------------------------------------------
    // apply_extra_headers
    // -----------------------------------------------------------------------

    #[test]
    fn apply_extra_headers_adds_authorization() {
        use std::collections::HashMap;
        let client = reqwest::Client::new();
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer token123".to_string());
        let req = client.get("https://example.com/file.zip");
        let req = super::apply_extra_headers(req, &headers);
        // 构建请求并验证 header 已正确添加
        let built = req.build().unwrap();
        assert_eq!(
            built
                .headers()
                .get("Authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer token123"
        );
    }

    #[test]
    fn apply_extra_headers_empty_map_is_noop() {
        use std::collections::HashMap;
        let client = reqwest::Client::new();
        let headers = HashMap::new();
        let req = client.get("https://example.com/file.zip");
        let req = super::apply_extra_headers(req, &headers);
        let built = req.build().unwrap();
        assert!(built.headers().get("Authorization").is_none());
    }

    #[test]
    fn apply_extra_headers_skips_invalid_header_name() {
        use std::collections::HashMap;
        let client = reqwest::Client::new();
        let mut headers = HashMap::new();
        // 无效的 header name（包含空格）应被跳过
        headers.insert("Invalid Header".to_string(), "value".to_string());
        headers.insert("Valid-Header".to_string(), "good".to_string());
        let req = client.get("https://example.com/file.zip");
        let req = super::apply_extra_headers(req, &headers);
        let built = req.build().unwrap();
        // 有效 header 正常添加
        assert_eq!(
            built
                .headers()
                .get("Valid-Header")
                .unwrap()
                .to_str()
                .unwrap(),
            "good"
        );
        // 无效 header 被跳过（HeaderName::from_bytes 会拒绝含空格的名称）
    }

    #[test]
    fn apply_extra_headers_filters_dangerous_headers() {
        use std::collections::HashMap;
        let client = reqwest::Client::new();
        let mut headers = HashMap::new();
        // All of these should be filtered out (defense-in-depth)
        headers.insert("Accept-Encoding".to_string(), "gzip, br".to_string());
        headers.insert("Content-Encoding".to_string(), "gzip".to_string());
        headers.insert("Transfer-Encoding".to_string(), "chunked".to_string());
        headers.insert("Host".to_string(), "evil.com".to_string());
        headers.insert("Content-Length".to_string(), "999".to_string());
        headers.insert("Connection".to_string(), "keep-alive".to_string());
        // This one should pass through
        headers.insert("Authorization".to_string(), "Bearer ok".to_string());
        let req = client.get("https://example.com/file.zip");
        let req = super::apply_extra_headers(req, &headers);
        let built = req.build().unwrap();
        assert!(built.headers().get("Accept-Encoding").is_none());
        assert!(built.headers().get("Content-Encoding").is_none());
        assert!(built.headers().get("Transfer-Encoding").is_none());
        assert!(built.headers().get("Host").is_none());
        assert!(built.headers().get("Content-Length").is_none());
        assert!(built.headers().get("Connection").is_none());
        assert_eq!(
            built
                .headers()
                .get("Authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer ok"
        );
    }

    // -----------------------------------------------------------------------
    // detect_content_encoding
    // -----------------------------------------------------------------------

    #[test]
    fn detect_content_encoding_none_when_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(super::detect_content_encoding(&headers).is_none());
    }

    #[test]
    fn detect_content_encoding_none_for_identity() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("identity"),
        );
        assert!(super::detect_content_encoding(&headers).is_none());
    }

    #[test]
    fn detect_content_encoding_gzip() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("gzip"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Gzip)
        );
    }

    #[test]
    fn detect_content_encoding_brotli() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("br"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Brotli)
        );
    }

    #[test]
    fn detect_content_encoding_zstd() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("zstd"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Zstd)
        );
    }

    #[test]
    fn detect_content_encoding_deflate() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("deflate"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Deflate)
        );
    }

    #[test]
    fn detect_content_encoding_empty_is_none() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static(""),
        );
        assert!(super::detect_content_encoding(&headers).is_none());
    }

    #[test]
    fn detect_content_encoding_comma_separated() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("gzip, identity"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Gzip)
        );
    }

    #[test]
    fn detect_content_encoding_x_gzip() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            reqwest::header::HeaderValue::from_static("x-gzip"),
        );
        assert_eq!(
            super::detect_content_encoding(&headers),
            Some(super::ContentEncoding::Gzip)
        );
    }

    // -----------------------------------------------------------------------
    // is_server_rejection
    // -----------------------------------------------------------------------

    /// 辅助函数：构造指定状态码的 DownloadError::Request。
    /// 利用 reqwest::Response::from(http_resp) 将 http::Response 转为 reqwest::Response，
    /// 再调用 error_for_status() 获取带状态码的 reqwest::Error。
    fn make_status_error(status: u16) -> super::DownloadError {
        let http_resp = ::reqwest::Response::from(
            ::http::Response::builder()
                .status(status)
                .body("")
                .unwrap_or_else(|_| {
                    panic!("failed to build http::Response with status {}", status)
                }),
        );
        let err = http_resp.error_for_status().unwrap_err();
        super::DownloadError::Request(err)
    }

    #[test]
    fn server_rejection_detects_403() {
        assert!(super::is_server_rejection(&make_status_error(403)));
    }

    #[test]
    fn server_rejection_detects_429() {
        assert!(super::is_server_rejection(&make_status_error(429)));
    }

    #[test]
    fn server_rejection_ignores_404() {
        assert!(!super::is_server_rejection(&make_status_error(404)));
    }

    #[test]
    fn server_rejection_ignores_500() {
        assert!(!super::is_server_rejection(&make_status_error(500)));
    }

    #[test]
    fn server_rejection_ignores_non_request_errors() {
        assert!(!super::is_server_rejection(
            &super::DownloadError::Cancelled
        ));
        assert!(!super::is_server_rejection(&super::DownloadError::Other(
            "403 forbidden".to_string()
        )));
    }
}
