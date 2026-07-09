//! 实证测试：直接调用 FluxDown 真实的 `run_coordinated_download`，
//! 反复下载同一 URL，检测是否产生内容损坏。
//!
//! 与 `tests_repro/test_repeated.sh` 的区别：
//!   - shell 脚本只用 curl 模拟"固定 Range 切片"，缺失了 FluxDown 的
//!     **动态拆分（split_largest / try_proactive_split）** 这一关键变量。
//!   - 本模块直接调用 `segment_coordinator::run_coordinated_download`，
//!     完全复用 FluxDown 的真实代码：BufWriter / fallocate /
//!     SetFileInformationByHandle / 拆分协调 / cancel_token / etc.
//!
//! 用法：
//!   cargo test -p hub --release --lib corruption_test \
//!       -- --ignored --nocapture --test-threads=1
//!
//! 默认 `#[ignore]`，需要网络才能跑。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use fluxdown_engine::db::Db;
use fluxdown_engine::downloader::{ProgressUpdate, build_client, resolve_file_info};
use fluxdown_engine::events::{EngineEvent, EventSink};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::segment_coordinator::run_coordinated_download;
use fluxdown_engine::speed_limiter::SpeedLimiter;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 测试专用 no-op sink——本测试只关心下载产物是否损坏，不关心事件流。
struct NoopTestSink;
impl EventSink for NoopTestSink {
    fn emit(&self, _event: EngineEvent) {}
}

// ---------------------------------------------------------------------------
// 测试参数
// ---------------------------------------------------------------------------

/// 测试 URL：阿里云镜像 R 4.3.0 安装包。
/// 81.9 MB 二进制，多层 CDN（Tengine + 多级 cache），无 Content-Encoding，
/// 与用户反馈的 zip/exe 损坏场景对齐。
const TEST_URL: &str = "https://mirrors.aliyun.com/CRAN/bin/windows/base/old/4.3.0/R-4.3.0-win.exe";

/// 反复下载次数。
const ITERATIONS: usize = 5;

/// 初始 segment 数（足够多，能触发动态拆分）。
const INITIAL_SEGMENTS: i32 = 32;

// ---------------------------------------------------------------------------
// 工具函数
// ---------------------------------------------------------------------------

/// 计算文件 SHA256。
async fn compute_sha256(path: &std::path::Path) -> String {
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
    let result = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in result {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", b);
    }
    hex
}

/// 用 FluxDown 自己的 build_client 单流下载，作为 baseline。
/// 与 multi-segment 用完全一致的 client 配置（identity / TLS / UA），
/// 确保对比公平：差异只在"单流 vs 多 segment 协调器"。
async fn download_baseline(
    url: &str,
    dest: &std::path::Path,
    ua: &str,
) -> Result<(i64, String), String> {
    use tokio::io::AsyncWriteExt;

    let proxy = ProxyConfig::default();
    let client = build_client(&proxy, ua).map_err(|e| format!("baseline build_client: {e}"))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("baseline GET: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("baseline status: {}", resp.status()));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("baseline read body: {e}"))?;

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| format!("baseline create file: {e}"))?;
    file.write_all(&bytes)
        .await
        .map_err(|e| format!("baseline write: {e}"))?;
    file.flush()
        .await
        .map_err(|e| format!("baseline flush: {e}"))?;
    drop(file);

    let size = bytes.len() as i64;
    let sha = compute_sha256(dest).await;
    Ok((size, sha))
}

/// 启动一个后台任务消费 progress channel 防止 sender block。
fn spawn_progress_drain(mut rx: mpsc::Receiver<ProgressUpdate>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // 丢弃所有进度更新，仅保证 sender 不会因 channel 满阻塞 worker
        }
    })
}

/// 单次完整的 multi-segment 下载，使用 FluxDown 真实代码路径。
///
/// 返回 (实际文件大小, SHA256, 耗时)。
async fn run_one_real_download(
    work_dir: &std::path::Path,
    iter: usize,
    total_bytes: i64,
    etag: &str,
    last_modified: &str,
) -> Result<(i64, String, Duration), String> {
    let task_id = format!("test-iter-{}", iter);
    let dest = work_dir.join(format!("multi_{}.bin", iter));

    // 清理上一次的产物
    let _ = tokio::fs::remove_file(&dest).await;

    // 构造 FluxDown 真实 client（与生产代码完全一致）
    let proxy = ProxyConfig::default();
    let client = build_client(&proxy, "Mozilla/5.0 FluxDownCorruptionTest/1.0")
        .map_err(|e| format!("build_client: {e}"))?;

    // 创建独立 SQLite（每次迭代用独立 db 避免脏状态）
    let db = Db::open(work_dir)
        .await
        .map_err(|e| format!("Db::open: {e:?}"))?;

    // 必须先 insert_task 才能 insert_segments（外键约束）
    db.insert_task(
        &task_id,
        TEST_URL,
        &dest.file_name().unwrap().to_string_lossy(),
        &work_dir.to_string_lossy(),
        INITIAL_SEGMENTS,
        total_bytes,
        "", // proxy_url
        "", // queue_id
        "", // checksum
    )
    .await
    .map_err(|e| format!("insert_task: {e:?}"))?;

    // 准备 multi-segment 调用所需依赖
    let cancel = CancellationToken::new();
    let speed_limiter = SpeedLimiter::new(0); // 0 = 无限速
    let (progress_tx, progress_rx) = mpsc::channel::<ProgressUpdate>(256);
    let drain_handle = spawn_progress_drain(progress_rx);

    let spec = fluxdown_engine::downloader::RequestSpec::empty_get();
    let sink = NoopTestSink;

    let started = Instant::now();

    let result = run_coordinated_download(
        &task_id,
        TEST_URL,
        &dest,
        total_bytes,
        INITIAL_SEGMENTS,
        &client,
        &db,
        &progress_tx,
        &cancel,
        &speed_limiter,
        &spec,
        &sink,
        etag,
        last_modified,
    )
    .await;

    let elapsed = started.elapsed();

    // 关闭 progress channel，让 drainer 退出
    drop(progress_tx);
    let _ = drain_handle.await;

    if let Err(e) = result {
        return Err(format!("run_coordinated_download: {e}"));
    }

    let size = match tokio::fs::metadata(&dest).await {
        Ok(m) => m.len() as i64,
        Err(e) => return Err(format!("stat dest: {e}")),
    };
    let sha = compute_sha256(&dest).await;

    Ok((size, sha, elapsed))
}

// ---------------------------------------------------------------------------
// 主测试
// ---------------------------------------------------------------------------

/// 反复跑 multi-segment 下载，检测是否偶发损坏。
///
/// 测试流程：
///   1. probe 拿到真实文件大小 + ETag + Last-Modified
///   2. 用 reqwest 单流下载一次作为 baseline
///   3. 反复用真实 `run_coordinated_download` 下载 N 次
///   4. 每次对比 SHA256，统计损坏率
///
/// 这是**真实测试**——直接复用 FluxDown 自己的代码，不是模拟。
#[tokio::test(flavor = "current_thread")]
#[ignore = "needs network and downloads ~80MB × N times, run with --ignored"]
async fn real_multi_segment_corruption_repeat() {
    let work_dir =
        std::env::temp_dir().join(format!("fluxdown_corruption_test_{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir)
        .await
        .expect("create work_dir");

    println!("==========================================");
    println!("FluxDown 真实代码 multi-segment 损坏测试");
    println!("URL          : {}", TEST_URL);
    println!("迭代次数     : {}", ITERATIONS);
    println!("初始 segment : {}", INITIAL_SEGMENTS);
    println!("工作目录     : {}", work_dir.display());
    println!("==========================================");

    // ---- Step 0: probe（与 FluxDown 生产代码一致）----
    println!("\n[Step 0] resolve_file_info — 模拟 FluxDown probe");
    let proxy = ProxyConfig::default();
    let client = build_client(&proxy, "Mozilla/5.0 FluxDownCorruptionTest/1.0")
        .expect("build_client for probe");

    let info = resolve_file_info(
        &client,
        TEST_URL,
        &fluxdown_engine::downloader::RequestSpec::empty_get(),
    )
    .await
    .expect("resolve_file_info");

    println!("  total_bytes       : {}", info.total_bytes);
    println!("  supports_range    : {}", info.supports_range);
    println!("  etag              : {:?}", info.etag);
    println!("  last_modified     : {:?}", info.last_modified);
    println!("  content_type      : {:?}", info.content_type);
    println!(
        "  content_encoding_compressed: {}",
        info.content_encoding_compressed
    );

    assert!(info.total_bytes > 0, "probe must return total_bytes > 0");
    assert!(info.supports_range, "test URL must support Range");

    let total = info.total_bytes;
    let etag = Arc::new(info.etag.clone());
    let last_modified = Arc::new(info.last_modified.clone());

    // ---- Step 1: baseline（独立 reqwest 单流下载）----
    println!("\n[Step 1] baseline — reqwest 单流下载");
    let base_dest = work_dir.join("baseline.bin");
    let base_started = Instant::now();
    let (base_size, base_sha) = download_baseline(
        TEST_URL,
        &base_dest,
        "Mozilla/5.0 FluxDownCorruptionTest/1.0",
    )
    .await
    .expect("baseline download");
    println!(
        "  baseline 大小: {}  SHA: {}  耗时: {:?}",
        base_size,
        base_sha,
        base_started.elapsed()
    );
    assert_eq!(base_size, total, "baseline size must equal probe total");

    // ---- Step 2: 反复 N 次 multi-segment ----
    println!("\n[Step 2] run_coordinated_download × {}", ITERATIONS);

    let mut corrupt_count = 0usize;
    let mut size_mismatch_count = 0usize;
    let mut error_count = 0usize;
    let mut results: Vec<(usize, String, String)> = Vec::new();

    for iter in 1..=ITERATIONS {
        println!("\n  ---- 迭代 {} / {} ----", iter, ITERATIONS);

        let outcome = run_one_real_download(&work_dir, iter, total, &etag, &last_modified).await;

        match outcome {
            Ok((size, sha, dur)) => {
                let size_ok = size == total;
                let sha_ok = sha == base_sha;
                if !size_ok {
                    size_mismatch_count += 1;
                }
                if !sha_ok {
                    corrupt_count += 1;
                }

                let tag = if sha_ok && size_ok {
                    "✅ OK"
                } else {
                    "❌ CORRUPT"
                };
                println!(
                    "    {} size={} ({}), sha={}, 耗时={:?}",
                    tag,
                    size,
                    if size_ok { "ok" } else { "MISMATCH" },
                    sha,
                    dur
                );

                if !sha_ok {
                    println!("      baseline: {}", base_sha);
                    println!("      multi   : {}", sha);

                    // 保留损坏文件以备分析
                    let preserve = work_dir.join(format!("CORRUPT_iter_{}.bin", iter));
                    if let Ok(()) =
                        tokio::fs::copy(work_dir.join(format!("multi_{}.bin", iter)), &preserve)
                            .await
                            .map(|_| ())
                    {
                        println!("      已保存损坏样本: {}", preserve.display());
                    }
                }

                results.push((iter, sha, format!("{:?}", dur)));
            }
            Err(e) => {
                error_count += 1;
                println!("    ⚠️ ERROR: {}", e);
                results.push((iter, format!("<error: {}>", e), "n/a".to_string()));
            }
        }
    }

    // ---- Step 3: 总结 ----
    println!("\n==========================================");
    println!("结果总结");
    println!("==========================================");
    println!("总迭代数     : {}", ITERATIONS);
    println!("损坏次数     : {}", corrupt_count);
    println!("大小异常次数 : {}", size_mismatch_count);
    println!("错误次数     : {}", error_count);
    println!("baseline SHA : {}", base_sha);
    println!("\n各次 SHA：");
    for (i, sha, dur) in &results {
        println!("  iter {} : {} ({})", i, sha, dur);
    }

    if corrupt_count == 0 && error_count == 0 {
        println!(
            "\n✅ {} 次真实下载全部 SHA 一致 — 该 URL 下未复现损坏",
            ITERATIONS
        );
        println!("   说明 FluxDown segment_coordinator 在该场景下行为正确");
    } else {
        println!(
            "\n❌ 复现了问题：{} 次损坏 / {} 次错误（共 {} 次）",
            corrupt_count, error_count, ITERATIONS
        );
    }

    // 测试本身不 fail，因为目的是诊断而非断言
    // （即使复现了 bug，也希望保留产物供分析，不希望 assert! 让 CI 红）
    println!("\n产物保留在: {}", work_dir.display());
}
