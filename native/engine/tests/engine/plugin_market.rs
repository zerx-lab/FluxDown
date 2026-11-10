//! 插件市场客户端集成测试（本地 HTTP，无外网依赖）。
//!
//! 覆盖：索引拉取/HTTP fallback、sequence 防回滚、代理真实请求路由与 None 忽略
//! 环境代理，以及 https-only 镜像白名单（拒 http 降级）。

#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::net::TcpListener;
use std::sync::Arc;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::plugin::{MarketClient, MarketError};
use fluxdown_engine::proxy_config::{ProxyConfig, ProxyMode};
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

/// 本地服务器：GET 任意路径返回预置 body（用于服务 index.json）。
fn spawn_index_server(body: String) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];

            std::io::Read::read(&mut stream, &mut buf).expect("read test HTTP request");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );

            stream.write_all(resp.as_bytes()).unwrap_or_else(|error| {
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                    ),
                    "test server response failed: {error}"
                );
            });

            stream.flush().unwrap_or_else(|error| {
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                    ),
                    "test server response failed: {error}"
                );
            });
        }
    });
    (port, handle)
}

fn uniq() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), n)
}

async fn make_engine(work: &std::path::Path) -> Engine {
    let cfg = EngineConfig {
        max_concurrent: 4,
        speed_limit_bps: 0,
        upload_limit_bps: 0,
        default_save_dir: work.to_string_lossy().into_owned(),
        app_data_dir: work.to_string_lossy().into_owned(),
        bt_config: BtConfig::default(),
        proxy_config: ProxyConfig::default(),
        user_agent: String::new(),
        data_dir_override: Some(work.to_path_buf()),
        database_url: None,
    };
    Engine::new(cfg, Arc::new(NoopSink), Arc::new(NoopSelection))
        .await
        .expect("engine")
}

fn index_json(sequence: u64, mirror: &str) -> String {
    format!(
        r#"{{"indexId":"11111111-1111-1111-1111-111111111111","sequence":{sequence},"updated":"now",
        "entries":[{{"pluginId":"test@rewriter","version":"1.0.0","sequence":{sequence},
        "contentHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "mirrors":["{mirror}"],"yanked":"none"}}]}}"#
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_index_parses_and_enforces_watermark() {
    let work = std::env::temp_dir().join(format!("fluxdown-market-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm");

    // 第一次：sequence=5。
    let (port, _s1) = spawn_index_server(index_json(5, "https://example.com/p.fxplug"));
    let url = format!("http://127.0.0.1:{port}/index.json");
    let mc = MarketClient::new(
        pm.clone(),
        engine.db.clone(),
        vec![url],
        &ProxyConfig::default(),
    )
    .expect("market client");
    let idx = mc.fetch_index().await.expect("fetch ok");
    assert_eq!(idx.index_id, "11111111-1111-1111-1111-111111111111");
    assert_eq!(idx.sequence, 5);
    assert_eq!(idx.entries.len(), 1);
    assert_eq!(idx.entries[0].plugin_id, "test@rewriter");

    // 第二次：sequence=3（< 高水位 5）→ 防回滚拒绝。
    let (port2, _s2) = spawn_index_server(index_json(3, "https://example.com/p.fxplug"));
    let url2 = format!("http://127.0.0.1:{port2}/index.json");
    let mc2 = MarketClient::new(pm, engine.db.clone(), vec![url2], &ProxyConfig::default())
        .expect("market client");
    let err = mc2.fetch_index().await.expect_err("rollback rejected");
    assert!(matches!(
        err,
        MarketError::SequenceRollback {
            seen: 3,
            watermark: 5
        }
    ));

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_mirror_rejected_https_only() {
    let work = std::env::temp_dir().join(format!("fluxdown-market-http-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm");

    // 镜像是 http:// → download_verified 跳过全部 → AllMirrorsFailed。
    let (port, _s) = spawn_index_server(index_json(1, "http://127.0.0.1:9/p.fxplug"));
    let url = format!("http://127.0.0.1:{port}/index.json");
    let mc = MarketClient::new(pm, engine.db.clone(), vec![url], &ProxyConfig::default())
        .expect("market client");
    let idx = mc.fetch_index().await.expect("fetch ok");
    let entry = idx.entries[0].clone();
    let err = mc
        .install_entry(&entry, false)
        .await
        .expect_err("http mirror rejected");
    assert!(matches!(err, MarketError::AllMirrorsFailed));

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

/// 回归（Bug：索引拉取无体积上限）：被投毒/损坏的源返回超大响应时必须流式
/// 截断报 `IndexTooLarge`，而非 `.text()` 全量缓冲撑爆内存。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_index_rejected() {
    let work = std::env::temp_dir().join(format!("fluxdown-market-big-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm");

    // 5MB 垃圾响应（> 4MB 上限）。
    let (port, _s) = spawn_index_server("x".repeat(5 * 1024 * 1024));
    let url = format!("http://127.0.0.1:{port}/index.json");
    let mc = MarketClient::new(pm, engine.db.clone(), vec![url], &ProxyConfig::default())
        .expect("market client");
    let err = mc
        .fetch_index()
        .await
        .expect_err("oversized index must be rejected");
    assert!(matches!(err, MarketError::IndexTooLarge), "got: {err:?}");

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

async fn response_server(
    responses: Vec<(&'static str, String)>,
) -> (u16, tokio::task::JoinHandle<Vec<String>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                    .await
                    .expect("HTTP request timeout")
                    .expect("accept");
            let mut request = Vec::new();
            loop {
                let mut chunk = [0u8; 4096];
                let read = stream.read(&mut chunk).await.expect("read request");
                assert_ne!(read, 0, "request ended before headers");
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|part| part == b"\r\n\r\n") {
                    break;
                }
                assert!(request.len() < 16 * 1024, "unexpected large test request");
            }
            requests.push(String::from_utf8(request).expect("request UTF-8"));
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write response");
        }
        requests
    });
    (port, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_proxy_routes_index_and_package_and_follows_latest_config() {
    let work = std::env::temp_dir().join(format!("fluxdown-market-proxy-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let mut engine = make_engine(&work).await;
    let source = "http://market-proxy.example.invalid/index.json";
    engine
        .db
        .set_config("market_index_sources", source)
        .await
        .expect("sources");
    let (port, proxy) = response_server(vec![
        (
            "200 OK",
            index_json(2, "https://market-package.example.invalid/p.fxplug"),
        ),
        ("502 Bad Gateway", String::new()),
    ])
    .await;
    engine
        .manager
        .set_proxy_config(ProxyConfig {
            mode: ProxyMode::Manual,
            host: "127.0.0.1".to_owned(),
            port,
            ..ProxyConfig::default()
        })
        .await
        .expect("set proxy");
    let client = engine
        .manager
        .market_client()
        .await
        .expect("market")
        .expect("plugins");
    let index = client.fetch_index().await.expect("proxied index");
    assert_eq!(index.sequence, 2);
    assert!(matches!(
        client.install_entry(&index.entries[0], false).await,
        Err(MarketError::AllMirrorsFailed)
    ));
    let observed = proxy.await.expect("proxy task");
    assert!(
        observed[0].starts_with("GET http://market-proxy.example.invalid/index.json HTTP/1.1\r\n"),
        "index was not requested through proxy: {:?}",
        observed[0]
    );
    assert!(
        observed[1].starts_with("CONNECT market-package.example.invalid:443 HTTP/1.1\r\n"),
        "package was not requested through proxy: {:?}",
        observed[1]
    );

    // 客户端每次从 manager 当前配置构造，不再钉住先前的代理端点。
    let (next_port, next_proxy) = response_server(vec![(
        "200 OK",
        index_json(3, "https://market-package.example.invalid/p.fxplug"),
    )])
    .await;
    engine
        .manager
        .set_proxy_config(ProxyConfig {
            mode: ProxyMode::Manual,
            host: "127.0.0.1".to_owned(),
            port: next_port,
            ..ProxyConfig::default()
        })
        .await
        .expect("update proxy");
    let next = engine
        .manager
        .market_client()
        .await
        .expect("market")
        .expect("plugins");
    assert_eq!(next.fetch_index().await.expect("new proxy").sequence, 3);
    let observed = next_proxy.await.expect("next proxy task");
    assert!(
        observed[0].starts_with("GET http://market-proxy.example.invalid/index.json HTTP/1.1\r\n")
    );
    drop(next);
    drop(client);
    drop(engine);
    tokio::fs::remove_dir_all(&work).await.expect("cleanup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_success_index_is_network_error_and_falls_back() {
    let work = std::env::temp_dir().join(format!("fluxdown-market-status-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let manager = engine.manager.plugin_manager().expect("plugins");
    // 即使非2xx body刚好是合法索引，也不得解析/接受。
    let (bad_port, bad) = response_server(vec![
        (
            "404 Not Found",
            index_json(9, "https://example.com/p.fxplug"),
        ),
        (
            "404 Not Found",
            index_json(9, "https://example.com/p.fxplug"),
        ),
    ])
    .await;
    let bad_url = format!("http://127.0.0.1:{bad_port}/index.json");
    let client = MarketClient::new(
        manager.clone(),
        engine.db.clone(),
        vec![bad_url.clone()],
        &ProxyConfig::default(),
    )
    .expect("client");
    let error = client.fetch_index().await.expect_err("non-success status");
    assert!(matches!(&error, MarketError::Network(detail) if detail.contains("HTTP 404")));
    let (good_port, good) = response_server(vec![(
        "200 OK",
        index_json(2, "https://example.com/p.fxplug"),
    )])
    .await;
    let fallback = MarketClient::new(
        manager,
        engine.db.clone(),
        vec![bad_url, format!("http://127.0.0.1:{good_port}/index.json")],
        &ProxyConfig::default(),
    )
    .expect("client");
    assert_eq!(
        fallback
            .fetch_index()
            .await
            .expect("fallback index")
            .sequence,
        2
    );
    bad.await.expect("bad source task");
    good.await.expect("good source task");
    drop(fallback);
    drop(client);
    drop(engine);
    tokio::fs::remove_dir_all(&work).await.expect("cleanup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn none_proxy_ignores_environment() {
    const CHILD: &str = "FLUXDOWN_MARKET_NONE_PROXY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Rust 2024 set_var需要unsafe；用隔离子进程验证，绝不污染并行测试环境。
        let trap = TcpListener::bind("127.0.0.1:0").expect("proxy trap");
        trap.set_nonblocking(true).expect("nonblocking");
        let proxy = format!(
            "http://127.0.0.1:{}",
            trap.local_addr().expect("addr").port()
        );
        let mut command =
            std::process::Command::new(std::env::current_exe().expect("test executable"));
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let child = command
            .args(["--exact", "none_proxy_ignores_environment", "--nocapture"])
            .env(CHILD, "1")
            .env("HTTP_PROXY", &proxy)
            .env("http_proxy", &proxy)
            .env("HTTPS_PROXY", &proxy)
            .env("https_proxy", &proxy)
            .env("ALL_PROXY", &proxy)
            .env("all_proxy", &proxy)
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .output()
            .expect("run isolated proxy test");
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        let error = trap
            .accept()
            .expect_err("None must not connect to env proxy");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        return;
    }
    let work = std::env::temp_dir().join(format!("fluxdown-market-none-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let (port, origin) = response_server(vec![(
        "200 OK",
        index_json(4, "https://example.com/p.fxplug"),
    )])
    .await;
    let client = MarketClient::new(
        engine.manager.plugin_manager().expect("plugins"),
        engine.db.clone(),
        vec![format!("http://127.0.0.1:{port}/index.json")],
        &ProxyConfig::default(),
    )
    .expect("client");
    assert_eq!(
        client.fetch_index().await.expect("direct index").sequence,
        4
    );
    let request = origin.await.expect("origin task");
    assert!(request[0].starts_with("GET /index.json HTTP/1.1\r\n"));
    drop(client);
    drop(engine);
    tokio::fs::remove_dir_all(&work).await.expect("cleanup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_proxy_is_not_replaced_with_direct_client() {
    let work = std::env::temp_dir().join(format!("fluxdown-market-invalid-proxy-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let error = MarketClient::new(
        engine.manager.plugin_manager().expect("plugins"),
        engine.db.clone(),
        vec!["https://example.invalid/index.json".to_owned()],
        &ProxyConfig {
            mode: ProxyMode::Manual,
            host: "[".to_owned(),
            port: 7890,
            ..ProxyConfig::default()
        },
    )
    .err()
    .expect("invalid proxy must fail construction");
    assert!(matches!(error, MarketError::Network(_)), "{error:?}");
    drop(engine);
    tokio::fs::remove_dir_all(&work).await.expect("cleanup");
}
