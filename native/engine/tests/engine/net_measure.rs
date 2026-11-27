//! `DownloadManager::measure_routes_future`（AI 助手 `fd_measure_routes`）的确定性 e2e。
//!
//! 本地 raw-tokio HTTP/1.1 服务器：一个支持 Range（多连接、读取量上限截停），一个忽略 Range
//! 只回 200（退回单连接、读完即止）。测量不建任务、不写盘：数据库任务数保持为零。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::net_probe::{MeasureError, MeasureRequest};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MIB: usize = 1024 * 1024;

struct Server {
    url: String,
    ranged_requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_server(len: usize, honor_range: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body: Arc<Vec<u8>> = Arc::new((0..len).map(|i| (i % 251) as u8).collect());
    let ranged_requests = Arc::new(AtomicUsize::new(0));
    let counter = ranged_requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let body = body.clone();
            let counter = counter.clone();
            tokio::spawn(async move {
                // 客户端在读取量达到上限时主动断开，写端的断连错误属正常。
                if let Err(error) = serve(stream, body, honor_range, counter).await {
                    assert!(
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ),
                        "serve: {error}"
                    );
                }
            });
        }
    });
    Server {
        url: format!("http://{addr}/file.bin"),
        ranged_requests,
        task,
    }
}

async fn serve(
    mut stream: TcpStream,
    body: Arc<Vec<u8>>,
    honor_range: bool,
    ranged_requests: Arc<AtomicUsize>,
) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await? == 0 {
            return Ok(());
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let offset = head
        .lines()
        .find_map(|line| line.strip_prefix("range: bytes="))
        .and_then(|range| range.trim().trim_end_matches('-').parse::<usize>().ok());
    let (status, start, extra) = match offset {
        Some(start) if honor_range && start < body.len() => {
            ranged_requests.fetch_add(1, Ordering::SeqCst);
            (
                "206 Partial Content",
                start,
                format!(
                    "Content-Range: bytes {start}-{}/{}\r\nAccept-Ranges: bytes\r\n",
                    body.len() - 1,
                    body.len()
                ),
            )
        }
        _ => ("200 OK", 0, String::new()),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
        body.len() - start
    );
    stream.write_all(response.as_bytes()).await?;
    for chunk in body[start..].chunks(64 * 1024) {
        stream.write_all(chunk).await?;
    }
    stream.shutdown().await
}

async fn engine(tag: &str) -> (Engine, std::path::PathBuf) {
    let work = std::env::temp_dir().join(format!(
        "fluxdown-net-measure-{tag}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    tokio::fs::create_dir_all(&work).await.unwrap();
    let engine = Engine::new(
        EngineConfig {
            max_concurrent: 2,
            speed_limit_bps: 0,
            upload_limit_bps: 0,
            default_save_dir: work.to_string_lossy().into_owned(),
            app_data_dir: work.to_string_lossy().into_owned(),
            bt_config: BtConfig::default(),
            proxy_config: ProxyConfig::default(),
            user_agent: String::new(),
            data_dir_override: Some(work.clone()),
            database_url: None,
        },
        Arc::new(NoopSink),
        Arc::new(NoopSelection),
    )
    .await
    .expect("engine");
    (engine, work)
}

fn request(url: &str, connections: u32, max_bytes: u64) -> MeasureRequest {
    MeasureRequest {
        url: Some(url.to_owned()),
        routes: vec!["current".to_owned(), "direct".to_owned()],
        connections: Some(connections),
        max_bytes,
        max_duration: Duration::from_secs(10),
        ..MeasureRequest::default()
    }
}

async fn cleanup(engine: Engine, work: &std::path::Path) {
    drop(engine);
    if let Err(error) = tokio::fs::remove_dir_all(work).await {
        eprintln!("leaving test dir {}: {error}", work.display());
    }
}

/// Range 来源：按计划开多条连接，读取量达到上限即停；两条路线依次测量，均为直连。
/// 同一时刻只允许一次测量；测量结束后释放。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ranged_source_uses_parallel_connections_and_stops_at_byte_budget() {
    let server = start_server(8 * MIB, true).await;
    let (engine, work) = engine("ranged").await;
    let budget = 2 * MIB as u64;
    let future = engine
        .manager
        .measure_routes_future(request(&server.url, 4, budget))
        .await
        .expect("plan");
    assert!(matches!(
        engine
            .manager
            .measure_routes_future(request(&server.url, 1, budget))
            .await,
        Err(MeasureError::Busy)
    ));
    let report = future.await;

    assert_eq!(report.scheme, "http");
    assert_eq!(report.host, "127.0.0.1");
    assert_eq!(report.routes.len(), 2);
    for route in &report.routes {
        assert!(route.ok, "{route:?}");
        assert_eq!(route.egress, "direct", "{route:?}");
        assert_eq!(route.http_status, Some(206));
        assert_eq!(route.range_supported, Some(true));
        assert_eq!(route.total_bytes, Some(8 * MIB as u64));
        assert_eq!(route.connections_used, 4, "{route:?}");
        assert_eq!(route.per_connection_bytes_per_sec.len(), 4);
        assert_eq!(route.stopped_by, "bytes", "{route:?}");
        assert!(route.bytes >= budget, "{route:?}");
        assert!(route.bytes < 8 * MIB as u64, "{route:?}");
        assert!(route.avg_bytes_per_sec > 0);
        assert_eq!(route.timing_target, "origin");
        assert!(route.connect_ms.is_some());
    }
    assert!(server.ranged_requests.load(Ordering::SeqCst) >= 8);

    // 锁已释放：下一次测量可以开始。
    let again = engine
        .manager
        .measure_routes_future(request(&server.url, 1, budget))
        .await;
    assert!(again.is_ok());
    drop(again);

    assert!(engine.db.load_all_tasks().await.unwrap().is_empty());
    cleanup(engine, &work).await;
}

/// 不支持 Range 的来源：只开一条连接，读完整个来源即停（`eof`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_without_range_falls_back_to_one_connection_until_eof() {
    let server = start_server(MIB, false).await;
    let (engine, work) = engine("plain").await;
    let mut request = request(&server.url, 8, 64 * MIB as u64);
    request.routes = vec!["direct".to_owned()];
    let report = engine
        .manager
        .measure_routes_future(request)
        .await
        .expect("plan")
        .await;

    let route = &report.routes[0];
    assert!(route.ok, "{route:?}");
    assert_eq!(route.http_status, Some(200));
    assert_eq!(route.range_supported, Some(false));
    assert_eq!(route.connections_used, 1);
    assert_eq!(route.bytes, MIB as u64);
    assert_eq!(route.stopped_by, "eof");
    assert_eq!(server.ranged_requests.load(Ordering::SeqCst), 0);
    cleanup(engine, &work).await;
}

/// 来源 URL 只接受 http / https / ftp，且任务与 URL 恰好给一个。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_sources_are_rejected_before_any_network_io() {
    let (engine, work) = engine("invalid").await;
    for request in [
        MeasureRequest {
            url: Some("file:///etc/passwd".to_owned()),
            ..MeasureRequest::default()
        },
        MeasureRequest::default(),
        MeasureRequest {
            url: Some("https://example.com/a".to_owned()),
            task_id: Some("t1".to_owned()),
            ..MeasureRequest::default()
        },
    ] {
        assert!(matches!(
            engine.manager.measure_routes_future(request).await,
            Err(MeasureError::InvalidArgument(_))
        ));
    }
    assert!(matches!(
        engine
            .manager
            .measure_routes_future(MeasureRequest {
                task_id: Some("missing".to_owned()),
                ..MeasureRequest::default()
            })
            .await,
        Err(MeasureError::NotFound)
    ));
    cleanup(engine, &work).await;
}
