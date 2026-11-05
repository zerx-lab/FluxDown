//! 插件变体选择被取消：任务暂停（不是失败），解析期间为「准备中」，恢复后用库里
//! 持久化的请求上下文（cookies / referrer / User-Agent / 自定义头）重新解析并再次询问。
//!
//! 仅 `plugins` feature 下编译运行。

#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::Write as _;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::download_manager::NewTaskSpec;
use fluxdown_engine::model::{BtFileEntry, HlsQualityOption, ResolveVariantOption};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::selection::{BtFileChoice, HostSelection, SelectionOutcome};
use fluxdown_engine::{Engine, EngineConfig, NoopSink};

const FILE_BODY: &[u8] = b"fluxdown variant cancel payload\n";
const PAGE_URL: &str = "https://www.origin.test/watch?v=1";
const COOKIES: &str = "sid=abc; pref=1";
const REFERRER: &str = "https://www.origin.test/";
const TASK_UA: &str = "TaskUA/1.0";

/// 记录每个请求的原始请求头文本。
fn spawn_server(seen: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 8192];
            let mut header = String::new();
            loop {
                let n = match std::io::Read::read(&mut stream, &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                header.push_str(&String::from_utf8_lossy(&buf[..n]));
                if header.contains("\r\n\r\n") {
                    break;
                }
            }
            seen.lock().unwrap().push(header.clone());
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                FILE_BODY.len()
            );
            // 测试服务器：客户端提前断开属正常，写失败不影响断言。
            if stream.write_all(resp.as_bytes()).is_ok() && !header.starts_with("HEAD ") {
                stream.write_all(FILE_BODY).unwrap_or_default();
            }
        }
    });
    port
}

/// 插件把收到的 resolve 上下文编码进变体标签，选择器据此断言两次解析拿到的上下文。
async fn write_plugin(dir: &std::path::Path, port: u16) {
    tokio::fs::create_dir_all(dir).await.expect("mkdir plugin");
    let manifest = r#"{
      "identity": "variants@site",
      "name": "Variants",
      "version": "1.0.0",
      "description": "resolves origin.test into variants",
      "resolvers": [{ "match": { "urls": ["*://*.origin.test/*"] }, "entry": "resolve.js", "timeoutMs": 5000 }]
    }"#;
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    let resolve_js = format!(
        r#"globalThis.resolve = async (ctx) => {{
  const seen = JSON.stringify({{
    cookies: ctx.cookies,
    referrer: ctx.referrer,
    userAgent: ctx.userAgent,
    custom: ctx.extraHeaders["X-Custom"] || "",
  }});
  return {{
    variants: [
      {{ label: "hi " + seen, url: "http://127.0.0.1:{port}/hi.bin" }},
      {{ label: "lo " + seen, url: "http://127.0.0.1:{port}/lo.bin" }},
    ],
  }};
}};"#
    );
    tokio::fs::write(dir.join("resolve.js"), resolve_js)
        .await
        .expect("write resolve.js");
}

/// 按顺序给出预设答复（-1 = 用户关闭弹窗），并记录每次收到的选项标签。
struct ScriptedVariants {
    answers: Mutex<Vec<i32>>,
    labels: Mutex<Vec<Vec<String>>>,
}

#[async_trait::async_trait]
impl HostSelection for ScriptedVariants {
    async fn select_hls_quality(
        &self,
        _: &str,
        _: &[HlsQualityOption],
        _: Duration,
    ) -> SelectionOutcome<i32> {
        SelectionOutcome::NoSelectorConfigured(0)
    }
    async fn select_bt_files(
        &self,
        _: &str,
        _: &[BtFileEntry],
        _: &str,
        _: Option<Duration>,
    ) -> SelectionOutcome<BtFileChoice> {
        SelectionOutcome::NoSelectorConfigured(BtFileChoice::default())
    }
    async fn select_resolve_variant(
        &self,
        _: &str,
        options: &[ResolveVariantOption],
        _: i32,
        _: Duration,
    ) -> SelectionOutcome<i32> {
        self.labels
            .lock()
            .unwrap()
            .push(options.iter().map(|o| o.label.clone()).collect());
        SelectionOutcome::UserChose(self.answers.lock().unwrap().remove(0))
    }
    fn provide_hls_selection(&self, _: &str, _: i32) {}
    fn provide_bt_selection(&self, _: &str, _: Vec<i32>) {}
    fn provide_variant_selection(&self, _: &str, _: i32) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_variant_pauses_and_resume_reresolves_with_persisted_context() {
    let work = std::env::temp_dir().join(format!(
        "fluxdown-variant-cancel-{}",
        uuid::Uuid::new_v4().simple()
    ));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_server(seen.clone());
    let selector = Arc::new(ScriptedVariants {
        answers: Mutex::new(vec![-1, 1]),
        labels: Mutex::new(Vec::new()),
    });
    let mut engine = Engine::new(
        EngineConfig {
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
        },
        Arc::new(NoopSink),
        selector.clone(),
    )
    .await
    .expect("engine");
    let pm = engine.manager.plugin_manager().expect("plugin manager");
    let src = work.join("src-plugin");
    write_plugin(&src, port).await;
    pm.install_from_dir(&src).await.expect("install");
    let mut resolve_rx = engine.manager.take_resolve_rx().expect("resolve_rx");
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");

    let task = engine
        .manager
        .create_task(NewTaskSpec {
            url: PAGE_URL.to_owned(),
            save_dir: work.to_string_lossy().into_owned(),
            file_name: "video.bin".to_owned(),
            segments: 1,
            cookies: COOKIES.to_owned(),
            referrer: REFERRER.to_owned(),
            user_agent: TASK_UA.to_owned(),
            extra_headers: HashMap::from([("X-Custom".to_owned(), "k1".to_owned())]),
            ..Default::default()
        })
        .await
        .expect("create task");

    // 解析在途：库里是「准备中」而不是「等待中」。
    let status = |engine: &Engine, task: String| {
        let db = engine.db.clone();
        async move { db.load_task_by_id(&task).await.unwrap().expect("task row") }
    };
    assert_eq!(status(&engine, task.clone()).await.status, 5);

    // 第一次：用户关闭规格选择框 → 暂停，不是失败。
    let out = tokio::time::timeout(Duration::from_secs(15), resolve_rx.recv())
        .await
        .expect("first resolve timeout")
        .expect("resolve channel");
    engine.manager.on_resolve_ready(out).await;
    let paused = status(&engine, task.clone()).await;
    assert_eq!(paused.status, 2, "取消规格选择应暂停任务");
    assert!(paused.error_message.is_empty());

    // 恢复：从库里的请求上下文重新解析（再次询问），选第二档后下载完成。
    engine.manager.resume_task(&task).await;
    assert_eq!(status(&engine, task.clone()).await.status, 5);
    loop {
        tokio::select! {
            Some(out) = resolve_rx.recv() => engine.manager.on_resolve_ready(out).await,
            Some(done) = done_rx.recv() => {
                engine.manager.on_task_done(&done).await;
                if done.task_id == task {
                    break;
                }
            }
            () = tokio::time::sleep(Duration::from_secs(15)) => panic!("timeout after resume"),
        }
    }
    assert_eq!(status(&engine, task.clone()).await.status, 3);
    assert_eq!(
        tokio::fs::read(work.join("video.bin")).await.unwrap(),
        FILE_BODY
    );

    // 两次解析拿到的上下文逐字节一致，且就是创建时的 cookies / referrer / UA / 自定义头。
    let labels = selector.labels.lock().unwrap().clone();
    assert_eq!(labels.len(), 2, "恢复时必须重新询问规格");
    assert_eq!(labels[0], labels[1]);
    let ctx: serde_json::Value =
        serde_json::from_str(labels[1][1].strip_prefix("lo ").expect("label prefix"))
            .expect("ctx json");
    assert_eq!(ctx["cookies"], COOKIES);
    assert_eq!(ctx["referrer"], REFERRER);
    assert_eq!(ctx["userAgent"], TASK_UA);
    assert_eq!(ctx["custom"], "k1");

    // 实际下载请求同样带着持久化的身份。
    let requests = seen.lock().unwrap().clone();
    let download = requests
        .iter()
        .find(|h| h.starts_with("GET /lo.bin"))
        .expect("download request for chosen variant");
    let lower = download.to_ascii_lowercase();
    assert!(lower.contains(&format!("cookie: {}", COOKIES.to_ascii_lowercase())));
    assert!(lower.contains(&format!("user-agent: {}", TASK_UA.to_ascii_lowercase())));
    assert!(lower.contains("x-custom: k1"));
    assert!(!requests.iter().any(|h| h.starts_with("GET /hi.bin")));

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}
