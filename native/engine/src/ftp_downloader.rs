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
    ProgressUpdate, SegmentProgressInfo, TEMP_EXT,
};
use crate::events::EventSink;
use crate::logger::log_info;
use crate::naming::{extract_from_url, sanitize_filename};
use crate::output;
use crate::proxy_config::{self, ProxyConfig, host_matches_no_proxy};
use crate::speed_limiter::SpeedLimiter;
use crate::transfer_activity::{TaskRuntime, TaskSegment, TransferTracker};

fn ftp_runtime(
    task_id: &str,
    total_bytes: i64,
    limit: u32,
    tracker: &TransferTracker,
    segments: &[SegmentProgressInfo],
    sample_sequence: u64,
) -> TaskRuntime {
    TaskRuntime {
        task_id: task_id.to_owned(),
        sampled_at_ms: chrono::Utc::now().timestamp_millis(),
        sample_sequence,
        active_transfers: Some(tracker.active()),
        connected_peers: None,
        parallelism_limit: Some(limit),
        total_bytes,
        segments: segments
            .iter()
            .map(|s| TaskSegment {
                index: s.index,
                start_byte: s.start_byte,
                end_byte: s.end_byte,
                downloaded_bytes: s.downloaded_bytes,
                active: s.active,
            })
            .collect(),
        source_bytes: None,
    }
}

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

    // authority 以第一个 '/' 为界：路径/文件名里的 '@'（如 icon@2x.png）不属于 userinfo。
    // authority 内用 rfind 兼容密码里的裸 '@'（ftp://user:p@ss@host/file）。
    let (authority, path_part) = match stripped.find('/') {
        Some(slash) => (&stripped[..slash], &stripped[slash..]),
        None => (stripped, "/"),
    };
    let (userinfo, hostport) = if let Some(at_pos) = authority.rfind('@') {
        (&authority[..at_pos], &authority[at_pos + 1..])
    } else {
        ("", authority)
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
    let path = url_decode(path_part);

    // 解码结果会原样拼进 FTP 命令行；CR/LF/NUL 会切断命令边界，一律拒绝。
    if [&username, &password, &path]
        .iter()
        .any(|s| s.contains(['\r', '\n', '\0']))
    {
        return Err(DownloadError::Other(
            "FTP URL contains control characters".to_string(),
        ));
    }

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
        path,
    })
}

/// 日志用：去掉 FTP URL 的 userinfo（含密码）。
fn redact_ftp_url(url: &str) -> String {
    let Some(rest) = url
        .get(..6)
        .filter(|p| p.eq_ignore_ascii_case("ftp://"))
        .map(|_| &url[6..])
    else {
        return url.to_string();
    };
    let auth_end = rest.find('/').unwrap_or(rest.len());
    match rest[..auth_end].rfind('@') {
        Some(at) => format!("{}{}", &url[..6], &rest[at + 1..]),
        None => url.to_string(),
    }
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
    // 优先 UTF-8，否则旧式字节打分解码（老旧中文 FTP 服务器常用 GBK），恒成功。
    crate::naming::decode_legacy_bytes(&result, crate::naming::NameHints::default())
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

fn set_control_timeouts(tcp: &std::net::TcpStream) -> std::io::Result<()> {
    tcp.set_read_timeout(Some(FTP_CONTROL_READ_TIMEOUT))?;
    tcp.set_write_timeout(Some(FTP_CONTROL_READ_TIMEOUT))?;
    Ok(())
}

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
    ftp_connect_sync(ftp_url, proxy, false)
}

/// 本次连接是否经代理隧道（命中 no_proxy 的主机直连，不算）。
fn connects_via_proxy(ftp_url: &FtpUrl, proxy: Option<&ProxyConfig>) -> bool {
    proxy
        .map(|p| {
            p.is_active()
                && !p.host.is_empty()
                && p.port > 0
                && !host_matches_no_proxy(&ftp_url.host, &p.no_proxy_list)
        })
        .unwrap_or(false)
}

/// [`ftp_connect_sync_with_proxy`] 的完整版：`prefer_epsv` 让直连的数据通道从
/// 一开始就走 EPSV（IPv6 控制连接本就强制 EPSV；代理路径不受影响）。
fn ftp_connect_sync(
    ftp_url: &FtpUrl,
    proxy: Option<&ProxyConfig>,
    prefer_epsv: bool,
) -> Result<FtpStream, DownloadError> {
    let timeout = Duration::from_secs(30);

    // 与 HTTP 路径一致：命中 no_proxy 的主机直连。
    let should_proxy = connects_via_proxy(ftp_url, proxy);

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
        // 代理握手完成后超时会被清除，这里重新设置，保证欢迎语/登录可超时。
        set_control_timeouts(&tcp)?;

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
        // 依次尝试所有解析出的地址（双栈下首选地址不通时回落到另一族），并在读
        // 220 欢迎语之前就设好控制连接超时，避免静默服务器无限阻塞登录/探测。
        let addrs: Vec<std::net::SocketAddr> = {
            use std::net::ToSocketAddrs;
            (ftp_url.host.as_str(), ftp_url.port)
                .to_socket_addrs()
                .map_err(|e| DownloadError::Other(format!("DNS resolve error: {}", e)))?
                .collect()
        };
        if addrs.is_empty() {
            return Err(DownloadError::Other(
                "DNS returned no addresses".to_string(),
            ));
        }
        let mut last_err = None;
        let mut tcp = None;
        for sock_addr in addrs {
            match std::net::TcpStream::connect_timeout(&sock_addr, timeout) {
                Ok(s) => {
                    tcp = Some(s);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let tcp = tcp.ok_or_else(|| {
            DownloadError::Other(format!(
                "FTP connect error: {}",
                last_err.map(|e| e.to_string()).unwrap_or_default()
            ))
        })?;
        set_control_timeouts(&tcp)?;
        let mut ftp = FtpStream::connect_with_stream(tcp)
            .map_err(|e| DownloadError::Other(format!("FTP connect error: {}", e)))?;
        // IPv6 控制连接上 PASV 只能返回 IPv4 地址，服务器通常直接拒绝；RFC 2428 的
        // EPSV 只返回端口，与下面的 NAT 容忍（复用控制主机）天然契合。
        if prefer_epsv
            || ftp
                .get_ref()
                .peer_addr()
                .map(|a| a.is_ipv6())
                .unwrap_or(false)
        {
            ftp.set_mode(suppaftp::Mode::ExtendedPassive);
        }
        // NAT 容忍：很多 NAT 后的 FTP 服务器在 PASV 227 响应里报告私网/不可路由
        // IP（如 192.168.x.x）。开启该开关后 suppaftp 忽略 PASV 自报 IP，复用控制
        // 连接的主机 + PASV 端口建立数据连接，对齐标准客户端行为（代理路径已在
        // 上面的 passive_stream_builder 中做了等价处理；此处覆盖直连路径）。
        ftp.set_passive_nat_workaround(true);
        // 默认 builder 用无超时的 TcpStream::connect；换成带连接超时的版本，防止数据
        // 连接被防火墙静默丢包时线程长时间卡在 connect。
        ftp = ftp.passive_stream_builder(move |data_addr: std::net::SocketAddr| {
            std::net::TcpStream::connect_timeout(&data_addr, timeout)
                .map_err(suppaftp::FtpError::ConnectionError)
        });
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

/// 被动模式建立阶段的失败（PASV 应答错误 / 数据连接建立失败），而非 RETR 本身被拒
/// （450/550 等）。只有前者值得换 EPSV 再试。
fn is_passive_setup_failure(e: &suppaftp::FtpError) -> bool {
    match e {
        suppaftp::FtpError::ConnectionError(_) | suppaftp::FtpError::BadResponse => true,
        suppaftp::FtpError::UnexpectedResponse(r) => {
            matches!(r.status.code(), 425 | 500..=504 | 522)
        }
        _ => false,
    }
}

/// 打开一条数据通道（`open` 通常是 `retr_as_stream`）。
///
/// IPv4 控制连接上的 PASV 路径失败（应答错误或数据连接建立失败）时，丢弃这条
/// 控制连接（数据命令可能已发出，应答流已不同步），重连后改用 EPSV 再试一次，
/// 成功则 `ftp` 替换为新连接。`rest_offset` 在新连接上重新发 REST。
///
/// 代理路径保持原样不回退：数据连接经代理隧道由 `passive_stream_builder` 建立，
/// EPSV 只换应答格式，解决不了隧道本身的可达性。IPv6 控制连接本就强制 EPSV。
fn open_data_with_epsv_fallback<R>(
    ftp: &mut FtpStream,
    ftp_url: &FtpUrl,
    proxy: Option<&ProxyConfig>,
    rest_offset: Option<usize>,
    open: impl Fn(&mut FtpStream) -> suppaftp::FtpResult<R>,
) -> suppaftp::FtpResult<R> {
    let first = match open(ftp) {
        Ok(stream) => return Ok(stream),
        Err(e) => e,
    };
    let ipv4_control = ftp
        .get_ref()
        .peer_addr()
        .map(|a| a.is_ipv4())
        .unwrap_or(false);
    if connects_via_proxy(ftp_url, proxy) || !ipv4_control || !is_passive_setup_failure(&first) {
        return Err(first);
    }
    log_info!(
        "[ftp-connect] PASV data channel failed ({}), retrying with EPSV on a fresh control connection",
        first
    );
    let mut fresh = match ftp_connect_sync(ftp_url, proxy, true) {
        Ok(f) => f,
        Err(e) => {
            log_info!("[ftp-connect] EPSV reconnect failed: {}", e);
            return Err(first);
        }
    };
    if let Some(offset) = rest_offset {
        fresh.resume_transfer(offset)?;
    }
    let stream = open(&mut fresh)?;
    *ftp = fresh;
    Ok(stream)
}

// ---------------------------------------------------------------------------
// Resolve FTP file info
// ---------------------------------------------------------------------------

const PROBE_MAX_RETRIES: u32 = 2;
const PROBE_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

/// 探测 FTP 文件信息（SIZE；`query_mtime` 为真时另发 `MDTM` 取服务器修改时间，
/// 结果以 IMF-fixdate 写入 `FileInfo::last_modified`，供「使用服务器文件时间」）。
pub async fn resolve_ftp_file_info(
    url: &str,
    proxy: &ProxyConfig,
    query_mtime: bool,
) -> Result<FileInfo, DownloadError> {
    let ftp_url = parse_ftp_url(url)?;

    let mut last_err = None;
    for attempt in 0..PROBE_MAX_RETRIES {
        let fu = ftp_url.clone();
        let px = proxy.clone();
        let result =
            tokio::task::spawn_blocking(move || resolve_ftp_info_sync(&fu, &px, query_mtime))
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

fn resolve_ftp_info_sync(
    ftp_url: &FtpUrl,
    proxy: &ProxyConfig,
    query_mtime: bool,
) -> Result<FileInfo, DownloadError> {
    let proxy_opt = if proxy.is_active() { Some(proxy) } else { None };
    let mut ftp = ftp_connect_sync_with_proxy(ftp_url, proxy_opt)?;

    let total_bytes = match ftp.size(&ftp_url.path) {
        Ok(size) => size as i64,
        Err(e) => {
            log_info!("[ftp-resolve] SIZE failed: {}, assuming unknown", e);
            0
        }
    };

    // MDTM（RFC 3659）返回 UTC 时间。服务器不支持 / 拒绝时只记日志、留空，
    // 由完成期按「无服务器时间」处理，绝不影响下载本身。
    let last_modified = if query_mtime {
        match ftp.mdtm(&ftp_url.path) {
            Ok(time) => crate::downloader::format_http_date(time),
            Err(e) => {
                log_info!(
                    "[ftp-resolve] MDTM failed: {}, server file time unavailable",
                    e
                );
                String::new()
            }
        }
    } else {
        String::new()
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

    if let Err(error) = ftp.quit() {
        tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
    }

    log_info!(
        "[ftp-resolve] path={}, name={}, size={}, range={}, mtime={}",
        ftp_url.path,
        file_name,
        total_bytes,
        supports_range,
        last_modified
    );

    Ok(FileInfo {
        file_name,
        name_source: crate::naming::NameSource::UrlPath,
        total_bytes,
        supports_range,
        content_type: String::new(),
        etag: String::new(),
        last_modified,
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
            Err(error) => {
                crate::logger::report_warning("ftp-probe", "connect for bandwidth sample", &error);
                return None;
            }
        };

        let start = std::time::Instant::now();

        let mut data_stream =
            match open_data_with_epsv_fallback(&mut ftp, &ftp_url, proxy_opt, None, |f| {
                f.retr_as_stream(&ftp_url.path)
            }) {
                Ok(s) => s,
                Err(error) => {
                    crate::logger::report_warning("ftp-probe", "open bandwidth data connection", &error);
                    if let Err(error) = ftp.quit() {
                        tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
                    }
                    return None;
                }
            };

        // Set read timeout on data connection to prevent indefinite blocking.
        if let Err(error) = data_stream
            .get_ref()
            .set_read_timeout(Some(FTP_DATA_READ_TIMEOUT)) {
            crate::logger::report_warning("ftp-probe", "set data read timeout", &error);
            return None;
        }

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
                Err(error) => {
                    tracing::debug!(%error, "FTP bandwidth sample ended after read error");
                    break;
                }
            }
        }

        // On cancel or early break, drop data_stream first to close the data
        // connection, then try to clean up.  finalize_retr_stream may block
        // waiting for a 226 response that never comes if we aborted early.
        if cancelled_clone.load(Ordering::SeqCst) {
            drop(data_stream);
            if let Err(error) = ftp.quit() {
                tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
            }
        } else {
            if let Err(error) = ftp.finalize_retr_stream(data_stream) {
                crate::logger::report_warning("ftp-download", "finalize known-size transfer", &error);
            }
            if let Err(error) = ftp.quit() {
                tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
            }
        }

        let elapsed = start.elapsed();
        if elapsed.as_millis() < 50 || total < 1024 {
            return None;
        }

        Some(total as f64 / elapsed.as_secs_f64())
    })
    .await
    .unwrap_or_else(|error| {
        if error.is_cancelled() { tracing::debug!("FTP bandwidth reader cancelled"); }
        else { crate::logger::report_warning("ftp-probe", "join bandwidth reader", &error); }
        None
    });

    cancel_watcher.abort();
    if let Err(error) = cancel_watcher.await
        && !error.is_cancelled()
    {
        crate::logger::report_warning("ftp-probe", "join bandwidth cancellation watcher", &error);
    }
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

    let mut ftp = match ftp_connect_sync_with_proxy(ftp_url, proxy) {
        Ok(ftp) => ftp,
        Err(error) => {
            crate::logger::report_warning("ftp-probe", "connect for REST verification", &error);
            return None;
        }
    };

    if let Err(e) = ftp.resume_transfer(probe_offset) {
        if let Err(error) = ftp.quit() {
            tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
        }
        // 500–504（命令未实现/参数错误）是服务器对 REST 的确定性拒绝：多段每段都要
        // REST，必败，降级单流。其它错误（网络、421 等）可能是瞬时故障，不确定。
        return match e {
            suppaftp::FtpError::UnexpectedResponse(ref r)
                if (500..=504).contains(&r.status.code()) =>
            {
                Some(false)
            }
            _ => None,
        };
    }

    let mut data_stream = match open_data_with_epsv_fallback(
        &mut ftp,
        ftp_url,
        proxy,
        Some(probe_offset),
        |f| f.retr_as_stream(&ftp_url.path),
    ) {
        Ok(s) => s,
        Err(error) => {
            crate::logger::report_warning(
                "ftp-probe",
                "open REST verification data connection",
                &error,
            );
            if let Err(error) = ftp.quit() {
                tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
            }
            return None;
        }
    };

    if let Err(error) = data_stream
        .get_ref()
        .set_read_timeout(Some(FTP_DATA_READ_TIMEOUT))
    {
        crate::logger::report_warning("ftp-probe", "set data read timeout", &error);
        return None;
    }

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
            Err(error) => {
                tracing::debug!(%error, "FTP REST verification inconclusive after read error");
                drop(data_stream);
                if let Err(error) = ftp.quit() {
                    tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
                }
                return None;
            }
        }
    };

    // 提前中止传输：直接关闭数据连接，不调用 finalize_retr_stream（会阻塞等 226）。
    drop(data_stream);
    if let Err(error) = ftp.quit() {
        tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
    }
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
    match tokio::task::spawn_blocking(move || {
        let proxy_opt = if px.is_active() { Some(&px) } else { None };
        verify_ftp_rest_honoured_sync(&fu, proxy_opt, total_bytes)
    })
    .await
    {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => {
            tracing::debug!("FTP REST verification reader cancelled");
            None
        }
        Err(error) => {
            crate::logger::report_warning("ftp-probe", "join REST verification reader", &error);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run_ftp_download(params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let result = match run_ftp_download_inner(&params).await {
        Ok(total) => {
            match crate::downloader::finalize_output(&params, crate::intercept::ProtocolKind::Ftp)
                .await
            {
                Ok(replaced) => {
                    let (total, renamed) = match replaced {
                        Some((name, size)) => (size, Some(name)),
                        None => (total, None),
                    };
                    params
                        .db
                        .update_task_status(&params.task_id, 3, "")
                        .await
                        .map(|()| (total, renamed))
                        .map_err(DownloadError::Db)
                }
                Err(e) => Err(e),
            }
        }
        Err(error) => Err(error),
    };

    match result {
        Ok((total, renamed)) => {
            log_info!(
                "[ftp-download] task {} completed, total={} bytes",
                task_id_log,
                total
            );
            if params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: total,
                    total_bytes: total,
                    status: 3,
                    error_message: String::new(),
                    file_name: renamed.unwrap_or_default(),
                    segment_details: None,
                    ..Default::default()
                })
                .await
                .is_err()
            {
                tracing::debug!("FTP progress receiver closed");
            }
        }
        Err(DownloadError::Cancelled) => {
            log_info!("[ftp-download] task {} cancelled", task_id_log);
        }
        Err(e) => {
            let msg = e.to_string();
            crate::logger::report_error("ftp-download", "run task", &e);
            if let Err(db_error) = params.db.update_task_status(&params.task_id, 4, &msg).await {
                crate::logger::report_error(
                    "ftp-download",
                    "persist terminal error status",
                    &db_error,
                );
            }

            // Preserve actual progress from DB so the UI doesn't jump back to 0%.
            let (dl, total) = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => (t.downloaded_bytes, t.total_bytes),
                other => {
                    crate::log_warn!(
                        "[ftp-download] task {} warning: failed to read progress from DB: {:?}",
                        task_id_log,
                        other.err()
                    );
                    (0, 0)
                }
            };
            if params
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
                .await
                .is_err()
            {
                tracing::debug!("FTP progress receiver closed");
            }
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

    // Auto 模式最大连接数上限（与 HTTP 一致的全局语义，<=0 = 不限）。
    let result = if p.auto_max_connections > 0 {
        result.min(p.auto_max_connections)
    } else {
        result
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
    log_info!(
        "[ftp-download] task {} starting, url={}",
        p.task_id,
        redact_ftp_url(&p.url)
    );

    // Transition to status=5 (preparing) — probing FTP server, resolving file info
    p.db.update_task_status(&p.task_id, 5, "").await?;
    if p.progress_tx
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
        .await
        .is_err()
    {
        tracing::debug!("FTP progress receiver closed");
    }

    // 探测期间的暂停/删除要立即生效，不能等阻塞线程里的握手超时。
    let info = tokio::select! {
        _ = p.cancel_token.cancelled() => return Err(DownloadError::Cancelled),
        r = resolve_ftp_file_info(&p.url, &p.proxy_config, p.use_server_time) => r?,
    };
    log_info!(
        "[ftp-download] task {} resolved: name={}, size={}, range={}",
        p.task_id,
        info.file_name,
        info.total_bytes,
        info.supports_range
    );

    let mut requested_segments = p.segment_count;
    let mut connection_cap = p.connection_cap;
    if crate::intercept::wants(
        p.interceptor.as_ref(),
        crate::intercept::InterceptStage::Probed,
    ) {
        let task_ctx = crate::intercept::ctx::task_ctx(p, crate::intercept::ProtocolKind::Ftp);
        let notes = crate::intercept::ActivityNotes::of(p);
        let decision = crate::intercept::drive::run_probed(
            p.interceptor.as_ref(),
            &notes,
            &p.cancel_token,
            &task_ctx,
            crate::intercept::patch::request_snapshot(&p.url, &p.spec, &p.run_meta),
            crate::intercept::ctx::ConnectionsCtx {
                segments: p.segment_count,
                max_connections: p.connection_cap,
                auto_max_connections: p.auto_max_connections,
            },
            crate::intercept::ctx::ProbeCtx {
                total_bytes: info.total_bytes,
                supports_range: info.supports_range,
                file_name: info.file_name.clone(),
                content_type: String::new(),
                etag: String::new(),
                last_modified: info.last_modified.clone(),
            },
        )
        .await?;
        requested_segments = decision.segments.unwrap_or(requested_segments);
        connection_cap = decision.max_connections.unwrap_or(connection_cap);
    }

    // p.file_name 来自外部（aria2 out、云端下发、接管请求等），与 HTTP/HLS 一致地清洗，
    // 否则 save_dir.join 可被 `..`/绝对路径带出保存目录。
    let auto_name = if p.file_name.is_empty() {
        info.file_name.clone()
    } else {
        sanitize_filename(&p.file_name)
    };

    let save_dir = PathBuf::from(&p.save_dir);
    // 文件名由 DownloadManager 在 do_start_task 同步段统一决策（含 dedup 和
    // 兄弟任务预订协调），FTP downloader 内不再做名称变更——保留
    // p.file_name 即可，仅当为空时（兜底）使用 probe 结果。
    let actual_name = auto_name.clone();

    // 换源结算（同 HTTP，见 `Db::settle_source_change`）：必须先于下面覆盖 total_bytes。
    // 新链接大小与旧总大小不一致 = 换成了另一个文件，清空进度并删除旧临时文件。
    if p.is_resume
        && p.db
            .settle_source_change(&p.task_id, info.total_bytes)
            .await?
    {
        log_info!(
            "[ftp-download] task {} 换源后大小与原文件不一致（新={}），丢弃已下进度从头下载",
            p.task_id,
            info.total_bytes
        );
        let stale = PathBuf::from(format!(
            "{}{}",
            save_dir.join(&actual_name).display(),
            TEMP_EXT
        ));
        match tokio::fs::remove_file(&stale).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }

    p.db.update_task_file_info(&p.task_id, &actual_name, info.total_bytes)
        .await?;

    // 早期取消检查：probe 完成后、创建文件之前检测 pause/delete，
    // 防止已取消的任务仍然在磁盘上创建临时文件。
    if p.cancel_token.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }

    p.db.update_task_status(&p.task_id, 1, "").await?;

    // For resume tasks, send persisted downloaded bytes as baseline so speed
    // smoothing won't misinterpret resumed bytes as fresh transfer rate.
    let initial_downloaded = if p.is_resume {
        p.db.load_task_by_id(&p.task_id)
            .await?
            .map(|t| t.downloaded_bytes.max(0))
            .unwrap_or(0)
    } else {
        0
    };

    if p.progress_tx
        .send(ProgressUpdate {
            task_id: p.task_id.clone(),
            downloaded_bytes: initial_downloaded,
            download_baseline: Some(initial_downloaded),
            total_bytes: info.total_bytes,
            status: 1,
            error_message: String::new(),
            file_name: actual_name.clone(),
            segment_details: None,
            ..Default::default()
        })
        .await
        .is_err()
    {
        tracing::debug!("FTP progress receiver closed");
    }

    let dest_path = save_dir.join(&actual_name);
    let temp_path = PathBuf::from(format!("{}{}", dest_path.display(), TEMP_EXT));

    // Dynamic segment calculation
    let segments = if requested_segments <= 0 {
        if p.is_resume {
            let existing = p.db.load_segments(&p.task_id).await?;
            if !existing.is_empty() {
                existing.len() as i32
            } else {
                compute_ftp_segments(p, &info).await
            }
        } else {
            compute_ftp_segments(p, &info).await
        }
    } else {
        requested_segments
    };
    let segments = if connection_cap > 0 {
        segments.min(connection_cap)
    } else {
        segments
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
                p.db.delete_segments(&p.task_id).await?;
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
    let tracker = TransferTracker::new();

    if use_segments {
        let outcome = ftp_download_multi_segment(
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
            &tracker,
            &p.sink,
            p.spawn_gen,
            crate::intercept::cap_connections(MAX_CONCURRENT_FTP_CONNECTIONS, connection_cap),
        )
        .await;
        // 暂停终态帧读 tasks.downloaded_bytes：退出前按已落库分段对齐（同 HTTP 多段）。
        if matches!(outcome, Err(DownloadError::Cancelled))
            && let Err(error) =
                p.db.sync_task_progress_from_segments(&p.task_id, p.spawn_gen)
                    .await
        {
            crate::logger::report_warning("ftp-download", "sync paused progress", &error);
        }
        outcome?;
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
                &tracker,
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
                    if let Err(journal_error) = crate::task_activity::record(
                        &p.db,
                        p.sink.as_ref(),
                        &p.task_id,
                        "retry",
                        format!("FTP 单流第 {attempts}/{MAX_RETRIES} 次尝试失败，即将重试：{e}"),
                        None,
                    )
                    .await
                    {
                        crate::log_error!(
                            "[task-activity] failed to persist retry: {}",
                            journal_error
                        );
                    }
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
                    if let Err(error) = tokio::fs::remove_file(&temp_path).await
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        crate::logger::report_warning(
                            "ftp-download",
                            "remove invalid partial file",
                            &error,
                        );
                    }
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

    if p.use_server_time {
        crate::downloader::apply_server_mtime(&dest_path, &info.last_modified, &p.task_id).await;
    }

    Ok(actual_total)
}

// ---------------------------------------------------------------------------
// Single-thread FTP download
// ---------------------------------------------------------------------------

const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
/// 服务器明确拒绝 REST 时 reader 返回的错误前缀，写端据此清空临时文件从头重下。
const REST_REJECTED_MSG: &str = "FTP REST rejected by server";

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
async fn persist_ftp_single_progress(
    file: &mut tokio::io::BufWriter<File>,
    db: &Db,
    task_id: &str,
    downloaded: i64,
) -> Result<(), DownloadError> {
    file.flush().await?;
    file.get_ref().sync_data().await?;
    db.update_task_progress(task_id, downloaded).await?;
    Ok(())
}

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
    tracker: &TransferTracker,
) -> Result<(), DownloadError> {
    output::ensure_parent(dest).await?;

    let existing_len = match tokio::fs::metadata(dest).await {
        Ok(m) => m.len() as i64,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };

    let resume =
        supports_range && existing_len > 0 && (total_bytes == 0 || existing_len < total_bytes);

    // Reset DB progress if starting fresh
    if !resume {
        // 单流新起时一并清除可能残留的多段记录(上次多段失败/降级遗留),保证
        // "不使用分段则 DB 无段行"不变式,防止后续续传被 load_segments 误判为多段。
        db.delete_segments(task_id).await?;
        db.update_task_progress(task_id, 0).await?;
    }

    let ftp_url = ftp_url.clone();
    let dest = dest.to_path_buf();
    let task_id = task_id.to_string();
    let db = db.clone();
    let progress_tx = progress_tx.clone();
    let cancel_token = cancel_token.clone();
    let speed_limiter = speed_limiter.clone();
    let tracker = tracker.clone();

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
        let tracker_reader = tracker.clone();
        let size_known = total_bytes > 0;

        tokio::task::spawn_blocking(move || -> Result<(), DownloadError> {
            let proxy_opt = if proxy.is_active() {
                Some(&proxy)
            } else {
                None
            };
            let mut ftp = ftp_connect_sync_with_proxy(&ftp_url, proxy_opt)?;

            if resume_offset > 0 {
                ftp.resume_transfer(resume_offset as usize).map_err(|e| {
                    if let suppaftp::FtpError::UnexpectedResponse(r) = &e
                        && (500..=504).contains(&r.status.code())
                    {
                        DownloadError::Other(format!("{REST_REJECTED_MSG}: {e}"))
                    } else {
                        DownloadError::Other(format!("FTP REST error: {}", e))
                    }
                })?;
            }

            let mut data_stream = open_data_with_epsv_fallback(
                &mut ftp,
                &ftp_url,
                proxy_opt,
                (resume_offset > 0).then_some(resume_offset as usize),
                |f| f.retr_as_stream(&ftp_url.path),
            )
            .map_err(|e| DownloadError::Other(format!("FTP RETR error: {}", e)))?;

            // Set read timeout so cancellation eventually unblocks this thread.
            data_stream
                .get_ref()
                .set_read_timeout(Some(FTP_DATA_READ_TIMEOUT))?;

            let mut buf = vec![0u8; 64 * 1024];
            let mut consecutive_timeouts: u32 = 0;

            loop {
                if cancelled.load(Ordering::SeqCst) {
                    break;
                }
                let read = {
                    let _transfer = tracker_reader.start(0);
                    data_stream.read(&mut buf)
                };
                match read {
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
                                if let Err(error) = ftp.quit() {
                                    tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
                                }
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
                        if let Err(error) = ftp.quit() {
                            tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
                        }
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
                // 无法设置超时就停止本次传输，不能让 blocking reader 无界挂起。
                set_control_timeouts(ftp.get_ref())?;
                match ftp.finalize_retr_stream(data_stream) {
                    Ok(()) => {}
                    // 大小未知时没有任何其它完整性核对手段，收尾应答异常（426/451、
                    // 超时等）意味着传输可能被截断，必须作为失败重试。
                    Err(e) if !size_known => {
                        if let Err(error) = ftp.quit() {
                            tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
                        }
                        return Err(DownloadError::Other(format!(
                            "FTP transfer did not complete cleanly: {e}"
                        )));
                    }
                    Err(error) => {
                        crate::logger::report_warning(
                            "ftp-download",
                            "finalize known-size transfer",
                            &error,
                        );
                    }
                }
            }
            if let Err(error) = ftp.quit() {
                tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
            }
            Ok(())
        })
    };

    // Every writer exit closes the channel and joins the blocking reader below.
    let writer_result = async {
        let mut downloaded = if resume { existing_len } else { 0 };
        let mut file = if resume {
            let f = OpenOptions::new().write(true).open(&dest).await?;
            let mut f = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, f);
            f.seek(std::io::SeekFrom::End(0)).await?;
            f
        } else {
            tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, File::create(&dest).await?)
        };
        // The actual file offset can differ from tasks.downloaded_bytes, and
        // REST fallback retries may have truncated it since the outer probe.
        // Establish this attempt's starting point before consuming body data.
        if progress_tx
            .send(ProgressUpdate {
                task_id: task_id.clone(),
                downloaded_bytes: downloaded,
                download_baseline: Some(downloaded),
                total_bytes,
                status: 1,
                ..Default::default()
            })
            .await
            .is_err()
        {
            tracing::debug!("FTP progress receiver closed");
            return Err(DownloadError::Cancelled);
        }
        let mut last_report = std::time::Instant::now();
        let mut last_db_save = std::time::Instant::now();
        loop {
            let bytes = tokio::select! {
                _ = cancel_token.cancelled() => {
                    persist_ftp_single_progress(&mut file, &db, &task_id, downloaded).await?;
                    return Err(DownloadError::Cancelled);
                }
                chunk = chunk_rx.recv() => match chunk { Some(bytes) => bytes, None => break },
            };
            let mut offset = 0;
            while offset < bytes.len() {
                let allowed = tokio::select! {
                    _ = cancel_token.cancelled() => {
                        persist_ftp_single_progress(&mut file, &db, &task_id, downloaded).await?;
                        return Err(DownloadError::Cancelled);
                    }
                    allowed = speed_limiter.consume((bytes.len() - offset) as u64) => allowed,
                };
                let end = offset + allowed as usize;
                file.write_all(&bytes[offset..end]).await?;
                downloaded += (end - offset) as i64;
                offset = end;
            }
            if last_report.elapsed().as_millis() >= 200 && !progress_tx.is_closed() {
                let segment = SegmentProgressInfo {
                    index: 0,
                    start_byte: 0,
                    end_byte: if total_bytes > 0 { total_bytes - 1 } else { 0 },
                    downloaded_bytes: downloaded,
                    active: Some(tracker.is_active(0)),
                };
                let runtime = ftp_runtime(
                    &task_id,
                    total_bytes,
                    1,
                    &tracker,
                    std::slice::from_ref(&segment),
                    crate::transfer_activity::next_sample_sequence(),
                );
                if progress_tx
                    .send(ProgressUpdate {
                        task_id: task_id.clone(),
                        downloaded_bytes: downloaded,
                        total_bytes,
                        status: 1,
                        segment_details: Some(vec![segment]),
                        runtime: Some(runtime),
                        ..Default::default()
                    })
                    .await
                    .is_err()
                {
                    tracing::debug!("FTP progress receiver closed");
                }
                last_report = std::time::Instant::now();
            }
            if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                persist_ftp_single_progress(&mut file, &db, &task_id, downloaded).await?;
                last_db_save = std::time::Instant::now();
            }
        }
        persist_ftp_single_progress(&mut file, &db, &task_id, downloaded).await
    }
    .await;
    if writer_result.is_err() {
        cancelled_writer.store(true, Ordering::SeqCst);
    }
    chunk_rx.close();
    cancel_watcher.abort();
    let watcher_error = match cancel_watcher.await {
        Ok(()) => None,
        Err(error) if error.is_cancelled() => None,
        Err(error) => Some(DownloadError::Other(format!(
            "FTP cancellation watcher join failed: {error}"
        ))),
    };
    let reader_result = ftp_reader
        .await
        .map_err(|e| DownloadError::Other(format!("FTP reader join error: {e}")))
        .and_then(|result| result);
    if let Err(error) = writer_result {
        if let Some(watcher_error) = watcher_error {
            crate::logger::report_warning(
                "ftp-download",
                "stop cancellation watcher after writer failure",
                &watcher_error,
            );
        }
        if let Err(reader_error) = reader_result {
            crate::logger::report_warning(
                "ftp-download",
                "stop reader after writer failure",
                &reader_error,
            );
        }
        return Err(error);
    }
    if let Some(error) = watcher_error {
        if let Err(reader_error) = reader_result {
            crate::logger::report_warning(
                "ftp-download",
                "stop reader after watcher failure",
                &reader_error,
            );
        }
        return Err(error);
    }
    match reader_result {
        Err(DownloadError::Other(m)) if m.starts_with(REST_REJECTED_MSG) => {
            // A rejected REST cannot resume; reset only after the reader has exited.
            File::create(&dest).await?;
            db.update_task_progress(&task_id, 0).await?;
            Err(DownloadError::Other(m))
        }
        other => other,
    }
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
    tracker: &TransferTracker,
    sink: &Arc<dyn EventSink>,
    spawn_gen: i64,
    max_connections: usize,
) -> Result<(), DownloadError> {
    output::ensure_parent(dest).await?;

    // Claim the layout epoch before loading rows or launching any writer.
    db.set_segments_epoch(task_id, spawn_gen).await?;

    // Load or create segment definitions
    let mut existing_segments = db.load_segments(task_id).await?;

    if !existing_segments.is_empty() {
        let db_downloaded: i64 = existing_segments.iter().map(|s| s.downloaded_bytes).sum();
        let file_len = match tokio::fs::metadata(dest).await {
            Ok(m) => m.len() as i64,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
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
                active: Some(false),
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

    // Use repaired segment checkpoints, not the independently persisted task
    // counter. No worker has started, so this contains restored bytes only.
    let baseline = total_downloaded.load(Ordering::Relaxed);
    if progress_tx
        .send(ProgressUpdate {
            task_id: task_id.to_owned(),
            downloaded_bytes: baseline,
            download_baseline: Some(baseline),
            total_bytes,
            status: 1,
            ..Default::default()
        })
        .await
        .is_err()
    {
        tracing::debug!("FTP progress receiver closed");
        return Err(DownloadError::Cancelled);
    }

    // Limit concurrent FTP connections to avoid server-side per-IP limits
    // (most FTP servers cap at 5-10 simultaneous connections per IP).
    let ftp_semaphore = Arc::new(tokio::sync::Semaphore::new(max_connections));
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
        let tracker = tracker.clone();
        let sink = sink.clone();
        let parallelism = max_connections as u32;

        let handle = tokio::spawn(async move {
            // 排队中的段遇到取消要立即退出，否则每个排队段都会先完整握手再发现已取消。
            let _permit = tokio::select! {
                _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                p = sem.acquire() => match p {
                    Ok(p) => p,
                    Err(_) => return Err(DownloadError::Cancelled),
                },
            };
            if cancel.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }
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
                &tracker,
                sink.as_ref(),
                spawn_gen,
                parallelism,
            )
            .await
        });
        handles.push(handle);
    }

    let mut final_error = None;
    for handle in handles {
        let result = match handle.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => {
                tracing::debug!("FTP worker task cancelled");
                Err(DownloadError::Cancelled)
            }
            Err(error) => Err(DownloadError::Other(format!(
                "FTP worker join failed: {error}"
            ))),
        };
        if let Err(error) = result {
            if !matches!(error, DownloadError::Cancelled) {
                cancel_token.cancel();
            }
            if final_error.is_none()
                || matches!(final_error, Some(DownloadError::Cancelled))
                    && !matches!(error, DownloadError::Cancelled)
            {
                final_error = Some(error);
            } else if !matches!(error, DownloadError::Cancelled) {
                crate::logger::report_warning(
                    "ftp-download",
                    "stop another segment after failure",
                    &error,
                );
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
    tracker: &TransferTracker,
    sink: &dyn EventSink,
    spawn_gen: i64,
    parallelism: u32,
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
            tracker,
            spawn_gen,
            parallelism,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(e);
                }
                attempts += 1;
                if attempts >= MAX_RETRIES {
                    return Err(e);
                }
                let segs = db.load_segments(task_id).await?;
                if let Some(seg) = segs.iter().find(|s| s.index == seg_idx) {
                    actual_start = seg_start + seg.downloaded_bytes;
                    if actual_start > seg_end {
                        return Ok(());
                    }
                }
                if let Err(journal_error) = crate::task_activity::record(
                    db,
                    sink,
                    task_id,
                    "retry",
                    format!(
                        "FTP 段 {seg_idx} 第 {attempts}/{MAX_RETRIES} 次尝试失败，即将重试：{e}"
                    ),
                    None,
                )
                .await
                {
                    crate::log_error!("[task-activity] failed to persist retry: {}", journal_error);
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
    tracker: &TransferTracker,
    spawn_gen: i64,
    parallelism: u32,
) -> Result<(), DownloadError> {
    let bytes_needed = (seg_end - actual_start + 1) as u64;
    if cancel.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
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
        let tracker_reader = tracker.clone();

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

            let mut data_stream = open_data_with_epsv_fallback(
                &mut ftp,
                &ftp_url,
                proxy_opt,
                Some(actual_start as usize),
                |f| f.retr_as_stream(&ftp_url.path),
            )
            .map_err(|e| {
                DownloadError::Other(format!("FTP RETR error (seg {}): {}", seg_idx, e))
            })?;

            // Set read timeout so cancellation eventually unblocks this thread.
            data_stream
                .get_ref()
                .set_read_timeout(Some(FTP_DATA_READ_TIMEOUT))?;

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

                let read = {
                    let _transfer = tracker_reader.start(seg_idx);
                    data_stream.read(&mut buf[..to_read])
                };
                match read {
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
                                if let Err(error) = ftp.quit() {
                                    tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
                                }
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
                        if let Err(error) = ftp.quit() {
                            tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
                        }
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
                if let Err(error) = ftp.quit() {
                    tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
                }
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
                // 无法设置超时就停止本次传输，不能让 blocking reader 无界挂起。
                set_control_timeouts(ftp.get_ref())?;
                if let Err(error) = ftp.finalize_retr_stream(data_stream) {
                    crate::logger::report_warning(
                        "ftp-download",
                        "finalize known-size transfer",
                        &error,
                    );
                }
            }
            if let Err(error) = ftp.quit() {
                tracing::debug!(%error, "FTP session shutdown did not receive QUIT acknowledgement");
            }
            Ok(())
        })
    };

    let mut seg_downloaded = actual_start - seg_start;
    let mut durable_downloaded = seg_downloaded;
    let writer_result = async {
        let raw_file = OpenOptions::new().write(true).open(dest).await?;
        let mut file = tokio::io::BufWriter::with_capacity(BUF_WRITER_CAPACITY, raw_file);
        file.seek(std::io::SeekFrom::Start(actual_start as u64)).await?;
        let mut last_report = std::time::Instant::now();
        let mut last_db_save = std::time::Instant::now();
        loop {
            let bytes = tokio::select! {
                _ = cancel.cancelled() => {
                    file.flush().await?;
                    file.get_ref().sync_data().await?;
                    durable_downloaded = seg_downloaded;
                    db.update_segment_progress_bounded(task_id, seg_idx, durable_downloaded, seg_start, spawn_gen).await?;
                    return Err(DownloadError::Cancelled);
                }
                chunk = chunk_rx.recv() => match chunk { Some(bytes) => bytes, None => break },
            };
            let mut offset = 0;
            while offset < bytes.len() {
                let allowed = tokio::select! {
                    _ = cancel.cancelled() => {
                        file.flush().await?;
                        file.get_ref().sync_data().await?;
                        durable_downloaded = seg_downloaded;
                        db.update_segment_progress_bounded(task_id, seg_idx, durable_downloaded, seg_start, spawn_gen).await?;
                        return Err(DownloadError::Cancelled);
                    }
                    allowed = speed_limiter.consume((bytes.len() - offset) as u64) => allowed,
                };
                let end = offset + allowed as usize;
                file.write_all(&bytes[offset..end]).await?;
                let written = (end - offset) as i64;
                seg_downloaded += written;
                total_downloaded.fetch_add(written, Ordering::Relaxed);
                offset = end;
            }
            if let Ok(mut states) = seg_states.lock()
                && let Some(s) = states.iter_mut().find(|s| s.index == seg_idx)
            { s.downloaded_bytes = seg_downloaded; }
            if last_report.elapsed().as_millis() >= 200 && !progress_tx.is_closed() {
                let (mut snapshot, sample_sequence) = {
                    let states = seg_states.lock().unwrap_or_else(|e| e.into_inner());
                    (states.clone(), crate::transfer_activity::next_sample_sequence())
                };
                let current_total = snapshot.iter().map(|s| s.downloaded_bytes).sum();
                for segment in &mut snapshot { segment.active = Some(tracker.is_active(segment.index)); }
                let runtime = ftp_runtime(task_id, total_bytes, parallelism, tracker, &snapshot, sample_sequence);
                if progress_tx.send(ProgressUpdate {
                    task_id: task_id.to_owned(), downloaded_bytes: current_total, total_bytes,
                    status: 1, segment_details: Some(snapshot), runtime: Some(runtime),
                    ..Default::default()
                }).await.is_err() { tracing::debug!("FTP progress receiver closed"); }
                last_report = std::time::Instant::now();
            }
            if last_db_save.elapsed().as_secs() >= DB_SAVE_INTERVAL_SECS {
                file.flush().await?;
                file.get_ref().sync_data().await?;
                durable_downloaded = seg_downloaded;
                db.update_segment_progress_bounded(task_id, seg_idx, durable_downloaded, seg_start, spawn_gen).await?;
                last_db_save = std::time::Instant::now();
            }
        }
        file.flush().await?;
        file.get_ref().sync_data().await?;
        durable_downloaded = seg_downloaded;
        db.update_segment_progress_bounded(task_id, seg_idx, durable_downloaded, seg_start, spawn_gen).await?;
        Ok::<(), DownloadError>(())
    }.await;
    if let Ok(mut states) = seg_states.lock()
        && let Some(s) = states.iter_mut().find(|s| s.index == seg_idx)
    {
        s.downloaded_bytes = durable_downloaded;
    }
    if writer_result.is_err() {
        cancelled_writer.store(true, Ordering::SeqCst);
    }
    chunk_rx.close();
    cancel_watcher.abort();
    let watcher_error = match cancel_watcher.await {
        Ok(()) => None,
        Err(error) if error.is_cancelled() => None,
        Err(error) => Some(DownloadError::Other(format!(
            "FTP cancellation watcher join failed: {error}"
        ))),
    };
    let reader_result = ftp_reader
        .await
        .map_err(|e| DownloadError::Other(format!("FTP segment reader join error: {e}")))
        .and_then(|result| result);
    if let Err(error) = writer_result {
        if let Some(watcher_error) = watcher_error {
            crate::logger::report_warning(
                "ftp-download",
                "stop cancellation watcher after writer failure",
                &watcher_error,
            );
        }
        if let Err(reader_error) = reader_result {
            crate::logger::report_warning(
                "ftp-download",
                "stop segment reader after writer failure",
                &reader_error,
            );
        }
        return Err(error);
    }
    if let Some(error) = watcher_error {
        if let Err(reader_error) = reader_result {
            crate::logger::report_warning(
                "ftp-download",
                "stop reader after watcher failure",
                &reader_error,
            );
        }
        return Err(error);
    }
    reader_result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{hex_nibble, parse_ftp_url, url_decode};
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use std::sync::Arc;
    use std::sync::atomic::Ordering;

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
    fn url_decode_undecodable_bytes_fall_back_to_windows_1252() {
        // 0xFF 0xFE 既不是合法 UTF-8 也不是合法 GBK/Big5/Shift_JIS 序列：旧式字节解码恒成功，
        // 末位兜底按 windows-1252 解码，不再回退成原始 `%FF%FE` 字符串。
        let result = url_decode("%FF%FE");
        assert_eq!(result, "ÿþ");
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

    #[test]
    fn parse_ftp_url_at_in_path_is_not_userinfo() {
        let u = parse_ftp_url("ftp://ftp.example.com/pub/icon@2x.png")
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(u.host, "ftp.example.com");
        assert_eq!(u.username, "anonymous");
        assert_eq!(u.path, "/pub/icon@2x.png");

        let u =
            parse_ftp_url("ftp://u:p@ss@host:2121/dir/a@b.txt").unwrap_or_else(|_| unreachable!());
        assert_eq!(u.host, "host");
        assert_eq!(u.port, 2121);
        assert_eq!(u.username, "u");
        assert_eq!(u.password, "p@ss");
        assert_eq!(u.path, "/dir/a@b.txt");
    }

    #[test]
    fn parse_ftp_url_rejects_control_chars() {
        assert!(parse_ftp_url("ftp://h/%0D%0ADELE%20x").is_err());
        assert!(parse_ftp_url("ftp://h/a%00b").is_err());
        assert!(parse_ftp_url("ftp://u%0D%0ASITE:p@h/f").is_err());
        assert!(parse_ftp_url("ftp://u:p%0Aq@h/f").is_err());
        assert!(parse_ftp_url("ftp://u:p@h/f%20g").is_ok());
    }

    #[test]
    fn redact_ftp_url_strips_userinfo_only() {
        assert_eq!(
            super::redact_ftp_url("ftp://user:p@ss@host:21/dir/a@b.txt"),
            "ftp://host:21/dir/a@b.txt"
        );
        assert_eq!(
            super::redact_ftp_url("ftp://host/icon@2x.png"),
            "ftp://host/icon@2x.png"
        );
    }

    #[test]
    fn no_proxy_matching() {
        use super::host_matches_no_proxy as m;
        assert!(m("nas.local", "*.local"));
        assert!(m("nas.local", ".local, other"));
        assert!(m("192.168.1.5", "192.168.0.0/16"));
        assert!(m("localhost", "localhost;x"));
        assert!(m("anything", "*"));
        assert!(!m("example.com", "*.local,192.168.0.0/16"));
        assert!(!m("notlocal", ".local"));
        assert!(!m("10.0.0.1", "192.168.0.0/16"));
    }

    #[test]
    fn passive_setup_failures_are_distinguished_from_rejected_retr() {
        use suppaftp::{FtpError, Status, types::Response};
        let reply =
            |code: u32| FtpError::UnexpectedResponse(Response::new(Status::from(code), vec![]));
        assert!(super::is_passive_setup_failure(&reply(502)));
        assert!(super::is_passive_setup_failure(&reply(500)));
        assert!(super::is_passive_setup_failure(&reply(425)));
        assert!(super::is_passive_setup_failure(&FtpError::ConnectionError(
            std::io::Error::from(std::io::ErrorKind::TimedOut)
        )));
        assert!(super::is_passive_setup_failure(&FtpError::BadResponse));
        // RETR 本身被拒（文件不存在/无权限）换 EPSV 也无济于事。
        assert!(!super::is_passive_setup_failure(&reply(550)));
        assert!(!super::is_passive_setup_failure(&reply(450)));
    }

    #[test]
    fn no_proxy_hit_counts_as_direct_connection() {
        use crate::proxy_config::{ProxyConfig, ProxyMode, ProxyType};
        let url = super::FtpUrl {
            host: "nas.local".to_string(),
            port: 21,
            username: String::new(),
            password: String::new(),
            path: "/f".to_string(),
        };
        let proxy = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Socks5,
            host: "127.0.0.1".to_string(),
            port: 1080,
            username: String::new(),
            password: String::new(),
            no_proxy_list: "*.local".to_string(),
            rules: None,
        };
        assert!(!super::connects_via_proxy(&url, Some(&proxy)));
        let remote = super::FtpUrl {
            host: "ftp.example.com".to_string(),
            ..url
        };
        assert!(super::connects_via_proxy(&remote, Some(&proxy)));
        assert!(!super::connects_via_proxy(&remote, None));
    }

    /// 最小 FTP 服务器:`refuse_pasv` 时 PASV 回 502(只支持 EPSV);`reject_retr` 时
    /// RETR 回 550。返回 `(控制端口, 控制连接数, 收到的命令)`。
    #[allow(clippy::type_complexity)]
    fn spawn_fake_ftp(
        refuse_pasv: bool,
        reject_retr: bool,
        payload: &'static [u8],
    ) -> std::io::Result<(
        u16,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::Mutex<Vec<String>>>,
    )> {
        spawn_fake_ftp_with_mdtm(refuse_pasv, reject_retr, payload, None)
    }

    /// 同 [`spawn_fake_ftp`]；`mdtm = Some("YYYYMMDDhhmmss")` 时 MDTM 回 213，否则 502。
    #[allow(clippy::type_complexity)]
    fn spawn_fake_ftp_with_mdtm(
        refuse_pasv: bool,
        reject_retr: bool,
        payload: &'static [u8],
        mdtm: Option<&'static str>,
    ) -> std::io::Result<(
        u16,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::Mutex<Vec<String>>>,
    )> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let commands = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let (conn_count, seen) = (Arc::clone(&connections), Arc::clone(&commands));
        std::thread::spawn(move || {
            for control in listener.incoming() {
                let Ok(control) = control else { return };
                conn_count.fetch_add(1, Ordering::SeqCst);
                let seen = Arc::clone(&seen);
                std::thread::spawn(move || {
                    let Ok(reader_half) = control.try_clone() else {
                        return;
                    };
                    let mut reader = BufReader::new(reader_half);
                    let mut control = control;
                    let mut data_listener: Option<TcpListener> = None;
                    if let Err(error) = control.write_all(b"220 ready\r\n") {
                        tracing::debug!(%error, "test FTP client closed connection");
                        return;
                    }
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) => return,
                            Ok(_) => {}
                            Err(error) => {
                                tracing::debug!(%error, "test FTP control connection closed");
                                return;
                            }
                        }
                        let command = line.trim().to_string();
                        if let Ok(mut seen) = seen.lock() {
                            seen.push(command.clone());
                        }
                        let verb = command.split_whitespace().next().unwrap_or("");
                        let reply: String = match verb {
                            "USER" => "331 need password\r\n".to_string(),
                            "PASS" => "230 logged in\r\n".to_string(),
                            "TYPE" => "200 binary\r\n".to_string(),
                            "SIZE" => format!("213 {}\r\n", payload.len()),
                            "REST" => "350 restarting\r\n".to_string(),
                            "MDTM" => match mdtm {
                                Some(time) => format!("213 {time}\r\n"),
                                None => "502 MDTM not implemented\r\n".to_string(),
                            },
                            "PASV" if refuse_pasv => "502 PASV not implemented\r\n".to_string(),
                            "PASV" | "EPSV" => {
                                let Ok(data) = TcpListener::bind(("127.0.0.1", 0)) else {
                                    return;
                                };
                                let data_port =
                                    data.local_addr().map(|a| a.port()).unwrap_or_default();
                                data_listener = Some(data);
                                if verb == "PASV" {
                                    format!(
                                        "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                                        data_port >> 8,
                                        data_port & 0xff
                                    )
                                } else {
                                    format!(
                                        "229 Entering Extended Passive Mode (|||{data_port}|)\r\n"
                                    )
                                }
                            }
                            "RETR" if reject_retr => "550 no such file\r\n".to_string(),
                            "RETR" => {
                                if let Err(error) =
                                    control.write_all(b"150 opening data connection\r\n")
                                {
                                    tracing::debug!(%error, "test FTP client closed connection");
                                    return;
                                }
                                if let Some(data) = data_listener.take()
                                    && let Ok((mut stream, _)) = data.accept()
                                    && let Err(error) = stream.write_all(payload)
                                {
                                    tracing::debug!(%error, "test FTP client closed connection");
                                    return;
                                }
                                "226 transfer complete\r\n".to_string()
                            }
                            "QUIT" => {
                                if let Err(error) = control.write_all(b"221 bye\r\n") {
                                    tracing::debug!(%error, "test FTP client closed connection");
                                    return;
                                }
                                return;
                            }
                            _ => "502 not implemented\r\n".to_string(),
                        };
                        if let Err(error) = control.write_all(reply.as_bytes()) {
                            tracing::debug!(%error, "test FTP client closed connection");
                            return;
                        }
                    }
                });
            }
        });
        Ok((port, connections, commands))
    }

    #[tokio::test]
    async fn completion_persistence_failure_publishes_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_ftp_completion_failure_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir).await?;
        let db_url = crate::db::sqlite_url(&dir.join("completion.db"));
        let (port, _, _) = spawn_fake_ftp(false, false, b"completed-file")?;
        let mut engine = crate::Engine::new(
            crate::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir.to_string_lossy().into_owned(),
                app_data_dir: dir.to_string_lossy().into_owned(),
                bt_config: crate::bt_config::BtConfig::default(),
                proxy_config: crate::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: Some(db_url.clone()),
            },
            Arc::new(crate::NoopSink),
            Arc::new(crate::NoopSelection),
        )
        .await?;
        let injector = sqlx::AnyPool::connect(&db_url).await?;
        sqlx::query("CREATE TRIGGER reject_completion BEFORE UPDATE OF status ON tasks WHEN NEW.status = 3 BEGIN SELECT RAISE(FAIL, 'completion write rejected'); END")
            .execute(&injector).await?;
        let mut done_rx = engine.manager.take_done_rx().expect("done receiver");
        let mut progress_rx = engine
            .manager
            .take_progress_rx()
            .expect("progress receiver");
        let id = engine
            .manager
            .create_task(crate::download_manager::NewTaskSpec {
                url: format!("ftp://u:p@127.0.0.1:{port}/file.bin"),
                save_dir: dir.to_string_lossy().into_owned(),
                file_name: "file.bin".to_owned(),
                segments: 1,
                ..Default::default()
            })
            .await
            .expect("create FTP task");
        let done = tokio::time::timeout(std::time::Duration::from_secs(10), done_rx.recv())
            .await?
            .expect("worker reports completion");
        assert_eq!(done.task_id, id);
        engine.manager.on_task_done(&done).await;
        let task = engine.db.load_task_by_id(&id).await?.expect("task remains");
        assert_eq!(task.status, 4, "worker exit must not leave a running row");
        assert!(task.error_message.contains("completion write rejected"));
        assert_eq!(
            tokio::fs::read(dir.join("file.bin")).await?,
            b"completed-file"
        );
        let mut terminal_error = None;
        while let Ok(progress) = progress_rx.try_recv() {
            assert_ne!(
                progress.status, 3,
                "failed persistence must not publish success"
            );
            if progress.status == 4 {
                terminal_error = Some(progress);
            }
        }
        let terminal_error = terminal_error.expect("terminal error reaches the consumer");
        assert_eq!(terminal_error.task_id, id);
        assert!(
            terminal_error
                .error_message
                .contains("completion write rejected")
        );
        engine.manager.shutdown().await;
        injector.close().await;
        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
        Ok(())
    }

    /// 经完整引擎跑一个 FTP 任务，返回 `(临时目录, 下载文件, 服务器收到的命令)`。
    async fn run_ftp_task_with_server_time(
        use_server_time: bool,
        mdtm: Option<&'static str>,
        payload: &'static [u8],
    ) -> Result<(PathBuf, PathBuf, Vec<String>), Box<dyn std::error::Error>> {
        let dir =
            std::env::temp_dir().join(format!("fluxdown_ftp_server_time_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await?;
        let db_url = crate::db::sqlite_url(&dir.join("server_time.db"));
        let (port, _, commands) = spawn_fake_ftp_with_mdtm(false, false, payload, mdtm)?;
        let mut engine = crate::Engine::new(
            crate::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir.to_string_lossy().into_owned(),
                app_data_dir: dir.to_string_lossy().into_owned(),
                bt_config: crate::bt_config::BtConfig::default(),
                proxy_config: crate::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: Some(db_url),
            },
            Arc::new(crate::NoopSink),
            Arc::new(crate::NoopSelection),
        )
        .await?;
        engine.manager.set_use_server_time(use_server_time);
        let mut done_rx = engine.manager.take_done_rx().expect("done receiver");
        let id = engine
            .manager
            .create_task(crate::download_manager::NewTaskSpec {
                url: format!("ftp://u:p@127.0.0.1:{port}/file.bin"),
                save_dir: dir.to_string_lossy().into_owned(),
                file_name: "file.bin".to_owned(),
                segments: 1,
                ..Default::default()
            })
            .await
            .expect("create FTP task");
        let done = tokio::time::timeout(std::time::Duration::from_secs(10), done_rx.recv())
            .await?
            .expect("worker reports completion");
        engine.manager.on_task_done(&done).await;
        let task = engine.db.load_task_by_id(&id).await?.expect("task remains");
        assert_eq!(
            task.status, 3,
            "download must succeed: {}",
            task.error_message
        );
        engine.manager.shutdown().await;
        drop(engine);
        let seen = commands.lock().map(|c| c.clone()).unwrap_or_default();
        let file = dir.join("file.bin");
        assert_eq!(tokio::fs::read(&file).await?, payload);
        Ok((dir, file, seen))
    }

    async fn remove_test_dir(dir: &Path) {
        if let Err(error) = tokio::fs::remove_dir_all(dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "test directory cleanup failed");
        }
    }

    #[tokio::test]
    async fn server_time_applies_mdtm_to_completed_file() -> Result<(), Box<dyn std::error::Error>>
    {
        let (dir, file, seen) =
            run_ftp_task_with_server_time(true, Some("20260318122819"), b"mdtm-payload").await?;
        let mtime = std::fs::metadata(&file)?.modified()?;
        assert_eq!(
            mtime,
            std::time::UNIX_EPOCH + Duration::from_secs(1_773_836_899),
            "file mtime must equal the server MDTM time (UTC)"
        );
        assert!(seen.iter().any(|c| c == "MDTM /file.bin"), "{seen:?}");
        remove_test_dir(&dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn server_time_without_mdtm_support_keeps_local_time()
    -> Result<(), Box<dyn std::error::Error>> {
        let before = std::time::SystemTime::now() - Duration::from_secs(5);
        let (dir, file, seen) = run_ftp_task_with_server_time(true, None, b"no-mdtm").await?;
        assert!(std::fs::metadata(&file)?.modified()? >= before);
        assert!(seen.iter().any(|c| c.starts_with("MDTM")), "{seen:?}");
        remove_test_dir(&dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn server_time_disabled_skips_mdtm_and_keeps_local_time()
    -> Result<(), Box<dyn std::error::Error>> {
        let before = std::time::SystemTime::now() - Duration::from_secs(5);
        let (dir, file, seen) =
            run_ftp_task_with_server_time(false, Some("20260318122819"), b"disabled").await?;
        assert!(std::fs::metadata(&file)?.modified()? >= before);
        assert!(!seen.iter().any(|c| c.starts_with("MDTM")), "{seen:?}");
        remove_test_dir(&dir).await;
        Ok(())
    }

    fn fake_ftp_url(port: u16) -> super::FtpUrl {
        super::FtpUrl {
            host: "127.0.0.1".to_string(),
            port,
            username: "u".to_string(),
            password: "p".to_string(),
            path: "/file.bin".to_string(),
        }
    }

    #[tokio::test]
    async fn failed_checkpoint_flush_does_not_advance_resume_progress()
    -> Result<(), Box<dyn std::error::Error>> {
        use tokio::io::AsyncWriteExt;
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        db.insert_task(
            "checkpoint",
            "ftp://localhost/f",
            "f",
            "",
            1,
            6,
            "",
            "",
            "",
            1,
        )
        .await?;
        let path =
            std::env::temp_dir().join(format!("fluxdown_ftp_checkpoint_{}", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, b"old").await?;
        let file = tokio::fs::File::open(&path).await?;
        let mut writer = tokio::io::BufWriter::with_capacity(1024, file);
        writer.write_all(b"new").await?;
        assert!(matches!(
            super::persist_ftp_single_progress(&mut writer, &db, "checkpoint", 6).await,
            Err(super::DownloadError::Io(_))
        ));
        assert_eq!(
            db.load_task_by_id("checkpoint")
                .await?
                .ok_or("missing task")?
                .downloaded_bytes,
            0
        );
        assert_eq!(tokio::fs::read(&path).await?, b"old");
        drop(writer);
        tokio::fs::remove_file(path).await?;
        Ok(())
    }

    #[tokio::test]
    async fn missing_ftp_partial_file_emits_zero_baseline_before_new_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        const PAYLOAD: &[u8] = b"ftp-baseline-payload";
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        db.insert_task(
            "baseline-reset",
            "ftp://localhost/f",
            "f",
            "",
            1,
            PAYLOAD.len() as i64,
            "",
            "",
            "",
            1,
        )
        .await?;
        db.update_task_progress("baseline-reset", 8).await?;
        let dir =
            std::env::temp_dir().join(format!("fluxdown_ftp_baseline_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await?;
        let dest = dir.join("missing-partial");
        let (port, _, _) = spawn_fake_ftp(false, false, PAYLOAD)?;
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::ftp_download_single(
                "baseline-reset",
                &fake_ftp_url(port),
                &dest,
                PAYLOAD.len() as i64,
                true,
                &db,
                &tx,
                &tokio_util::sync::CancellationToken::new(),
                &crate::speed_limiter::SpeedLimiter::new(0),
                &crate::proxy_config::ProxyConfig::default(),
                &crate::transfer_activity::TransferTracker::new(),
            ),
        )
        .await??;
        let baseline = rx
            .try_recv()
            .expect("actual baseline precedes body progress");
        assert_eq!(baseline.download_baseline, Some(0));
        assert_eq!(baseline.downloaded_bytes, 0);
        assert_eq!(tokio::fs::read(&dest).await?, PAYLOAD);
        remove_test_dir(&dir).await;
        Ok(())
    }

    #[tokio::test]
    async fn writer_failure_joins_reader_and_preserves_first_error()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::downloader::DownloadError;
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        db.insert_task(
            "writer-error",
            "ftp://localhost/f",
            "f",
            "",
            1,
            7,
            "",
            "",
            "",
            1,
        )
        .await?;
        let dest = std::env::temp_dir().join(format!(
            "fluxdown_ftp_writer_error_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dest).await?;
        // The local directory cannot be opened as an output file; RETR also fails.
        let (port, _, _) = spawn_fake_ftp(false, true, b"")?;
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::ftp_download_single(
                "writer-error",
                &fake_ftp_url(port),
                &dest,
                7,
                false,
                &db,
                &tx,
                &tokio_util::sync::CancellationToken::new(),
                &crate::speed_limiter::SpeedLimiter::new(0),
                &crate::proxy_config::ProxyConfig::default(),
                &crate::transfer_activity::TransferTracker::new(),
            ),
        )
        .await?;
        assert!(
            matches!(result, Err(DownloadError::Io(_))),
            "writer I/O error must survive reader RETR failure: {result:?}"
        );
        assert_eq!(
            db.load_task_by_id("writer-error")
                .await?
                .ok_or("missing task")?
                .downloaded_bytes,
            0
        );
        tokio::fs::remove_dir(dest).await?;
        Ok(())
    }

    #[test]
    fn retr_falls_back_to_epsv_when_pasv_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        const PAYLOAD: &[u8] = b"epsv-fallback-payload";
        let (port, connections, commands) = spawn_fake_ftp(true, false, PAYLOAD)?;
        let url = fake_ftp_url(port);
        let mut ftp = super::ftp_connect_sync_with_proxy(&url, None)?;

        let mut stream = super::open_data_with_epsv_fallback(&mut ftp, &url, None, Some(5), |f| {
            f.retr_as_stream(&url.path)
        })?;
        let mut received = Vec::new();
        stream.read_to_end(&mut received)?;

        assert_eq!(received, PAYLOAD);
        assert_eq!(connections.load(Ordering::SeqCst), 2);
        let seen = commands.lock().map(|c| c.clone()).unwrap_or_default();
        assert!(seen.iter().any(|c| c == "PASV"), "{seen:?}");
        assert!(seen.iter().any(|c| c == "EPSV"), "{seen:?}");
        // 新连接上要重新发 REST，断点偏移不能丢。
        assert!(seen.iter().any(|c| c == "REST 5"), "{seen:?}");
        Ok(())
    }

    #[test]
    fn retr_rejection_does_not_trigger_epsv_fallback() -> Result<(), Box<dyn std::error::Error>> {
        let (port, connections, commands) = spawn_fake_ftp(false, true, b"")?;
        let url = fake_ftp_url(port);
        let mut ftp = super::ftp_connect_sync_with_proxy(&url, None)?;

        let result = super::open_data_with_epsv_fallback(&mut ftp, &url, None, None, |f| {
            f.retr_as_stream(&url.path)
        });

        assert!(result.is_err());
        assert_eq!(connections.load(Ordering::SeqCst), 1);
        let seen = commands.lock().map(|c| c.clone()).unwrap_or_default();
        assert!(!seen.iter().any(|c| c == "EPSV"), "{seen:?}");
        Ok(())
    }
}
