//! 组件载荷（ffmpeg 归档 / yt-dlp 二进制）的多段并发下载（#453）。
//!
//! 先发 `Range: bytes=0-0` 探测：服务器回 206 且给出总长 → 预分配 `dest`，按
//! [`SegmentPolicy`] 拆成至多 4 段并发 Range 写入，单段断流 / 停滞 / 5xx 从断点
//! 指数退避重试；任一段最终失败则整体回退单流。服务器忽略 Range（回 200）时直接
//! 把探测响应当单流消费，不多发请求。哈希一律对落盘文件计算，由调用方比对。

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use super::ComponentError;
use crate::egress::EgressAttempt;

/// 进度上报步长：每落盘 256KB 上报一次，避免信号风暴。
const REPORT_STEP: u64 = 256 * 1024;

/// 分段下载参数。
#[derive(Clone, Copy, Debug)]
pub(super) struct SegmentPolicy {
    /// 并发段数上限。
    pub max_segments: u64,
    /// 每段最小字节数：小文件少拆或不拆（单段仍享有断点重试）。
    pub min_segment_bytes: u64,
    /// 每段总尝试次数（含首次）。
    pub attempts: u32,
    /// 重试退避基数，第 n 次重试等待 `retry_base * 2^(n-1)`。
    pub retry_base: Duration,
    /// 等响应头 / 两个数据块之间的最长空闲，超时按瞬时失败重试。
    pub idle_timeout: Duration,
}

/// 组件安装使用的分段参数。
pub(super) const SEGMENT_POLICY: SegmentPolicy = SegmentPolicy {
    max_segments: 4,
    min_segment_bytes: 2 * 1024 * 1024,
    attempts: 3,
    retry_base: Duration::from_secs(2),
    idle_timeout: Duration::from_secs(30),
};

/// 下载 `attempt` 到 `dest`，返回落盘内容的 SHA-256（小写 hex）。
/// `progress(downloaded, total)` 语义同 [`super::download_to_file`]（total=0 未知）。
pub(super) async fn fetch_to_file(
    client: &reqwest::Client,
    attempt: &EgressAttempt,
    dest: &Path,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
    policy: &SegmentPolicy,
) -> Result<String, ComponentError> {
    let probe = request(client, attempt)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|e| ComponentError::Http(e.to_string()))?;
    match probe.status() {
        reqwest::StatusCode::PARTIAL_CONTENT => {
            if let Some(total) = probed_total(probe.headers()) {
                drop(probe);
                match download_segmented(client, attempt, dest, total, progress, policy).await {
                    Ok(()) => return hash_file(dest).await,
                    Err(error) => crate::log_info!(
                        "[components] segmented download failed, falling back to single stream: {error}"
                    ),
                }
            }
        }
        // 服务器忽略 Range：探测响应就是完整载荷。
        status if status.is_success() => return stream_to_file(probe, dest, progress).await,
        // 空文件等无法满足 `0-0` 的情况：交给不带 Range 的单流。
        reqwest::StatusCode::RANGE_NOT_SATISFIABLE => {}
        status => {
            return Err(ComponentError::Http(format!("download returned {status}")));
        }
    }
    let resp = request(client, attempt)
        .send()
        .await
        .map_err(|e| ComponentError::Http(e.to_string()))?;
    stream_to_file(resp, dest, progress).await
}

fn request(client: &reqwest::Client, attempt: &EgressAttempt) -> reqwest::RequestBuilder {
    let mut request = client.get(&attempt.url).header("User-Agent", "FluxDown");
    for (name, value) in &attempt.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    request
}

/// 探测 206 可分段时返回总长：`Content-Range` 从 0 起、总长已知且非零、未压缩。
fn probed_total(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    if is_encoded(headers) {
        return None;
    }
    let value = headers.get(reqwest::header::CONTENT_RANGE)?.to_str().ok()?;
    match parse_content_range(value)? {
        (0, _, Some(total)) if total > 0 => Some(total),
        _ => None,
    }
}

fn is_encoded(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_ENCODING)
        .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
}

/// 解析 `bytes <start>-<end>/<total|*>`；`end < start` 或格式不符返回 `None`。
fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (span, total) = rest.split_once('/')?;
    let (start, end) = span.trim().split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end: u64 = end.trim().parse().ok()?;
    if end < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((start, end, total))
}

/// 单流：边写边算哈希（服务器不支持 Range 或分段失败时的兜底，即原实现）。
async fn stream_to_file(
    resp: reqwest::Response,
    dest: &Path,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<String, ComponentError> {
    if !resp.status().is_success() {
        return Err(ComponentError::Http(format!(
            "download returned {}",
            resp.status()
        )));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| ComponentError::Io(e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_report: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ComponentError::Http(e.to_string()))?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| ComponentError::Io(e.to_string()))?;
        downloaded += chunk.len() as u64;
        if downloaded - last_report >= REPORT_STEP || downloaded == total {
            progress(downloaded, total);
            last_report = downloaded;
        }
    }
    file.flush()
        .await
        .map_err(|e| ComponentError::Io(e.to_string()))?;
    progress(downloaded, total);
    Ok(hex::encode(hasher.finalize()))
}

/// 段数：按 `min_segment_bytes` 折算，夹在 `1..=max_segments`。
fn segment_count(total: u64, policy: &SegmentPolicy) -> u64 {
    (total / policy.min_segment_bytes.max(1)).clamp(1, policy.max_segments.max(1))
}

/// 把 `[0, total)` 均分为 `count` 段闭区间，余数并入最后一段。
fn segment_ranges(total: u64, count: u64) -> Vec<(u64, u64)> {
    let base = total / count;
    (0..count)
        .map(|i| {
            let start = i * base;
            let end = if i + 1 == count {
                total - 1
            } else {
                start + base - 1
            };
            (start, end)
        })
        .collect()
}

/// 一次分段下载的共享上下文。各段 future 在同一任务内轮询，计数与上报之间无 await，
/// 上报值单调不减。
struct Job<'a> {
    client: &'a reqwest::Client,
    attempt: &'a EgressAttempt,
    dest: &'a Path,
    total: u64,
    policy: &'a SegmentPolicy,
    progress: &'a (dyn Fn(u64, u64) + Send + Sync),
    downloaded: AtomicU64,
    last_report: AtomicU64,
}

impl Job<'_> {
    fn add(&self, n: u64) {
        let now = self.downloaded.fetch_add(n, Ordering::Relaxed) + n;
        if now - self.last_report.load(Ordering::Relaxed) >= REPORT_STEP {
            self.last_report.store(now, Ordering::Relaxed);
            (self.progress)(now, self.total);
        }
    }
}

async fn download_segmented(
    client: &reqwest::Client,
    attempt: &EgressAttempt,
    dest: &Path,
    total: u64,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
    policy: &SegmentPolicy,
) -> Result<(), ComponentError> {
    let file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| ComponentError::Io(e.to_string()))?;
    file.set_len(total)
        .await
        .map_err(|e| ComponentError::Io(e.to_string()))?;
    drop(file);
    let job = Job {
        client,
        attempt,
        dest,
        total,
        policy,
        progress,
        downloaded: AtomicU64::new(0),
        last_report: AtomicU64::new(0),
    };
    let segments = segment_ranges(total, segment_count(total, policy))
        .into_iter()
        .map(|(start, end)| fetch_segment(&job, start, end));
    // 任一段失败即丢弃其余段的 future（取消在途连接）。
    futures_util::future::try_join_all(segments).await?;
    progress(total, total);
    Ok(())
}

/// 段失败的分类：瞬时（断流、停滞、5xx / 408 / 429）从断点重试，其余直接失败。
enum SegmentError {
    Retryable(ComponentError),
    Fatal(ComponentError),
}

async fn fetch_segment(job: &Job<'_>, start: u64, end: u64) -> Result<(), ComponentError> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(job.dest)
        .await
        .map_err(|e| ComponentError::Io(e.to_string()))?;
    let mut pos = start;
    let mut tried: u32 = 1;
    loop {
        match fetch_segment_once(job, &mut file, &mut pos, end).await {
            Ok(()) => break,
            Err(SegmentError::Retryable(error)) if tried < job.policy.attempts => {
                crate::log_info!(
                    "[components] segment {start}-{end} interrupted at {pos} (attempt {tried}): {error}; retrying"
                );
                tokio::time::sleep(job.policy.retry_base * 2u32.saturating_pow(tried - 1)).await;
                tried += 1;
            }
            Err(SegmentError::Retryable(error) | SegmentError::Fatal(error)) => return Err(error),
        }
    }
    file.flush()
        .await
        .map_err(|e| ComponentError::Io(e.to_string()))
}

async fn fetch_segment_once(
    job: &Job<'_>,
    file: &mut tokio::fs::File,
    pos: &mut u64,
    end: u64,
) -> Result<(), SegmentError> {
    let total = job.total;
    let io = |e: std::io::Error| SegmentError::Fatal(ComponentError::Io(e.to_string()));
    let transient = |msg: String| SegmentError::Retryable(ComponentError::Http(msg));
    let fatal = |msg: String| SegmentError::Fatal(ComponentError::Http(msg));

    file.seek(std::io::SeekFrom::Start(*pos))
        .await
        .map_err(io)?;
    let sent = request(job.client, job.attempt)
        .header(reqwest::header::RANGE, format!("bytes={}-{end}", *pos))
        .send();
    let resp = tokio::time::timeout(job.policy.idle_timeout, sent)
        .await
        .map_err(|_| transient("segment response timed out".to_string()))?
        .map_err(|e| transient(e.to_string()))?;
    let status = resp.status();
    if status != reqwest::StatusCode::PARTIAL_CONTENT {
        let msg = format!("segment request returned {status}");
        let retry = status.is_server_error()
            || status == reqwest::StatusCode::REQUEST_TIMEOUT
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
        return Err(if retry { transient(msg) } else { fatal(msg) });
    }
    if is_encoded(resp.headers()) {
        return Err(fatal("segment response is content-encoded".to_string()));
    }
    let range = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range);
    if range != Some((*pos, end, Some(total))) {
        return Err(fatal(format!(
            "unexpected segment Content-Range {range:?} for bytes {}-{end}/{total}",
            *pos
        )));
    }
    let mut stream = resp.bytes_stream();
    loop {
        let next = tokio::time::timeout(job.policy.idle_timeout, stream.next())
            .await
            .map_err(|_| transient("segment stalled".to_string()))?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|e| transient(e.to_string()))?;
        let len = chunk.len() as u64;
        if len > end + 1 - *pos {
            return Err(fatal("segment response longer than requested".to_string()));
        }
        file.write_all(&chunk).await.map_err(io)?;
        *pos += len;
        job.add(len);
    }
    if *pos <= end {
        return Err(transient(format!("segment ended early at byte {}", *pos)));
    }
    Ok(())
}

/// 读回落盘文件计算 SHA-256（阻塞读放到阻塞线程池）。
async fn hash_file(path: &Path) -> Result<String, ComponentError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let mut file = std::fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher)?;
        Ok(hex::encode(hasher.finalize()))
    })
    .await
    .map_err(|e| ComponentError::Io(e.to_string()))?
    .map_err(|e| ComponentError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        /// 标准 Range 服务器。
        Ranges,
        /// 忽略 Range，恒回 200 全量。
        IgnoreRange,
        /// 第一个非首段的段请求中途断开，之后正常。
        FlakyOnce,
        /// 所有段请求（非探测）都中途断开；不带 Range 的 GET 正常。
        TruncateSegments,
        /// 探测正常，其余所有请求都中途断开。
        TruncateAll,
    }

    /// 每个请求的 Range 头（`None` = 不带 Range）。
    type RangeLog = Arc<Mutex<Vec<Option<(u64, u64)>>>>;

    struct Server {
        url: String,
        ranges: RangeLog,
    }

    fn body(len: usize) -> Arc<Vec<u8>> {
        Arc::new((0..len).map(|i| (i * 31 % 251) as u8).collect())
    }

    fn sha(data: &[u8]) -> String {
        hex::encode(Sha256::digest(data))
    }

    fn temp_dest(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fluxdown-component-seg-{}-{tag}.bin",
            std::process::id()
        ))
    }

    fn policy() -> SegmentPolicy {
        SegmentPolicy {
            max_segments: 4,
            min_segment_bytes: 8 * 1024,
            attempts: 3,
            retry_base: Duration::from_millis(10),
            idle_timeout: Duration::from_secs(5),
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    fn parse_range(head: &str) -> Option<(u64, u64)> {
        head.lines().find_map(|line| {
            let spec = line
                .to_ascii_lowercase()
                .strip_prefix("range: bytes=")?
                .to_string();
            let (a, b) = spec.trim().split_once('-')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
    }

    async fn serve(mode: Mode, data: Arc<Vec<u8>>) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/component.bin", listener.local_addr().unwrap());
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let flaky_used = Arc::new(AtomicBool::new(false));
        let seen = ranges.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let data = data.clone();
                let seen = seen.clone();
                let flaky_used = flaky_used.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let range = parse_range(&String::from_utf8_lossy(&buf));
                    seen.lock().unwrap().push(range);
                    let len = data.len() as u64;
                    let (head, slice, truncate) = match range {
                        Some((a, b)) if mode != Mode::IgnoreRange => {
                            let b = b.min(len - 1);
                            let probe = a == 0 && b == 0;
                            let truncate = !probe
                                && match mode {
                                    Mode::TruncateSegments | Mode::TruncateAll => true,
                                    Mode::FlakyOnce => {
                                        a > 0 && !flaky_used.swap(true, Ordering::SeqCst)
                                    }
                                    Mode::Ranges | Mode::IgnoreRange => false,
                                };
                            let slice = &data[a as usize..=b as usize];
                            (
                                format!(
                                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{len}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    slice.len()
                                ),
                                slice,
                                truncate,
                            )
                        }
                        _ => (
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
                            ),
                            &data[..],
                            mode == Mode::TruncateAll,
                        ),
                    };
                    let sent = if truncate {
                        &slice[..slice.len() / 2]
                    } else {
                        slice
                    };
                    let mut out = head.into_bytes();
                    out.extend_from_slice(sent);
                    if let Err(error) = stream.write_all(&out).await {
                        tracing::debug!(%error, "test client closed");
                    }
                    if let Err(error) = stream.shutdown().await {
                        tracing::debug!(%error, "test client closed");
                    }
                });
            }
        });
        Server { url, ranges }
    }

    fn attempt(url: &str) -> EgressAttempt {
        EgressAttempt {
            url: url.to_string(),
            headers: Vec::new(),
            plugin: None,
        }
    }

    type Calls = Arc<Mutex<Vec<(u64, u64)>>>;

    fn recorder() -> (Calls, impl Fn(u64, u64) + Send + Sync) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let sink = calls.clone();
        (calls, move |done, total| {
            sink.lock().unwrap().push((done, total))
        })
    }

    fn assert_monotonic_complete(calls: &[(u64, u64)], len: u64) {
        assert!(
            calls.windows(2).all(|w| w[0].0 <= w[1].0),
            "progress must not go backwards: {calls:?}"
        );
        assert_eq!(calls.last(), Some(&(len, len)));
    }

    fn segment_requests(server: &Server) -> Vec<(u64, u64)> {
        server
            .ranges
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .copied()
            .filter(|r| *r != (0, 0))
            .collect()
    }

    #[test]
    fn content_range_parsing() {
        assert_eq!(
            parse_content_range("bytes 0-0/100"),
            Some((0, 0, Some(100)))
        );
        assert_eq!(parse_content_range(" bytes 5-9/*"), Some((5, 9, None)));
        assert_eq!(parse_content_range("bytes 9-5/100"), None);
        assert_eq!(parse_content_range("bytes */100"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
    }

    #[test]
    fn segments_cover_whole_file_without_overlap() {
        let p = policy();
        assert_eq!(segment_count(1, &p), 1);
        assert_eq!(segment_count(16 * 1024, &p), 2);
        assert_eq!(segment_count(10 * 1024 * 1024, &p), 4);
        let ranges = segment_ranges(100_003, 4);
        assert_eq!(ranges.first().map(|r| r.0), Some(0));
        assert_eq!(ranges.last().map(|r| r.1), Some(100_002));
        assert!(ranges.windows(2).all(|w| w[0].1 + 1 == w[1].0));
    }

    #[tokio::test]
    async fn range_server_is_downloaded_in_parallel_segments() {
        let data = body(100_003);
        let server = serve(Mode::Ranges, data.clone()).await;
        let dest = temp_dest("ranges");
        let (calls, progress) = recorder();
        let hash = fetch_to_file(
            &client(),
            &attempt(&server.url),
            &dest,
            &progress,
            &policy(),
        )
        .await
        .unwrap();
        let written = std::fs::read(&dest).unwrap();
        std::fs::remove_file(&dest).unwrap();

        assert_eq!(hash, sha(&data));
        assert_eq!(written, *data);
        let mut segments = segment_requests(&server);
        segments.sort_unstable();
        assert_eq!(segments, segment_ranges(100_003, 4));
        assert!(server.ranges.lock().unwrap().iter().all(Option::is_some));
        assert_monotonic_complete(&calls.lock().unwrap(), 100_003);
    }

    #[tokio::test]
    async fn server_ignoring_range_falls_back_to_single_stream() {
        let data = body(50_000);
        let server = serve(Mode::IgnoreRange, data.clone()).await;
        let dest = temp_dest("ignore");
        let (calls, progress) = recorder();
        let hash = fetch_to_file(
            &client(),
            &attempt(&server.url),
            &dest,
            &progress,
            &policy(),
        )
        .await
        .unwrap();
        let written = std::fs::read(&dest).unwrap();
        std::fs::remove_file(&dest).unwrap();

        assert_eq!(hash, sha(&data));
        assert_eq!(written, *data);
        // 探测的 200 响应即被当作完整载荷消费，不再发第二个请求。
        assert_eq!(server.ranges.lock().unwrap().len(), 1);
        assert_monotonic_complete(&calls.lock().unwrap(), 50_000);
    }

    #[tokio::test]
    async fn interrupted_segment_resumes_from_where_it_stopped() {
        let data = body(100_003);
        let server = serve(Mode::FlakyOnce, data.clone()).await;
        let dest = temp_dest("flaky");
        let (calls, progress) = recorder();
        let hash = fetch_to_file(
            &client(),
            &attempt(&server.url),
            &dest,
            &progress,
            &policy(),
        )
        .await
        .unwrap();
        let written = std::fs::read(&dest).unwrap();
        std::fs::remove_file(&dest).unwrap();

        assert_eq!(hash, sha(&data));
        assert_eq!(written, *data);
        let segments = segment_requests(&server);
        // 4 段 + 1 次续传；续传起点在被截断段的中间，不从段首重下。
        assert_eq!(segments.len(), 5);
        let planned = segment_ranges(100_003, 4);
        let resumed: Vec<_> = segments.iter().filter(|r| !planned.contains(r)).collect();
        assert_eq!(resumed.len(), 1);
        assert!(
            planned
                .iter()
                .any(|p| p.1 == resumed[0].1 && p.0 < resumed[0].0)
        );
        assert!(server.ranges.lock().unwrap().iter().all(Option::is_some));
        assert_monotonic_complete(&calls.lock().unwrap(), 100_003);
    }

    #[tokio::test]
    async fn exhausted_segment_retries_fall_back_to_single_stream() {
        let data = body(100_003);
        let server = serve(Mode::TruncateSegments, data.clone()).await;
        let dest = temp_dest("fallback");
        let (_calls, progress) = recorder();
        let hash = fetch_to_file(
            &client(),
            &attempt(&server.url),
            &dest,
            &progress,
            &policy(),
        )
        .await
        .unwrap();
        let written = std::fs::read(&dest).unwrap();
        std::fs::remove_file(&dest).unwrap();

        assert_eq!(hash, sha(&data));
        assert_eq!(written, *data);
        assert_eq!(server.ranges.lock().unwrap().last(), Some(&None));
    }

    #[tokio::test]
    async fn broken_source_fails_after_segment_retries_and_single_stream() {
        let data = body(100_003);
        let server = serve(Mode::TruncateAll, data.clone()).await;
        let dest = temp_dest("broken");
        let (_calls, progress) = recorder();
        let result = fetch_to_file(
            &client(),
            &attempt(&server.url),
            &dest,
            &progress,
            &policy(),
        )
        .await;
        std::fs::remove_file(&dest).unwrap();

        assert!(matches!(result, Err(ComponentError::Http(_))));
        // 每段用满重试预算后才回退单流（单流也断流 → 整体失败）。
        assert!(segment_requests(&server).len() >= 4 + 2);
        assert_eq!(server.ranges.lock().unwrap().last(), Some(&None));
    }

    /// 安装入口 `download_to_file` 走 Range 下载；哈希不符删除文件并报校验错。
    #[tokio::test]
    async fn install_download_uses_ranges_and_rejects_hash_mismatch() {
        let data = body(100_003);
        let server = serve(Mode::Ranges, data.clone()).await;
        let clients = crate::egress::RoutedClients::new(
            &crate::proxy_config::ProxyConfig::default(),
            "",
            |b| b.no_proxy(),
        )
        .unwrap();
        let route = crate::egress::EgressRoute::direct(server.url.clone());
        let (_calls, progress) = recorder();

        let dest = temp_dest("mismatch");
        let result =
            super::super::download_to_file(&clients, &route, "", &dest, &"0".repeat(64), &progress)
                .await;
        assert!(matches!(result, Err(ComponentError::Verify(_))));
        assert!(!dest.exists());

        let dest = temp_dest("match");
        super::super::download_to_file(&clients, &route, "", &dest, &sha(&data), &progress)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), *data);
        std::fs::remove_file(&dest).unwrap();
        // 小于分段阈值也走带 Range 的单段（可断点重试），不再是裸 GET。
        assert!(server.ranges.lock().unwrap().iter().all(Option::is_some));
        assert!(segment_requests(&server).contains(&(0, 100_002)));
    }
}
