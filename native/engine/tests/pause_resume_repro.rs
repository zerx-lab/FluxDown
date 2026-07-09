//! 复现调查：HTTP 多段任务 暂停→恢复 后是否退化为单线程。
//!
//! 走真实 `Engine` → `DownloadManager::create_task / pause_task / resume_task`
//! 全链路（与桌面 App 相同的代码路径），本地慢速 HTTP 服务器统计
//! **并发 Range GET 峰值**，对比暂停前/每次恢复后的并发度。
//!
//! 场景矩阵：显式段数 / auto(0) / 浏览器扩展 hint 模式，各含多轮暂停恢复。
//!
//! 运行：
//!   cargo test -p fluxdown_engine --test pause_resume_repro -- --ignored --nocapture

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fluxdown_engine::bt_downloader::BtConfig;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ---------------------------------------------------------------------------
// 慢速本地服务器：完整支持 HEAD / Range GET，统计并发 Range 连接峰值
// ---------------------------------------------------------------------------

struct Gauge {
    active: AtomicUsize,
    peak: AtomicUsize,
    range_gets: AtomicUsize,
    full_gets: AtomicUsize,
}

impl Gauge {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            range_gets: AtomicUsize::new(0),
            full_gets: AtomicUsize::new(0),
        }
    }
    fn enter(&self) {
        let cur = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(cur, Ordering::SeqCst);
    }
    fn exit(&self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
    fn reset_peak(&self) {
        self.peak.store(0, Ordering::SeqCst);
        self.range_gets.store(0, Ordering::SeqCst);
        self.full_gets.store(0, Ordering::SeqCst);
    }
}

fn gen_body(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.max(1);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        out.extend_from_slice(&x.wrapping_mul(0x2545F4914F6CDD1D).to_le_bytes());
    }
    out.truncate(len);
    out
}

async fn read_request(stream: &mut TcpStream) -> Option<(String, Option<(i64, Option<i64>)>)> {
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
    let method = request_line.split_whitespace().next()?.to_string();
    let mut range = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("range")
        {
            let v = value.trim().strip_prefix("bytes=")?;
            let (s, e) = v.split_once('-')?;
            let start: i64 = s.trim().parse().ok()?;
            let end = if e.trim().is_empty() {
                None
            } else {
                Some(e.trim().parse::<i64>().ok()?)
            };
            range = Some((start, end));
        }
    }
    Some((method, range))
}

async fn handle_conn(
    mut stream: TcpStream,
    body: Arc<Vec<u8>>,
    gauge: Arc<Gauge>,
) -> std::io::Result<()> {
    let Some((method, range)) = read_request(&mut stream).await else {
        return Ok(());
    };
    let total = body.len() as i64;
    let etag = "\"repro-etag-1\"";
    let lm = "Wed, 21 Oct 2025 07:28:00 GMT";

    if method.eq_ignore_ascii_case("HEAD") {
        let h = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\nETag: {etag}\r\nLast-Modified: {lm}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(h.as_bytes()).await?;
        let _ = stream.shutdown().await;
        return Ok(());
    }

    match range {
        Some((start, end_opt)) => {
            let end = end_opt.unwrap_or(total - 1).min(total - 1);
            if start < 0 || start > end {
                let h = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(h.as_bytes()).await?;
                let _ = stream.shutdown().await;
                return Ok(());
            }
            gauge.range_gets.fetch_add(1, Ordering::SeqCst);
            let chunk = &body[start as usize..=(end as usize)];
            let h = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{total}\r\nAccept-Ranges: bytes\r\nETag: {etag}\r\nLast-Modified: {lm}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                chunk.len()
            );
            stream.write_all(h.as_bytes()).await?;
            // 慢速发送：16 KiB / 20ms ≈ 800 KB/s，仅对 >4KiB 的请求限速
            // （0-0 探测请求瞬间返回，不计入并发观察窗口）。
            let track = chunk.len() > 4096;
            if track {
                gauge.enter();
            }
            let mut off = 0usize;
            while off < chunk.len() {
                let n = (chunk.len() - off).min(16 * 1024);
                if stream.write_all(&chunk[off..off + n]).await.is_err() {
                    break;
                }
                off += n;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if track {
                gauge.exit();
            }
            let _ = stream.shutdown().await;
        }
        None => {
            gauge.full_gets.fetch_add(1, Ordering::SeqCst);
            let h = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\nETag: {etag}\r\nLast-Modified: {lm}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(h.as_bytes()).await?;
            gauge.enter();
            let mut off = 0usize;
            while off < body.len() {
                let n = (body.len() - off).min(16 * 1024);
                if stream.write_all(&body[off..off + n]).await.is_err() {
                    break;
                }
                off += n;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            gauge.exit();
            let _ = stream.shutdown().await;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 场景执行器
// ---------------------------------------------------------------------------

struct ScenarioResult {
    phase1_peak: usize,
    resume_peaks: Vec<usize>,
}

/// 跑一个完整场景：创建任务 → 下载一会 → N 轮(暂停 → 恢复 → 观察并发)。
/// `method`: 模拟浏览器扩展捕获的原始 method（None = 默认 GET）。
async fn run_scenario(
    name: &str,
    segments: i32,
    use_hint: bool,
    cycles: usize,
    method: Option<String>,
) -> ScenarioResult {
    let work_dir =
        std::env::temp_dir().join(format!("fluxdown_pr_{}_{}", name, std::process::id()));
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    // 8 MiB body，慢速服务器（~800KB/s/连接）给出充足的暂停窗口。
    let size = 8 * 1024 * 1024usize;
    let body = Arc::new(gen_body(size, 42));
    let gauge = Arc::new(Gauge::new());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    {
        let body = body.clone();
        let gauge = gauge.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let b = body.clone();
                let g = gauge.clone();
                tokio::spawn(async move {
                    let _ = handle_conn(stream, b, g).await;
                });
            }
        });
    }
    let url = format!("http://{addr}/file.bin");

    let config = EngineConfig {
        max_concurrent: 5,
        speed_limit_bps: 0,
        default_save_dir: work_dir.to_string_lossy().to_string(),
        app_data_dir: work_dir.to_string_lossy().to_string(),
        bt_config: BtConfig::default(),
        proxy_config: ProxyConfig::default(),
        user_agent: String::new(),
        data_dir_override: Some(work_dir.clone()),
        database_url: None,
    };
    let mut engine = Engine::new(config, Arc::new(NoopSink), Arc::new(NoopSelection))
        .await
        .expect("engine");

    // 排空进度/完成通道，防止通道塞满阻塞下载端。
    if let Some(mut rx) = engine.manager.take_progress_rx() {
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
    }
    if let Some(mut done_rx) = engine.manager.take_done_rx() {
        tokio::spawn(async move { while done_rx.recv().await.is_some() {} });
    }

    let hint = if use_hint { size as i64 } else { 0 };
    let task_id = engine
        .manager
        .create_task(
            url.clone(),
            work_dir.to_string_lossy().to_string(),
            "file.bin".to_string(),
            segments,
            String::new(),
            String::new(),
            hint,
            Vec::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            std::collections::HashMap::new(),
            Vec::new(),
            method,
            None,
            None,
        )
        .await
        .expect("create_task");
    eprintln!("[{name}] task={task_id} segments={segments} hint={hint}");

    // 等第一阶段并发爬升（最多 10s），再攒 1.5s 进度。
    let mut waited = 0u64;
    while gauge.peak.load(Ordering::SeqCst) < 2 && waited < 10_000 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        waited += 100;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let phase1_peak = gauge.peak.load(Ordering::SeqCst);
    eprintln!(
        "[{name}] phase1: peak={phase1_peak}, range_gets={}, full_gets={}",
        gauge.range_gets.load(Ordering::SeqCst),
        gauge.full_gets.load(Ordering::SeqCst)
    );

    let mut resume_peaks = Vec::new();
    for cycle in 1..=cycles {
        // ---- 暂停 ----
        engine.manager.pause_task(&task_id).await;
        let mut waited = 0u64;
        while gauge.active.load(Ordering::SeqCst) > 0 && waited < 10_000 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            waited += 100;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let segs = engine.db.load_segments(&task_id).await.unwrap();
        eprintln!(
            "[{name}] cycle{cycle} after pause: tasks.segments={}, segment_rows={}, seg_downloaded={:?}",
            engine.db.get_task_segments(&task_id).await.unwrap_or(-1),
            segs.len(),
            segs.iter().map(|s| s.downloaded_bytes).collect::<Vec<_>>()
        );

        // ---- 恢复 ----
        gauge.reset_peak();
        engine.manager.resume_task(&task_id).await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        let peak = gauge.peak.load(Ordering::SeqCst);
        let status = engine
            .db
            .load_task_by_id(&task_id)
            .await
            .unwrap()
            .map(|t| t.status)
            .unwrap_or(-1);
        eprintln!(
            "[{name}] cycle{cycle} resume: peak={peak}, range_gets={}, full_gets={}, status={status}",
            gauge.range_gets.load(Ordering::SeqCst),
            gauge.full_gets.load(Ordering::SeqCst)
        );
        resume_peaks.push(peak);
        if status == 3 {
            break; // 已完成，无法再暂停
        }
    }

    engine.manager.pause_task(&task_id).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    ScenarioResult {
        phase1_peak,
        resume_peaks,
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn pause_then_resume_keeps_multi_thread() {
    // 场景 1：显式 4 线程，两轮暂停/恢复
    let explicit = run_scenario("explicit4", 4, false, 2, None).await;
    // 场景 2：auto（segments=0，advisor 动态计算），两轮
    let auto = run_scenario("auto0", 0, false, 2, None).await;
    // 场景 3：浏览器扩展 hint 模式（跳过 probe），两轮
    let hint = run_scenario("hint4", 4, true, 2, None).await;
    // 场景 4：auto + hint（扩展流量 + 自动段数）
    let auto_hint = run_scenario("auto_hint", 0, true, 2, None).await;
    // 场景 5：扩展误捕获 CORS 预检 OPTIONS 的 payload（飞书云盘实录）——
    // RequestSpec::from_captured 应重映射为 GET，保持多线程。
    let options = run_scenario("options_remap", 4, true, 1, Some("OPTIONS".to_string())).await;

    let mut failures = Vec::new();
    for (name, r) in [
        ("explicit4", &explicit),
        ("auto0", &auto),
        ("hint4", &hint),
        ("auto_hint", &auto_hint),
        ("options_remap", &options),
    ] {
        if r.phase1_peak < 2 {
            failures.push(format!("{name}: 第一阶段未达多线程 peak={}", r.phase1_peak));
        }
        for (i, p) in r.resume_peaks.iter().enumerate() {
            if *p < 2 {
                failures.push(format!(
                    "{name}: 第 {} 次恢复退化为单线程 peak={p}（暂停前={}）",
                    i + 1,
                    r.phase1_peak
                ));
            }
        }
    }
    assert!(failures.is_empty(), "复现成功：\n{}", failures.join("\n"));
}
