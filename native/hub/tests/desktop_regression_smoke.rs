//! 桌面回归冒烟测试 —— `docs/fluxdown-engine-decouple-plan.md` Verification
//! 条款 6。
//!
//! **范围调整说明**：原条款要求"迁移前后对比"，但 hub → `fluxdown_engine`
//! 的搬移已经完成合并，无法再产出"迁移前"基线（现有代码就是迁移后的唯一
//! 版本）。按计划 Assumptions 第 28/142 行"若触发范围上限降级，回归验证降级
//! 为静态等价性检查"的先例，本测试改为**静态等价性检查**：不做前后对比，而
//! 是验证当前（迁移后）通过 hub 使用的确切构造路径
//! （`fluxdown_engine::Engine::new` → `DownloadManager::create_task` →
//! `downloader`/`segment_coordinator` → `EventSink` → `progress_reporter`）
//! 发起一次真实多段下载时，产生的 `TaskProgress`/`SegmentProgress` 事件序列
//! 在结构上是自洽、正确的 —— 这本身就是对整个 hub 适配层端到端正确性的
//! 证明：`hub` 除了把 `EventSink`/`HostSelection` 换成 `RinfEventSink`/
//! `RinfHostSelection`（分别在 `rinf_sink.rs`/`rinf_selection.rs` 独立测试）
//! 之外，构造 `Engine` 与调用 `manager.create_task`/`take_progress_rx`/
//! `progress_reporter`/`take_done_rx`/`on_task_done` 的方式与本测试逐行一致
//! （见 `native/hub/src/actors/download_actor.rs::run`）。
//!
//! 用法：
//!   cargo test -p hub --test desktop_regression_smoke -- --ignored --nocapture
//!
//! 默认 `#[ignore]`（绑定本地端口 + 真实网络 I/O），与 `realtest.rs` 现有
//! 测试的约定一致。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluxdown_engine::bt_downloader::BtConfig;
use fluxdown_engine::events::{EngineEvent, EventSink};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ===========================================================================
// 本地测试文件服务器 —— 支持 Range，模拟真实多段下载路径
// ===========================================================================

/// 10 MiB 确定性内容 —— 足够大以触发 `segment_advisor`/`downloader.rs` 的
/// 多段判定阈值（>1MB），足够小以保证测试在数秒内跑完。
const FILE_SIZE: usize = 10 * 1024 * 1024;

/// 显式请求的分段数（>1，确保走 `download_multi_segment` 路径而非单流）。
const SEGMENTS: i32 = 4;

/// 确定性伪随机内容生成（xorshift64*），保证非平凡字节流（能暴露偏移/
/// 拼接错误），与 `native/engine/tests/realtest.rs::gen_body` 同一算法。
fn gen_body(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.max(1);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// 解析出的请求要素（method + path + 可选 Range）。
struct ParsedReq {
    method: String,
    range: Option<(i64, Option<i64>)>,
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

/// 读取并解析一个 HTTP 请求（GET/HEAD 无 body，读到 `\r\n\r\n` 即可）。
async fn read_request(stream: &mut TcpStream) -> Option<ParsedReq> {
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let _path = parts.next()?.to_string();

    let mut range = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("range")
        {
            range = parse_range(value.trim());
        }
    }
    Some(ParsedReq { method, range })
}

/// 处理单个连接：解析请求 → 生成响应 → `Connection: close`。
///
/// 分段（大于探测用的 `bytes=0-0`）响应体按小 chunk + 短暂延迟分批写出，
/// 使单个 segment 的传输耗时略高于 `segment_coordinator.rs` 的
/// `UI_REPORT_INTERVAL_MS`（200ms）周期上报阈值 —— 否则本地回环下载可能在
/// 一次 `poll_next` 内完成，UI 进度上报窗口永远不会到期，导致
/// `SegmentProgress` 事件从未产生（这是本测试要验证的信号，不能被回环速度
/// 意外掩盖）。探测请求（HEAD / `Range: bytes=0-0`）不受此节流影响，立即
/// 返回，避免拖慢 probe 阶段。
async fn handle_conn(mut stream: TcpStream, body: Arc<Vec<u8>>) -> std::io::Result<()> {
    let req = match read_request(&mut stream).await {
        Some(r) => r,
        None => return Ok(()),
    };

    let total = body.len() as i64;
    let is_head = req.method.eq_ignore_ascii_case("HEAD");

    if is_head {
        let h = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\n\
             Content-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(h.as_bytes()).await?;
        let _ = stream.shutdown().await;
        return Ok(());
    }

    match req.range {
        None => {
            // 全量 GET（正常多段路径不会走到这里，仅作兜底）。
            let h = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\n\
                 Content-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(h.as_bytes()).await?;
            stream.write_all(&body).await?;
        }
        Some((start, end_opt)) => {
            let end = end_opt.unwrap_or(total - 1).min(total - 1);
            if start < 0 || start > end || start >= total {
                let h = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(h.as_bytes()).await?;
                let _ = stream.shutdown().await;
                return Ok(());
            }
            let slice = &body[start as usize..=end as usize];
            let len = slice.len() as i64;
            let h = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {len}\r\n\
                 Content-Range: bytes {start}-{end}/{total}\r\nAccept-Ranges: bytes\r\n\
                 Content-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(h.as_bytes()).await?;

            // 探测请求（1 字节）立即返回；真正的段传输按 chunk 节流。
            const PACE_CHUNK: usize = 96 * 1024;
            const PACE_DELAY: Duration = Duration::from_millis(25);
            if slice.len() <= 1 {
                stream.write_all(slice).await?;
            } else {
                for chunk in slice.chunks(PACE_CHUNK) {
                    stream.write_all(chunk).await?;
                    tokio::time::sleep(PACE_DELAY).await;
                }
            }
        }
    }
    let _ = stream.shutdown().await;
    Ok(())
}

/// 运行中的测试服务器句柄；drop 时停掉 accept 循环。
struct TestServer {
    addr: std::net::SocketAddr,
    accept_task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    fn url(&self) -> String {
        format!("http://{}/regression-payload.bin", self.addr)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

/// 启动本地服务器，监听 `127.0.0.1:0`（随机端口）。
async fn start_server(body: Arc<Vec<u8>>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local_addr");
    let accept_task = tokio::spawn(async move {
        while let Ok((stream, _peer)) = listener.accept().await {
            let b = body.clone();
            tokio::spawn(async move {
                let _ = handle_conn(stream, b).await;
            });
        }
    });
    TestServer { addr, accept_task }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ===========================================================================
// 测试桩 EventSink —— 捕获 TaskProgress / SegmentProgress
// ===========================================================================

/// 捕获到的、与本测试相关的两类事件（其余变体按 `EventSink` 契约静默丢弃）。
#[derive(Debug, Clone)]
enum CapturedEvent {
    TaskProgress {
        task_id: String,
        status: i32,
        total_bytes: i64,
        file_name: String,
        save_dir: String,
        url: String,
    },
    SegmentProgress {
        task_id: String,
        total_bytes: i64,
        segment_count: i32,
        segments: Vec<(i32, i64, i64)>, // (index, start_byte, end_byte)
    },
}

struct CapturingSink {
    events: Mutex<Vec<CapturedEvent>>,
}

impl CapturingSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn snapshot(&self) -> Vec<CapturedEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl EventSink for CapturingSink {
    fn emit(&self, event: EngineEvent) {
        let captured = match event {
            EngineEvent::TaskProgress {
                task_id,
                status,
                total_bytes,
                file_name,
                save_dir,
                url,
                ..
            } => Some(CapturedEvent::TaskProgress {
                task_id,
                status,
                total_bytes,
                file_name,
                save_dir,
                url,
            }),
            EngineEvent::SegmentProgress {
                task_id,
                total_bytes,
                segment_count,
                segments,
            } => Some(CapturedEvent::SegmentProgress {
                task_id,
                total_bytes,
                segment_count,
                segments: segments
                    .iter()
                    .map(|s| (s.index, s.start_byte, s.end_byte))
                    .collect(),
            }),
            _ => None,
        };
        if let Some(c) = captured
            && let Ok(mut guard) = self.events.lock()
        {
            guard.push(c);
        }
    }
}

/// 校验一组段是否恰好无缝覆盖 `[0, total_bytes-1]`（等价于
/// `segment_coordinator.rs::validate_coverage` 的不变式，该函数是私有的，
/// 集成测试无法直接调用，故在此重新实现等价校验逻辑）。
fn assert_contiguous_coverage(segments: &[(i32, i64, i64)], total_bytes: i64, ctx: &str) {
    assert!(!segments.is_empty(), "{ctx}: segments must not be empty");
    let mut sorted = segments.to_vec();
    sorted.sort_by_key(|s| s.1); // sort by start_byte
    assert_eq!(
        sorted[0].1, 0,
        "{ctx}: first segment must start at byte 0, got {}",
        sorted[0].1
    );
    for w in sorted.windows(2) {
        let (_, _, prev_end) = w[0];
        let (idx, start, _) = w[1];
        assert_eq!(
            start,
            prev_end + 1,
            "{ctx}: gap/overlap between segments — prev end_byte={prev_end}, \
             next segment {idx} start_byte={start}"
        );
    }
    let (_, _, last_end) = *sorted.last().expect("checked non-empty above");
    assert_eq!(
        last_end,
        total_bytes - 1,
        "{ctx}: last segment must end at total_bytes-1={}, got {last_end}",
        total_bytes - 1
    );
    // index 必须两两不同（覆盖校验之外的结构完整性检查）。
    let mut indices: Vec<i32> = segments.iter().map(|s| s.0).collect();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(
        indices.len(),
        segments.len(),
        "{ctx}: duplicate segment index detected"
    );
}

// ===========================================================================
// 测试
// ===========================================================================

/// 端到端验证：hub 使用的 `fluxdown_engine::Engine` 构造路径
/// （`Engine::new` → `DownloadManager::create_task` → 真实多段下载 →
/// `EventSink`/`progress_reporter`）产生结构自洽的 `TaskProgress`/
/// `SegmentProgress` 事件序列，且下载文件字节级正确。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "binds a local port + real network I/O; run with --ignored"]
async fn desktop_regression_smoke() {
    let source_body = Arc::new(gen_body(FILE_SIZE, 0xD35C_7001));
    let source_sha256 = sha256_hex(&source_body);

    let server = start_server(source_body.clone()).await;
    let url = server.url();

    let work_dir = std::env::temp_dir().join(format!(
        "fluxdown_desktop_regression_smoke_{}",
        std::process::id()
    ));
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir)
        .await
        .expect("create work dir");
    let save_dir = work_dir.to_string_lossy().into_owned();

    let sink = Arc::new(CapturingSink::new());
    let mut engine = Engine::new(
        EngineConfig {
            max_concurrent: 4,
            speed_limit_bps: 0,
            default_save_dir: save_dir.clone(),
            app_data_dir: save_dir.clone(),
            bt_config: BtConfig::default(),
            proxy_config: ProxyConfig::default(),
            user_agent: String::new(),
            data_dir_override: Some(work_dir.clone()),
            database_url: None,
        },
        sink.clone(),
        Arc::new(NoopSelection),
    )
    .await
    .expect("Engine::new");

    // 与 `download_actor.rs::run` 完全一致的接线方式：`take_progress_rx` 独立
    // 取走 progress channel，`progress_reporter` 独立消费并调用 `sink.emit`。
    let progress_rx = engine
        .manager
        .take_progress_rx()
        .expect("take_progress_rx should return Some on first call");
    tokio::spawn(fluxdown_engine::download_manager::progress_reporter(
        progress_rx,
        engine.db.clone(),
        sink.clone(),
    ));

    let mut done_rx = engine
        .manager
        .take_done_rx()
        .expect("take_done_rx should return Some on first call");

    let file_name = "regression-payload.bin".to_string();

    engine
        .manager
        .create_task(fluxdown_engine::download_manager::NewTaskSpec {
            url: url.clone(),
            save_dir: save_dir.clone(),
            file_name: file_name.clone(),
            segments: SEGMENTS,
            ..Default::default()
        })
        .await;

    // 等待任务完成通知（带超时，避免测试意外挂起）。
    let done = tokio::time::timeout(Duration::from_secs(10), done_rx.recv())
        .await
        .expect("timed out waiting for TaskDone")
        .expect("done channel closed unexpectedly");
    let task_id = done.task_id.clone();
    engine.manager.on_task_done(&done).await;

    // `TaskProgress{status:3}` 由 `progress_reporter` 异步消费 progress
    // channel 后才 emit（不是 TaskDone 到达时同步完成），故轮询捕获到的事件
    // 列表直至出现该终态，带超时保护。
    let completed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = sink.snapshot();
            let done = events.iter().any(|e| {
                matches!(
                    e,
                    CapturedEvent::TaskProgress { task_id: t, status: 3, .. } if t == &task_id
                )
            });
            if done {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out waiting for TaskProgress{status:3}");

    // ---- TaskProgress 字段级断言 -------------------------------------------
    let task_progress: Vec<_> = completed
        .iter()
        .filter_map(|e| match e {
            CapturedEvent::TaskProgress {
                task_id: t,
                status,
                total_bytes,
                file_name: fname,
                save_dir: sdir,
                url: u,
            } if t == &task_id => Some((
                *status,
                *total_bytes,
                fname.clone(),
                sdir.clone(),
                u.clone(),
            )),
            _ => None,
        })
        .collect();
    assert!(
        !task_progress.is_empty(),
        "expected at least one TaskProgress event for task {task_id}"
    );

    // create_task 同步 emit 的首条事件（status=0）携带创建时传入的原始
    // file_name/save_dir/url —— progress_reporter 转发的后续事件因
    // rate-limiting/latching 机制会把 save_dir/url 置空（这是既有行为，非本
    // 次迁移引入），故字段级一致性只在这条初始事件上校验。
    let (initial_status, _initial_total, initial_name, initial_save_dir, initial_url) =
        &task_progress[0];
    assert_eq!(
        *initial_status, 0,
        "first TaskProgress must be status=0 (pending)"
    );
    assert_eq!(
        initial_name, &file_name,
        "file_name must match create_task input"
    );
    assert_eq!(
        initial_save_dir, &save_dir,
        "save_dir must match create_task input"
    );
    assert_eq!(initial_url, &url, "url must match create_task input");

    // 至少一条终态事件：status==3（完成），total_bytes 等于源文件大小。
    let completed_event = task_progress
        .iter()
        .find(|(status, ..)| *status == 3)
        .expect("expected a TaskProgress{status:3} (completed) event");
    assert_eq!(
        completed_event.1, FILE_SIZE as i64,
        "completed TaskProgress.total_bytes must equal source file size"
    );

    // ---- SegmentProgress 字段级断言 ----------------------------------------
    let segment_progress: Vec<_> = completed
        .iter()
        .filter_map(|e| match e {
            CapturedEvent::SegmentProgress {
                task_id: t,
                total_bytes,
                segment_count,
                segments,
            } if t == &task_id => Some((*total_bytes, *segment_count, segments.clone())),
            _ => None,
        })
        .collect();
    assert!(
        !segment_progress.is_empty(),
        "expected at least one SegmentProgress event — multi-segment path was not exercised"
    );

    // segments 参数显式传入正数（4），不会被 segment_advisor 的 auto 逻辑
    // （仅在 segments<=0 时触发，见 download_manager.rs 的四层优先级注释）
    // 覆盖，故初始拆分必然恰好是 SEGMENTS 份；后续 coordinator 的 proactive
    // split 只会增加段数不会减少 —— 用 >= 而非 == 断言，兼顾拆分定时器
    // （2 秒周期）理论上可能在慢速 CI 环境下触发一次的情况。
    for (total_bytes, segment_count, segments) in &segment_progress {
        assert_eq!(
            *total_bytes, FILE_SIZE as i64,
            "SegmentProgress.total_bytes must equal source file size"
        );
        assert!(
            *segment_count >= SEGMENTS,
            "segment_count must be >= requested SEGMENTS ({SEGMENTS}), got {segment_count}"
        );
        assert_eq!(
            segments.len(),
            *segment_count as usize,
            "segments.len() must match segment_count"
        );
        assert_contiguous_coverage(segments, FILE_SIZE as i64, "SegmentProgress snapshot");
    }
    // 首个快照（拆分定时器生效前）必须恰好是显式请求的段数。
    assert_eq!(
        segment_progress[0].1, SEGMENTS,
        "initial split must produce exactly the requested segment count"
    );

    // ---- 文件内容字节级正确性 ------------------------------------------------
    let dest = work_dir.join(&file_name);
    let downloaded = tokio::fs::read(&dest).await.expect("read downloaded file");
    assert_eq!(
        downloaded.len(),
        FILE_SIZE,
        "downloaded file size must match source"
    );
    assert_eq!(
        sha256_hex(&downloaded),
        source_sha256,
        "downloaded file content must match source byte-for-byte (SHA256)"
    );
    assert_eq!(
        downloaded.as_slice(),
        source_body.as_slice(),
        "downloaded file must be byte-identical to source"
    );

    let _ = tokio::fs::remove_dir_all(&work_dir).await;
}
