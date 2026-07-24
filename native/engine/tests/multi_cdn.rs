//! Multi-CDN 多节点并发下载的确定性 e2e（方案 §3.7-2）。
//!
//! 三个本地 HTTP/1.1 服务分别绑 `127.0.0.1 / 127.0.0.2 / 127.0.0.3` 的
//! **同一端口**服务同一文件（Windows 环回整个 127/8 免配置可绑）；候选 IP
//! 经 `NodePool::multi` 静态注入（绕过需要真实网络的 DNS 聚合与门控——那些
//! 由 `cdn` 模块单元测试覆盖），host 用 `localhost`：SYS 节点走系统解析落到
//! `127.0.0.1`，钉定节点经 `.resolve()` 落到 `.2` / `.3`。
//!
//! 覆盖 #127 的两条主诉求：
//! 1. **分流**：分片分布在 ≥2 个 IP（`lease()` 的 per-节点并发上限强制分散）；
//! 2. **故障切换**：中途 kill 一个正在服务的节点 → 任务仍完成且文件逐字节
//!    一致（worker 翻译 `CdnNodeFailed` → coordinator 回收重派 → 其余节点
//!    接管）；以及"候选里混入死节点"的 SYS 兜底不变量。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fluxdown_engine::cdn::{ClientTemplate, NodePool};
use fluxdown_engine::db::Db;
use fluxdown_engine::downloader::{ProgressUpdate, RequestSpec, build_client};
use fluxdown_engine::events::{EngineEvent, EventSink};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::segment_coordinator::{ReportScope, run_coordinated_download};
use fluxdown_engine::speed_limiter::SpeedLimiter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct NoopTestSink;
impl EventSink for NoopTestSink {
    fn emit(&self, _event: EngineEvent) {}
}

/// 确定性伪随机 body（LCG）。
fn gen_body(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    while out.len() < len {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// 单节点测试服务器：支持 Range 的最小 HTTP/1.1 实现 + range GET 计数 +
/// 可中途 kill（accept 循环与所有活跃连接一并终止，模拟节点死亡）。
struct NodeServer {
    range_gets: Arc<AtomicUsize>,
    accept_task: tokio::task::JoinHandle<()>,
    conns: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl NodeServer {
    fn kill(&self) {
        self.accept_task.abort();
        for h in self.conns.lock().unwrap().drain(..) {
            h.abort();
        }
    }
}

impl Drop for NodeServer {
    fn drop(&mut self) {
        self.kill();
    }
}

/// 在指定地址启动节点服务器。`throttle_ms` 为每 64KB 响应块的延迟（拉长
/// 下载时长，给中途 kill 留出确定性窗口）。
async fn start_node(addr: SocketAddr, body: Arc<Vec<u8>>, throttle_ms: u64) -> NodeServer {
    let listener = TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("bind {addr}: {e}"));
    let range_gets = Arc::new(AtomicUsize::new(0));
    let conns: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let rg = range_gets.clone();
    let cs = conns.clone();
    let accept_task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            let rg = rg.clone();
            let h = tokio::spawn(async move {
                let _ = serve_conn(stream, body, rg, throttle_ms).await;
            });
            cs.lock().unwrap().push(h);
        }
    });
    NodeServer {
        range_gets,
        accept_task,
        conns,
    }
}

/// keep-alive 循环：解析请求头 → 200/206 响应。
async fn serve_conn(
    mut stream: TcpStream,
    body: Arc<Vec<u8>>,
    range_gets: Arc<AtomicUsize>,
    throttle_ms: u64,
) -> std::io::Result<()> {
    loop {
        // 读到 \r\n\r\n 为止。
        let mut buf = Vec::with_capacity(1024);
        let mut tmp = [0u8; 1024];
        loop {
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            if buf.len() > 64 * 1024 {
                return Ok(());
            }
        }
        let text = String::from_utf8_lossy(&buf);
        let mut range: Option<(usize, Option<usize>)> = None;
        let mut method = "GET";
        for (i, line) in text.split("\r\n").enumerate() {
            if i == 0 {
                if line.starts_with("HEAD") {
                    method = "HEAD";
                }
                continue;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.trim().eq_ignore_ascii_case("range")
                && let Some(v) = value.trim().strip_prefix("bytes=")
                && let Some((s, e)) = v.split_once('-')
            {
                let start = s.parse::<usize>().unwrap_or(0);
                let end = e.parse::<usize>().ok();
                range = Some((start, end));
            }
        }
        let total = body.len();
        let (status, start, end) = match range {
            Some((s, e)) => {
                range_gets.fetch_add(1, Ordering::Relaxed);
                let end = e.unwrap_or(total - 1).min(total - 1);
                ("206 Partial Content", s.min(total - 1), end)
            }
            None => ("200 OK", 0, total - 1),
        };
        let len = end - start + 1;
        let mut head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nETag: \"mcdn-v1\"\r\nContent-Type: application/octet-stream\r\n"
        );
        if status.starts_with("206") {
            head.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).await?;
        if method == "HEAD" {
            continue;
        }
        // 分块写出（可限速）。
        let mut off = start;
        while off <= end {
            let chunk_end = (off + 64 * 1024 - 1).min(end);
            stream.write_all(&body[off..=chunk_end]).await?;
            stream.flush().await?;
            off = chunk_end + 1;
            if throttle_ms > 0 && off <= end {
                tokio::time::sleep(std::time::Duration::from_millis(throttle_ms)).await;
            }
        }
    }
}

fn work_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fluxdown_mcdn_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create work dir");
    d
}

const TEST_UA: &str = "FluxDownMultiCdnTest/1.0";

/// 用注入的候选构建多节点池并跑 coordinator 到完成。
async fn run_with_pool(
    dir: &std::path::Path,
    task_id: &str,
    url: &str,
    total: i64,
    segments: i32,
    candidates: Vec<IpAddr>,
) -> (
    Result<i64, fluxdown_engine::downloader::DownloadError>,
    std::path::PathBuf,
) {
    let dest = dir.join(format!("{task_id}.bin"));
    let db = Db::open(dir).await.expect("Db::open");
    db.insert_task(
        task_id,
        url,
        &dest.file_name().unwrap().to_string_lossy(),
        &dir.to_string_lossy(),
        segments,
        total,
        "",
        "",
        "",
        0,
    )
    .await
    .expect("insert_task");

    let task_client = build_client(&ProxyConfig::default(), TEST_UA).expect("client");
    let template = ClientTemplate {
        proxy: ProxyConfig::default(),
        user_agent: TEST_UA.to_string(),
    };
    let nodes = NodePool::multi(
        template,
        "localhost",
        candidates,
        std::collections::HashMap::new(),
        task_client,
        db.clone(),
        task_id,
        None,
    );
    assert!(nodes.is_multi());

    let speed_limiter = SpeedLimiter::new(0);
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(256);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let cancel = CancellationToken::new();
    let res = run_coordinated_download(
        task_id,
        url,
        &dest,
        total,
        false,
        segments,
        nodes,
        &db,
        &tx,
        &cancel,
        &speed_limiter,
        &RequestSpec::empty_get(),
        &NoopTestSink,
        "",
        "",
        ReportScope::whole_task(),
        0,
    )
    .await;
    drop(tx);
    let _ = drain.await;
    (res, dest)
}

/// 分流 + 故障切换：三 IP 服务同一文件；等 `.2` 真正服务到 range GET 后
/// kill 它 → 任务仍完成、文件逐字节一致、且分片至少落在 2 个 IP 上。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_cdn_distributes_and_survives_node_kill() {
    let body = Arc::new(gen_body(8 * 1024 * 1024, 0xC0FFEE));
    let ip1 = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let ip2 = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
    let ip3 = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 3));

    // 先在 127.0.0.1:0 拿随机端口，再把 .2/.3 绑到同一端口。
    let probe = TcpListener::bind((ip1, 0)).await.expect("bind :0");
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let s1 = start_node(SocketAddr::new(ip1, port), body.clone(), 25).await;
    let s2 = start_node(SocketAddr::new(ip2, port), body.clone(), 25).await;
    let s3 = start_node(SocketAddr::new(ip3, port), body.clone(), 25).await;

    let dir = work_dir("kill");
    let url = format!("http://localhost:{port}/f.bin");
    let run = run_with_pool(
        &dir,
        "mcdn-kill",
        &url,
        body.len() as i64,
        8,
        vec![ip2, ip3],
    );
    tokio::pin!(run);

    // 与下载并行：等 .2 服务到第一个 range GET 后 kill 之（含活跃连接）。
    let killer = async {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while s2.range_gets.load(Ordering::Relaxed) == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "30s 内节点 .2 未收到任何 range GET——分散调度失效"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        s2.kill();
    };

    let ((res, dest), _) = tokio::join!(run, killer);
    let total = res.expect("kill 一个节点后任务必须仍然完成");
    assert_eq!(total, body.len() as i64);

    let got = tokio::fs::read(&dest).await.expect("read dest");
    assert_eq!(got.len(), body.len());
    assert_eq!(got, *body, "多节点拼装的文件必须与源逐字节一致");

    let counts = [
        s1.range_gets.load(Ordering::Relaxed),
        s2.range_gets.load(Ordering::Relaxed),
        s3.range_gets.load(Ordering::Relaxed),
    ];
    let served = counts.iter().filter(|&&c| c > 0).count();
    assert!(
        served >= 2,
        "分片必须分布在 ≥2 个 IP（实际 range GET 计数: {counts:?}）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// SYS 兜底不变量（不变量 1）：候选里混入一个从未启动的死节点（connect
/// 必败）——任务仍完成且内容一致（死节点失败被翻译为 CdnNodeFailed 回收
/// 重派，绝不升级为任务失败）。
///
/// 注意：不断言健康钉定节点的参与度——两个测试共享进程级健康度缓存
///（同 host `localhost`），上一测试 kill `.2` 后其 EWMA 已被降权，调度分数
/// 属实现细节；"钉定节点真实参与分流"由上方 kill 测试的 served>=2 覆盖。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_cdn_dead_candidate_never_fails_task() {
    let body = Arc::new(gen_body(2 * 1024 * 1024, 0xBEEF));
    let ip1 = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let ip2 = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
    let dead = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 3)); // 不启动服务器

    let probe = TcpListener::bind((ip1, 0)).await.expect("bind :0");
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let _s1 = start_node(SocketAddr::new(ip1, port), body.clone(), 0).await;
    let s2 = start_node(SocketAddr::new(ip2, port), body.clone(), 0).await;

    let dir = work_dir("dead");
    let url = format!("http://localhost:{port}/f.bin");
    let (res, dest) = run_with_pool(
        &dir,
        "mcdn-dead",
        &url,
        body.len() as i64,
        4,
        vec![ip2, dead],
    )
    .await;
    let total = res.expect("死候选节点绝不导致任务失败");
    assert_eq!(total, body.len() as i64);
    let got = tokio::fs::read(&dest).await.expect("read dest");
    assert_eq!(got, *body);
    println!(
        "healthy pinned node range GETs: {}",
        s2.range_gets.load(Ordering::Relaxed)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
