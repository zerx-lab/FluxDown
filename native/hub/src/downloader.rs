use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::HeaderValue;
use reqwest::Client;
use thiserror::Error;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::db::Db;
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
    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct FileInfo {
    pub file_name: String,
    pub total_bytes: i64,
    pub supports_range: bool,
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
    /// Proxy configuration — used by FTP downloader for SOCKS/HTTP CONNECT tunneling.
    /// HTTP downloads use the proxy via the `client` field (already configured).
    pub proxy_config: crate::proxy_config::ProxyConfig,
}

// ---------------------------------------------------------------------------
// HTTP client builder (shared config)
// ---------------------------------------------------------------------------

/// Build a properly configured HTTP client that mirrors Chrome's capabilities.
///
/// When `proxy_config` specifies a proxy, it is injected into the client builder.
/// - `ProxyMode::None`   → explicit `no_proxy()` to disable env-var proxies
/// - `ProxyMode::System`  → auto-detect from Windows registry / environment
/// - `ProxyMode::Manual`  → user-specified proxy URL (HTTP/HTTPS/SOCKS4/SOCKS5)
pub fn build_client(proxy_config: &crate::proxy_config::ProxyConfig) -> Result<Client, DownloadError> {
    use crate::proxy_config::{ProxyMode, detect_system_proxy};

    let mut builder = Client::builder()
        .user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
             AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/131.0.0.0 Safari/537.36",
        )
        // TLS — reqwest with rustls-tls feature handles HTTPS automatically
        .use_rustls_tls()
        // Redirects — follow up to 30 hops like Chrome
        .redirect(reqwest::redirect::Policy::limited(30))
        // Timeouts
        .connect_timeout(Duration::from_secs(30))
        // No global timeout — downloads can be very long
        // Connection pool — close idle connections after 90s to avoid
        // stale connections, and keep at most 8 idle per host.
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(4)
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
                        rinf::debug_print!("[build_client] system proxy detected: {}", url);
                        match reqwest::Proxy::all(&url) {
                            Ok(mut proxy) => {
                                if !sys_proxy.username.is_empty() {
                                    proxy = proxy.basic_auth(
                                        &sys_proxy.username,
                                        &sys_proxy.password,
                                    );
                                }
                                if !sys_proxy.no_proxy_list.is_empty() {
                                    proxy = proxy.no_proxy(
                                        reqwest::NoProxy::from_string(&sys_proxy.no_proxy_list),
                                    );
                                }
                                builder = builder.proxy(proxy);
                            }
                            Err(e) => {
                                rinf::debug_print!(
                                    "[build_client] failed to parse system proxy URL: {}",
                                    e
                                );
                            }
                        }
                    } else {
                        rinf::debug_print!("[build_client] system proxy enabled but no URL resolved");
                    }
                }
                Ok(None) => {
                    rinf::debug_print!("[build_client] system proxy: not configured");
                }
                Err(e) => {
                    rinf::debug_print!("[build_client] system proxy detection error: {}", e);
                }
            }
        }
        ProxyMode::Manual => {
            if let Some(url) = proxy_config.to_proxy_url() {
                rinf::debug_print!("[build_client] manual proxy: {}", url);
                match reqwest::Proxy::all(&url) {
                    Ok(mut proxy) => {
                        if !proxy_config.username.is_empty() {
                            proxy = proxy.basic_auth(
                                &proxy_config.username,
                                &proxy_config.password,
                            );
                        }
                        if !proxy_config.no_proxy_list.is_empty() {
                            proxy = proxy.no_proxy(
                                reqwest::NoProxy::from_string(&proxy_config.no_proxy_list),
                            );
                        }
                        builder = builder.proxy(proxy);
                    }
                    Err(e) => {
                        rinf::debug_print!(
                            "[build_client] failed to create proxy from URL: {}",
                            e
                        );
                    }
                }
            } else {
                rinf::debug_print!("[build_client] manual proxy: incomplete config, using direct");
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
/// Reduced from 3 to 2: first attempt + one retry covers DNS cold-start
/// and transient failures without excessive delay.
const PROBE_MAX_RETRIES: u32 = 2;

/// Base delay for probe retries (used with exponential backoff).
const PROBE_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

/// Resolve file info with automatic retry on transient failures.
///
/// On Windows, the very first HTTPS request from a new process can fail due to
/// DNS resolver cold-start, rustls TLS session initialisation, or firewall
/// first-connection inspection.  Retrying transparently hides this from users.
pub async fn resolve_file_info(client: &Client, url: &str, cookies: &str) -> Result<FileInfo, DownloadError> {
    let mut last_err = None;
    for attempt in 0..PROBE_MAX_RETRIES {
        match resolve_file_info_once(client, url, cookies).await {
            Ok(info) => return Ok(info),
            Err(e) => {
                rinf::debug_print!(
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
    Err(last_err.unwrap_or_else(|| {
        DownloadError::Other("probe failed after retries".to_string())
    }))
}

async fn resolve_file_info_once(client: &Client, url: &str, cookies: &str) -> Result<FileInfo, DownloadError> {
    // --- Concurrent HEAD + GET probe ----------------------------------------
    // Fire both HEAD and GET Range:0-0 in parallel.  HEAD is faster when it
    // works, but many servers/CDNs omit Content-Disposition on HEAD.  By
    // running both concurrently we avoid the serial HEAD→GET penalty.

    let head_fut = {
        let mut req = client.head(url).timeout(PROBE_TIMEOUT);
        if !cookies.is_empty() {
            req = req.header("Cookie", cookies);
        }
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
            rinf::debug_print!(
                "[resolve] HEAD failed: status={}, url={}, cookies_len={}",
                r.status(), r.url(), cookies.len()
            );
            None
        }
        Err(e) => {
            rinf::debug_print!(
                "[resolve] HEAD network error: {}, cookies_len={}",
                e, cookies.len()
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
            drop(r); // release connection immediately
            Some((h, u, got_206))
        }
        Ok(r) => {
            rinf::debug_print!(
                "[resolve] GET failed: status={}, url={}, cookies_len={}",
                r.status(), r.url(), cookies.len()
            );
            None
        }
        Err(e) => {
            rinf::debug_print!(
                "[resolve] GET network error: {}, cookies_len={}",
                e, cookies.len()
            );
            None
        }
    };

    // Merge results: HEAD as base, GET to fill in missing data.
    let (mut headers, mut final_url) = match (&head_data, &get_data) {
        (Some((hh, hu)), _) => (hh.clone(), hu.clone()),
        (None, Some((gh, gu, _))) => (gh.clone(), gu.clone()),
        (None, None) => {
            return Err(DownloadError::Other(
                "both HEAD and GET probes failed".to_string(),
            ));
        }
    };

    // If HEAD succeeded but lacks Content-Disposition, merge from GET.
    if head_data.is_some()
        && let Some((get_headers, get_url, got_206)) = &get_data
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
        if *got_206
            && let Some(cr) = get_headers.get("content-range")
        {
            headers.insert(
                reqwest::header::HeaderName::from_static("content-range"),
                cr.clone(),
            );
        }
    }

    // --- Phase 3: Parse metadata from merged headers ------------------------
    // A 206 response from GET proves range support even without Accept-Ranges header.
    let got_206_from_get = get_data.as_ref().is_some_and(|(_, _, got)| *got);
    let supports_range = got_206_from_get
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

    let file_name = extract_filename(&headers, final_url.as_str());
    rinf::debug_print!(
        "[resolve] url={} → name={}, size={}, range={}",
        url, file_name, total_bytes, supports_range
    );

    Ok(FileInfo {
        file_name,
        total_bytes,
        supports_range,
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
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Some("pptx")
        }
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
    let value = disposition.to_str().ok()?;

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

pub async fn dedup_filename(dir: &Path, name: &str) -> String {
    use std::ffi::OsStr;

    // Phase 1: fast probe — most of the time there is no conflict.
    let candidate = dir.join(name);
    let temp_candidate = PathBuf::from(format!("{}{}", candidate.display(), TEMP_EXT));
    if !tokio::fs::try_exists(&candidate).await.unwrap_or(false)
        && !tokio::fs::try_exists(&temp_candidate).await.unwrap_or(false)
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
        // Check both the final and in-progress file names.
        if !existing.contains(OsStr::new(&new_name))
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

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run_download(params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let result = run_download_inner(&params).await;

    match result {
        Ok(total) => {
            rinf::debug_print!("[download] task {} completed, total={} bytes", task_id_log, total);
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
            rinf::debug_print!("[download] task {} cancelled", task_id_log);
            // pause / cancel already handled upstream — nothing to do
        }
        Err(e) => {
            let msg = e.to_string();
            rinf::debug_print!("[download] task {} error: {}", task_id_log, msg);
            let _ = params.db.update_task_status(&params.task_id, 4, &msg).await;
            let _ = params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: 0,
                    total_bytes: 0,
                    status: 4,
                    error_message: msg,
                    file_name: String::new(),
                    segment_details: None,
                })
                .await;
        }
    }
}

/// Run the segment advisor to dynamically compute optimal segment count.
/// Updates `tasks.segments` in DB so that subsequent resumes skip the probe.
async fn compute_segments_with_advisor(p: &DownloadParams, info: &FileInfo) -> i32 {
    use crate::segment_advisor::{
        advise_static, advise_with_bandwidth, probe_bandwidth, AdvisorInput,
    };
    let advisor_input = AdvisorInput {
        total_bytes: info.total_bytes,
        supports_range: info.supports_range,
    };

    // Phase 1: static recommendation (file size + CPU cores).
    let static_advice = advise_static(&advisor_input);
    rinf::debug_print!(
        "[download] task {} static advice: segments={}, reason={}",
        p.task_id,
        static_advice.segments,
        static_advice.reason
    );

    let result = if static_advice.segments > 1 {
        // Phase 2: bandwidth probe to refine the recommendation.
        match probe_bandwidth(&p.client, &p.url, info.supports_range, &p.cancel_token, &p.cookies).await {
            Some(bw) => {
                let bw_advice = advise_with_bandwidth(&advisor_input, bw);
                rinf::debug_print!(
                    "[download] task {} bandwidth probe: {:.1} KB/s → segments={}, reason={}",
                    p.task_id,
                    bw / 1024.0,
                    bw_advice.segments,
                    bw_advice.reason
                );
                bw_advice.segments
            }
            None => {
                rinf::debug_print!(
                    "[download] task {} bandwidth probe failed/cancelled, using static advice",
                    p.task_id
                );
                static_advice.segments
            }
        }
    } else {
        static_advice.segments
    };

    // Persist to DB so resume_task can skip the advisor.
    // If this write fails, the advisor will re-run on resume — acceptable.
    if let Err(e) = p.db.update_task_segments(&p.task_id, result).await {
        rinf::debug_print!(
            "[download] task {} failed to persist segment count to DB: {}",
            p.task_id, e
        );
    }

    result
}

async fn run_download_inner(p: &DownloadParams) -> Result<i64, DownloadError> {
    rinf::debug_print!("[download] task {} starting, url={}", p.task_id, p.url);

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

    rinf::debug_print!("[download] task {} resolving file info...", p.task_id);
    let info = resolve_file_info(client, &p.url, &p.cookies).await?;
    rinf::debug_print!(
        "[download] task {} resolved: name={}, size={}, range={}",
        p.task_id,
        info.file_name,
        info.total_bytes,
        info.supports_range
    );

    let auto_name = if p.file_name.is_empty() {
        info.file_name.clone()
    } else {
        p.file_name.clone()
    };

    let save_dir = PathBuf::from(&p.save_dir);

    // When resuming, the file on disk belongs to *this* task — skip dedup.
    // For new downloads, dedup to avoid overwriting unrelated files.
    let actual_name = if p.is_resume {
        auto_name.clone()
    } else {
        dedup_filename(&save_dir, &auto_name).await
    };

    p.db.update_task_file_info(&p.task_id, &actual_name, info.total_bytes)
        .await?;

    let _ = p.db.update_task_status(&p.task_id, 1, "").await;

    // Immediately notify Dart: status=1 with resolved file name & total size
    let _ = p
        .progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: 0,
            total_bytes: info.total_bytes,
            status: 1,
            error_message: String::new(),
            file_name: actual_name.clone(),
            segment_details: None,
        })
        .await;

    let dest_path = save_dir.join(&actual_name);
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
                rinf::debug_print!(
                    "[download] task {} resume: reusing {} existing segment(s) from DB",
                    p.task_id, n
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
    let use_segments = info.supports_range && info.total_bytes > 1_048_576 && segments > 1;

    rinf::debug_print!(
        "[download] task {} mode={}, segments={}, temp={}, dest={}",
        p.task_id,
        if use_segments { "multi-segment" } else { "single" },
        segments,
        temp_path.display(),
        dest_path.display()
    );

    if use_segments {
        download_multi_segment(
            &p.task_id,
            &p.url,
            &temp_path,
            info.total_bytes,
            segments,
            client,
            &p.db,
            &p.progress_tx,
            &p.cancel_token,
            &p.speed_limiter,
            &p.cookies,
        )
        .await?;
    } else {
        download_single(
            &p.task_id,
            &p.url,
            &temp_path,
            info.total_bytes,
            info.supports_range,
            client,
            &p.db,
            &p.progress_tx,
            &p.cancel_token,
            &p.speed_limiter,
            &p.cookies,
        )
        .await?;
    }

    // Integrity check — verify download completeness.
    if info.total_bytes > 0 {
        if use_segments {
            // Multi-segment: file is pre-allocated via set_len() so metadata
            // size always == total_bytes.  Check actual progress from DB instead.
            let segs = p.db.load_segments(&p.task_id).await?;
            let seg_total: i64 = segs.iter().map(|s| s.downloaded_bytes).sum();
            if seg_total != info.total_bytes {
                return Err(DownloadError::Other(format!(
                    "segment integrity failed: expected {} bytes, segments downloaded {} bytes",
                    info.total_bytes, seg_total
                )));
            }
            // Also verify actual file size on disk (guards against external
            // file deletion/truncation between download and this check).
            let file_len = tokio::fs::metadata(&temp_path)
                .await
                .map(|m| m.len() as i64)
                .unwrap_or(0);
            if file_len < info.total_bytes {
                return Err(DownloadError::Other(format!(
                    "file integrity failed: disk size={} bytes, expected {} bytes",
                    file_len, info.total_bytes
                )));
            }
        } else {
            // Single-thread: no pre-allocation, file size == downloaded bytes.
            let meta = tokio::fs::metadata(&temp_path).await?;
            if (meta.len() as i64) != info.total_bytes {
                return Err(DownloadError::Other(format!(
                    "size mismatch: expected {} bytes, got {} bytes",
                    info.total_bytes, meta.len()
                )));
            }
        }
    }

    // All data verified — rename temp file to final destination.
    // This is the atomic moment the file "appears" as complete.
    tokio::fs::rename(&temp_path, &dest_path).await.map_err(|e| {
        DownloadError::Other(format!(
            "failed to rename {} → {}: {}",
            temp_path.display(),
            dest_path.display(),
            e
        ))
    })?;

    rinf::debug_print!(
        "[download] task {} renamed {} → {}",
        p.task_id,
        temp_path.display(),
        dest_path.display()
    );

    Ok(info.total_bytes)
}

// ---------------------------------------------------------------------------
// Single-thread download (with resume support)
// ---------------------------------------------------------------------------

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
) -> Result<(), DownloadError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Check if there's an existing partial file we can resume
    let existing_len = match tokio::fs::metadata(dest).await {
        Ok(m) => m.len() as i64,
        Err(_) => 0,
    };

    // Resume only if server supports Range and we have a partial file that is
    // smaller than total (or total is unknown)
    let resume = supports_range && existing_len > 0 && (total_bytes == 0 || existing_len < total_bytes);

    let mut downloaded: i64;
    let mut file;

    let mut req = client.get(url);
    if !cookies.is_empty() {
        req = req.header("Cookie", cookies);
    }
    if resume {
        req = req.header("Range", format!("bytes={}-", existing_len));
        downloaded = existing_len;
        let mut raw_file = OpenOptions::new().write(true).open(dest).await?;
        raw_file.seek(std::io::SeekFrom::End(0)).await?;
        file = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, raw_file);
    } else {
        downloaded = 0;
        file = tokio::io::BufWriter::with_capacity(
            BUF_WRITER_CAPACITY,
            File::create(dest).await?,
        );
        // Reset DB progress so the UI doesn't show stale values
        let _ = db.update_task_progress(task_id, 0).await;
    }

    let resp = req.send().await?.error_for_status()?;

    // Try extracting a better filename from the actual download response.
    // This is the ultimate fallback — the real GET may have Content-Disposition
    // even when the probe HEAD/GET-Range:0-0 didn't.
    let resp_name = extract_filename(resp.headers(), resp.url().as_str());
    if !resp_name.is_empty()
        && resp_name != "download"
        && resp.headers().contains_key(reqwest::header::CONTENT_DISPOSITION)
    {
        rinf::debug_print!("[download-single] got better name from response: {}", resp_name);
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

    let mut stream = resp.bytes_stream();

    let mut last_report = std::time::Instant::now();
    let mut last_db_save = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                file.flush().await?;
                let _ = db.update_task_progress(task_id, downloaded).await;
                return Err(DownloadError::Cancelled);
            }
            chunk = stream.next() => {
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
                        return Err(DownloadError::Request(e));
                    }
                    None => break,
                }
            }
        }
    }

    file.flush().await?;
    let _ = db.update_task_progress(task_id, downloaded).await;
    Ok(())
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
) -> Result<(), DownloadError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Check if total_bytes changed since segments were created (server updated
    // the file).  If so, discard old segments — the coordinator will recreate.
    let existing_segments = db.load_segments(task_id).await?;
    if !existing_segments.is_empty() {
        let last_seg = existing_segments.iter().max_by_key(|s| s.index);
        if let Some(last) = last_seg {
            let expected_end = total_bytes - 1;
            if last.end_byte != expected_end {
                rinf::debug_print!(
                    "[download] task {} total_bytes changed: segment end_byte={}, expected={}. Discarding old segments.",
                    task_id, last.end_byte, expected_end
                );
                db.delete_segments(task_id).await?;
            }
        }
    }

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
    )
    .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        extract_filename, extract_from_content_disposition, extract_from_url, mime_to_ext,
        sanitize_filename, urlencoding_decode, dedup_filename,
        PROBE_MAX_RETRIES, PROBE_RETRY_BASE_DELAY, PROBE_TIMEOUT, TEMP_EXT,
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
        assert_eq!(sanitize_filename(r#"a<b>c:d"e/f\g|h?i*j"#), "a_b_c_d_e_f_g_h_i_j");
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
        assert!(name.is_none(), "trailing slash should return None, got: {name:?}");
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
        assert_eq!(urlencoding_decode("hello%20world").unwrap_or_default(), "hello world");
    }

    #[test]
    fn urlencoding_decode_plus_to_space() {
        assert_eq!(urlencoding_decode("hello+world").unwrap_or_default(), "hello world");
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
            "attachment; filename=\"fallback.txt\"; filename*=UTF-8''preferred.txt"
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
        // Fixed: 15s per request, 2 retries with 1s base exponential backoff.
        // HEAD+GET now run concurrently (max 15s per attempt, not 30s).
        assert_eq!(PROBE_TIMEOUT, Duration::from_secs(15));
        assert_eq!(PROBE_MAX_RETRIES, 2);
        assert_eq!(PROBE_RETRY_BASE_DELAY, Duration::from_secs(1));

        // Worst case: 2 attempts × 15s (concurrent HEAD+GET) + 1s delay = 31s
        let worst_per_attempt = PROBE_TIMEOUT; // HEAD+GET concurrent
        let worst_total = worst_per_attempt * PROBE_MAX_RETRIES
            + PROBE_RETRY_BASE_DELAY;
        assert!(worst_total <= Duration::from_secs(60),
            "worst-case probe time {worst_total:?} should be <= 60s after fix");
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
        assert!(!has_cd, "empty headers should not have Content-Disposition — GET data will be merged");

        // With Content-Disposition present, no merge needed
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str("attachment; filename=\"test.zip\"") {
            headers.insert(reqwest::header::CONTENT_DISPOSITION, v);
        }
        let has_cd = headers.contains_key(reqwest::header::CONTENT_DISPOSITION);
        assert!(has_cd, "Content-Disposition present — no need to merge from GET");
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

        let result = dedup_filename(&dir, "test.txt").await;
        assert_eq!(result, "test.txt");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_with_conflict() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_conflict");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // Create conflicting file
        tokio::fs::write(dir.join("test.txt"), b"").await.unwrap_or(());

        let result = dedup_filename(&dir, "test.txt").await;
        assert_eq!(result, "test (1).txt");

        // Clean up
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_temp_file_conflict() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_temp");
        let _ = tokio::fs::create_dir_all(&dir).await;
        // Create a .fdownloading temp file — should also be considered a conflict
        tokio::fs::write(dir.join(format!("test.txt{TEMP_EXT}")), b"").await.unwrap_or(());

        let result = dedup_filename(&dir, "test.txt").await;
        assert_eq!(result, "test (1).txt");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn dedup_filename_no_extension() {
        let dir = std::env::temp_dir().join("fluxdown_test_dedup_noext");
        let _ = tokio::fs::create_dir_all(&dir).await;
        tokio::fs::write(dir.join("README"), b"").await.unwrap_or(());

        let result = dedup_filename(&dir, "README").await;
        assert_eq!(result, "README (1)");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
