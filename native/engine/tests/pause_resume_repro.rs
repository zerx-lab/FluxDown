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
        .create_task(fluxdown_engine::download_manager::NewTaskSpec {
            url: url.clone(),
            save_dir: work_dir.to_string_lossy().to_string(),
            file_name: "file.bin".to_string(),
            segments,
            hint_file_size: hint,
            method,
            ..Default::default()
        })
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

// ---------------------------------------------------------------------------
// 改线程数保留进度：暂停 → set_task_segments → 恢复。断言段行/已下字节
// 完整保留、tasks.segments 更新、恢复期间绝无全量 GET（不浪费一次性 token）。
// ---------------------------------------------------------------------------

/// 改线程数场景的关键观测量。
struct CsResult {
    downloaded_before: i64,
    downloaded_after: i64,
    tasks_segments: i32,
    rows_after: usize,
    full_gets: usize,
    status: i32,
    file_len: i64,
}

/// 跑一次"下载一会 → (可选暂停) → 改线程数 → 恢复至完成"，返回关键观测量。
/// `pause_first=false` 时不手动暂停，直接在下载中改线程数，验证引擎的
/// 自动暂停/恢复路径。
async fn run_change_segments(
    name: &str,
    initial_segments: i32,
    new_segments: i32,
    pause_first: bool,
) -> CsResult {
    let work_dir =
        std::env::temp_dir().join(format!("fluxdown_cs_{}_{}", name, std::process::id()));
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let size = 6 * 1024 * 1024usize;
    let body = Arc::new(gen_body(size, 7));
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
    if let Some(mut rx) = engine.manager.take_progress_rx() {
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
    }
    if let Some(mut done_rx) = engine.manager.take_done_rx() {
        tokio::spawn(async move { while done_rx.recv().await.is_some() {} });
    }

    let task_id = engine
        .manager
        .create_task(fluxdown_engine::download_manager::NewTaskSpec {
            url: url.clone(),
            save_dir: work_dir.to_string_lossy().to_string(),
            file_name: "file.bin".to_string(),
            segments: initial_segments,
            ..Default::default()
        })
        .await
        .expect("create_task");

    // 攒一点进度（部分下载）。
    let mut waited = 0u64;
    while gauge.peak.load(Ordering::SeqCst) < 2 && waited < 10_000 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        waited += 100;
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // pause_first：手动暂停后再改（详情面板暂停态入口）。
    // 否则：下载中直接改，验证引擎自动暂停→改→恢复（右键菜单入口）。
    if pause_first {
        engine.manager.pause_task(&task_id).await;
        let mut waited = 0u64;
        while gauge.active.load(Ordering::SeqCst) > 0 && waited < 10_000 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            waited += 100;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // 改线程数前快照。
    let segs_before = engine.db.load_segments(&task_id).await.unwrap();
    let rows_before = segs_before.len();
    let downloaded_before: i64 = segs_before.iter().map(|s| s.downloaded_bytes).sum();
    assert!(rows_before > 0, "[{name}] 改前应有段行");
    assert!(downloaded_before > 0, "[{name}] 改前应有已下进度");

    // 清计数，之后统计到完成为止的全量 GET（应为 0 —— 续传不浪费一次性 token）。
    gauge.reset_peak();

    // 改线程数（核心被测行为）。活跃任务会被引擎自动暂停→改→恢复。
    let ok = engine
        .manager
        .set_task_segments(&task_id, new_segments)
        .await
        .expect("set_task_segments");
    assert!(ok, "[{name}] 改线程数应成功");

    // 改后快照：段行必须保留、tasks.segments 已更新、已下字节不减少。
    let segs_after = engine.db.load_segments(&task_id).await.unwrap();
    let rows_after = segs_after.len();
    let downloaded_after: i64 = segs_after.iter().map(|s| s.downloaded_bytes).sum();
    let tasks_segments = engine.db.get_task_segments(&task_id).await.unwrap();

    // pause_first 时需手动恢复；活跃场景引擎已在 set_task_segments 内自动恢复。
    if pause_first {
        engine.manager.resume_task(&task_id).await;
    }
    let mut status = -1;
    let mut waited = 0u64;
    while waited < 30_000 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        waited += 200;
        status = engine
            .db
            .load_task_by_id(&task_id)
            .await
            .unwrap()
            .map(|t| t.status)
            .unwrap_or(-1);
        if status == 3 {
            break;
        }
    }
    let full_gets = gauge.full_gets.load(Ordering::SeqCst);
    let range_gets = gauge.range_gets.load(Ordering::SeqCst);

    // 完成后目标文件大小校验。
    let dest = work_dir.join("file.bin");
    let file_len = tokio::fs::metadata(&dest)
        .await
        .map(|m| m.len() as i64)
        .unwrap_or(-1);

    engine.manager.pause_task(&task_id).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    eprintln!(
        "[change_segments {name}] before={downloaded_before} after={downloaded_after} \
         tasks_segments={tasks_segments} rows={rows_after} full_gets={full_gets} \
         range_gets={range_gets} status={status} file_len={file_len}"
    );
    CsResult {
        downloaded_before,
        downloaded_after,
        tasks_segments,
        rows_after,
        full_gets,
        status,
        file_len,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn change_segments_preserves_progress() {
    let size = 6 * 1024 * 1024i64;

    // 暂停态增线程 4 → 8：进度精确保留、段行保留、tasks.segments=8、恢复无全量 GET、完成。
    let up = run_change_segments("up_4_8", 4, 8, true).await;
    assert_eq!(
        up.downloaded_after, up.downloaded_before,
        "增线程：已下字节必须完整保留"
    );
    assert!(up.rows_after > 0, "增线程：段行必须保留（未被删除）");
    assert_eq!(up.tasks_segments, 8, "增线程：tasks.segments 应更新为 8");
    assert_eq!(
        up.full_gets, 0,
        "增线程：恢复期间绝不应出现全量 GET（否则浪费一次性 token）"
    );
    assert_eq!(up.status, 3, "增线程：应最终下载完成");
    assert_eq!(up.file_len, size, "增线程：完成文件大小应正确");

    // 暂停态减线程 8 → 2：同样保留进度并完成。
    let down = run_change_segments("down_8_2", 8, 2, true).await;
    assert_eq!(
        down.downloaded_after, down.downloaded_before,
        "减线程：已下字节必须完整保留"
    );
    assert!(down.rows_after > 0, "减线程：段行必须保留");
    assert_eq!(down.tasks_segments, 2, "减线程：tasks.segments 应更新为 2");
    assert_eq!(down.full_gets, 0, "减线程：恢复期间绝不应出现全量 GET");
    assert_eq!(down.status, 3, "减线程：应最终下载完成");
    assert_eq!(down.file_len, size, "减线程：完成文件大小应正确");

    // 下载中直接改 4 → 16（右键菜单入口）：引擎自动暂停→改→恢复。
    // 进度不减少（活跃任务字节仍在增长）、段行保留、无全量 GET、完成。
    let active = run_change_segments("active_4_16", 4, 16, false).await;
    assert!(
        active.downloaded_after >= active.downloaded_before,
        "下载中改：进度绝不回退（before={}, after={}）",
        active.downloaded_before,
        active.downloaded_after
    );
    assert!(active.rows_after > 0, "下载中改：段行必须保留");
    assert_eq!(
        active.tasks_segments, 16,
        "下载中改：tasks.segments 应更新为 16"
    );
    assert_eq!(active.full_gets, 0, "下载中改：全程绝不应出现全量 GET");
    assert_eq!(active.status, 3, "下载中改：应最终下载完成");
    assert_eq!(active.file_len, size, "下载中改：完成文件大小应正确");
}
