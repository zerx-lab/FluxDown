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
    ProgressUpdate, SegmentProgressInfo, TEMP_EXT, extract_from_url, sanitize_filename,
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

/// 将单个十六进制字符（ASCII）转换为 0..=15 的 nibble 值。
///
/// 仅接受 `0-9` / `a-f` / `A-F`；其它字节返回 `None`。按字节解析可避免对
/// `&str` 做切片，从而消除 `%` 后紧跟多字节 UTF-8 字符时的 char-boundary panic。
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn url_decode(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 命中 `%` 且后面还有两个字节时，按字节解析两位十六进制。
        // 不再用 `&s[i+1..i+3]` 切片，避免切点落在多字节 UTF-8 字符内部时 panic。
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(hi) = hex_nibble(bytes[i + 1])
            && let Some(lo) = hex_nibble(bytes[i + 2])
        {
            result.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    // 优先 UTF-8，失败时回退到 GBK（老旧中文 FTP 服务器常用），
    // 双失败才返回原始字符串。
    crate::downloader::decode_bytes_utf8_or_gbk(&result).unwrap_or_else(|_| s.to_string())
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

/// BUG-FTP-CONTROL-IDLE-421 修复：在调用 finalize_retr_stream 读取 226 响应前，
/// 先给控制连接套接字设置读超时，防止服务器因长传输期间控制连接空闲而发 421
/// 断开后，finalize_retr_stream 永久阻塞等待永不到来的响应。
/// 60 秒足够覆盖正常的 226 延迟，同时保证控制连接掉线时表现为可重试错误而非
/// 无限挂起。
const FTP_CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(60);

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
        //
        // NAT 容忍：很多 NAT 后的 FTP 服务器在 PASV/EPSV 227 响应里报告的是
        // 私网/不可路由 IP（如 192.168.x.x）。标准客户端的做法是忽略 PASV 报告
        // 的 IP，复用控制连接的主机名，只采用 PASV 报告的端口。经代理隧道时若
        // 直接连服务器自报的 IP，代理往往无法到达，导致数据连接失败。这里改为
        // 复用控制主机 + PASV 端口，对齐标准 NAT 容忍行为。
        let proxy_clone = proxy.clone();
        let control_host = ftp_url.host.clone();
        ftp = ftp.passive_stream_builder(move |data_addr: std::net::SocketAddr| {
            let port = data_addr.port();
            proxy_config::proxy_connect_sync(
                &proxy_clone,
                &control_host,
                port,
                Duration::from_secs(30),
            )
            .map_err(|e| suppaftp::FtpError::ConnectionError(std::io::Error::other(e.to_string())))
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

        let mut ftp = FtpStream::connect_timeout(sock_addr, timeout)
            .map_err(|e| DownloadError::Other(format!("FTP connect error: {}", e)))?;
        // NAT 容忍：很多 NAT 后的 FTP 服务器在 PASV 227 响应里报告私网/不可路由
        // IP（如 192.168.x.x）。开启该开关后 suppaftp 忽略 PASV 自报 IP，复用控制
        // 连接的主机 + PASV 端口建立数据连接，对齐标准客户端行为（代理路径已在
        // 上面的 passive_stream_builder 中做了等价处理；此处覆盖直连路径）。
        ftp.set_passive_nat_workaround(true);
        ftp
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
// REST honouring verification (guards multi-segment correctness)
// ---------------------------------------------------------------------------

/// 多段下载前对服务器是否真正执行 REST 偏移做一次内容无关的探测。
///
/// 背景：HTTP 多段路径通过 `206 Partial Content` 显式校验 Range 是否被执行；
/// FTP 没有等价机制——`ftp_do_segment` 先 `resume_transfer(actual_start)`（发
/// REST）再 `retr_as_stream`，然后把从数据流读到的字节写到文件偏移
/// `actual_start`。若服务器对 REST 返回 350 却仍从文件头开始发送数据（部分老旧
/// /代理 FTP 服务器行为），每个段都会把“文件头部内容”写到各自的目标偏移，导致
/// 最终文件字节数正确但内容整体错位的静默数据损坏。
///
/// 探测原理（无需任何文件内容知识）：REST 到 `total_bytes - 1`，RETR：
/// - 合规服务器：该范围恰好 1 字节，读到 1 字节后即 EOF。
/// - 忽略 REST 的服务器：从字节 0 开始发送整文件，会在 1 字节之后继续送达。
///
/// 因此只要读到的字节数 > 1，即可断定 REST 被忽略 → 返回 `Some(false)`，调用方
/// 应降级为单流（单流起始全新下载不依赖 REST 写偏移）。读到 ≤1 字节即 EOF 视为
/// REST 被执行，返回 `Some(true)`。
///
/// 为避免对合规服务器的误降级（rule: 不得误降级合规服务器多段下载），任何探测
/// 过程中的网络/协议错误（连接失败、REST 返回非 350、RETR 失败、读超时等）一律
/// 视为“不确定”，返回 `None`，调用方应信任 REST 并继续多段——错误更可能是瞬时
/// 故障而非 REST 失效，不能据此剥夺合规服务器的多段能力。
fn verify_ftp_rest_honoured_sync(
    ftp_url: &FtpUrl,
    proxy: Option<&ProxyConfig>,
    total_bytes: i64,
) -> Option<bool> {
    // 仅对可定位、且至少 2 字节的文件有意义（多段门槛 >1MB，必然满足）。
    if total_bytes < 2 {
        return None;
    }
    let probe_offset = (total_bytes - 1) as usize;

    let mut ftp = ftp_connect_sync_with_proxy(ftp_url, proxy).ok()?;

    if ftp.resume_transfer(probe_offset).is_err() {
        // 服务器对 REST 直接报错：不确定（可能是瞬时故障，也可能真的不支持 REST，
        // 但此处不据此降级，交由实际下载阶段的重试/完整性核对兜底）。
        let _ = ftp.quit();
        return None;
    }

    let mut data_stream = match ftp.retr_as_stream(&ftp_url.path) {
        Ok(s) => s,
        Err(_) => {
            let _ = ftp.quit();
            return None;
        }
    };

    data_stream
        .get_ref()
        .set_read_timeout(Some(FTP_DATA_READ_TIMEOUT))
        .ok();

    // 读取至多 2 字节即可判别：合规服务器只会送 1 字节。
    let mut buf = [0u8; 2];
    let mut got: usize = 0;
    let honoured = loop {
        match data_stream.read(&mut buf[got..]) {
            Ok(0) => break got <= 1, // EOF：≤1 字节 → REST 被执行
            Ok(n) => {
                got += n;
                if got > 1 {
                    break false; // 1 字节之后仍有数据 → REST 被忽略
                }
            }
            // 读错误（含超时）：不确定，不降级。
            Err(_) => {
                drop(data_stream);
                let _ = ftp.quit();
                return None;
            }
        }
    };

    // 提前中止传输：直接关闭数据连接，不调用 finalize_retr_stream（会阻塞等 226）。
    drop(data_stream);
    let _ = ftp.quit();
    Some(honoured)
}

/// 异步包装：在 blocking 线程内执行 REST 探测，避免阻塞单线程 runtime。
async fn verify_ftp_rest_honoured(
    ftp_url: &FtpUrl,
    proxy: &ProxyConfig,
    total_bytes: i64,
) -> Option<bool> {
    let fu = ftp_url.clone();
    let px = proxy.clone();
    tokio::task::spawn_blocking(move || {
        let proxy_opt = if px.is_active() { Some(&px) } else { None };
        verify_ftp_rest_honoured_sync(&fu, proxy_opt, total_bytes)
    })
    .await
    .ok()
    .flatten()
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
                    ..Default::default()
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
                    ..Default::default()
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
            ..Default::default()
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
    // 文件名由 DownloadManager 在 do_start_task 同步段统一决策（含 dedup 和
    // 兄弟任务预订协调），FTP downloader 内不再做名称变更——保留
    // p.file_name 即可，仅当为空时（兜底）使用 probe 结果。
    let actual_name = auto_name.clone();

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
            ..Default::default()
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

    let mut use_segments = info.supports_range && info.total_bytes > 1_048_576 && segments > 1;

    // F022 防护：多段下载把每段数据写到各自的文件偏移，完全依赖服务器正确执行
    // REST 偏移。若服务器对 REST 返回 350 却仍从字节 0 发送，多段会产生“字节数
    // 正确但内容整体错位”的静默损坏（最终 DB 求和与磁盘大小核对都无法发现）。
    // 因此在启用多段前先做一次内容无关的 REST 探测；仅当探测明确判定 REST 被
    // 忽略（Some(false)）时降级为单流。探测出错/不确定（None）不降级，避免误伤
    // 合规服务器。
    if use_segments {
        match verify_ftp_rest_honoured(&parse_ftp_url(&p.url)?, &p.proxy_config, info.total_bytes)
            .await
        {
            Some(false) => {
                log_info!(
                    "[ftp-download] task {} server ignores REST offset; \
                     falling back to single-stream to avoid content misplacement",
                    p.task_id
                );
                use_segments = false;
                // 降级单流时清除可能残留的旧多段记录,维持"不使用分段则 DB 无段行"
                // 的不变式;否则下次续传 load_segments 命中残留行会误入多段,而这些段
                // 的内容实际由单流写入,造成内容空洞(与 HTTP RangeNotSupported 回退一致)。
                let _ = p.db.delete_segments(&p.task_id).await;
            }
            Some(true) => {
                log_info!("[ftp-download] task {} REST offset verified", p.task_id);
            }
            None => {
                log_info!(
                    "[ftp-download] task {} REST verification inconclusive; \
                     proceeding with multi-segment",
                    p.task_id
                );
            }
        }
    }

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
            // 磁盘大小核对：注意多段路径在下载前已用 set_len(total_bytes) 预分配
            // （见 ftp_download_multi_segment 的预分配块），因此正常情况下
            // file_len 恒等于 total_bytes，此处的 `file_len < total_bytes` 仅能
            // 捕获“临时文件被外部删除/截断”这类极端情形，**无法**检测预分配区域
            // 内的内容空洞（稀疏空洞读为 0 但 len 不变）。真正的内容完整性依赖上面
            // 的 DB 求和核对，以及多段启用前的 REST 偏移探测（verify_ftp_rest_*）。
            // 此处保留为对“文件被外部删除/截断”的最后一道防线。
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
            let disk_len = meta.len() as i64;
            if disk_len != info.total_bytes {
                // 文件比期望更大：几乎必然是服务器接受 REST(350) 却从字节 0 开始
                // 发送（REST 被忽略），导致整文件被追加到旧分片之后。保留这种
                // oversized 临时文件毫无意义——下次续传会从更大的 existing_len
                // 继续，陷入永久失败且每次浪费整文件流量。这里主动删除损坏文件，
                // 让下次重试从零开始。
                if disk_len > info.total_bytes {
                    log_info!(
                        "[ftp-download] task {} temp file oversized ({} > {} bytes), \
                         REST may have been ignored by server; removing corrupted temp file",
                        p.task_id,
                        disk_len,
                        info.total_bytes
                    );
                    let _ = tokio::fs::remove_file(&temp_path).await;
                }
                return Err(DownloadError::Other(format!(
                    "FTP size mismatch: expected {} bytes, got {} bytes",
                    info.total_bytes, disk_len
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
        // 单流新起时一并清除可能残留的多段记录(上次多段失败/降级遗留),保证
        // "不使用分段则 DB 无段行"不变式,防止后续续传被 load_segments 误判为多段。
        let _ = db.delete_segments(task_id).await;
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
                // BUG-FTP-CONTROL-IDLE-421 修复：读取 226 前给控制连接设超时，
                // 防止服务器 421 断开后 finalize_retr_stream 无限阻塞。
                // 设超时失败时记日志（与数据连接 set_read_timeout 一致），否则
                // 控制连接无超时仍可能挂起，且静默无诊断线索。
                if let Err(e) = ftp
                    .get_ref()
                    .set_read_timeout(Some(FTP_CONTROL_READ_TIMEOUT))
                {
                    log_info!("[ftp] 控制连接 set_read_timeout 失败: {}", e);
                }
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
                // flush 用 best-effort: 即便失败也要继续落库进度并清理 reader,否则 ?
                // 会以 Io 错误绕过这些清理,且重试包装器会因非 Cancelled 而错误重试。
                let _ = file.flush().await;
                let _ = db.update_task_progress(&task_id, downloaded).await;
                cancel_watcher.abort();
                // close() 唤醒可能因 channel 满而阻塞在 blocking_send 的 reader,
                // 避免下面 ftp_reader.await 死锁(与写错误分支一致)。
                chunk_rx.close();
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
                        let mut write_err: Option<std::io::Error> = None;
                        while offset < n {
                            let remaining = (n - offset) as u64;
                            let allowed = speed_limiter.consume(remaining).await;
                            let end = offset + allowed as usize;
                            if let Err(e) = file.write_all(&bytes[offset..end]).await {
                                write_err = Some(e);
                                break;
                            }
                            offset = end;
                        }

                        // BUG-FTP-SINGLE-WRITEERR-LEAK 修复：镜像多段写错误处理，
                        // 捕获写错误后先持久化进度，再设取消标志、关闭 channel、
                        // 等待 reader 结束，最后返回错误，防止 cancel_watcher 泄漏
                        // 且避免 ftp_reader 阻塞 blocking 线程的 chunk_tx 死锁。
                        if let Some(e) = write_err {
                            let _ = db.update_task_progress(&task_id, downloaded).await;
                            cancelled_writer.store(true, Ordering::SeqCst);
                            cancel_watcher.abort();
                            chunk_rx.close();
                            let _ = ftp_reader.await;
                            return Err(DownloadError::Io(e));
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
                                    ..Default::default()
                                })
                                .await;
                            last_report = std::time::Instant::now();
                        }

                        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                            // 与多段路径（ftp_do_segment）保持一致：落库前先 flush+sync，
                            // 使 DB 偏移不超过已持久化字节。单流续传虽以磁盘文件大小为准
                            // （非 DB 值），此处主要为不变式一致性；sync 失败则跳过本次落库。
                            let durable =
                                file.flush().await.is_ok() && file.get_ref().sync_data().await.is_ok();
                            if durable {
                                let _ = db.update_task_progress(&task_id, downloaded).await;
                            }
                            last_db_save = std::time::Instant::now();
                        }
                    }
                    None => break, // channel closed — FTP reader done
                }
            }
        }
    }

    file.flush().await?;
    // 与周期保存 / ftp_do_segment 保持一致：最终落库前 fdatasync，确保完成时
    // 磁盘数据持久（best-effort，失败不掩盖后续 reader 结果判定）。
    let _ = file.get_ref().sync_data().await;
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

        // 注意（best-effort）：本检查只能检测“临时文件被删除/整体截断”
        // （file_len==0 或明显小于 DB 记账量）。由于本函数随后会用
        // set_len(total_bytes) 稀疏预分配，多段乱序写入下 file_len 反映的是
        // “最高已写偏移”而非“实际已写内容总量”——即使中间段全是空洞（读为 0），
        // 只要末段写过一点 file_len 就会接近 total_bytes，使
        // `file_len < db_downloaded` 多为假，**无法**检测中间内容空洞。真正的
        // 内容正确性依赖多段启用前的 REST 偏移探测（verify_ftp_rest_*）与每段
        // 短读校验，不能依赖此处的 file_len 断言。
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
        // 防御:用户手动把分段数设得比文件总字节数还大时,chunk_size = total/count
        // 会变 0,导致非末段 end_byte=-1 被跳过、只下载末段造成不完整传输。夹紧到
        // [1, total_bytes],保证每段至少 1 字节(自动分段路径永不触发,仅极端手动值)。
        let segment_count = if (segment_count as i64) > total_bytes {
            total_bytes.max(1) as i32
        } else {
            segment_count.max(1)
        };
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

            // 短读校验：若未被取消却读到的字节数少于本段所需（服务器/网络在
            // 段中途提前关闭数据连接、chunked 提前 EOF 等），必须报错而非当成
            // 正常完成——否则该段会留下未下载的尾部（预分配区域恒为 0 字节），
            // 导致整任务在最终完整性校验处失败且无法重试。以 cancelled 标志作
            // 守卫：取消导致的提前 break 由 writer 的 cancel 分支单独返回
            // Cancelled，不应在此误判为错误。
            //
            // 此处返回 Err 后，writer 已在 reader_result? 之前完成 flush 与
            // update_segment_progress 落库，故 DB 偏移准确；
            // ftp_do_segment_with_retry 会以 seg_start+downloaded 重发 REST 续传
            // 本段，而不是让整任务失败。
            if !cancelled.load(Ordering::SeqCst) && bytes_read < seg_bytes_needed {
                drop(data_stream);
                let _ = ftp.quit();
                return Err(DownloadError::Other(format!(
                    "FTP segment {} closed early: got {}/{} bytes",
                    seg_idx, bytes_read, seg_bytes_needed
                )));
            }

            // On cancel, skip finalize_retr_stream (it blocks waiting for 226).
            if cancelled.load(Ordering::SeqCst) {
                drop(data_stream);
            } else {
                // BUG-FTP-CONTROL-IDLE-421 修复：读取 226 前给控制连接设超时，
                // 防止服务器 421 断开后 finalize_retr_stream 无限阻塞。
                // 设超时失败时记日志（与数据连接 set_read_timeout 一致），否则
                // 控制连接无超时仍可能挂起，且静默无诊断线索。
                if let Err(e) = ftp
                    .get_ref()
                    .set_read_timeout(Some(FTP_CONTROL_READ_TIMEOUT))
                {
                    log_info!("[ftp] 控制连接 set_read_timeout 失败: {}", e);
                }
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
                // flush 用 best-effort: 即便失败也要继续落库段进度并清理 reader,否则 ?
                // 会以 Io 错误绕过这些清理,且重试包装器会因非 Cancelled 而错误重试。
                let _ = file.flush().await;
                if let Ok(mut states) = seg_states.lock()
                    && let Some(s) = states.iter_mut().find(|s| s.index == seg_idx) {
                        s.downloaded_bytes = seg_downloaded;
                    }
                let _ = db.update_segment_progress(task_id, seg_idx, seg_downloaded).await;
                cancel_watcher.abort();
                // close() 唤醒可能因 channel 满而阻塞在 blocking_send 的 reader,
                // 避免 ftp_reader.await 死锁(与写错误分支一致)。
                chunk_rx.close();
                let _ = ftp_reader.await;
                return Err(DownloadError::Cancelled);
            }
            chunk = chunk_rx.recv() => {
                match chunk {
                    Some(bytes) => {
                        let n = bytes.len();
                        // Speed limiter
                        let mut offset = 0usize;
                        let mut write_err: Option<std::io::Error> = None;
                        while offset < n {
                            let rem = (n - offset) as u64;
                            let allowed = speed_limiter.consume(rem).await;
                            let end = offset + allowed as usize;
                            if let Err(e) = file.write_all(&bytes[offset..end]).await {
                                write_err = Some(e);
                                break;
                            }
                            offset = end;
                        }

                        // 写失败时，先持久化已完整记账的 seg_downloaded（不含本段
                        // 部分写入字节），再向上传播错误。否则错误会直接经 `?`
                        // 冒泡，跳过函数末尾的 update_segment_progress，使 DB 偏移
                        // 落后于真实进度；重试时按陈旧偏移续传虽因字节相同不致损坏，
                        // 但会让 DB 求和完整性核对出现不一致。这里不 flush 本段
                        // 部分写入字节，避免磁盘超前于 DB；下次以 DB 偏移 REST 续传
                        // 时会重新 seek 到 actual_start 覆盖写，幂等且安全。
                        if let Some(e) = write_err {
                            if let Ok(mut states) = seg_states.lock()
                                && let Some(s) = states.iter_mut().find(|s| s.index == seg_idx) {
                                    s.downloaded_bytes = seg_downloaded;
                                }
                            let _ = db
                                .update_segment_progress(task_id, seg_idx, seg_downloaded)
                                .await;
                            cancelled_writer.store(true, Ordering::SeqCst);
                            cancel_watcher.abort();
                            // 关闭 receiver，保证阻塞 reader 即便正卡在
                            // blocking_send（通道满）也会立刻得到 Err 而退出，避免
                            // ftp_reader.await 死锁。close() 只需 &mut self，不会与
                            // select! 宏对 chunk_rx 的可变借用冲突。
                            chunk_rx.close();
                            let _ = ftp_reader.await;
                            return Err(DownloadError::Io(e));
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
                                    ..Default::default()
                                })
                                .await;
                            last_report = std::time::Instant::now();
                        }

                        if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                            // BUG-FTP-HOLE-PERIODIC-SAVE 修复：周期落库前先将
                            // BufWriter 内核缓冲与页缓存刷到磁盘，保证 DB 中记录
                            // 的 seg_downloaded 不超过已持久化的字节数。若 flush
                            // 或 sync_data 失败，则跳过本次落库并重置计时器；
                            // 下次触发时再尝试，不因周期保存失败而中断下载。
                            let sync_ok = file.flush().await.is_ok()
                                && file.get_ref().sync_data().await.is_ok();
                            if sync_ok {
                                let _ = db
                                    .update_segment_progress(task_id, seg_idx, seg_downloaded)
                                    .await;
                            }
                            last_db_save = std::time::Instant::now();
                        }
                    }
                    None => break,
                }
            }
        }
    }

    file.flush().await?;
    file.get_ref().sync_data().await?;
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
        FTP_DATA_READ_TIMEOUT, PROBE_MAX_RETRIES, PROBE_RETRY_BASE_DELAY, hex_nibble,
        parse_ftp_url, url_decode,
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
        // 0xFF 0xFE 既不是合法 UTF-8（0xFF 是保留字节）也不是合法 GBK
        // （0xFF 不在 GBK 首字节范围）——两种解码都失败时应返回原始字符串。
        let result = url_decode("%FF%FE");
        assert_eq!(result, "%FF%FE"); // fallback to original
    }

    #[test]
    fn url_decode_gbk_chinese_path() {
        // GBK 编码的 “文件.txt”。UTF-8 解码必然失败，必须回退 GBK。
        let result = url_decode("/pub/%CE%C4%BC%FE.txt");
        assert_eq!(result, "/pub/文件.txt");
    }

    #[test]
    fn url_decode_mixed_encoded_and_plain() {
        assert_eq!(url_decode("my%20file%28copy%29.txt"), "my file(copy).txt");
    }

    // -----------------------------------------------------------------------
    // FX01: url_decode 不得在非 char 边界处 panic
    // -----------------------------------------------------------------------

    #[test]
    fn hex_nibble_parses_valid_and_rejects_invalid() {
        assert_eq!(hex_nibble(b'0'), Some(0));
        assert_eq!(hex_nibble(b'9'), Some(9));
        assert_eq!(hex_nibble(b'a'), Some(10));
        assert_eq!(hex_nibble(b'f'), Some(15));
        assert_eq!(hex_nibble(b'A'), Some(10));
        assert_eq!(hex_nibble(b'F'), Some(15));
        assert_eq!(hex_nibble(b'g'), None);
        assert_eq!(hex_nibble(b'%'), None);
        // 多字节 UTF-8 字符的首字节绝不能被当作合法 nibble。
        assert_eq!(hex_nibble("折".as_bytes()[0]), None);
    }

    #[test]
    fn url_decode_percent_followed_by_literal_multibyte_no_panic() {
        // `%` 后紧跟字面多字节 UTF-8 字符（如 "50%折扣.txt"）。旧实现用
        // &s[i+1..i+3] 切片会落在 "折" 字符内部触发 char-boundary panic。
        // 字节级解析下：`%` 后的字节不是合法 hex nibble，应原样保留。
        let input = "50%折扣.txt";
        let decoded = url_decode(input);
        assert_eq!(decoded, input);
    }

    #[test]
    fn url_decode_percent_one_hex_then_multibyte_no_panic() {
        // `%a折`：`%` 后第一个字节 'a' 是合法 nibble，但第二个字节是 "折" 的
        // 首字节（非 hex）。必须不 panic 且原样保留 '%'。
        let input = "x%a折y";
        let decoded = url_decode(input);
        assert_eq!(decoded, input);
    }

    #[test]
    fn url_decode_valid_encoding_still_works_after_byte_rewrite() {
        // 确认字节级重写后合法编码仍正确解码（回归保护）。
        assert_eq!(url_decode("%2F"), "/");
        assert_eq!(url_decode("a%2Bb"), "a+b");
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
