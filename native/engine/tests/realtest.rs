//! 确定性真实下载测试床 —— 用一个**完全受控的本地 HTTP/1.1 服务器**驱动
//! FluxDown 引擎的真实代码路径（`run_coordinated_download` / `download_single` /
//! `resolve_file_info`），并能注入对抗行为：不支持 Range、Content-Length 撒谎、
//! ETag 中途变化、gzip、断流重试、每连接返回不同字节（CDN 不一致）等。
//!
//! 相比 `corruption_test.rs`（依赖阿里云镜像、不可控、慢），本模块：
//!   - 确定性：body 由固定种子生成，SHA 可预测，离线可跑（仅绑定 127.0.0.1）。
//!   - 可注入故障：精确复现 multi-thread 下载器的经典损坏场景。
//!   - 完整复用引擎真实代码（BufWriter / fallocate / 拆分协调 / 续传 / 重试）。
//!
//! 用法：
//!   cargo test -p hub --lib realtest -- --ignored --nocapture --test-threads=1
//!
//! 默认 `#[ignore]`（绑定端口 + 略慢），需显式 `--ignored` 运行。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fluxdown_engine::db::Db;
use fluxdown_engine::downloader::{ProgressUpdate, RequestSpec, build_client, resolve_file_info};
use fluxdown_engine::events::{EngineEvent, EventSink};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::segment_coordinator::run_coordinated_download;
use fluxdown_engine::speed_limiter::SpeedLimiter;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

/// 测试专用 no-op sink——仅关心下载产物字节是否正确。
struct NoopTestSink;
impl EventSink for NoopTestSink {
    fn emit(&self, _event: EngineEvent) {}
}

// ===========================================================================
// 测试用本地 HTTP/1.1 服务器
// ===========================================================================

/// 控制服务器对抗行为的可变状态（跨连接共享）。
struct ServerState {
    /// 当前 body（可被 swap 钩子替换以模拟“文件中途变化”）。
    body: Mutex<Arc<Vec<u8>>>,
    /// 当前 ETag（随 body 一起切换）。
    etag: Mutex<String>,
    last_modified: String,
    content_type: String,

    /// false → 忽略所有 Range 请求，永远返回 200 全量，且不发 Accept-Ranges。
    support_range: bool,
    /// 即便支持 range 也可隐藏 Accept-Ranges 头（靠 206 证明 range 支持）。
    advertise_accept_ranges: bool,
    /// 200 全量响应里上报的 Content-Length（None = 诚实上报真实长度）。
    fake_full_content_length: Option<i64>,

    // --- 故障注入钩子 ---
    /// 在累计第 N 个 *range* GET 完成后，把 body/etag 切换为 (新 body, 新 etag)。
    swap_after_range_gets: Option<(usize, Arc<Vec<u8>>, String)>,
    /// 对第 N 个 *range* GET（1-indexed），只写响应体前 K 字节然后强行关闭连接
    /// （模拟传输中途断流 → 触发引擎重试/续传）。仅生效一次。
    drop_range_get_nth: Option<(usize, usize)>,
    /// 对第 N 个 *range* GET（1-indexed），把响应体每个字节 XOR 0xFF 后发出
    /// （模拟 CDN 返回损坏/不一致字节）。
    corrupt_range_get_nth: Option<usize>,
    /// 全量 GET 返回 `Content-Encoding: gzip` + 该 gzip 字节（模拟服务器无视
    /// Accept-Encoding: identity 仍压缩）。设置时通常配合 support_range=false。
    gzip_body: Option<Arc<Vec<u8>>>,
    /// 覆盖全量 GET 的 Content-Encoding 头值（如 "gzip, gzip" 多层 / "compress" 未知）。
    /// 设置时优先于 gzip_body 自动添加的 "gzip"。仅影响头部，不改 body。
    content_encoding_override: Option<String>,
    /// 全量 GET 不发 Content-Length（靠连接关闭定界），模拟 chunked/无 CL 服务器。
    omit_content_length_full: bool,
    /// 全量 GET 只发前 K 字节然后关闭（模拟传输中途断流/截断）。
    close_full_after: Option<usize>,
    /// 运行时关闭 close_full_after（续传第二程需要服务器恢复完整传输）。
    disable_close_full: std::sync::atomic::AtomicBool,
    /// 206 响应是否携带 ETag/Last-Modified。false=剥离（模拟 CDN 边缘节点行为）。
    emit_validators_on_range: bool,
    /// 钩子 A（永久型）：对所有「分段」range GET（区间长度 end-start+1 > 1）强制
    /// 返回 200 全量，保留 probe 的 `bytes=0-0`（长度 1）走 206——复现“服务器支持
    /// Range，但对分段请求持续返回 200”（alist 代理迅雷/光鸭云盘真实行为）。
    force_full_on_segment_range: bool,
    /// 钩子 B（一次性）：置为 true 后，下一次「分段」range GET 强制返回一次 200
    /// 全量（消费后自动复位为 false，其余请求照常 206）——复现“瞬时 200”。
    force_full_range_get_once: std::sync::atomic::AtomicBool,
    /// 206 `Content-Range` 分母（服务器自报总大小）注入序列：每个 206 依次弹出
    /// 一个值作为分母（body 照常按真实区间发送）；序列耗尽后回落为诚实的 body
    /// 长度。模拟【渐进上传中文件不断增长】/病态分母膨胀（BUG-HTTP-HINT-UNDERSIZED
    /// 的扩容配额路径）。注意：注入值必须 >= 请求区间末尾+1 才自洽。
    range_total_sequence: std::sync::Mutex<Vec<i64>>,

    // --- 计数器（断言用）---
    head_count: AtomicUsize,
    full_get_count: AtomicUsize,
    range_get_count: AtomicUsize,
    swapped: AtomicUsize,
}

impl ServerState {
    fn new(body: Arc<Vec<u8>>, etag: &str) -> Self {
        Self {
            body: Mutex::new(body),
            etag: Mutex::new(etag.to_string()),
            last_modified: "Wed, 21 Oct 2025 07:28:00 GMT".to_string(),
            content_type: "application/octet-stream".to_string(),
            support_range: true,
            advertise_accept_ranges: true,
            fake_full_content_length: None,
            swap_after_range_gets: None,
            drop_range_get_nth: None,
            corrupt_range_get_nth: None,
            gzip_body: None,
            content_encoding_override: None,
            omit_content_length_full: false,
            close_full_after: None,
            disable_close_full: std::sync::atomic::AtomicBool::new(false),
            emit_validators_on_range: true,
            force_full_on_segment_range: false,
            force_full_range_get_once: std::sync::atomic::AtomicBool::new(false),
            range_total_sequence: std::sync::Mutex::new(Vec::new()),
            head_count: AtomicUsize::new(0),
            full_get_count: AtomicUsize::new(0),
            range_get_count: AtomicUsize::new(0),
            swapped: AtomicUsize::new(0),
        }
    }
}

/// 运行中的测试服务器句柄；drop 时停掉 accept 循环。
struct TestServer {
    addr: std::net::SocketAddr,
    /// 保持 `Arc<ServerState>` 存活并与 accept 循环共享所有权；测试用例
    /// 通过各自持有的 `Arc<ServerState>` clone 修改对抗行为标志,不经过
    /// 这个字段读取,但它是 `state` 生命周期与 `TestServer` 绑定的必要持有点。
    #[allow(dead_code)]
    state: Arc<ServerState>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

/// 启动本地服务器，返回句柄。监听 127.0.0.1:0（随机端口）。
async fn start_server(state: Arc<ServerState>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local_addr");
    let st = state.clone();
    let accept_task = tokio::spawn(async move {
        while let Ok((stream, _peer)) = listener.accept().await {
            let st2 = st.clone();
            tokio::spawn(async move {
                let _ = handle_conn(stream, st2).await;
            });
        }
    });
    TestServer {
        addr,
        state,
        accept_task,
    }
}

/// 解析出的请求要素。
struct ParsedReq {
    method: String,
    path: String,
    /// (start, end_inclusive_or_None) —— 来自 `Range: bytes=S-E` 或 `bytes=S-`。
    range: Option<(i64, Option<i64>)>,
    /// `If-Range` 头原始值（ETag 带引号 或 HTTP-date）。
    if_range: Option<String>,
}

/// 读取并解析一个 HTTP 请求（GET/HEAD 无 body，读到 \r\n\r\n 即可）。
async fn read_request(stream: &mut TcpStream) -> Option<ParsedReq> {
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    // 读到头部结束符
    loop {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None; // 对端关闭
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 64 * 1024 {
            return None; // 防御：头部过大
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut range = None;
    let mut if_range = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let nm = name.trim();
            if nm.eq_ignore_ascii_case("range") {
                range = parse_range(value.trim());
            } else if nm.eq_ignore_ascii_case("if-range") {
                if_range = Some(value.trim().to_string());
            }
        }
    }
    Some(ParsedReq {
        method,
        path,
        range,
        if_range,
    })
}

/// 解析 `bytes=S-E` / `bytes=S-`。
fn parse_range(v: &str) -> Option<(i64, Option<i64>)> {
    let v = v.strip_prefix("bytes=")?;
    let (s, e) = v.split_once('-')?;
    let start: i64 = s.trim().parse().ok()?;
    let end = {
        let e = e.trim();
        if e.is_empty() {
            None
        } else {
            Some(e.parse::<i64>().ok()?)
        }
    };
    Some((start, end))
}

async fn write_all(stream: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    stream.write_all(data).await
}

/// 处理单个连接：解析请求 → 按状态生成响应 → 关闭（Connection: close）。
async fn handle_conn(mut stream: TcpStream, st: Arc<ServerState>) -> std::io::Result<()> {
    let req = match read_request(&mut stream).await {
        Some(r) => r,
        None => return Ok(()),
    };

    // 重定向钩子：/redirect → 302 到 /file
    if req.path == "/redirect" {
        let body = st.body.lock().await.clone();
        let _ = body; // not used directly
        let resp = "HTTP/1.1 302 Found\r\nLocation: /file\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        write_all(&mut stream, resp.as_bytes()).await?;
        let _ = stream.shutdown().await;
        return Ok(());
    }

    let body = st.body.lock().await.clone();
    let etag = st.etag.lock().await.clone();
    let total = body.len() as i64;

    if std::env::var("RT_TRACE").is_ok() {
        eprintln!(
            "[srv] {} {} range={:?} total={}",
            req.method, req.path, req.range, total
        );
    }

    let is_head = req.method.eq_ignore_ascii_case("HEAD");
    if is_head {
        st.head_count.fetch_add(1, Ordering::SeqCst);
        let mut h = String::new();
        h.push_str("HTTP/1.1 200 OK\r\n");
        // 真实 gzip 服务器：HEAD 报告【压缩后】Content-Length 并带 Content-Encoding。
        let cl = match (&st.gzip_body, st.fake_full_content_length) {
            (_, Some(f)) => f,
            (Some(g), None) => g.len() as i64,
            (None, None) => total,
        };
        h.push_str(&format!("Content-Length: {}\r\n", cl));
        if let Some(ce) = &st.content_encoding_override {
            h.push_str(&format!("Content-Encoding: {}\r\n", ce));
        } else if st.gzip_body.is_some() {
            h.push_str("Content-Encoding: gzip\r\n");
        }
        if st.support_range && st.advertise_accept_ranges {
            h.push_str("Accept-Ranges: bytes\r\n");
        }
        h.push_str(&format!("ETag: \"{}\"\r\n", etag));
        h.push_str(&format!("Last-Modified: {}\r\n", st.last_modified));
        h.push_str(&format!("Content-Type: {}\r\n", st.content_type));
        h.push_str("Connection: close\r\n\r\n");
        write_all(&mut stream, h.as_bytes()).await?;
        let _ = stream.shutdown().await;
        return Ok(());
    }

    // GET
    // If-Range 语义（真实服务器行为）：若客户端带了 If-Range 且 validator 与当前
    // 版本不一致（ETag 变化 或 Last-Modified 变化），服务器**忽略 Range，返回 200
    // 全量当前文件**。FluxDown 的续传/分段修复正是依赖这一点来检出"文件中途变化"。
    let if_range_matches = match &req.if_range {
        None => true, // 无 If-Range → 正常按 Range 处理
        Some(v) => v == &format!("\"{}\"", etag) || v == &st.last_modified,
    };
    let wants_range = req.range.is_some() && st.support_range && if_range_matches;

    // 区间长度：probe 用 `bytes=0-0`（长度 1）；真实分段请求远大于 1。用这个
    // 区分，让下面两个故障注入钩子只作用于“分段” range GET，不影响 probe——
    // probe 仍需 206 才能让客户端判定“服务器支持 Range”从而选择多段下载。
    let is_segment_range_get = wants_range
        && req
            .range
            .map(|(s, e)| e.unwrap_or(total - 1).min(total - 1) - s + 1 > 1)
            .unwrap_or(false);
    // 钩子 A（永久型）：所有分段 range GET 强制走 200 全量分支（保留 probe 走 206），
    // 复现“服务器支持 Range，但对分段请求偶发/持续返回 200”（alist 代理云盘行为）。
    // 钩子 B（一次性）：消费一次「武装」标志，让下一次分段 range GET 强制返回一次
    // 200（随后自动复位、恢复 206），模拟“瞬时 200”。
    let force_full = is_segment_range_get
        && (st.force_full_on_segment_range
            || st.force_full_range_get_once.swap(false, Ordering::SeqCst));
    let wants_range = wants_range && !force_full;

    if !wants_range {
        // 200 全量
        st.full_get_count.fetch_add(1, Ordering::SeqCst);
        // gzip 模式：发压缩体，Content-Length=压缩长度，附 Content-Encoding: gzip。
        let send_body: Vec<u8> = match &st.gzip_body {
            Some(g) => g.as_ref().clone(),
            None => body.as_ref().clone(),
        };
        let cl = st
            .fake_full_content_length
            .unwrap_or(send_body.len() as i64);
        let mut h = String::new();
        h.push_str("HTTP/1.1 200 OK\r\n");
        if !st.omit_content_length_full {
            h.push_str(&format!("Content-Length: {}\r\n", cl));
        }
        if let Some(ce) = &st.content_encoding_override {
            h.push_str(&format!("Content-Encoding: {}\r\n", ce));
        } else if st.gzip_body.is_some() {
            h.push_str("Content-Encoding: gzip\r\n");
        }
        if st.support_range && st.advertise_accept_ranges {
            h.push_str("Accept-Ranges: bytes\r\n");
        }
        h.push_str(&format!("ETag: \"{}\"\r\n", etag));
        h.push_str(&format!("Last-Modified: {}\r\n", st.last_modified));
        h.push_str(&format!("Content-Type: {}\r\n", st.content_type));
        h.push_str("Connection: close\r\n\r\n");
        write_all(&mut stream, h.as_bytes()).await?;
        if let Some(k) = st.close_full_after
            && !st.disable_close_full.load(Ordering::SeqCst)
        {
            let k = k.min(send_body.len());
            let _ = write_all(&mut stream, &send_body[..k]).await;
            let _ = stream.shutdown().await;
            return Ok(());
        }
        write_all(&mut stream, &send_body).await?;
        let _ = stream.shutdown().await;
        return Ok(());
    }

    // 206 partial
    let (start, end_opt) = req.range.expect("range present");
    let end = end_opt.unwrap_or(total - 1).min(total - 1);
    if start < 0 || start > end || start >= total {
        // 416 Range Not Satisfiable
        let h = format!(
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            total
        );
        write_all(&mut stream, h.as_bytes()).await?;
        let _ = stream.shutdown().await;
        return Ok(());
    }

    let n = st.range_get_count.fetch_add(1, Ordering::SeqCst) + 1; // 1-indexed

    let slice_start = start as usize;
    let slice_end = (end + 1) as usize;
    let mut chunk = body[slice_start..slice_end].to_vec();

    // 损坏注入：第 N 个 range GET 的字节被篡改
    if let Some(corrupt_n) = st.corrupt_range_get_nth
        && n == corrupt_n
    {
        for b in chunk.iter_mut() {
            *b ^= 0xFF;
        }
    }

    let len = chunk.len() as i64;
    let mut h = String::new();
    h.push_str("HTTP/1.1 206 Partial Content\r\n");
    h.push_str(&format!("Content-Length: {}\r\n", len));
    // 分母注入：序列非空时弹出首个值伪造服务器自报总大小（模拟文件仍在增长）。
    let reported_total = {
        let mut seq = st
            .range_total_sequence
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if seq.is_empty() { total } else { seq.remove(0) }
    };
    h.push_str(&format!(
        "Content-Range: bytes {}-{}/{}\r\n",
        start, end, reported_total
    ));
    h.push_str("Accept-Ranges: bytes\r\n");
    if st.emit_validators_on_range {
        h.push_str(&format!("ETag: \"{}\"\r\n", etag));
        h.push_str(&format!("Last-Modified: {}\r\n", st.last_modified));
    }
    h.push_str(&format!("Content-Type: {}\r\n", st.content_type));
    h.push_str("Connection: close\r\n\r\n");
    write_all(&mut stream, h.as_bytes()).await?;

    // 断流注入：第 N 个 range GET 只写前 K 字节然后关闭
    if let Some((drop_n, k)) = st.drop_range_get_nth
        && n == drop_n
    {
        let k = k.min(chunk.len());
        let _ = write_all(&mut stream, &chunk[..k]).await;
        // 直接关闭，制造不完整传输
        let _ = stream.shutdown().await;
        return Ok(());
    }

    write_all(&mut stream, &chunk).await?;
    if std::env::var("RT_TRACE").is_ok() {
        eprintln!(
            "[srv] sent 206 {}-{} ({} bytes), closing",
            start,
            end,
            chunk.len()
        );
    }
    let _ = stream.shutdown().await;

    // body 切换注入：达到阈值后替换 body+etag（模拟下载中文件变化）
    if let Some((after, ref new_body, ref new_etag)) = st.swap_after_range_gets
        && n >= after
        && st.swapped.swap(1, Ordering::SeqCst) == 0
    {
        *st.body.lock().await = new_body.clone();
        *st.etag.lock().await = new_etag.clone();
    }

    Ok(())
}

// ===========================================================================
// 测试工具
// ===========================================================================

/// 确定性伪随机 body（xorshift64*），保证内容非平凡（能暴露偏移/拼接错误）。
fn gen_body(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.max(1);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        let v = x.wrapping_mul(0x2545F4914F6CDD1D);
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn sha256_bytes(b: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b);
    hex_str(&hasher.finalize())
}

async fn sha256_file(path: &Path) -> String {
    use tokio::io::AsyncReadExt;
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => return format!("<open-error: {e}>"),
    };
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        match file.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) => return format!("<read-error: {e}>"),
        }
    }
    hex_str(&hasher.finalize())
}

fn hex_str(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn test_client() -> reqwest::Client {
    let proxy = ProxyConfig::default();
    build_client(&proxy, "FluxDownRealTest/1.0").expect("build_client")
}

fn unique_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fluxdown_realtest_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&d).expect("create work dir");
    d
}

fn drain(rx: mpsc::Receiver<ProgressUpdate>) -> tokio::task::JoinHandle<()> {
    let mut rx = rx;
    tokio::spawn(async move { while rx.recv().await.is_some() {} })
}

/// 用引擎真实 coordinator 跑一次多段下载，返回 (Result, dest path)。
async fn run_coord(
    work_dir: &Path,
    task_id: &str,
    url: &str,
    total: i64,
    segments: i32,
    etag: &str,
    cancel: &CancellationToken,
) -> (
    Result<(), fluxdown_engine::downloader::DownloadError>,
    std::path::PathBuf,
) {
    let dest = work_dir.join(format!("{task_id}.bin"));
    let client = test_client();
    let db = Db::open(work_dir).await.expect("Db::open");
    db.insert_task(
        task_id,
        url,
        &dest.file_name().unwrap().to_string_lossy(),
        &work_dir.to_string_lossy(),
        segments,
        total,
        "",
        "",
        "",
    )
    .await
    .expect("insert_task");

    let speed_limiter = SpeedLimiter::new(0);
    let (tx, rx) = mpsc::channel::<ProgressUpdate>(256);
    let dh = drain(rx);
    let spec = RequestSpec::empty_get();
    let sink = NoopTestSink;

    let res = run_coordinated_download(
        task_id,
        url,
        &dest,
        total,
        false,
        segments,
        &client,
        &db,
        &tx,
        cancel,
        &speed_limiter,
        &spec,
        &sink,
        etag,
        "",
    )
    .await;
    drop(tx);
    let _ = dh.await;
    (res.map(|_| ()), dest)
}

// ===========================================================================
// 测试 1：多段正确性矩阵（多尺寸 × 多段数 × 多次）
// ===========================================================================

/// 在完全正常的服务器上，反复用真实 coordinator 多段下载，逐字节对比 SHA。
/// 覆盖各种 (文件大小, 段数) 组合 —— 直接暴露段边界 off-by-one / 拼接 / 偏移错误。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn multi_segment_correctness_matrix() {
    let work_dir = unique_dir("matrix");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    // (size, seed)
    let sizes: &[(usize, u64)] = &[
        (1, 1),
        (2, 2),
        (255, 3),
        (256, 4),
        (257, 5),
        (1023, 6),
        (1024, 7),
        (1025, 8),
        (65_535, 9),
        (65_537, 10),
        (1_000_003, 11), // 素数大小，最易暴露段切分错误
        (4_194_304, 12), // 4 MiB
        (5_000_000, 13),
    ];
    let segment_counts: &[i32] = &[1, 2, 3, 4, 8, 16, 32];

    let mut failures: Vec<String> = Vec::new();
    let mut total_runs = 0usize;

    for &(size, seed) in sizes {
        let body = Arc::new(gen_body(size, seed));
        let expected = sha256_bytes(&body);
        let st = Arc::new(ServerState::new(body.clone(), &format!("etag-{seed}")));
        let server = start_server(st).await;
        let url = server.url("/file");

        for &segs in segment_counts {
            // 仅测试 chunk >= 1 的合理组合（段数不超过文件字节数）。
            // 段数 > 文件大小属于退化输入，由 coordinator_handles_degenerate_segment_count 单独覆盖。
            if (segs as usize) > size {
                continue;
            }
            total_runs += 1;
            let task_id = format!("matrix-{size}-{segs}");
            eprintln!("[matrix] START size={size} segs={segs}");
            let cancel = CancellationToken::new();
            let run = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                run_coord(&work_dir, &task_id, &url, size as i64, segs, "", &cancel),
            )
            .await;
            let (res, dest) = match run {
                Ok(v) => v,
                Err(_) => {
                    failures.push(format!("size={size} segs={segs}: 超时（疑似死循环/hang）"));
                    continue;
                }
            };

            match res {
                Ok(()) => {
                    let got_size = tokio::fs::metadata(&dest)
                        .await
                        .map(|m| m.len() as i64)
                        .unwrap_or(-1);
                    let got_sha = sha256_file(&dest).await;
                    if got_size != size as i64 {
                        failures.push(format!(
                            "size={size} segs={segs}: 大小不符 期望 {size} 实得 {got_size}"
                        ));
                    } else if got_sha != expected {
                        failures.push(format!(
                            "size={size} segs={segs}: SHA 损坏\n  期望 {expected}\n  实得 {got_sha}"
                        ));
                    }
                }
                Err(e) => {
                    failures.push(format!("size={size} segs={segs}: 下载错误 {e}"));
                }
            }
            let _ = tokio::fs::remove_file(&dest).await;
            // 清理 DB 段，避免下次复用脏状态（每个 task_id 唯一其实已隔离）
        }
        drop(server);
    }

    println!("\n=== 多段正确性矩阵：共 {total_runs} 次组合 ===");
    if failures.is_empty() {
        println!("✅ 全部逐字节一致");
    } else {
        println!("❌ {} 处失败：", failures.len());
        for f in &failures {
            println!("  - {f}");
        }
    }
    assert!(
        failures.is_empty(),
        "多段下载出现 {} 处损坏/错误",
        failures.len()
    );
}

/// 隔离复现：2 字节单段下载是否稳定 hang。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn repro_small_single_segment() {
    for &size in &[1usize, 2, 3, 10, 100] {
        let work_dir = unique_dir(&format!("repro-{size}"));
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
        tokio::fs::create_dir_all(&work_dir).await.unwrap();
        let body = Arc::new(gen_body(size, size as u64 + 7));
        let expected = sha256_bytes(&body);
        let st = Arc::new(ServerState::new(body.clone(), "et"));
        let server = start_server(st).await;
        let url = server.url("/file");
        let cancel = CancellationToken::new();
        eprintln!("[repro] START size={size} segs=1");
        let run = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_coord(
                &work_dir,
                &format!("r{size}"),
                &url,
                size as i64,
                1,
                "",
                &cancel,
            ),
        )
        .await;
        match run {
            Err(_) => eprintln!("[repro] size={size} segs=1: ❌ HANG"),
            Ok((Ok(()), dest)) => {
                let got = sha256_file(&dest).await;
                eprintln!(
                    "[repro] size={size} segs=1: {} (sha {})",
                    if got == expected {
                        "✅ OK"
                    } else {
                        "❌ CORRUPT"
                    },
                    &got[..8]
                );
            }
            Ok((Err(e), _)) => eprintln!("[repro] size={size} segs=1: ERR {e}"),
        }
        drop(server);
    }
}

// ===========================================================================
// 测试 1b：退化段数（段数 > 文件字节数）——绝不能 hang，应正确完成或报错
// ===========================================================================

/// `run_coordinated_download` 的防御检查只挡 total_bytes<=0 / segment_count<1，
/// 但未按文件大小钳制段数。当 segment_count > total_bytes 时，
/// build_fresh_segments 的 `chunk = total/count = 0` 会产生大量 start>end 的空段。
/// 本测试确保这种退化输入**不会 hang**，且最终结果要么是正确文件、要么是明确错误。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn coordinator_handles_degenerate_segment_count() {
    let work_dir = unique_dir("degen");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let cases: &[(usize, i32)] = &[(1, 32), (2, 32), (5, 64), (10, 100), (3, 8)];
    let mut hangs: Vec<String> = Vec::new();
    let mut corruptions: Vec<String> = Vec::new();

    for &(size, segs) in cases {
        let body = Arc::new(gen_body(size, (size as u64) * 31 + segs as u64));
        let expected = sha256_bytes(&body);
        let st = Arc::new(ServerState::new(body.clone(), "et-degen"));
        let server = start_server(st).await;
        let url = server.url("/file");
        let task_id = format!("degen-{size}-{segs}");
        let cancel = CancellationToken::new();

        let run = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            run_coord(&work_dir, &task_id, &url, size as i64, segs, "", &cancel),
        )
        .await;

        match run {
            Err(_) => {
                hangs.push(format!("size={size} segs={segs}: HANG（15s 超时）"));
            }
            Ok((Ok(()), dest)) => {
                let got = sha256_file(&dest).await;
                let got_size = tokio::fs::metadata(&dest)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);
                if got != expected {
                    corruptions.push(format!(
                        "size={size} segs={segs}: 报告成功但 SHA 损坏 (size={got_size})"
                    ));
                } else {
                    println!("size={size} segs={segs}: ✅ 成功且正确");
                }
            }
            Ok((Err(e), _)) => {
                println!("size={size} segs={segs}: 明确报错（可接受）: {e}");
            }
        }
        drop(server);
    }

    println!("\n=== 退化段数测试 ===");
    if hangs.is_empty() && corruptions.is_empty() {
        println!("✅ 无 hang、无静默损坏");
    } else {
        for h in &hangs {
            println!("  ❌ {h}");
        }
        for c in &corruptions {
            println!("  ❌ {c}");
        }
    }
    assert!(hangs.is_empty(), "退化段数导致 hang：{:?}", hangs);
    assert!(
        corruptions.is_empty(),
        "退化段数导致静默损坏：{:?}",
        corruptions
    );
}

// ===========================================================================
// 测试 2：断点续传正确性（取消中途 → 续传 → 最终一致）
// ===========================================================================

/// 多段下载中途 cancel，制造部分完成的 DB 段 + 部分磁盘文件，
/// 再用同一 task_id 续传，最终 SHA 必须与完整下载一致。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn resume_after_cancel_is_byte_exact() {
    let work_dir = unique_dir("resume");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let size = 8_000_003usize; // ~8MB 素数
    let body = Arc::new(gen_body(size, 42));
    let expected = sha256_bytes(&body);
    let st = Arc::new(ServerState::new(body.clone(), "etag-resume"));
    let server = start_server(st.clone()).await;
    let url = server.url("/file");

    let task_id = "resume-task";
    let dest = work_dir.join(format!("{task_id}.bin"));
    let client = test_client();
    let db = Db::open(&work_dir).await.expect("db");
    db.insert_task(
        task_id,
        &url,
        &dest.file_name().unwrap().to_string_lossy(),
        &work_dir.to_string_lossy(),
        8,
        size as i64,
        "",
        "",
        "",
    )
    .await
    .unwrap();

    // ---- 第一程：启动后短暂运行即 cancel ----
    let speed_limiter = SpeedLimiter::new(0);
    let (tx, rx) = mpsc::channel::<ProgressUpdate>(256);
    let dh = drain(rx);
    let spec = RequestSpec::empty_get();
    let sink = NoopTestSink;
    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();
    // 在收到一定进度后取消（这里用定时器近似“中途”）
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        cancel2.cancel();
    });

    let first = run_coordinated_download(
        task_id,
        &url,
        &dest,
        size as i64,
        false,
        8,
        &client,
        &db,
        &tx,
        &cancel,
        &speed_limiter,
        &spec,
        &sink,
        "",
        "",
    )
    .await;
    drop(tx);
    let _ = dh.await;
    let _ = canceller.await;
    println!("第一程结果: {:?}", first.as_ref().map(|_| "ok"));

    let partial_len = tokio::fs::metadata(&dest)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let segs_in_db = db.load_segments(task_id).await.unwrap();
    let dl_in_db: i64 = segs_in_db.iter().map(|s| s.downloaded_bytes).sum();
    println!(
        "中断后：磁盘 {partial_len} 字节，DB 记录已下载 {dl_in_db} 字节，段数 {}",
        segs_in_db.len()
    );

    // ---- 第二程：续传到完成 ----
    let (tx2, rx2) = mpsc::channel::<ProgressUpdate>(256);
    let dh2 = drain(rx2);
    let cancel_done = CancellationToken::new();
    let second = run_coordinated_download(
        task_id,
        &url,
        &dest,
        size as i64,
        false,
        8,
        &client,
        &db,
        &tx2,
        &cancel_done,
        &speed_limiter,
        &spec,
        &sink,
        "",
        "",
    )
    .await;
    drop(tx2);
    let _ = dh2.await;
    second.expect("续传应成功");

    let got_size = tokio::fs::metadata(&dest).await.unwrap().len() as i64;
    let got_sha = sha256_file(&dest).await;
    println!("续传后：大小 {got_size}，SHA {got_sha}");
    assert_eq!(got_size, size as i64, "续传后大小不符");
    assert_eq!(got_sha, expected, "续传后 SHA 损坏 —— 续传偏移错误");
    drop(server);
}

// ===========================================================================
// 测试 3：传输中途断流 → 引擎内重试/续传，最终一致
// ===========================================================================

/// 服务器对第 2 个 range GET 只发一半字节就断开，引擎应在 do_segment_with_retry
/// 内重试该段剩余部分，最终文件逐字节正确。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn mid_transfer_drop_retries_and_completes() {
    let work_dir = unique_dir("drop");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let size = 4_000_037usize;
    let body = Arc::new(gen_body(size, 77));
    let expected = sha256_bytes(&body);
    let mut state = ServerState::new(body.clone(), "etag-drop");
    // 第 2 个 range GET 只写 1024 字节然后断开
    state.drop_range_get_nth = Some((2, 1024));
    let st = Arc::new(state);
    let server = start_server(st.clone()).await;
    let url = server.url("/file");

    let cancel = CancellationToken::new();
    let (res, dest) = run_coord(&work_dir, "drop-task", &url, size as i64, 4, "", &cancel).await;

    match res {
        Ok(()) => {
            let got_size = tokio::fs::metadata(&dest).await.unwrap().len() as i64;
            let got_sha = sha256_file(&dest).await;
            println!("断流重试后：大小 {got_size}，SHA {got_sha}");
            assert_eq!(got_size, size as i64, "断流重试后大小不符");
            assert_eq!(got_sha, expected, "断流重试后 SHA 损坏");
        }
        Err(e) => panic!("断流后引擎未能恢复: {e}"),
    }
    drop(server);
}

// ===========================================================================
// 测试 4：probe 正确性（range 支持 / 不支持 / 大小）
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn probe_detects_range_support_and_size() {
    let size = 1_234_567usize;
    let body = Arc::new(gen_body(size, 99));

    // 支持 range
    {
        let st = Arc::new(ServerState::new(body.clone(), "et"));
        let server = start_server(st).await;
        let client = test_client();
        let info = resolve_file_info(&client, &server.url("/file"), &RequestSpec::empty_get())
            .await
            .expect("probe");
        println!(
            "支持range: total={} supports_range={}",
            info.total_bytes, info.supports_range
        );
        assert_eq!(info.total_bytes, size as i64);
        assert!(info.supports_range, "应检测到 range 支持");
        drop(server);
    }

    // 不支持 range（200 全量、无 Accept-Ranges）
    {
        let mut s = ServerState::new(body.clone(), "et2");
        s.support_range = false;
        let st = Arc::new(s);
        let server = start_server(st).await;
        let client = test_client();
        let info = resolve_file_info(&client, &server.url("/file"), &RequestSpec::empty_get())
            .await
            .expect("probe2");
        println!(
            "不支持range: total={} supports_range={}",
            info.total_bytes, info.supports_range
        );
        assert_eq!(info.total_bytes, size as i64);
        assert!(!info.supports_range, "应检测到不支持 range");
        drop(server);
    }
}

// ===========================================================================
// 测试 5：服务器中途更换文件内容（ETag 变化）—— 多段拼接应被检出而非静默损坏
// ===========================================================================

/// 服务器在第 2 个 range GET 后把 body 整体替换（并改 ETag），
/// 模拟 CDN 在下载途中换了文件。引擎要么检出不一致并报错，要么
/// 重新基于一致版本完成；**绝不能**产出一个“新旧字节混合”的损坏文件却报告成功。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn file_changed_midway_must_not_silently_corrupt() {
    let work_dir = unique_dir("etagswap");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let size = 6_000_011usize;
    let body_v1 = Arc::new(gen_body(size, 100));
    let body_v2 = Arc::new(gen_body(size, 200)); // 完全不同内容、相同大小
    let sha_v1 = sha256_bytes(&body_v1);
    let sha_v2 = sha256_bytes(&body_v2);

    let mut state = ServerState::new(body_v1.clone(), "etag-v1");
    state.swap_after_range_gets = Some((2, body_v2.clone(), "etag-v2".to_string()));
    let st = Arc::new(state);
    let server = start_server(st.clone()).await;
    let url = server.url("/file");

    // 用 v1 的 etag 启动（模拟 probe 时看到的是 v1）
    let cancel = CancellationToken::new();
    let (res, dest) = run_coord(
        &work_dir,
        "swap-task",
        &url,
        size as i64,
        8,
        "\"etag-v1\"",
        &cancel,
    )
    .await;

    let got_sha = if dest.exists() {
        sha256_file(&dest).await
    } else {
        "<no file>".into()
    };
    println!("结果: {:?}", res.as_ref().map(|_| "ok"));
    println!("  最终 SHA: {got_sha}");
    println!("  v1 SHA  : {sha_v1}");
    println!("  v2 SHA  : {sha_v2}");

    // 可接受：报错；或最终等于 v1 或 v2 之一（自洽）。
    // 不可接受：报告成功且 SHA 既不等于 v1 也不等于 v2（混合损坏）。
    if res.is_ok() {
        let consistent = got_sha == sha_v1 || got_sha == sha_v2;
        assert!(
            consistent,
            "❌ 文件中途变化导致静默混合损坏：最终 SHA 既非 v1 也非 v2 —— 引擎报告成功但产出损坏文件"
        );
    }
    drop(server);
}

// ===========================================================================
// 完整 run_download 驱动（覆盖 probe → 单/多段决策 → 完整性校验 → checksum）
// ===========================================================================

/// 用 async-compression 把数据 gzip 压缩（构造“服务器无视 identity 仍压缩”场景）。
async fn gzip_bytes(data: &[u8]) -> Vec<u8> {
    use async_compression::tokio::write::GzipEncoder;
    use tokio::io::AsyncWriteExt;
    let mut enc = GzipEncoder::new(Vec::new());
    enc.write_all(data).await.expect("gzip write");
    enc.shutdown().await.expect("gzip shutdown");
    enc.into_inner()
}

/// 驱动 pub `run_download` 跑完整真实路径，返回 (最终状态码, dest 路径)。
/// 状态码：3=成功，4=失败（与 DB/UI 语义一致）。
#[allow(clippy::too_many_arguments)]
async fn run_full(
    work_dir: &Path,
    db: &Db,
    task_id: &str,
    url: &str,
    file_name: &str,
    segment_count: i32,
    hint_file_size: i64,
    is_resume: bool,
    checksum: &str,
    cancel: &CancellationToken,
) -> (i32, std::path::PathBuf) {
    use fluxdown_engine::downloader::{DownloadParams, run_download};

    let client = test_client();
    let speed_limiter = SpeedLimiter::new(0);
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(256);
    let last_status = Arc::new(std::sync::atomic::AtomicI32::new(0));
    let ls = last_status.clone();
    let collector = tokio::spawn(async move {
        while let Some(u) = rx.recv().await {
            if u.status >= 3 {
                ls.store(u.status, std::sync::atomic::Ordering::SeqCst);
            }
        }
    });

    let params = DownloadParams {
        task_id: task_id.to_string(),
        url: url.to_string(),
        save_dir: work_dir.to_string_lossy().to_string(),
        file_name: file_name.to_string(),
        segment_count,
        is_resume,
        db: db.clone(),
        client,
        progress_tx: tx,
        cancel_token: cancel.clone(),
        speed_limiter,
        cookies: String::new(),
        referrer: String::new(),
        hint_file_size,
        proxy_config: ProxyConfig::default(),
        sink: std::sync::Arc::new(NoopTestSink),
        selector: std::sync::Arc::new(fluxdown_engine::NoopSelection),
        checksum: checksum.to_string(),
        extra_headers: std::collections::HashMap::new(),
        spec: RequestSpec::empty_get(),
        audio_url: None,
    };

    run_download(params).await;
    let _ = collector.await;
    let status = last_status.load(std::sync::atomic::Ordering::SeqCst);
    (status, work_dir.join(file_name))
}

async fn insert_simple_task(
    db: &Db,
    work_dir: &Path,
    task_id: &str,
    url: &str,
    name: &str,
    segs: i32,
    total: i64,
) {
    db.insert_task(
        task_id,
        url,
        name,
        &work_dir.to_string_lossy(),
        segs,
        total,
        "",
        "",
        "",
    )
    .await
    .expect("insert_task");
}

// ---------------------------------------------------------------------------
// BUG-HTTP-DECOMPRESS-INTEGRITY：gzip 单流下载解压后被完整性校验误杀
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn gzip_single_stream_should_succeed() {
    let work_dir = unique_dir("gzip");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let plain = gen_body(300_000, 555);
    let expected = sha256_bytes(&plain);
    let gz = gzip_bytes(&plain).await;
    eprintln!("[gzip] plain={} gz={}", plain.len(), gz.len());

    let mut s = ServerState::new(Arc::new(plain.clone()), "etg");
    s.support_range = false; // 强制单流
    s.advertise_accept_ranges = false;
    s.gzip_body = Some(Arc::new(gz)); // 全量 GET 返回 gzip 体
    let st = Arc::new(s);
    let server = start_server(st).await;
    let url = server.url("/file");

    let db = Db::open(&work_dir).await.expect("db");
    insert_simple_task(&db, &work_dir, "gz", &url, "out.bin", 0, 0).await;
    let cancel = CancellationToken::new();
    let (status, dest) = run_full(
        &work_dir, &db, "gz", &url, "out.bin", 0, 0, false, "", &cancel,
    )
    .await;

    let got = if dest.exists() {
        sha256_file(&dest).await
    } else {
        "<missing>".into()
    };
    eprintln!(
        "[gzip] status={status} dest_exists={} sha={}",
        dest.exists(),
        got
    );
    // 正确行为：解压后写盘的明文应被接受为完成，且内容正确。
    assert_eq!(
        status, 3,
        "gzip 单流下载应成功（解压后正确文件不该被完整性校验拒绝）"
    );
    assert_eq!(got, expected, "解压内容应与原始明文一致");
    drop(server);
}

// ---------------------------------------------------------------------------
// BUG-HTTP-LAYERED-ENCODING：多层压缩（gzip, gzip）只能解一层 → 必须报错而非静默损坏
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn layered_content_encoding_must_error_not_corrupt() {
    let work_dir = unique_dir("layered");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let plain = gen_body(200_000, 4321);
    let gz = gzip_bytes(&plain).await; // body 仅单层；引擎应据【头部】"gzip, gzip" 判定多层并报错

    let mut s = ServerState::new(Arc::new(plain), "etl");
    s.support_range = false;
    s.advertise_accept_ranges = false;
    s.gzip_body = Some(Arc::new(gz));
    s.content_encoding_override = Some("gzip, gzip".to_string()); // 多层
    let st = Arc::new(s);
    let server = start_server(st).await;
    let url = server.url("/file");

    let db = Db::open(&work_dir).await.expect("db");
    insert_simple_task(&db, &work_dir, "ly", &url, "out.bin", 0, 0).await;
    let cancel = CancellationToken::new();
    let (status, dest) = run_full(
        &work_dir, &db, "ly", &url, "out.bin", 0, 0, false, "", &cancel,
    )
    .await;
    eprintln!("[layered] status={status} dest_exists={}", dest.exists());
    // 正确行为：无法完整解码的多层压缩必须报错（status=4），绝不静默落盘残留压缩字节。
    assert_eq!(
        status, 4,
        "❌ 多层 Content-Encoding 被静默接受——只解一层、内层压缩字节当成功落盘"
    );
    drop(server);
}

// ---------------------------------------------------------------------------
// BUG-HTTP-NO-CL-TRUNCATION：已知大小 + 无 Content-Length + 中途断流 → 截断文件被当成功
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn no_content_length_truncation_must_not_be_accepted() {
    let work_dir = unique_dir("nocl");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let full = gen_body(500_000, 888);
    let truncated_at = 123_456usize;

    let mut s = ServerState::new(Arc::new(full.clone()), "etn");
    s.support_range = false; // 单流
    s.advertise_accept_ranges = false;
    s.omit_content_length_full = true; // 下载响应不发 Content-Length
    s.close_full_after = Some(truncated_at); // 只发前 123456 字节就关闭
    let st = Arc::new(s);
    let server = start_server(st).await;
    let url = server.url("/file");

    let db = Db::open(&work_dir).await.expect("db");
    // 走 probe 路径（hint=0）：HEAD 报告可信的完整大小 N；下载响应却无 CL 且只
    // 收到 K<N 字节。可信大小 + 截断 + 无 CL → 必须判失败而非把截断文件当完成。
    insert_simple_task(&db, &work_dir, "nc", &url, "out.bin", 0, 0).await;
    let cancel = CancellationToken::new();
    let (status, dest) = run_full(
        &work_dir, &db, "nc", &url, "out.bin", 0, 0, false, "", &cancel,
    )
    .await;

    let dlen = if dest.exists() {
        tokio::fs::metadata(&dest)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    eprintln!(
        "[nocl] status={status} dest_exists={} len={} (期望完整 {})",
        dest.exists(),
        dlen,
        full.len()
    );
    // 正确行为：已知完整大小却只收到部分字节且无 CL 兜底 → 必须判失败，绝不能把截断文件当完成。
    if status == 3 {
        assert_eq!(
            dlen as usize,
            full.len(),
            "❌ 截断文件（{}/{}）被当作成功完成——静默数据丢失",
            dlen,
            full.len()
        );
    }
    drop(server);
}

// ---------------------------------------------------------------------------
// BUG-HTTP-HINT-UNDERSIZED（真正根因）：扩展 hint 小于服务器真实大小 →
// 多段只请求 [0, hint) 区间，拿满即判完成 → 静默截断
// ---------------------------------------------------------------------------
//
// 这是用户实证「视频反复下载不完整」的**真实根因**（由 downloader 真实日志坐实，
// 推翻了最初的“无 CL 断流”猜测）。关键日志：
//   [ExtDownSvc] received request: ... size=2585179          ← 扩展抓到的大小
//   [download] using hint: size=2585179 (probe skipped, hint=2585179)
//   [download] static advice: segments=2, reason=file=2585179 bytes
//   [download] hint mode: skipping bandwidth probe
//   [download] mode=multi-segment, segments=2
//   [download] completed, total=2585179 bytes               ← 0.17s「完成」
// 文件真实大小是 4747867（次日、以及另一下载器同期都拿到完整文件，SHA 一致；
// 已下载的 2585179 字节经逐字节比对是完整文件的**干净前缀**，零损坏）。
//
// 因果链：这是一段仍在**渐进上传**的生成视频，扩展在 <video> Range 流式播放时
// 抓到的是**当时的部分大小** 2585179。downloader 的 hint 模式为「保护一次性签名
// URL」而**完全跳过 probe**，于是把这个偏小的 hint 当作权威总大小，多段只请求
// [0, hint) 区间，拿满即完成——**从不校验服务器自己在 206 响应
// `Content-Range: bytes X-Y/<total>` 分母里给出的真实总大小**。hint 偏小 → 静默
// 截断。两次都截在完全相同的 2585179，正因为那不是网络断流、而**就是 hint 本身**。
// 另一下载器免疫是因为它下载时自己 probe 拿到当时真实大小。
//
// 正确行为：hint 只应作为「跳过 probe 的乐观下限」；一旦下载响应（206 的
// Content-Range 分母、或 200 的 Content-Length）暴露出更大的真实总大小，必须据此
// 下满整文件，而不是停在 hint。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn hint_smaller_than_true_server_size_must_not_truncate() {
    let work_dir = unique_dir("hintunder");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    // 服务器真实文件 3_000_000；扩展 hint 只有 1_400_000（模拟渐进上传中途的部分大小）。
    let full = gen_body(3_000_000, 6063);
    let expected = sha256_bytes(&full);
    let hint = 1_400_000i64; // > 1MB → 触发多段（复现日志里的 segments=2 路径）

    // 诚实服务器：支持 Range，每个 206 都带 Content-Range .../3000000 暴露真实总大小。
    let st = Arc::new(ServerState::new(Arc::new(full.clone()), "etu"));
    let server = start_server(st).await;
    let url = server.url("/file");

    let db = Db::open(&work_dir).await.expect("db");
    insert_simple_task(&db, &work_dir, "hu", &url, "out.mp4", 2, hint).await;
    let cancel = CancellationToken::new();
    let (status, dest) = run_full(
        &work_dir, &db, "hu", &url, "out.mp4", 2, hint, false, "", &cancel,
    )
    .await;

    let dlen = if dest.exists() {
        tokio::fs::metadata(&dest)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    let got = if dest.exists() {
        sha256_file(&dest).await
    } else {
        "<missing>".into()
    };
    eprintln!(
        "[hintunder] status={status} len={} (hint={}, 服务器真实={})",
        dlen,
        hint,
        full.len()
    );
    // 正确行为：即便扩展 hint 偏小，服务器在 206 里暴露了真实总大小 → 必须下满整文件。
    // 断言【无条件】：status 必须为 3（完成）。若回归成 fail-instead-of-complete
    // （status=4）本测试必须变红，绝不可因 `if status == 3` 的旧写法被静默跳过。
    assert_eq!(
        status,
        3,
        "❌ hint({}) < 真实({})：必须以真实大小下满整文件并完成，而非报错跳过（status={}）",
        hint,
        full.len(),
        status
    );
    assert_eq!(
        dlen as usize,
        full.len(),
        "❌ hint({}) < 真实({})：只下 {} 字节就判完成——静默截断（复现用户视频不完整）",
        hint,
        full.len(),
        dlen
    );
    assert_eq!(got, expected, "内容应与完整文件一致");
    // 就地扩容（而非清空重规划）的结构性证据：最终 DB 段行里存在起点恰为原 hint
    // 边界的尾段 [1_400_000, ...]。清空重下会以真实大小重建均匀切分
    // （3_000_000/2 → 边界 1_500_000），绝不会出现 1_400_000 这个边界——回归成
    // 清空版（丢弃已下数据整体重下）时本断言变红。
    let segs = db.load_segments("hu").await.expect("load segments");
    assert!(
        segs.iter().any(|s| s.start_byte == hint),
        "❌ 应就地扩容追加尾段 [hint={}, 真实)（保留已下数据），而非清空重规划；实际段边界: {:?}",
        hint,
        segs.iter()
            .map(|s| (s.start_byte, s.end_byte))
            .collect::<Vec<_>>()
    );
    drop(server);
}

// ---------------------------------------------------------------------------
// BUG-HTTP-HINT-UNDERSIZED 容差窗口回归：hint 偏小的缺口【落在旧的 1% 漂移容差内】
// ---------------------------------------------------------------------------
//
// 修复前，do_segment 对 Content-Range 分母（服务器自报真实大小）一律套用 resume 端
// 的 CDN 漂移容差（1%，上限 1MB），仅当 true_total > total_bytes + 容差 才触发扩容。
// 这为 hint 模式重新打开了一条静默截断窗口：若 hint 的缺口【小于】容差，尾部数据会
// 被静默丢弃。本例：真实 3_000_000、hint 2_985_000，缺口 15_000 < 1% 容差 29_850 →
// 修复前会停在 2_985_000 判完成（截掉尾部 15 KB），且字节数校验通过、无从察觉。
//
// 修复：fresh hint 模式（size_is_estimate=true）对权威的 Content-Range 分母采【零容
// 差】——true_total > total_bytes（精确）即扩容。本测试证明该窗口已关闭：必须下满
// 整 3_000_000 字节且 SHA 一致。（resume 路径仍保留漂移容差，不受影响。）
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn hint_undersized_within_old_drift_tolerance_must_not_truncate() {
    let work_dir = unique_dir("hintdrift");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    // 服务器真实文件 3_000_000；hint 2_985_000：缺口 15_000 落在旧 1% 容差(29_850)【内】。
    let full = gen_body(3_000_000, 9271);
    let expected = sha256_bytes(&full);
    let hint = 2_985_000i64; // > 1MB → 多段路径；缺口 < 旧容差 → 修复前会静默截断

    // 诚实服务器：支持 Range，每个 206 都带 Content-Range .../3000000 暴露真实总大小。
    let st = Arc::new(ServerState::new(Arc::new(full.clone()), "etd"));
    let server = start_server(st).await;
    let url = server.url("/file");

    let db = Db::open(&work_dir).await.expect("db");
    insert_simple_task(&db, &work_dir, "hw", &url, "out.mp4", 2, hint).await;
    let cancel = CancellationToken::new();
    let (status, dest) = run_full(
        &work_dir, &db, "hw", &url, "out.mp4", 2, hint, false, "", &cancel,
    )
    .await;

    let dlen = if dest.exists() {
        tokio::fs::metadata(&dest)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    let got = if dest.exists() {
        sha256_file(&dest).await
    } else {
        "<missing>".into()
    };
    eprintln!(
        "[hintdrift] status={status} len={} (hint={}, 服务器真实={})",
        dlen,
        hint,
        full.len()
    );
    // 断言【无条件】：缺口虽落在旧容差内，仍必须以真实大小下满整文件并完成。
    assert_eq!(
        status,
        3,
        "❌ hint({}) 缺口 {} 落在旧 1% 容差内：必须扩容下满整文件并完成，而非报错跳过（status={}）",
        hint,
        full.len() as i64 - hint,
        status
    );
    assert_eq!(
        dlen as usize,
        full.len(),
        "❌ 缺口({})落在旧 1% 容差内被静默丢弃：只下 {} 字节就判完成——尾部截断",
        full.len() as i64 - hint,
        dlen
    );
    assert_eq!(got, expected, "内容应与完整文件一致");
    // 同上：缺口 15_000 落在旧容差内也必须走就地扩容——尾段起点恰为 hint 边界。
    let segs = db.load_segments("hw").await.expect("load segments");
    assert!(
        segs.iter().any(|s| s.start_byte == hint),
        "❌ 应就地扩容追加尾段 [hint={}, 真实)，而非清空重规划；实际段边界: {:?}",
        hint,
        segs.iter()
            .map(|s| (s.start_byte, s.end_byte))
            .collect::<Vec<_>>()
    );
    drop(server);
}

// ---------------------------------------------------------------------------
// BUG-HTTP-HINT-UNDERSIZED 扩容配额：分母持续膨胀（文件不停增长/病态服务器）
// ---------------------------------------------------------------------------
//
// 每个 206 的 Content-Range 分母都比引擎当前规划更大（注入严格递增序列，模拟
// 一段【始终在渐进上传】的文件或无限膨胀分母的病态服务器）。正确行为：
//   1. coordinator 就地扩容至多 MAX_SIZE_EXPANSIONS（3）次；
//   2. 仍在膨胀 → 以 TrueSizeLarger 显式失败（status=4），绝不静默把某一时刻的
//      前缀当完成（fail-loud）；
//   3. 失败时【不清数据】——DB 段行保留，用户重试时 resume 重新 probe 续下
//      （对照：清空版实现失败即全丢）。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn hint_expansion_quota_exhausted_fails_loud_and_keeps_data() {
    let work_dir = unique_dir("hintquota");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    // 真实 body 足够大，注入的分母序列全部 <= body 长度，服务器能自洽地服务
    // 任何落在注入分母之内的区间请求。
    let full = gen_body(4_000_000, 7177);
    let hint = 1_100_000i64; // > 1MB → 多段路径

    let s = ServerState::new(Arc::new(full.clone()), "etq");
    // 严格递增的分母序列：每个 206 都自报比引擎当前规划更大的总大小 →
    // 每次都触发扩容检查；3 次配额烧完后第 4 次触发必须 fail-loud。
    // 序列长度给足（40 个），耗尽后回落诚实 body 长度（4_000_000）仍大于
    // 任何中间规划值，兜底保证触发。
    {
        let mut seq = s.range_total_sequence.lock().unwrap();
        let mut v = hint;
        for _ in 0..40 {
            v += 60_000;
            seq.push(v.min(full.len() as i64));
        }
    }
    let st = Arc::new(s);
    let server = start_server(st).await;
    let url = server.url("/file");

    let db = Db::open(&work_dir).await.expect("db");
    insert_simple_task(&db, &work_dir, "hq", &url, "out.mp4", 2, hint).await;
    let cancel = CancellationToken::new();
    let (status, dest) = run_full(
        &work_dir, &db, "hq", &url, "out.mp4", 2, hint, false, "", &cancel,
    )
    .await;

    eprintln!("[hintquota] status={status} dest_exists={}", dest.exists());
    // 1) fail-loud：绝不能把某个中间规划的前缀当完成。
    assert_eq!(
        status, 4,
        "❌ 分母持续膨胀应在扩容配额耗尽后显式失败（status=4），而非报完成/其它（status={status}）"
    );
    assert!(
        !dest.exists(),
        "❌ 失败任务绝不能产出最终成品文件（finalize 泄漏）"
    );
    // 2) 数据保留：DB 段行仍在（resume 重新 probe 后可续），绝不清空。
    let segs = db.load_segments("hq").await.expect("load segments");
    assert!(
        !segs.is_empty(),
        "❌ 配额耗尽失败时必须保留 DB 段行（进度可续），实际被清空"
    );
    drop(server);
}

// ---------------------------------------------------------------------------
// BUG-HTTP-HINT-NO-CL-TRUNCATION（相邻空子，非本次事故成因）：hint 模式下，下载
// 响应无 Content-Length + 中途断流 → 干净 EOF 的截断文件被 downloader.rs 的
//   resp_cl <= 0 && file_len > 0 && (file_len >= total || hint_file_size > 0)
// 中 `|| hint_file_size > 0` 分支当成完成。这是调查上面那个真实事故时**顺带发现**
// 的另一处静默截断入口——用户本次事故不是它造成的（本次下载响应带的是分段 206，
// 非无 CL 断流），但它同样能把截断文件判成功，值得单独留一道回归防线。
// 本测试用 segment_count=1 强制单流，精确命中该空子（无多段协调器干扰）。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn hint_no_content_length_truncation_single_stream_must_not_be_accepted() {
    let work_dir = unique_dir("hintnocl");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let full = gen_body(500_000, 4041); // 模拟一段视频
    let truncated_at = 137_000usize; // CDN 中途断流处

    let mut s = ServerState::new(Arc::new(full.clone()), "etv");
    s.support_range = false; // 单流
    s.advertise_accept_ranges = false;
    s.omit_content_length_full = true; // 下载响应不发 Content-Length（连接关闭定界）
    s.close_full_after = Some(truncated_at); // 只发前 137000 字节就断
    let st = Arc::new(s);
    let server = start_server(st).await;
    let url = server.url("/file");

    let db = Db::open(&work_dir).await.expect("db");
    // hint_file_size = full.len()：扩展抓包看到的可信大小；segment_count=1 强制单流。
    insert_simple_task(&db, &work_dir, "hv", &url, "out.mp4", 1, full.len() as i64).await;
    let cancel = CancellationToken::new();
    let (status, dest) = run_full(
        &work_dir,
        &db,
        "hv",
        &url,
        "out.mp4",
        1,                 // segment_count=1 → 单流
        full.len() as i64, // hint_file_size > 0 → 跳过 probe，走 hint 旁路
        false,
        "",
        &cancel,
    )
    .await;

    let dlen = if dest.exists() {
        tokio::fs::metadata(&dest)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    eprintln!(
        "[hintnocl-single] status={status} dest_exists={} len={} (hint/期望完整 {})",
        dest.exists(),
        dlen,
        full.len()
    );
    // 正确行为：hint 可信总大小 + 截断 + 无 CL → 必须判失败，绝不能把截断文件当完成。
    // 断言【无条件】：截断的无 CL 单流必须被拒绝（status=4 错误），绝不能报完成（status=3）。
    assert_ne!(
        status,
        3,
        "❌ hint 旁路截断视频（{}/{}）被当作成功完成——截断的无 CL 单流必须判失败而非完成",
        dlen,
        full.len()
    );
    // 失败任务绝不能产出最终成品文件（.fluxdown 临时文件允许残留）。
    assert!(
        !dest.exists(),
        "❌ 截断的无 CL 单流被 finalize 成了成品文件"
    );
    drop(server);
}

// ---------------------------------------------------------------------------
// BUG-HTTP-HINT-NO-CL-TRUNCATION（多段回退面）：扩展 hint + 自动多段，CDN 对
// 分段请求不回 206（无视 Range）→ 回退单流 → 无 CL + 断流 → 截断被当成功
// ---------------------------------------------------------------------------
//
// 更贴近用户真实链路：扩展给了 hint 大小，视频 > 1MB，downloader 乐观假设支持
// Range 并发起多段下载；但 CDN 对真实分段 GET 不回 206（返回 200 全量，
// alist/签名回源常见），触发 `RangeNotSupported` → 回退单流。回退后的单流 GET
// 无 Content-Length 且中途断流，最终仍命中同一 `|| hint_file_size > 0` 空子被
// 当成完成。验证该空子在「多段→单流回退」路径上同样存在，回退不该丢掉完整性兜底。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn hint_no_content_length_truncation_via_range_fallback_must_not_be_accepted() {
    let work_dir = unique_dir("hintnoclms");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let full = gen_body(3_000_000, 5052); // > 1MB → 触发多段
    let truncated_at = 900_000usize;

    let mut s = ServerState::new(Arc::new(full.clone()), "etv2");
    s.support_range = false; // 对所有 Range 请求回 200 全量 → 触发 RangeNotSupported 回退单流
    s.advertise_accept_ranges = false;
    s.omit_content_length_full = true; // 下载响应无 Content-Length
    s.close_full_after = Some(truncated_at); // 中途断流
    let st = Arc::new(s);
    let server = start_server(st).await;
    let url = server.url("/file");

    let db = Db::open(&work_dir).await.expect("db");
    insert_simple_task(&db, &work_dir, "hm", &url, "out.mp4", 4, full.len() as i64).await;
    let cancel = CancellationToken::new();
    let (status, dest) = run_full(
        &work_dir,
        &db,
        "hm",
        &url,
        "out.mp4",
        4,                 // segment_count=4 → 乐观走多段（随后因 CDN 无视 Range 回退单流）
        full.len() as i64, // hint_file_size > 0 → 跳过 probe
        false,
        "",
        &cancel,
    )
    .await;

    let dlen = if dest.exists() {
        tokio::fs::metadata(&dest)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    eprintln!(
        "[hintnocl-multi] status={status} dest_exists={} len={} (hint/期望完整 {})",
        dest.exists(),
        dlen,
        full.len()
    );
    // 正确行为：多段回退单流后，hint 可信大小 + 截断 + 无 CL → 必须判失败。
    // 断言【无条件】：截断的无 CL 流必须被拒绝（status=4 错误），绝不能报完成（status=3）。
    assert_ne!(
        status,
        3,
        "❌ 多段回退单流后截断视频（{}/{}）被当作成功完成——截断的无 CL 流必须判失败而非完成",
        dlen,
        full.len()
    );
    // 失败任务绝不能产出最终成品文件（.fluxdown 临时文件允许残留）。
    assert!(
        !dest.exists(),
        "❌ 多段回退单流后截断文件被 finalize 成了成品文件"
    );
    drop(server);
}

// ---------------------------------------------------------------------------
// BUG-HTTP-SINGLE-RESUME-SPLICE：单流续传不带 If-Range，文件中途变化 → 新旧拼接
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn single_stream_resume_must_not_splice_changed_file() {
    let work_dir = unique_dir("splice");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let size = 400_000usize;
    let build_a = Arc::new(gen_body(size, 1001));
    let build_b = Arc::new(gen_body(size, 2002)); // 同长度，不同内容
    let sha_a = sha256_bytes(&build_a);
    let sha_b = sha256_bytes(&build_b);
    let cut = 150_000usize;

    let mut s = ServerState::new(build_a.clone(), "vA");
    s.support_range = true; // 支持 range（续传发 Range）
    s.close_full_after = Some(cut); // 第一程：全量 GET 发到 cut 就断（留下部分文件）
    let st = Arc::new(s);
    let server = start_server(st.clone()).await;
    let url = server.url("/file");

    let db = Db::open(&work_dir).await.expect("db");
    insert_simple_task(&db, &work_dir, "sp", &url, "out.bin", 1, size as i64).await;

    // 第一程：segment_count=1 强制单流；hint=0 走 probe（probe 看到支持 range）
    let cancel = CancellationToken::new();
    let (st1, dest) = run_full(
        &work_dir, &db, "sp", &url, "out.bin", 1, 0, false, "", &cancel,
    )
    .await;
    let partial = if dest.with_extension("bin.fdownloading").exists() {
        tokio::fs::metadata(dest.with_extension("bin.fdownloading"))
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    eprintln!(
        "[splice] 第一程 status={st1} 部分文件? dest_exists={} partial_temp={}",
        dest.exists(),
        partial
    );

    // 服务器切换到 build B（新版本，新 etag），并恢复完整传输（关闭提前断流）
    {
        *st.body.lock().await = build_b.clone();
        *st.etag.lock().await = "vB".to_string();
        st.disable_close_full
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    // 第二程：is_resume=true 续传
    let cancel2 = CancellationToken::new();
    let (st2, _dest2) = run_full(
        &work_dir, &db, "sp", &url, "out.bin", 1, 0, true, "", &cancel2,
    )
    .await;
    let got = if dest.exists() {
        sha256_file(&dest).await
    } else {
        "<missing>".into()
    };
    eprintln!("[splice] 第二程 status={st2} sha={got}\n  A={sha_a}\n  B={sha_b}");

    // 正确行为：续传检测到文件变化（If-Range），应整文件按 B 重下或报错；
    // 绝不能产出 A 前缀 + B 尾部 的拼接（既非 A 也非 B）。
    if st2 == 3 {
        assert!(
            got == sha_b || got == sha_a,
            "❌ 单流续传把变化的文件静默拼接：最终 SHA 既非 A 也非 B"
        );
    }
    drop(server);
}

// ---------------------------------------------------------------------------
// BUG-COORD-XVERSION-NO-CONDITIONAL：多段下载，CDN 在 206 上剥离 etag，文件中途变化 → 拼接
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn multiseg_etag_stripped_must_not_silently_splice() {
    let work_dir = unique_dir("xver");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let size = 6_000_011usize;
    let v1 = Arc::new(gen_body(size, 31));
    let v2 = Arc::new(gen_body(size, 62));
    let sha1 = sha256_bytes(&v1);
    let sha2 = sha256_bytes(&v2);

    let mut s = ServerState::new(v1.clone(), "etag-1");
    s.emit_validators_on_range = false; // 206 不带 etag/last-modified（CDN 行为）
    s.swap_after_range_gets = Some((2, v2.clone(), "etag-2".to_string()));
    let st = Arc::new(s);
    let server = start_server(st).await;
    let url = server.url("/file");

    // 多段：传入 probe 看到的 etag（带引号），但 206 不回 etag → do_segment 的校验被短路
    let cancel = CancellationToken::new();
    let (res, dest) = run_coord(&work_dir, "xv", &url, size as i64, 8, "\"etag-1\"", &cancel).await;
    let got = if dest.exists() {
        sha256_file(&dest).await
    } else {
        "<missing>".into()
    };
    eprintln!(
        "[xver] result={:?} sha={got}\n  v1={sha1}\n  v2={sha2}",
        res.as_ref().map(|_| "ok")
    );

    if res.is_ok() {
        assert!(
            got == sha1 || got == sha2,
            "❌ 多段下载在 CDN 剥离 etag 且文件变化时静默拼接：最终 SHA 既非 v1 也非 v2"
        );
    }
    drop(server);
}

// ===========================================================================
// 回归测试：alist 代理迅雷/光鸭云盘——分段 Range 请求偶发/持续返回 200 而非 206
// （瞬时 200 卡住下载 + .fdownloading 变 0kb + 强制单线程，已在 src/ 修复）
// ===========================================================================

/// 服务器广播支持 Range（probe 的 `bytes=0-0` 正常 206），但每一个真实分段的
/// range GET 都被钩子 A 强制返回 200 全量。coordinator 因此从未拿到任何 206
/// 数据（total_downloaded 始终为 0），必须快速回退单流下载——且**必须用存活
/// 的主令牌**（而非被 Path B 误 cancel 的令牌）完成，产出字节完整的文件。
///
/// 修复前：coordinator 的 Path B 会 cancel 主令牌，run_download_inner 拿同一个
/// 已取消的令牌调用 download_single → 瞬间命中 cancelled()，一个字节都下不了；
/// 回退前的 remove_file 还会删掉预分配的临时文件。本测试的两个断言（status==3、
/// SHA 字节完整）在修复前都会失败（status 非 3，且文件缺失/为空）。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn range_advertised_but_all_segments_get_200_falls_back_single_stream() {
    let work_dir = unique_dir("force200");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let size = 5_000_011usize; // ~5MB 素数大小
    let body = Arc::new(gen_body(size, 200));
    let expected = sha256_bytes(&body);

    let mut s = ServerState::new(body.clone(), "etag-force200");
    // support_range 保持默认 true：probe（0-0）与 HEAD 都表现为“服务器支持 Range”。
    s.force_full_on_segment_range = true; // 但所有真实分段请求强制返回 200。
    let st = Arc::new(s);
    let server = start_server(st).await;
    let url = server.url("/file");

    let db = Db::open(&work_dir).await.expect("db");
    insert_simple_task(&db, &work_dir, "f200", &url, "out.bin", 8, size as i64).await;

    // hint_file_size=0：走真实 probe 路径（不是浏览器扩展 hint 旁路）——
    // 这正是复现“probe 判定支持 Range → 选多段 → 每段却拿到 200”的关键。
    let cancel = CancellationToken::new();
    let (status, dest) = run_full(
        &work_dir, &db, "f200", &url, "out.bin", 8, 0, false, "", &cancel,
    )
    .await;

    assert_eq!(
        status, 3,
        "❌ 分段全部返回 200 时应回退单流并成功（status=3），实得 {status}——\
         很可能是 BUG 1 复现：主令牌被 Path B 误 cancel，单流回退瞬间 cancelled()"
    );
    assert!(dest.exists(), "❌ 下载“成功”却没有产物文件");
    let got = sha256_file(&dest).await;
    assert_eq!(
        got, expected,
        "❌ 单流回退产出的文件不是字节完整的原文件（可能是 BUG 2：回退前误删已下载数据，\
         download_single 只写出了空/截断文件）"
    );
    drop(server);
}

/// 复现“下载过半后瞬时 200”：直接构造一个“已下载过半”的续传起点——按引擎真实的
/// 均匀切分方案手写 DB 段行（1 个段已过半完成、其余段全新未开始）+ 预写磁盘上
/// 对应字节，而不是靠真实网络下载 + 短延时 cancel 来竞速产生部分进度（本地回环
/// 服务器 write_all 到内核 socket buffer 几乎瞬时完成，与客户端限速下的实际写盘
/// 速度不同步，短延时 cancel 无法可靠留下部分状态）。
///
/// 单程 `run_coordinated_download`（模拟续传）时，钩子 B 对其中一个分段的
/// range GET 强制返回一次 200（随后自动恢复 206）。根据 do_segment_with_retry
/// 的两义性处理，total_downloaded>0（段 0 已有过半进度）时收到的
/// RangeNotSupported 不会立即失败，而是像普通瞬时错误一样带退避重试——换一次
/// 请求即恢复 206，最终文件字节完整，coordinator 返回 Ok（证明修复 3：瞬时 200
/// 不再被灾难化处理、不丢数据）。
///
/// 注意：命中退避的首次等待为 RETRY_BASE_DELAY（2s），本测试因此至少耗时 ~2s，
/// 属预期行为。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn transient_200_on_resume_is_absorbed_byte_exact() {
    let work_dir = unique_dir("transient200");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let segs_count = 8i32;
    let size = 8_000_003i64; // ~8MB 素数
    let body = Arc::new(gen_body(size as usize, 909));
    let expected = sha256_bytes(&body);
    let st = Arc::new(ServerState::new(body.clone(), "etag-transient"));
    let server = start_server(st.clone()).await;
    let url = server.url("/file");

    let task_id = "transient-task";
    let dest = work_dir.join(format!("{task_id}.bin"));
    let client = test_client();
    let db = Db::open(&work_dir).await.expect("db");
    db.insert_task(
        task_id,
        &url,
        &dest.file_name().unwrap().to_string_lossy(),
        &work_dir.to_string_lossy(),
        segs_count,
        size,
        "",
        "",
        "",
    )
    .await
    .unwrap();

    // ---- 直接构造"下载过半"的续传起点：DB 段进度 + 预写磁盘内容 ----
    // 与 coordinator 的 build_fresh_segments 完全相同的均匀切分方案
    // （chunk = total/count，末段兜底余数），确保与 run_coordinated_download
    // 对已存在 DB 段的 resume 分支假设一致。
    let chunk = size / segs_count as i64;
    let mut db_segs = Vec::with_capacity(segs_count as usize);
    for i in 0..segs_count {
        let start = i as i64 * chunk;
        let end = if i == segs_count - 1 {
            size - 1
        } else {
            (i as i64 + 1) * chunk - 1
        };
        db_segs.push((i, start, end));
    }
    db.insert_segments(task_id, &db_segs).await.unwrap();

    // 段 0 已下载过半（模拟真实下载进行到一半）；其余段全新未开始。
    let seg0_done = chunk / 2;
    db.update_segment_progress(task_id, 0, seg0_done)
        .await
        .unwrap();

    // 预写磁盘：段 0 的 [0, seg0_done) 写入真实字节；文件其余部分按引擎真实
    // 预分配行为延伸到完整大小（sparse tail，与 fallocate/set_len 语义一致，
    // coordinator 自身的预分配步骤 truncate(false) 不会破坏这里预写的内容）。
    {
        let mut f = tokio::fs::File::create(&dest).await.unwrap();
        f.write_all(&body[0..seg0_done as usize]).await.unwrap();
        f.set_len(size as u64).await.unwrap();
    }

    // ---- 武装钩子 B：唯一一程 run_coordinated_download（即"续传"）里，
    // 对某个分段的 range GET 注入一次瞬时 200 ----
    st.force_full_range_get_once.store(true, Ordering::SeqCst);

    let speed_limiter = SpeedLimiter::new(0);
    let (tx, rx) = mpsc::channel::<ProgressUpdate>(256);
    let dh = drain(rx);
    let spec = RequestSpec::empty_get();
    let sink = NoopTestSink;
    let cancel = CancellationToken::new();
    let result = run_coordinated_download(
        task_id,
        &url,
        &dest,
        size,
        false,
        segs_count,
        &client,
        &db,
        &tx,
        &cancel,
        &speed_limiter,
        &spec,
        &sink,
        "",
        "",
    )
    .await;
    drop(tx);
    let _ = dh.await;

    result.expect("瞬时 200 应被 do_segment_with_retry 退避重试吸收，续传应成功");

    let got_sha = sha256_file(&dest).await;
    assert_eq!(
        got_sha, expected,
        "❌ 瞬时 200 吸收后文件 SHA 不完整——数据丢失/损坏"
    );
    drop(server);
}
