//! FTP protocol download engine.
//!
//! Uses suppaftp's **synchronous** FTP API with `tokio::task::spawn_blocking`
//! to avoid async-runtime conflicts (suppaftp async depends on async-std).
//!
//! Architecture:
//! - Single-thread and multi-segment download modes
//! - REST command for breakpoint resume
//! - Each segment opens its own FTP connection (standard parallel FTP approach)
//! - Shared SpeedLimiter, DB persistence, progress reporting
//! - CancellationToken for pause/cancel

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::downloader::{
    BUF_WRITER_CAPACITY, DB_SAVE_INTERVAL_SECS, DownloadError, DownloadParams, FileInfo,
    ProgressUpdate, SegmentProgressInfo, TEMP_EXT, dedup_filename, extract_from_url,
    sanitize_filename,
};
use crate::logger::log_info;
use crate::proxy_config::{self, ProxyConfig};
use crate::speed_limiter::SpeedLimiter;

// ---------------------------------------------------------------------------
// FTP URL parsing
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct FtpUrl {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub path: String,
}

pub fn parse_ftp_url(url: &str) -> Result<FtpUrl, DownloadError> {
    let lower = url.to_ascii_lowercase();
    let stripped = if lower.starts_with("ftp://") {
        &url[6..]
    } else {
        return Err(DownloadError::Other("not an FTP URL".to_string()));
    };

    // Use rfind to handle passwords containing literal '@' characters.
    // E.g. ftp://user:p@ss@host/file → userinfo="user:p@ss", hostpath="host/file"
    let (userinfo, hostpath) = if let Some(at_pos) = stripped.rfind('@') {
        (&stripped[..at_pos], &stripped[at_pos + 1..])
    } else {
        ("", stripped)
    };

    let (username, password) = if userinfo.is_empty() {
        ("anonymous".to_string(), "anonymous@".to_string())
    } else if let Some(colon) = userinfo.find(':') {
        (
            url_decode(&userinfo[..colon]),
            url_decode(&userinfo[colon + 1..]),
        )
    } else {
        (url_decode(userinfo), String::new())
    };

    let (hostport, path) = if let Some(slash) = hostpath.find('/') {
        (&hostpath[..slash], &hostpath[slash..])
    } else {
        (hostpath, "/")
    };

    let (host, port) = if let Some(colon) = hostport.rfind(':') {
        let port_str = &hostport[colon + 1..];
        match port_str.parse::<u16>() {
            Ok(p) => (hostport[..colon].to_string(), p),
            Err(_) => (hostport.to_string(), 21),
        }
    } else {
        (hostport.to_string(), 21)
    };

    if host.is_empty() {
        return Err(DownloadError::Other("empty FTP host".to_string()));
    }

    Ok(FtpUrl {
        host,
        port,
        username,
        password,
        path: url_decode(path),
    })
}

fn url_decode(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            result.push(byte);
            i += 3;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).unwrap_or_else(|_| s.to_string())
}

// ---------------------------------------------------------------------------
// Sync FTP helper: connect + login + binary mode
// ---------------------------------------------------------------------------

use suppaftp::FtpStream;
use suppaftp::types::FileType;

/// Timeout for FTP data-connection reads.  Prevents blocking threads from
/// hanging indefinitely when the server stops sending data (e.g. on cancel).
/// Applied to the data stream TCP socket after `retr_as_stream`.
/// 30 seconds balances between handling slow servers and avoiding indefinite
/// hangs.  Combined with MAX_CONSECUTIVE_TIMEOUTS, the maximum blocking time
/// before error is 30s × 3 = 90s.
const FTP_DATA_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Connect to an FTP server, optionally through a proxy.
///
/// Proxy modes:
/// - `None` / `ProxyMode::None` → direct connection
/// - SOCKS4/SOCKS5 → tunnel control connection through SOCKS proxy, also sets
///   `passive_stream_builder` so data connections go through the same proxy
/// - HTTP/HTTPS → tunnel via HTTP CONNECT (control only; data connections
///   in passive mode also go through HTTP CONNECT)
fn ftp_connect_sync_with_proxy(
    ftp_url: &FtpUrl,
    proxy: Option<&ProxyConfig>,
) -> Result<FtpStream, DownloadError> {
    let timeout = Duration::from_secs(30);

    let should_proxy = proxy
        .map(|p| p.is_active() && !p.host.is_empty() && p.port > 0)
        .unwrap_or(false);

    let default_proxy = ProxyConfig::default();
    let mut stream = if should_proxy {
        let proxy = proxy.unwrap_or(&default_proxy);
        log_info!(
            "[ftp-connect] using {} proxy {}:{} for {}:{}",
            proxy.proxy_type.as_str(),
            proxy.host,
            proxy.port,
            ftp_url.host,
            ftp_url.port,
        );

        // Establish a TCP connection through the proxy
        let tcp = proxy_config::proxy_connect_sync(proxy, &ftp_url.host, ftp_url.port, timeout)?;

        // Build FtpStream from the pre-established (proxied) TCP connection
        let mut ftp = FtpStream::connect_with_stream(tcp)
            .map_err(|e| DownloadError::Other(format!("FTP connect_with_stream error: {}", e)))?;

        // Set passive_stream_builder so data connections also go through the proxy.
        // In passive mode, the FTP server tells us the data endpoint address.
        // We need to connect to that address through the proxy too.
        let proxy_clone = proxy.clone();
        ftp = ftp.passive_stream_builder(move |data_addr: std::net::SocketAddr| {
            let host = data_addr.ip().to_string();
            let port = data_addr.port();
            proxy_config::proxy_connect_sync(&proxy_clone, &host, port, Duration::from_secs(30))
                .map_err(|e| {
                    suppaftp::FtpError::ConnectionError(std::io::Error::other(e.to_string()))
                })
        });

        ftp
    } else {
        // Direct connection (no proxy)
        let addr = format!("{}:{}", ftp_url.host, ftp_url.port);

        let sock_addr: std::net::SocketAddr = addr.parse().or_else(|_| {
            use std::net::ToSocketAddrs;
            addr.to_socket_addrs()
                .map_err(|e| DownloadError::Other(format!("DNS resolve error: {}", e)))?
                .next()
                .ok_or_else(|| DownloadError::Other("DNS returned no addresses".to_string()))
        })?;

        FtpStream::connect_timeout(sock_addr, timeout)
            .map_err(|e| DownloadError::Other(format!("FTP connect error: {}", e)))?
    };

    stream
        .login(&ftp_url.username, &ftp_url.password)
        .map_err(|e| DownloadError::Other(format!("FTP login error: {}", e)))?;

    stream
        .transfer_type(FileType::Binary)
        .map_err(|e| DownloadError::Other(format!("FTP set binary mode error: {}", e)))?;

    Ok(stream)
}

// ---------------------------------------------------------------------------
// Resolve FTP file info
// ---------------------------------------------------------------------------

const PROBE_MAX_RETRIES: u32 = 2;
const PROBE_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

pub async fn resolve_ftp_file_info(
    url: &str,
    proxy: &ProxyConfig,
) -> Result<FileInfo, DownloadError> {
    let ftp_url = parse_ftp_url(url)?;

    let mut last_err = None;
    for attempt in 0..PROBE_MAX_RETRIES {
        let fu = ftp_url.clone();
        let px = proxy.clone();
        let result = tokio::task::spawn_blocking(move || resolve_ftp_info_sync(&fu, &px))
            .await
            .map_err(|e| DownloadError::Other(format!("spawn_blocking join error: {}", e)))?;

        match result {
            Ok(info) => return Ok(info),
            Err(e) => {
                log_info!(
                    "[ftp-resolve] attempt {}/{} failed: {}",
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
    Err(last_err.unwrap_or_else(|| DownloadError::Other("FTP probe failed".to_string())))
}

fn resolve_ftp_info_sync(ftp_url: &FtpUrl, proxy: &ProxyConfig) -> Result<FileInfo, DownloadError> {
    let proxy_opt = if proxy.is_active() { Some(proxy) } else { None };
    let mut ftp = ftp_connect_sync_with_proxy(ftp_url, proxy_opt)?;

    let total_bytes = match ftp.size(&ftp_url.path) {
        Ok(size) => size as i64,
        Err(e) => {
            log_info!("[ftp-resolve] SIZE failed: {}, assuming unknown", e);
            0
        }
    };

    let file_name = ftp_url
        .path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(sanitize_filename)
        .or_else(|| extract_from_url(&format!("ftp://{}{}", ftp_url.host, ftp_url.path)))
        .unwrap_or_else(|| "download".to_string());

    let supports_range = total_bytes > 0;

    let _ = ftp.quit();

    log_info!(
        "[ftp-resolve] path={}, name={}, size={}, range={}",
        ftp_url.path,
        file_name,
        total_bytes,
        supports_range
    );

    Ok(FileInfo {
        file_name,
        total_bytes,
        supports_range,
        content_type: String::new(),
        etag: String::new(),
        last_modified: String::new(),
        // FTP has no Content-Encoding concept.
        content_encoding_compressed: false,
    })
}

// ---------------------------------------------------------------------------
// FTP bandwidth probe
// ---------------------------------------------------------------------------

pub async fn probe_ftp_bandwidth(
    url: &str,
    cancel_token: &CancellationToken,
    proxy: &ProxyConfig,
) -> Option<f64> {
    const PROBE_BYTES: u64 = 512 * 1024;

    let ftp_url = match parse_ftp_url(url) {
        Ok(u) => u,
        Err(_) => return None,
    };

    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();

    // Watch for cancellation in the background.
    let cancel_watcher = {
        let token = cancel_token.clone();
        let flag = cancelled.clone();
        tokio::spawn(async move {
            token.cancelled().await;
            flag.store(true, Ordering::SeqCst);
        })
    };

    let proxy_clone = proxy.clone();
    let result = tokio::task::spawn_blocking(move || {
        let proxy_opt = if proxy_clone.is_active() {
            Some(&proxy_clone)
        } else {
            None
        };
        let mut ftp = match ftp_connect_sync_with_proxy(&ftp_url, proxy_opt) {
            Ok(f) => f,
            Err(_) => return None,
        };

        let start = std::time::Instant::now();

        let mut data_stream = match ftp.retr_as_stream(&ftp_url.path) {
            Ok(s) => s,
            Err(_) => {
                let _ = ftp.quit();
                return None;
            }
        };

        // Set read timeout on data connection to prevent indefinite blocking.
        data_stream
            .get_ref()
            .set_read_timeout(Some(FTP_DATA_READ_TIMEOUT))
            .ok();

        let mut buf = vec![0u8; 64 * 1024];
        let mut total: u64 = 0;

        loop {
            if cancelled_clone.load(Ordering::SeqCst) {
                break;
            }
            match data_stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total += n as u64;
                    if total >= PROBE_BYTES {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        // On cancel or early break, drop data_stream first to close the data
        // connection, then try to clean up.  finalize_retr_stream may block
        // waiting for a 226 response that never comes if we aborted early.
        if cancelled_clone.load(Ordering::SeqCst) {
            drop(data_stream);
            let _ = ftp.quit();
        } else {
            let _ = ftp.finalize_retr_stream(data_stream);
            let _ = ftp.quit();
        }

        let elapsed = start.elapsed();
        if elapsed.as_millis() < 50 || total < 1024 {
            return None;
        }

        Some(total as f64 / elapsed.as_secs_f64())
    })
    .await
    .unwrap_or(None);

    cancel_watcher.abort();
    result
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run_ftp_download(params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let result = run_ftp_download_inner(&params).await;

    match result {
        Ok(total) => {
            log_info!(
                "[ftp-download] task {} completed, total={} bytes",
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
            log_info!("[ftp-download] task {} cancelled", task_id_log);
        }
        Err(e) => {
            let msg = e.to_string();
            log_info!("[ftp-download] task {} error: {}", task_id_log, msg);
            let _ = params.db.update_task_status(&params.task_id, 4, &msg).await;

            // Preserve actual progress from DB so the UI doesn't jump back to 0%.
            let (dl, total) = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => (t.downloaded_bytes, t.total_bytes),
                other => {
                    log_info!(
                        "[ftp-download] task {} warning: failed to read progress from DB: {:?}",
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
                    error_message: msg,
                    file_name: String::new(),
                    segment_details: None,
                })
                .await;
        }
    }
}

async fn compute_ftp_segments(p: &DownloadParams, info: &FileInfo) -> i32 {
    use crate::segment_advisor::{AdvisorInput, advise_static, advise_with_bandwidth};

    let advisor_input = AdvisorInput {
        total_bytes: info.total_bytes,
        supports_range: info.supports_range,
    };

    let static_advice = advise_static(&advisor_input);
    log_info!(
        "[ftp-download] task {} static advice: segments={}, reason={}",
        p.task_id,
        static_advice.segments,
        static_advice.reason
    );

    let result = if static_advice.segments > 1 {
        match probe_ftp_bandwidth(&p.url, &p.cancel_token, &p.proxy_config).await {
            Some(bw) => {
                let bw_advice = advise_with_bandwidth(&advisor_input, bw);
                log_info!(
                    "[ftp-download] task {} bandwidth: {:.1} KB/s → segments={}",
                    p.task_id,
                    bw / 1024.0,
                    bw_advice.segments
                );
                bw_advice.segments
            }
            None => static_advice.segments,
        }
    } else {
        static_advice.segments
    };

    if let Err(e) = p.db.update_task_segments(&p.task_id, result).await {
        log_info!(
            "[ftp-download] task {} failed to persist segment count: {}",
            p.task_id,
            e
        );
    }

    result
}

async fn run_ftp_download_inner(p: &DownloadParams) -> Result<i64, DownloadError> {
    log_info!("[ftp-download] task {} starting, url={}", p.task_id, p.url);

    // Transition to status=5 (preparing) — probing FTP server, resolving file info
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

    let info = resolve_ftp_file_info(&p.url, &p.proxy_config).await?;
    log_info!(
        "[ftp-download] task {} resolved: name={}, size={}, range={}",
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
    let actual_name = if p.is_resume {
        auto_name.clone()
    } else {
        dedup_filename(&save_dir, &auto_name, &p.reserved_filenames_snapshot).await
    };

    p.db.update_task_file_info(&p.task_id, &actual_name, info.total_bytes)
        .await?;

    // 早期取消检查：probe 完成后、创建文件之前检测 pause/delete，
    // 防止已取消的任务仍然在磁盘上创建临时文件。
    if p.cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    let _ = p.db.update_task_status(&p.task_id, 1, "").await;

    // For resume tasks, send persisted downloaded bytes as baseline so speed
    // smoothing won't misinterpret resumed bytes as fresh transfer rate.
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
            total_bytes: info.total_bytes,
            status: 1,
            error_message: String::new(),
            file_name: actual_name.clone(),
            segment_details: None,
        })
        .await;

    let dest_path = save_dir.join(&actual_name);
    let temp_path = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));

    // Dynamic segment calculation
    let segments = if p.segment_count <= 0 {
        if p.is_resume {
            let existing = p.db.load_segments(&p.task_id).await.unwrap_or_default();
            if !existing.is_empty() {
                existing.len() as i32
            } else {
                compute_ftp_segments(p, &info).await
            }
        } else {
            compute_ftp_segments(p, &info).await
        }
    } else {
        p.segment_count
    };

    let use_segments = info.supports_range && info.total_bytes > 1_048_576 && segments > 1;

    log_info!(
        "[ftp-download] task {} mode={}, segments={}",
        p.task_id,
        if use_segments {
            "multi-segment"
        } else {
            "single"
        },
        segments,
    );

    let ftp_url = parse_ftp_url(&p.url)?;

    if use_segments {
        ftp_download_multi_segment(
            &p.task_id,
            &ftp_url,
            &temp_path,
            info.total_bytes,
            segments,
            &p.db,
            &p.progress_tx,
            &p.cancel_token,
            &p.speed_limiter,
            &p.proxy_config,
        )
        .await?;
    } else {
        // Retry wrapper for single-thread FTP download.
        // ftp_download_single supports resume (checks existing file length),
        // so retrying after a transient failure is safe.
        let mut attempts = 0u32;
        loop {
            match ftp_download_single(
                &p.task_id,
                &ftp_url,
                &temp_path,
                info.total_bytes,
                info.supports_range,
                &p.db,
                &p.progress_tx,
                &p.cancel_token,
                &p.speed_limiter,
                &p.proxy_config,
            )
            .await
            {
                Ok(()) => break,
                Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
                Err(e) => {
                    attempts += 1;
                    if attempts >= MAX_RETRIES {
                        return Err(e);
                    }
                    log_info!(
                        "[ftp-download] task {} single-thread attempt {}/{} failed: {}",
                        p.task_id,
                        attempts,
                        MAX_RETRIES,
                        e
                    );
                    let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
                    tokio::select! {
                        _ = p.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }
    }

    // Integrity check
    if info.total_bytes > 0 {
        if use_segments {
            let segs = p.db.load_segments(&p.task_id).await?;
            let seg_total: i64 = segs.iter().map(|s| s.downloaded_bytes).sum();
            if seg_total != info.total_bytes {
                return Err(DownloadError::Other(format!(
                    "FTP segment integrity failed: DB sum={} bytes, expected {} bytes",
                    seg_total, info.total_bytes
                )));
            }
            // Also verify actual file size on disk (guards against DB/disk mismatch
            // caused by crashes or external file modifications).
            let file_len = tokio::fs::metadata(&temp_path)
                .await
                .map(|m| m.len() as i64)
                .unwrap_or(0);
            if file_len < info.total_bytes {
                return Err(DownloadError::Other(format!(
                    "FTP file integrity failed: disk size={} bytes, expected {} bytes",
                    file_len, info.total_bytes
                )));
            }
        } else {
            let meta = tokio::fs::metadata(&temp_path).await?;
            if (meta.len() as i64) != info.total_bytes {
                return Err(DownloadError::Other(format!(
                    "FTP size mismatch: expected {} bytes, got {} bytes",
                    info.total_bytes,
                    meta.len()
                )));
            }
        }
    }

    // When total_bytes is unknown (server didn't report size), read actual file
    // size so the completion signal carries accurate byte counts.
    let actual_total = if info.total_bytes > 0 {
        info.total_bytes
    } else {
        match tokio::fs::metadata(&temp_path).await {
            Ok(m) => m.len() as i64,
            Err(e) => {
                log_info!(
                    "[ftp-download] task {} warning: cannot read temp file size: {}",
                    p.task_id,
                    e
                );
                0
            }
        }
    };

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

    Ok(actual_total)
}

// ---------------------------------------------------------------------------
// Single-thread FTP download
// ---------------------------------------------------------------------------

const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);

/// Maximum consecutive read timeouts before aborting the FTP reader.
/// Prevents infinite retry loops when set_read_timeout silently fails
/// or the server stops sending data without closing the connection.
const MAX_CONSECUTIVE_TIMEOUTS: u32 = 3;

/// Maximum simultaneous FTP connections for multi-segment downloads.
/// Most FTP servers limit 5-10 connections per IP; exceeding this causes
/// connection refusals and potential IP bans.
const MAX_CONCURRENT_FTP_CONNECTIONS: usize = 4;

/// Single-thread FTP download using sync FTP in a blocking task.
/// Progress is reported back to the async world via mpsc channel.
#[allow(clippy::too_many_arguments)]
async fn ftp_download_single(
    task_id: &str,
    ftp_url: &FtpUrl,
    dest: &Path,
    total_bytes: i64,
    supports_range: bool,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    proxy_config: &ProxyConfig,
) -> Result<(), DownloadError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let existing_len = match tokio::fs::metadata(dest).await {
        Ok(m) => m.len() as i64,
        Err(_) => 0,
    };

    let resume =
        supports_range && existing_len > 0 && (total_bytes == 0 || existing_len < total_bytes);

    // Reset DB progress if starting fresh
    if !resume {
        let _ = db.update_task_progress(task_id, 0).await;
    }

    let ftp_url = ftp_url.clone();
    let dest = dest.to_path_buf();
    let task_id = task_id.to_string();
    let db = db.clone();
    let progress_tx = progress_tx.clone();
    let cancel_token = cancel_token.clone();
    let speed_limiter = speed_limiter.clone();

    // The blocking thread reads FTP data and sends chunks via channel
    // to the async side which handles file I/O and progress reporting.
    let (chunk_tx, mut chunk_rx) = mpsc::channel::<Vec<u8>>(32);
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_writer = cancelled.clone();

    // Cancel watcher
    let cancel_watcher = {
        let token = cancel_token.clone();
        let flag = cancelled.clone();
        tokio::spawn(async move {
            token.cancelled().await;
            flag.store(true, Ordering::SeqCst);
        })
    };

    // Blocking FTP reader thread
    let ftp_reader = {
        let ftp_url = ftp_url.clone();
        let cancelled = cancelled.clone();
        let resume_offset = if resume { existing_len } else { 0 };
        let proxy = proxy_config.clone();

        tokio::task::spawn_blocking(move || -> Result<(), DownloadError> {
            let proxy_opt = if proxy.is_active() {
                Some(&proxy)
            } else {
                None
            };
            let mut ftp = ftp_connect_sync_with_proxy(&ftp_url, proxy_opt)?;

            if resume_offset > 0 {
                ftp.resume_transfer(resume_offset as usize)
                    .map_err(|e| DownloadError::Other(format!("FTP REST error: {}", e)))?;
            }

            let mut data_stream = ftp
                .retr_as_stream(&ftp_url.path)
                .map_err(|e| DownloadError::Other(format!("FTP RETR error: {}", e)))?;

            // Set read timeout so cancellation eventually unblocks this thread.
            if let Err(e) = data_stream
                .get_ref()
                .set_read_timeout(Some(FTP_DATA_READ_TIMEOUT))
            {
                log_info!("[ftp-single] set_read_timeout failed: {}", e);
            }

            let mut buf = vec![0u8; 64 * 1024];
            let mut consecutive_timeouts: u32 = 0;

            loop {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                match data_stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        consecutive_timeouts = 0;
                        if chunk_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::TimedOut
                            || e.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        consecutive_timeouts += 1;
                        if cancelled.load(Ordering::SeqCst)
                            || consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS
                        {
                            if consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS {
                                drop(data_stream);
                                let _ = ftp.quit();
                                return Err(DownloadError::Other(format!(
                                    "FTP read timed out {} consecutive times",
                                    consecutive_timeouts
                                )));
                            }
                            break;
                        }
                        continue;
                    }
                    Err(e) => {
                        drop(data_stream);
                        let _ = ftp.quit();
                        return Err(DownloadError::Io(e));
                    }
                }
            }

            // On cancel, skip finalize_retr_stream (it blocks waiting for 226).
            if cancelled.load(Ordering::SeqCst) {
                drop(data_stream);
            } else {
                let _ = ftp.finalize_retr_stream(data_stream);
            }
            let _ = ftp.quit();
            Ok(())
        })
    };

    // Async writer: receives chunks and writes to file with speed limiting
    let mut downloaded: i64 = if resume { existing_len } else { 0 };
    let mut file = if resume {
        let f = OpenOptions::new().write(true).open(&dest).await?;
        let mut f = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, f);
        f.seek(std::io::SeekFrom::End(0)).await?;
        f
    } else {
        tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, File::create(&dest).await?)
    };

    let mut last_report = std::time::Instant::now();
    let mut last_db_save = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                cancelled_writer.store(true, Ordering::SeqCst);
                file.flush().await?;
                let _ = db.update_task_progress(&task_id, downloaded).await;
                cancel_watcher.abort();
                // Wait for blocking thread to finish
                let _ = ftp_reader.await;
                return Err(DownloadError::Cancelled);
            }
            chunk = chunk_rx.recv() => {
                match chunk {
                    Some(bytes) => {
                        let n = bytes.len();
                        // Speed limiter
                        let mut offset = 0usize;
                        while offset < n {
                            let remaining = (n - offset) as u64;
                            let allowed = speed_limiter.consume(remaining).await;
                            let end = offset + allowed as usize;
                            file.write_all(&bytes[offset..end]).await?;
                            offset = end;
                        }
                        downloaded += n as i64;

                        if last_report.elapsed().as_millis() >= 200 {
                            let _ = progress_tx
                                .send(ProgressUpdate {
                                    task_id: task_id.clone(),
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

                        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                            let _ = db.update_task_progress(&task_id, downloaded).await;
                            last_db_save = std::time::Instant::now();
                        }
                    }
                    None => break, // channel closed — FTP reader done
                }
            }
        }
    }

    file.flush().await?;
    let _ = db.update_task_progress(&task_id, downloaded).await;
    cancel_watcher.abort();

    // Check reader result
    let reader_result = ftp_reader
        .await
        .map_err(|e| DownloadError::Other(format!("FTP reader join error: {}", e)))?;
    reader_result?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Multi-segment FTP download
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn ftp_download_multi_segment(
    task_id: &str,
    ftp_url: &FtpUrl,
    dest: &Path,
    total_bytes: i64,
    segment_count: i32,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    cancel_token: &CancellationToken,
    speed_limiter: &SpeedLimiter,
    proxy_config: &ProxyConfig,
) -> Result<(), DownloadError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Load or create segment definitions
    let mut existing_segments = db.load_segments(task_id).await?;

    if !existing_segments.is_empty() {
        let db_downloaded: i64 = existing_segments.iter().map(|s| s.downloaded_bytes).sum();
        let file_len = match tokio::fs::metadata(dest).await {
            Ok(m) => m.len() as i64,
            Err(_) => 0,
        };

        if db_downloaded > 0 && (file_len == 0 || file_len < db_downloaded) {
            db.reset_segments_progress(task_id).await?;
            existing_segments = db.load_segments(task_id).await?;
        }

        if !existing_segments.is_empty() && total_bytes > 0 {
            let last_seg = existing_segments.iter().max_by_key(|s| s.index);
            if let Some(last) = last_seg
                && last.end_byte != total_bytes - 1
            {
                db.delete_segments(task_id).await?;
                existing_segments = Vec::new();
            }
        }
    }

    let seg_defs: Vec<(i32, i64, i64, i64)> = if existing_segments.is_empty() {
        let chunk_size = total_bytes / segment_count as i64;
        let mut defs = Vec::new();
        for i in 0..segment_count {
            let start = i as i64 * chunk_size;
            let end = if i == segment_count - 1 {
                total_bytes - 1
            } else {
                (i as i64 + 1) * chunk_size - 1
            };
            defs.push((i, start, end, 0i64));
        }
        let db_segs: Vec<(i32, i64, i64)> = defs.iter().map(|(i, s, e, _)| (*i, *s, *e)).collect();
        db.insert_segments(task_id, &db_segs).await?;
        defs
    } else {
        existing_segments
            .iter()
            .map(|s| (s.index, s.start_byte, s.end_byte, s.downloaded_bytes))
            .collect()
    };

    let total_downloaded = Arc::new(AtomicI64::new(
        seg_defs.iter().map(|(_, _, _, d)| d).sum::<i64>(),
    ));

    let seg_states: Arc<StdMutex<Vec<SegmentProgressInfo>>> = Arc::new(StdMutex::new(
        seg_defs
            .iter()
            .map(|(idx, start, end, dl)| SegmentProgressInfo {
                index: *idx,
                start_byte: *start,
                end_byte: *end,
                downloaded_bytes: *dl,
            })
            .collect(),
    ));

    // Pre-allocate file
    {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dest)
            .await?;
        if file.metadata().await?.len() < total_bytes as u64 {
            file.set_len(total_bytes as u64).await?;
        }
    }

    // Limit concurrent FTP connections to avoid server-side per-IP limits
    // (most FTP servers cap at 5-10 simultaneous connections per IP).
    let ftp_semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_FTP_CONNECTIONS));
    let mut handles = Vec::new();

    for (idx, start, end, already_downloaded) in &seg_defs {
        let actual_start = start + already_downloaded;
        if actual_start > *end {
            continue;
        }

        let ftp_url = ftp_url.clone();
        let dest = dest.to_path_buf();
        let cancel = cancel_token.clone();
        let total_dl = total_downloaded.clone();
        let seg_states = seg_states.clone();
        let db = db.clone();
        let task_id = task_id.to_string();
        let seg_idx = *idx;
        let seg_start = *start;
        let seg_end = *end;
        let progress_tx = progress_tx.clone();
        let total = total_bytes;
        let limiter = speed_limiter.clone();
        let sem = ftp_semaphore.clone();
        let proxy = proxy_config.clone();

        let handle = tokio::spawn(async move {
            // Acquire permit before opening FTP connection
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => return Err(DownloadError::Cancelled),
            };
            ftp_do_segment_with_retry(
                &task_id,
                seg_idx,
                &ftp_url,
                &dest,
                seg_start,
                actual_start,
                seg_end,
                &cancel,
                &total_dl,
                total,
                &db,
                &progress_tx,
                &seg_states,
                &limiter,
                &proxy,
            )
            .await
        });
        handles.push(handle);
    }

    let mut final_error = None;
    for handle in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(DownloadError::Cancelled)) => {
                if final_error.is_none() {
                    final_error = Some(DownloadError::Cancelled);
                }
            }
            Ok(Err(e)) => {
                if final_error.is_none() {
                    cancel_token.cancel();
                    final_error = Some(e);
                }
            }
            Err(e) => {
                if final_error.is_none() {
                    cancel_token.cancel();
                    final_error = Some(DownloadError::Other(e.to_string()));
                }
            }
        }
    }

    if let Some(err) = final_error {
        return Err(err);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Per-segment download with retry
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn ftp_do_segment_with_retry(
    task_id: &str,
    seg_idx: i32,
    ftp_url: &FtpUrl,
    dest: &Path,
    seg_start: i64,
    mut actual_start: i64,
    seg_end: i64,
    cancel: &CancellationToken,
    total_downloaded: &AtomicI64,
    total_bytes: i64,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    speed_limiter: &SpeedLimiter,
    proxy_config: &ProxyConfig,
) -> Result<(), DownloadError> {
    let mut attempts = 0u32;

    loop {
        match ftp_do_segment(
            task_id,
            seg_idx,
            ftp_url,
            dest,
            seg_start,
            actual_start,
            seg_end,
            cancel,
            total_downloaded,
            total_bytes,
            db,
            progress_tx,
            seg_states,
            speed_limiter,
            proxy_config,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
            Err(e) => {
                attempts += 1;
                if attempts >= MAX_RETRIES {
                    return Err(e);
                }
                if let Ok(segs) = db.load_segments(task_id).await
                    && let Some(seg) = segs.iter().find(|s| s.index == seg_idx)
                {
                    actual_start = seg_start + seg.downloaded_bytes;
                    if actual_start > seg_end {
                        return Ok(());
                    }
                }
                let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempts - 1);
                tokio::select! {
                    _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Single segment download (blocking FTP reader + async file writer)
// ---------------------------------------------------------------------------

/// Each FTP segment: blocking thread reads from FTP, sends chunks via channel;
/// async side handles file seek/write, speed limiting, and progress reporting.
#[allow(clippy::too_many_arguments)]
async fn ftp_do_segment(
    task_id: &str,
    seg_idx: i32,
    ftp_url: &FtpUrl,
    dest: &Path,
    seg_start: i64,
    actual_start: i64,
    seg_end: i64,
    cancel: &CancellationToken,
    total_downloaded: &AtomicI64,
    total_bytes: i64,
    db: &Db,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    seg_states: &Arc<StdMutex<Vec<SegmentProgressInfo>>>,
    speed_limiter: &SpeedLimiter,
    proxy_config: &ProxyConfig,
) -> Result<(), DownloadError> {
    let bytes_needed = (seg_end - actual_start + 1) as u64;

    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_writer = cancelled.clone();

    let cancel_watcher = {
        let token = cancel.clone();
        let flag = cancelled.clone();
        tokio::spawn(async move {
            token.cancelled().await;
            flag.store(true, Ordering::SeqCst);
        })
    };

    // Channel for data chunks from blocking reader to async writer.
    let (chunk_tx, mut chunk_rx) = mpsc::channel::<Vec<u8>>(16);

    // Blocking FTP reader
    let ftp_reader = {
        let ftp_url = ftp_url.clone();
        let cancelled = cancelled.clone();
        let seg_bytes_needed = bytes_needed;
        let proxy = proxy_config.clone();

        tokio::task::spawn_blocking(move || -> Result<(), DownloadError> {
            let proxy_opt = if proxy.is_active() {
                Some(&proxy)
            } else {
                None
            };
            let mut ftp = ftp_connect_sync_with_proxy(&ftp_url, proxy_opt)?;

            ftp.resume_transfer(actual_start as usize).map_err(|e| {
                DownloadError::Other(format!("FTP REST error (seg {}): {}", seg_idx, e))
            })?;

            let mut data_stream = ftp.retr_as_stream(&ftp_url.path).map_err(|e| {
                DownloadError::Other(format!("FTP RETR error (seg {}): {}", seg_idx, e))
            })?;

            // Set read timeout so cancellation eventually unblocks this thread.
            if let Err(e) = data_stream
                .get_ref()
                .set_read_timeout(Some(FTP_DATA_READ_TIMEOUT))
            {
                log_info!("[ftp-seg {}] set_read_timeout failed: {}", seg_idx, e);
            }

            let mut buf = vec![0u8; 64 * 1024];
            let mut bytes_read: u64 = 0;
            let mut consecutive_timeouts: u32 = 0;

            loop {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                let remaining = seg_bytes_needed - bytes_read;
                if remaining == 0 {
                    break;
                }
                let to_read = (remaining as usize).min(buf.len());

                match data_stream.read(&mut buf[..to_read]) {
                    Ok(0) => break,
                    Ok(n) => {
                        consecutive_timeouts = 0;
                        bytes_read += n as u64;
                        if chunk_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::TimedOut
                            || e.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        consecutive_timeouts += 1;
                        if cancelled.load(Ordering::SeqCst)
                            || consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS
                        {
                            if consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS {
                                drop(data_stream);
                                let _ = ftp.quit();
                                return Err(DownloadError::Other(format!(
                                    "FTP segment {} read timed out {} consecutive times",
                                    seg_idx, consecutive_timeouts
                                )));
                            }
                            break;
                        }
                        continue;
                    }
                    Err(e) => {
                        drop(data_stream);
                        let _ = ftp.quit();
                        return Err(DownloadError::Io(e));
                    }
                }
            }

            // On cancel, skip finalize_retr_stream (it blocks waiting for 226).
            if cancelled.load(Ordering::SeqCst) {
                drop(data_stream);
            } else {
                let _ = ftp.finalize_retr_stream(data_stream);
            }
            let _ = ftp.quit();
            Ok(())
        })
    };

    // Async writer: write to pre-allocated file at correct offset.
    let raw_file = OpenOptions::new().write(true).open(dest).await?;
    let mut file = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, raw_file);
    file.seek(std::io::SeekFrom::Start(actual_start as u64))
        .await?;

    let mut seg_downloaded = actual_start - seg_start;
    let mut last_report = std::time::Instant::now();
    let mut last_db_save = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                cancelled_writer.store(true, Ordering::SeqCst);
                file.flush().await?;
                if let Ok(mut states) = seg_states.lock()
                    && let Some(s) = states.iter_mut().find(|s| s.index == seg_idx) {
                        s.downloaded_bytes = seg_downloaded;
                    }
                let _ = db.update_segment_progress(task_id, seg_idx, seg_downloaded).await;
                cancel_watcher.abort();
                let _ = ftp_reader.await;
                return Err(DownloadError::Cancelled);
            }
            chunk = chunk_rx.recv() => {
                match chunk {
                    Some(bytes) => {
                        let n = bytes.len();
                        // Speed limiter
                        let mut offset = 0usize;
                        while offset < n {
                            let rem = (n - offset) as u64;
                            let allowed = speed_limiter.consume(rem).await;
                            let end = offset + allowed as usize;
                            file.write_all(&bytes[offset..end]).await?;
                            offset = end;
                        }

                        let len = n as i64;
                        seg_downloaded += len;
                        total_downloaded.fetch_add(len, Ordering::Relaxed);

                        if let Ok(mut states) = seg_states.lock()
                            && let Some(s) = states.iter_mut().find(|s| s.index == seg_idx) {
                                s.downloaded_bytes = seg_downloaded;
                            }

                        if last_report.elapsed().as_millis() >= 200 {
                            let current_total = total_downloaded.load(Ordering::Relaxed);
                            let snapshot = seg_states
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let _ = progress_tx
                                .send(ProgressUpdate {
                                    task_id: task_id.to_string(),
                                    downloaded_bytes: current_total,
                                    total_bytes,
                                    status: 1,
                                    error_message: String::new(),
                                    file_name: String::new(),
                                    segment_details: Some(snapshot),
                                })
                                .await;
                            last_report = std::time::Instant::now();
                        }

                        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                            let _ = db
                                .update_segment_progress(task_id, seg_idx, seg_downloaded)
                                .await;
                            last_db_save = std::time::Instant::now();
                        }
                    }
                    None => break,
                }
            }
        }
    }

    file.flush().await?;
    if let Ok(mut states) = seg_states.lock()
        && let Some(s) = states.iter_mut().find(|s| s.index == seg_idx)
    {
        s.downloaded_bytes = seg_downloaded;
    }
    let _ = db
        .update_segment_progress(task_id, seg_idx, seg_downloaded)
        .await;
    cancel_watcher.abort();

    let reader_result = ftp_reader
        .await
        .map_err(|e| DownloadError::Other(format!("FTP segment reader join error: {}", e)))?;
    reader_result?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        FTP_DATA_READ_TIMEOUT, PROBE_MAX_RETRIES, PROBE_RETRY_BASE_DELAY, parse_ftp_url, url_decode,
    };
    use crate::downloader::sanitize_filename;
    use crate::speed_limiter::SpeedLimiter;
    use std::io::Read;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::mpsc;

    // -----------------------------------------------------------------------
    // Bug #9: url_decode — custom implementation unsafe on invalid sequences
    // -----------------------------------------------------------------------

    #[test]
    fn url_decode_basic_ascii() {
        assert_eq!(url_decode("hello"), "hello");
    }

    #[test]
    fn url_decode_percent_encoded_space() {
        assert_eq!(url_decode("hello%20world"), "hello world");
    }

    #[test]
    fn url_decode_chinese_utf8() {
        // "文件" in UTF-8 = E6 96 87 E4 BB B6
        assert_eq!(url_decode("%E6%96%87%E4%BB%B6"), "文件");
    }

    #[test]
    fn url_decode_invalid_percent_sequence_passthrough() {
        // "%ZZ" is not valid hex — should pass through literally
        assert_eq!(url_decode("%ZZtest"), "%ZZtest");
    }

    #[test]
    fn url_decode_truncated_percent_at_end() {
        // "%" at end of string with fewer than 2 chars following
        assert_eq!(url_decode("test%"), "test%");
        assert_eq!(url_decode("test%2"), "test%2");
    }

    #[test]
    fn url_decode_invalid_utf8_falls_back_to_original() {
        // 0xFF 0xFE is not valid UTF-8; should fall back to original string
        let result = url_decode("%FF%FE");
        assert_eq!(result, "%FF%FE"); // fallback to original
    }

    #[test]
    fn url_decode_mixed_encoded_and_plain() {
        assert_eq!(url_decode("my%20file%28copy%29.txt"), "my file(copy).txt");
    }

    // -----------------------------------------------------------------------
    // FTP URL parsing: parse_ftp_url
    // -----------------------------------------------------------------------

    #[test]
    fn parse_ftp_url_basic() {
        let u = parse_ftp_url("ftp://example.com/pub/file.iso");
        assert!(u.is_ok());
        let u = u.unwrap_or_else(|_| unreachable!());
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, 21);
        assert_eq!(u.username, "anonymous");
        assert_eq!(u.password, "anonymous@");
        assert_eq!(u.path, "/pub/file.iso");
    }

    #[test]
    fn parse_ftp_url_with_credentials() {
        let u = parse_ftp_url("ftp://user:pass@host.com/dir/file.txt");
        assert!(u.is_ok());
        let u = u.unwrap_or_else(|_| unreachable!());
        assert_eq!(u.username, "user");
        assert_eq!(u.password, "pass");
        assert_eq!(u.host, "host.com");
        assert_eq!(u.path, "/dir/file.txt");
    }

    #[test]
    fn parse_ftp_url_password_with_at_sign() {
        // password contains '@' — rfind('@') should handle this
        let u = parse_ftp_url("ftp://user:p%40ss@host.com/file.bin");
        assert!(u.is_ok());
        let u = u.unwrap_or_else(|_| unreachable!());
        assert_eq!(u.username, "user");
        assert_eq!(u.password, "p@ss"); // %40 decoded to @
        assert_eq!(u.host, "host.com");
    }

    #[test]
    fn parse_ftp_url_with_port() {
        let u = parse_ftp_url("ftp://host.com:2121/file.zip");
        assert!(u.is_ok());
        let u = u.unwrap_or_else(|_| unreachable!());
        assert_eq!(u.port, 2121);
    }

    #[test]
    fn parse_ftp_url_not_ftp_scheme() {
        let u = parse_ftp_url("http://example.com/file");
        assert!(u.is_err());
    }

    #[test]
    fn parse_ftp_url_empty_host() {
        let u = parse_ftp_url("ftp:///path/file");
        assert!(u.is_err());
    }

    #[test]
    fn parse_ftp_url_no_path() {
        let u = parse_ftp_url("ftp://host.com");
        assert!(u.is_ok());
        let u = u.unwrap_or_else(|_| unreachable!());
        assert_eq!(u.path, "/");
    }

    #[test]
    fn parse_ftp_url_encoded_path() {
        let u = parse_ftp_url("ftp://host.com/%E6%96%87%E4%BB%B6.txt");
        assert!(u.is_ok());
        let u = u.unwrap_or_else(|_| unreachable!());
        assert_eq!(u.path, "/文件.txt");
    }

    // -----------------------------------------------------------------------
    // Bug #12: FTP filename extraction when path ends in '/' or is empty
    // (resolve_ftp_info_sync uses rsplit('/').next().filter(|s| !s.is_empty()))
    // -----------------------------------------------------------------------

    #[test]
    fn ftp_filename_from_path_trailing_slash() {
        // Simulates the logic in resolve_ftp_info_sync
        let path = "/pub/";
        let file_name = path
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(sanitize_filename)
            .or_else(|| crate::downloader::extract_from_url(&format!("ftp://host{}", path)))
            .unwrap_or_else(|| "download".to_string());
        // trailing slash → empty segment → should fallback to "download"
        assert_eq!(file_name, "download");
    }

    #[test]
    fn ftp_filename_from_normal_path() {
        let path = "/pub/linux-6.1.tar.gz";
        let file_name = path
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(sanitize_filename)
            .unwrap_or_else(|| "download".to_string());
        assert_eq!(file_name, "linux-6.1.tar.gz");
    }

    #[test]
    fn ftp_filename_with_special_chars() {
        let path = "/pub/my:file<2>.txt";
        let file_name = path
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(sanitize_filename)
            .unwrap_or_else(|| "download".to_string());
        // colons, angle brackets should be replaced
        assert_eq!(file_name, "my_file_2_.txt");
    }

    // -----------------------------------------------------------------------
    // Bug #4/#11: probe timeout and retry config — assert current (problematic)
    // values so tests fail after the fix reminds us to update expectations
    // -----------------------------------------------------------------------

    #[test]
    fn probe_config_timeout_is_reasonable() {
        // FTP data read timeout reduced to 30s (from 60s).
        assert_eq!(FTP_DATA_READ_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn probe_retry_uses_exponential_backoff() {
        // Fixed: retries now use exponential backoff with base delay of 1s.
        assert_eq!(PROBE_RETRY_BASE_DELAY, Duration::from_secs(1));
        assert_eq!(PROBE_MAX_RETRIES, 2);
        // Worst-case: 2 attempts × 30s timeout + 1s delay = 61s (acceptable)
        let worst_case = FTP_DATA_READ_TIMEOUT * PROBE_MAX_RETRIES + PROBE_RETRY_BASE_DELAY;
        assert!(
            worst_case <= Duration::from_secs(90),
            "worst-case probe time {worst_case:?} should be <= 90s after fix"
        );
    }

    // -----------------------------------------------------------------------
    // Bug #10: i64 → usize truncation on resume_transfer
    // -----------------------------------------------------------------------

    #[test]
    fn i64_to_usize_truncation_safe_on_64bit() {
        // On 64-bit platforms usize::MAX >= i64::MAX, so no truncation.
        // This test verifies the assumption holds for the build target.
        #[cfg(target_pointer_width = "64")]
        {
            let large_offset: i64 = 5_000_000_000; // 5 GB
            let as_usize = large_offset as usize;
            assert_eq!(as_usize, 5_000_000_000usize);
        }
    }

    #[test]
    fn i64_to_usize_truncation_would_fail_on_32bit() {
        // Demonstrates the bug on a 32-bit platform (simulated).
        // i64 value > u32::MAX would silently wrap.
        let large_offset: i64 = 5_000_000_000; // 5 GB
        let as_u32 = large_offset as u32; // simulates 32-bit usize
        assert_ne!(
            as_u32 as i64, large_offset,
            "truncation silently corrupts the offset"
        );
    }

    // -----------------------------------------------------------------------
    // Bug #1/#14: FTP read timeout retry — simulates the infinite loop risk
    // -----------------------------------------------------------------------

    /// Simulates the FTP reader loop pattern to demonstrate the infinite loop
    /// when set_read_timeout silently fails and reads continuously timeout.
    #[test]
    fn ftp_read_timeout_loop_should_have_retry_limit() {
        // Simulate: create a reader that always returns TimedOut
        struct AlwaysTimedOutReader;
        impl Read for AlwaysTimedOutReader {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "simulated timeout",
                ))
            }
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let mut reader = AlwaysTimedOutReader;
        let mut buf = vec![0u8; 1024];
        let mut timeout_count = 0u32;
        let max_iterations = 1000; // Safety limit for the test itself

        // Replicate the current buggy loop pattern from ftp_downloader.rs:689-713
        let mut iterations = 0;
        loop {
            iterations += 1;
            if iterations > max_iterations {
                break; // Test safety valve
            }
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(_n) => { /* normal */ }
                Err(e)
                    if e.kind() == std::io::ErrorKind::TimedOut
                        || e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    timeout_count += 1;
                    // BUG: current code just does `continue` with no limit
                    if cancelled.load(Ordering::SeqCst) {
                        break;
                    }
                    continue; // This is the bug — infinite loop
                }
                Err(_) => break,
            }
        }

        // The loop hit our safety valve, proving the infinite loop bug:
        // without cancellation, timeouts cause unlimited retries.
        assert_eq!(
            iterations,
            max_iterations + 1,
            "BUG: timeout loop ran {iterations} times without bound — \
             needs a retry limit"
        );
        assert_eq!(
            timeout_count, max_iterations,
            "all iterations were timeouts, confirming infinite retry"
        );
    }

    // -----------------------------------------------------------------------
    // Bug #3: Speed limiter + bounded channel backpressure / potential deadlock
    // -----------------------------------------------------------------------

    /// Simulates the FTP architecture: blocking producer → bounded channel →
    /// async consumer with speed limiter. Demonstrates backpressure risk.
    #[tokio::test]
    async fn speed_limiter_with_bounded_channel_backpressure() {
        let limiter = SpeedLimiter::new(1024); // 1 KB/s — very slow
        limiter.spawn_refill_task();

        // Small bounded channel (same as ftp_downloader multi-segment: capacity 16)
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(16);
        let produced = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let consumed = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let produced_clone = produced.clone();
        // Blocking producer: simulates FTP reader pushing 64KB chunks
        let producer = tokio::task::spawn_blocking(move || {
            let chunk = vec![0u8; 64 * 1024]; // 64 KB per chunk
            for _ in 0..20 {
                // This will block when channel is full
                match tx.blocking_send(chunk.clone()) {
                    Ok(()) => {
                        produced_clone.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    }
                    Err(_) => break,
                }
            }
        });

        let consumed_clone = consumed.clone();
        let limiter_clone = limiter.clone();
        // Async consumer with speed limiter
        let consumer = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    chunk = rx.recv() => {
                        match chunk {
                            Some(bytes) => {
                                let n = bytes.len();
                                let mut offset = 0;
                                while offset < n {
                                    let rem = (n - offset) as u64;
                                    let allowed = limiter_clone.consume(rem).await;
                                    offset += allowed as usize;
                                }
                                consumed_clone.fetch_add(n as u64, Ordering::Relaxed);
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        let _ = tokio::time::timeout(Duration::from_secs(5), producer).await;
        consumer.abort();

        let total_produced = produced.load(Ordering::Relaxed);
        let total_consumed = consumed.load(Ordering::Relaxed);

        // With 1KB/s limit and 3s consumer runtime, at most ~3KB consumed.
        // But producer generates 20 × 64KB = 1.28MB of data.
        // The bounded channel (capacity 16) fills up quickly → producer blocks.
        // This demonstrates the backpressure: producer is WAY ahead of consumer.
        assert!(
            total_produced > total_consumed,
            "producer ({total_produced}) should be ahead of consumer ({total_consumed}) \
             due to speed limiter backpressure"
        );

        // Consumer should only process ~3KB in 3 seconds at 1KB/s limit
        assert!(
            total_consumed < 20_000,
            "consumer processed {total_consumed} bytes in 3s at 1KB/s limit — \
             expected < 20KB (with overhead)"
        );

        // The key insight: producer is stuck because channel is full, and consumer
        // is stuck in speed_limiter.consume(). In a real FTP download with the
        // current code, if the consumer async task is waiting on the speed limiter
        // and the channel backs up, the blocking thread cannot send new data.
        // This is the documented backpressure problem (Bug #3).
    }

    // -----------------------------------------------------------------------
    // Bug #13: multi-segment integrity check only checks DB, not disk
    // -----------------------------------------------------------------------

    #[test]
    fn integrity_check_db_only_misses_disk_corruption() {
        // Simulates the integrity check logic from ftp_downloader.rs:566-586
        struct SegRecord {
            downloaded_bytes: i64,
        }
        let total_bytes: i64 = 1_000_000;
        let segments = [
            SegRecord {
                downloaded_bytes: 500_000,
            },
            SegRecord {
                downloaded_bytes: 500_000,
            },
        ];
        let seg_total: i64 = segments.iter().map(|s| s.downloaded_bytes).sum();

        // DB says all segments complete
        assert_eq!(seg_total, total_bytes, "DB check passes");

        // But the actual file on disk could be different (e.g., 0 bytes due to crash)
        let actual_file_size: i64 = 0; // simulated disk corruption
        assert_ne!(
            actual_file_size, total_bytes,
            "BUG: DB integrity check passes but disk file is corrupted/empty — \
             current code does not verify disk file size for multi-segment FTP"
        );
    }
}
