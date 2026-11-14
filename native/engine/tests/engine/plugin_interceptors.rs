//! 插件拦截平面端到端（真实 QuickJS）：
//! - A：`prepare` 改写的请求身份与并发在探测自适应、失败后重试中持续生效，且不落库；
//!   `failed` 阶段请求重试。
//! - B：HLS `segments` 按标签过滤分片（被丢弃分片从不请求）；`finalize` 的 `fail` 让任务失败。
//! - 归档插件：分类目录下叠加子目录、失败回退并记任务活动，完成期按最终内容重评，
//!   无结果时保留已保存的目录选择。
//!
//! 仅 `plugins` feature 下编译运行。

#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::download_manager::NewTaskSpec;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::task_activity::TaskActivityQuery;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

fn uniq() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), n)
}

async fn make_engine(work: &Path) -> Engine {
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

/// 写 dev 插件并安装、授权、启用。
async fn install_plugin(engine: &Engine, dir: &Path, identity: &str, stages: &str, script: &str) {
    tokio::fs::create_dir_all(dir).await.expect("mkdir plugin");
    let manifest = format!(
        r#"{{
      "identity": "{identity}",
      "name": "Intercept Test",
      "version": "1.0.0",
      "permissions": ["intercept"],
      "interceptors": {{
        "entry": "intercept.js",
        "stages": {stages},
        "match": {{ "urls": ["http://127.0.0.1*"] }}
      }}
    }}"#
    );
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    tokio::fs::write(dir.join("intercept.js"), script)
        .await
        .expect("write intercept.js");
    let pm = engine.manager.plugin_manager().expect("pm installed");
    let id = pm.install_dev(dir).await.expect("install dev plugin");
    pm.grant_consent(&id, &["intercept".to_string()], &[])
        .await
        .expect("grant consent");
    pm.set_enabled(&id, true).await.expect("enable");
}

const ARCHIVE_ANIME: &str =
    r#"globalThis.archive = async ({fileName}) => ({folderName:"Anime Title"});"#;

/// 写 dev 归档插件（无 URL 拦截器）并安装、授权、启用。
async fn install_archive_plugin(engine: &Engine, dir: &Path, script: &str) {
    tokio::fs::create_dir_all(dir).await.expect("mkdir plugin");
    tokio::fs::write(
        dir.join("manifest.json"),
        r#"{
            "identity":"tester@archive",
            "name":"Archive test",
            "version":"1.0.0",
            "permissions":["archive"],
            "main":{"entry":"main.js"},
            "contributes":{"archive":"archive.js"}
        }"#,
    )
    .await
    .expect("write manifest");
    tokio::fs::write(dir.join("main.js"), "")
        .await
        .expect("write main");
    tokio::fs::write(dir.join("archive.js"), script)
        .await
        .expect("write archive entry");
    let pm = engine.manager.plugin_manager().expect("plugin manager");
    let id = pm.install_dev(dir).await.expect("install archive plugin");
    pm.grant_consent(&id, &["archive".to_string()], &[])
        .await
        .expect("grant archive permission");
    pm.set_enabled(&id, true)
        .await
        .expect("enable archive plugin");
}

async fn plugin_notes(engine: &Engine, task_id: &str) -> Vec<String> {
    engine
        .db
        .query_task_activity(&TaskActivityQuery {
            task_id: task_id.to_string(),
            limit: 200,
            ..Default::default()
        })
        .await
        .expect("activity")
        .entries
        .into_iter()
        .filter(|e| e.kind == "plugin")
        .map(|e| e.message)
        .collect()
}

async fn cleanup(work: &Path) {
    if let Err(error) = tokio::fs::remove_dir_all(work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

// ---------------------------------------------------------------------------
// 测试服务器
// ---------------------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = [0u8; 8192];
    let mut raw = String::new();
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => n,
        };
        raw.push_str(&String::from_utf8_lossy(&buf[..n]));
        if raw.contains("\r\n\r\n") {
            break;
        }
    }
    let mut lines = raw.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let path = first.next()?.to_string();
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Some(Request {
        method,
        path,
        headers,
    })
}

fn write_ignoring_disconnect(stream: &mut TcpStream, bytes: &[u8]) -> bool {
    match stream.write_all(bytes) {
        Ok(()) => true,
        Err(error) => {
            assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                ),
                "test server write failed: {error}"
            );
            false
        }
    }
}

fn parse_range(value: &str, total: usize) -> Option<(usize, usize)> {
    let spec = value.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    let start: usize = a.parse().ok()?;
    let end = if b.is_empty() {
        total - 1
    } else {
        b.parse::<usize>().ok()?.min(total - 1)
    };
    (start <= end).then_some((start, end))
}

#[derive(Default)]
struct RangeServerState {
    requests: Mutex<Vec<Request>>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

/// 支持 Range 的文件服务器；`armed` 时一律 403。记录请求头与并发连接峰值。
fn spawn_range_server(body: Arc<Vec<u8>>, armed: Arc<AtomicBool>) -> (u16, Arc<RangeServerState>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let state = Arc::new(RangeServerState::default());
    let shared = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let state = shared.clone();
            let body = body.clone();
            let armed = armed.clone();
            std::thread::spawn(move || {
                let now = state.active.fetch_add(1, Ordering::SeqCst) + 1;
                state.peak.fetch_max(now, Ordering::SeqCst);
                serve_range(&mut stream, &state, &body, &armed);
                state.active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (port, state)
}

fn serve_range(stream: &mut TcpStream, state: &RangeServerState, body: &[u8], armed: &AtomicBool) {
    let Some(req) = read_request(stream) else {
        return;
    };
    let range = req
        .headers
        .get("range")
        .and_then(|r| parse_range(r, body.len()));
    let is_head = req.method == "HEAD";
    state.requests.lock().expect("requests").push(req);
    if armed.load(Ordering::SeqCst) {
        write_ignoring_disconnect(
            stream,
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    }
    let (status, start, end) = match range {
        Some((s, e)) => ("206 Partial Content", s, e),
        None => ("200 OK", 0, body.len() - 1),
    };
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n",
        end - start + 1
    );
    if range.is_some() {
        head.push_str(&format!(
            "Content-Range: bytes {start}-{end}/{}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    if !write_ignoring_disconnect(stream, head.as_bytes()) || is_head {
        return;
    }
    // 分块慢写，让并发连接在时间上重叠。
    for chunk in body[start..=end].chunks(64 * 1024) {
        if !write_ignoring_disconnect(stream, chunk) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// 按路径返回固定内容（路径不含查询串；404 兜底），记录请求路径。
fn spawn_static_server(routes: HashMap<String, Vec<u8>>) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let Some(req) = read_request(&mut stream) else {
                continue;
            };
            let path = req.path.split('?').next().unwrap_or("").to_string();
            log.lock().expect("seen").push(path.clone());
            match routes.get(&path) {
                Some(body) => {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if write_ignoring_disconnect(&mut stream, head.as_bytes())
                        && req.method != "HEAD"
                    {
                        write_ignoring_disconnect(&mut stream, body);
                    }
                }
                None => {
                    write_ignoring_disconnect(
                        &mut stream,
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        }
    });
    (port, seen)
}

// ---------------------------------------------------------------------------
// A：请求身份、并发，以及改写在重试中持续生效
// ---------------------------------------------------------------------------

const SCRIPT_A: &str = r#"
globalThis.intercept = async (ctx) => {
  if (ctx.stage === "prepare") {
    return {
      userAgent: "Interceptor-UA/1",
      setHeaders: { "X-Intercept": "on" },
      segments: 4,
      maxConnections: 2,
    };
  }
  if (ctx.stage === "failed") {
    return { retry: { delayMs: 5 } };
  }
  return null;
};
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepare_patch_survives_adaptation_and_retry_without_persisting() {
    let work = std::env::temp_dir().join(format!("fluxdown-intercept-a-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let body: Arc<Vec<u8>> = Arc::new((0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect());
    let armed = Arc::new(AtomicBool::new(true));
    let (port, server) = spawn_range_server(body.clone(), armed.clone());

    let mut engine = make_engine(&work).await;
    install_plugin(
        &engine,
        &work.join("plugin_src"),
        "tester@intercept",
        r#"["prepare", "failed"]"#,
        SCRIPT_A,
    )
    .await;
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");
    let mut retry_rx = engine
        .manager
        .take_plugin_retry_rx()
        .expect("plugin retry rx");

    let id = engine
        .manager
        .create_task(NewTaskSpec {
            url: format!("http://127.0.0.1:{port}/file.bin"),
            save_dir: work.to_string_lossy().into_owned(),
            file_name: "file.bin".to_string(),
            segments: 1,
            ..Default::default()
        })
        .await
        .expect("create task");

    let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
        .await
        .expect("first run finishes")
        .expect("done channel open");
    engine.manager.on_task_done(&done).await;
    let task = engine.db.load_task_by_id(&id).await.unwrap().unwrap();
    assert_eq!(task.status, 4, "first run must fail under 403");

    let (retry_id, delay) = tokio::time::timeout(Duration::from_secs(10), retry_rx.recv())
        .await
        .expect("failed stage requests retry")
        .expect("retry channel open");
    assert_eq!((retry_id.as_str(), delay), (id.as_str(), 5));

    armed.store(false, Ordering::SeqCst);
    engine.manager.resume_task(&id).await;
    let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
        .await
        .expect("retry run finishes")
        .expect("done channel open");
    engine.manager.on_task_done(&done).await;
    let task = engine.db.load_task_by_id(&id).await.unwrap().unwrap();
    assert_eq!(task.status, 3, "retry failed: {}", task.error_message);
    let bytes = tokio::fs::read(work.join(&task.file_name))
        .await
        .expect("read output");
    assert!(
        bytes == *body,
        "downloaded content differs from server body"
    );

    {
        let requests = server.requests.lock().expect("requests");
        assert!(requests.len() >= 3, "expected probe + segment requests");
        for req in requests.iter() {
            assert_eq!(
                req.headers.get("user-agent").map(String::as_str),
                Some("Interceptor-UA/1"),
                "{} {} lost the pinned UA",
                req.method,
                req.path
            );
            assert_eq!(
                req.headers.get("x-intercept").map(String::as_str),
                Some("on"),
                "{} {} lost the plugin header",
                req.method,
                req.path
            );
        }
        // 插件的 segments=4 覆盖用户的 1：出现从非 0 偏移开始的分段请求。
        let mid_ranges = requests
            .iter()
            .filter_map(|r| r.headers.get("range"))
            .filter_map(|r| parse_range(r, body.len()))
            .filter(|(start, _)| *start > 0)
            .count();
        assert!(
            mid_ranges >= 1,
            "plugin segments override did not split the download"
        );
    }
    let peak = server.peak.load(Ordering::SeqCst);
    assert!(peak <= 2, "maxConnections=2 exceeded: peak {peak}");

    let (_, _, headers_json) = engine
        .db
        .load_task_request_context(&id)
        .await
        .unwrap()
        .unwrap();
    assert!(!headers_json.contains("X-Intercept"), "{headers_json}");
    assert!(!headers_json.contains("Interceptor-UA"), "{headers_json}");

    let notes = plugin_notes(&engine, &id).await;
    assert!(!notes.is_empty(), "no plugin activity recorded");
    assert!(
        notes.iter().all(|n| !n.contains("Interceptor-UA/1")),
        "activity must not leak header values: {notes:?}"
    );

    cleanup(&work).await;
}

// ---------------------------------------------------------------------------
// B：HLS 去广告与 finalize
// ---------------------------------------------------------------------------

const AD_MARKER: &str = "#EXT-X-CUE-OUT";

fn script_b() -> String {
    format!(
        r#"
globalThis.intercept = async (ctx) => {{
  if (ctx.stage === "segments") {{
    const drop = ctx.segments
      .filter((s) => s.tags.some((t) => t.startsWith("{AD_MARKER}")))
      .map((s) => s.id);
    return {{ drop }};
  }}
  if (ctx.stage === "finalize") {{
    return ctx.task.url.includes("/blocked/") ? {{ fail: "blocked" }} : null;
  }}
  return null;
}};
"#
    )
}

fn playlist_b() -> String {
    format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:10\n\
         #EXTINF:2,\ns0.ts\n{AD_MARKER}:2\n#EXTINF:2,\nad.ts\n#EXT-X-CUE-IN\n\
         #EXTINF:2,\ns1.ts\n#EXT-X-ENDLIST\n"
    )
}

async fn run_to_done(
    engine: &mut Engine,
    done_rx: &mut tokio::sync::mpsc::Receiver<fluxdown_engine::download_manager::TaskDone>,
    url: String,
    save_dir: &Path,
) -> fluxdown_engine::model::TaskInfo {
    let id = engine
        .manager
        .create_task(NewTaskSpec {
            url,
            save_dir: save_dir.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .await
        .expect("create task");
    loop {
        let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
            .await
            .expect("task finishes")
            .expect("done channel open");
        engine.manager.on_task_done(&done).await;
        if done.task_id == id {
            break;
        }
    }
    engine.db.load_task_by_id(&id).await.unwrap().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hls_segment_filter_and_finalize_fail() {
    let work = std::env::temp_dir().join(format!("fluxdown-intercept-b-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let mut routes = HashMap::new();
    for prefix in ["", "/blocked"] {
        routes.insert(format!("{prefix}/v.m3u8"), playlist_b().into_bytes());
        routes.insert(format!("{prefix}/s0.ts"), b"AAAA".to_vec());
        routes.insert(format!("{prefix}/ad.ts"), b"BBBB".to_vec());
        routes.insert(format!("{prefix}/s1.ts"), b"CCCC".to_vec());
    }
    let (port, seen) = spawn_static_server(routes);

    let mut engine = make_engine(&work).await;
    install_plugin(
        &engine,
        &work.join("plugin_src"),
        "tester@adfilter",
        r#"["segments", "finalize"]"#,
        &script_b(),
    )
    .await;
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");

    let ok_dir = work.join("ok");
    tokio::fs::create_dir_all(&ok_dir).await.expect("mkdir ok");
    let task = run_to_done(
        &mut engine,
        &mut done_rx,
        format!("http://127.0.0.1:{port}/v.m3u8"),
        &ok_dir,
    )
    .await;
    assert_eq!(task.status, 3, "hls task failed: {}", task.error_message);
    let bytes = tokio::fs::read(ok_dir.join(&task.file_name))
        .await
        .expect("read output");
    assert_eq!(bytes, b"AAAACCCC");
    assert!(
        seen.lock()
            .expect("seen")
            .iter()
            .all(|p| !p.ends_with("ad.ts")),
        "dropped segment must never be requested"
    );
    let notes = plugin_notes(&engine, &task.task_id).await;
    assert!(
        notes.iter().any(|n| n.contains("过滤了 1 个视频分片")),
        "{notes:?}"
    );

    let blocked_dir = work.join("blocked");
    tokio::fs::create_dir_all(&blocked_dir)
        .await
        .expect("mkdir blocked");
    let task = run_to_done(
        &mut engine,
        &mut done_rx,
        format!("http://127.0.0.1:{port}/blocked/v.m3u8"),
        &blocked_dir,
    )
    .await;
    assert_eq!(task.status, 4);
    assert!(
        task.error_message.contains("blocked"),
        "{}",
        task.error_message
    );

    cleanup(&work).await;
}

// ---------------------------------------------------------------------------
// C：每请求一次性令牌（ephemeral 跳过探测 + 被拒即刷新）
// ---------------------------------------------------------------------------

#[derive(Default)]
struct TokenServerState {
    /// (方法, 令牌, 是否被接受)
    log: Mutex<Vec<(String, Option<String>, bool)>>,
}

/// 支持 Range 的文件服务器；每个 `X-Token` 值只接受一次（缺失或重复 → 403）。
fn spawn_token_server(body: Arc<Vec<u8>>) -> (u16, Arc<TokenServerState>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let state = Arc::new(TokenServerState::default());
    let used = Arc::new(Mutex::new(std::collections::HashSet::<String>::new()));
    let shared = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let state = shared.clone();
            let used = used.clone();
            let body = body.clone();
            std::thread::spawn(move || {
                let Some(req) = read_request(&mut stream) else {
                    return;
                };
                let token = req.headers.get("x-token").cloned();
                let fresh = match &token {
                    Some(t) => used.lock().expect("used").insert(t.clone()),
                    None => false,
                };
                state
                    .log
                    .lock()
                    .expect("log")
                    .push((req.method.clone(), token, fresh));
                if !fresh {
                    write_ignoring_disconnect(
                        &mut stream,
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    return;
                }
                let range = req
                    .headers
                    .get("range")
                    .and_then(|r| parse_range(r, body.len()));
                let (status, start, end) = match range {
                    Some((s, e)) => ("206 Partial Content", s, e),
                    None => ("200 OK", 0, body.len() - 1),
                };
                let mut head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n",
                    end - start + 1
                );
                if range.is_some() {
                    head.push_str(&format!(
                        "Content-Range: bytes {start}-{end}/{}\r\n",
                        body.len()
                    ));
                }
                head.push_str("\r\n");
                if !write_ignoring_disconnect(&mut stream, head.as_bytes()) || req.method == "HEAD"
                {
                    return;
                }
                for chunk in body[start..=end].chunks(64 * 1024) {
                    if !write_ignoring_disconnect(&mut stream, chunk) {
                        return;
                    }
                }
            });
        }
    });
    (port, state)
}

fn script_c(total: usize) -> String {
    format!(
        r#"
// 每次调用都在全新 JS 上下文中执行，令牌用随机数 + 阶段 + 计数生成，保证互不相同。
const token = (ctx) =>
  `tok-${{ctx.stage}}-${{ctx.refreshCount ?? 0}}-${{Math.random().toString(36).slice(2)}}`;
globalThis.intercept = async (ctx) => {{
  if (ctx.stage === "prepare") {{
    return {{
      ephemeral: true, totalBytes: {total}, rangeSupported: true,
      segments: 2, maxConnections: 2,
      setHeaders: {{ "X-Token": token(ctx) }},
    }};
  }}
  if (ctx.stage === "refresh") {{
    return {{ setHeaders: {{ "X-Token": token(ctx) }} }};
  }}
  return null;
}};
"#
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ephemeral_prepare_and_refresh_issue_one_token_per_request() {
    let work = std::env::temp_dir().join(format!("fluxdown-intercept-c-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let body: Arc<Vec<u8>> = Arc::new((0..4 * 1024 * 1024).map(|i| (i % 249) as u8).collect());
    let (port, server) = spawn_token_server(body.clone());

    let mut engine = make_engine(&work).await;
    install_plugin(
        &engine,
        &work.join("plugin_src"),
        "tester@onetime",
        r#"["prepare", "refresh"]"#,
        &script_c(body.len()),
    )
    .await;
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");
    let task = run_to_done(
        &mut engine,
        &mut done_rx,
        format!("http://127.0.0.1:{port}/file.bin"),
        &work,
    )
    .await;
    if task.status != 3 {
        let notes = plugin_notes(&engine, &task.task_id).await;
        let log = format!("{:?}", server.log.lock().expect("log"));
        panic!(
            "task failed: {} | server log: {log} | notes: {notes:?}",
            task.error_message
        );
    }
    let bytes = tokio::fs::read(work.join(&task.file_name))
        .await
        .expect("read output");
    assert!(
        bytes == *body,
        "downloaded content differs from server body"
    );

    {
        let log = server.log.lock().expect("log");
        assert!(
            log.iter().all(|(method, _, _)| method != "HEAD"),
            "ephemeral task must not be probed: {log:?}"
        );
        let mut accepted = std::collections::HashSet::new();
        for (_, token, ok) in log.iter() {
            if *ok {
                let token = token.clone().expect("accepted request has token");
                assert!(accepted.insert(token), "token accepted twice");
            }
        }
        assert!(!accepted.is_empty(), "no request was accepted");
    }

    let notes = plugin_notes(&engine, &task.task_id).await;
    assert!(notes.iter().any(|n| n.contains("刷新了凭据")), "{notes:?}");
    let (_, _, headers_json) = engine
        .db
        .load_task_request_context(&task.task_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!headers_json.contains("X-Token"), "{headers_json}");

    cleanup(&work).await;
}

// ---------------------------------------------------------------------------
// D：关闭自动重试时 failed 阶段仍能重试
// ---------------------------------------------------------------------------

const SCRIPT_D: &str = r#"
globalThis.intercept = async (ctx) => {
  if (ctx.stage === "failed") {
    return { retry: { delayMs: 5 } };
  }
  return null;
};
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_stage_retry_works_with_auto_retry_disabled() {
    let work = std::env::temp_dir().join(format!("fluxdown-intercept-d-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let body: Arc<Vec<u8>> = Arc::new((0..1024 * 1024).map(|i| (i % 247) as u8).collect());
    let armed = Arc::new(AtomicBool::new(true));
    let (port, _server) = spawn_range_server(body.clone(), armed.clone());

    let mut engine = make_engine(&work).await;
    engine.manager.set_max_auto_retries(0);
    install_plugin(
        &engine,
        &work.join("plugin_src"),
        "tester@failedretry",
        r#"["failed"]"#,
        SCRIPT_D,
    )
    .await;
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");
    let mut plugin_retry_rx = engine
        .manager
        .take_plugin_retry_rx()
        .expect("plugin retry rx");
    let mut retry_rx = engine.manager.take_retry_rx().expect("retry rx");

    let id = engine
        .manager
        .create_task(NewTaskSpec {
            url: format!("http://127.0.0.1:{port}/file.bin"),
            save_dir: work.to_string_lossy().into_owned(),
            file_name: "file.bin".to_string(),
            ..Default::default()
        })
        .await
        .expect("create task");

    let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
        .await
        .expect("first run finishes")
        .expect("done channel open");
    engine.manager.on_task_done(&done).await;
    let task = engine.db.load_task_by_id(&id).await.unwrap().unwrap();
    assert_eq!(task.status, 4, "first run must fail under 403");

    let (retry_id, delay) = tokio::time::timeout(Duration::from_secs(10), plugin_retry_rx.recv())
        .await
        .expect("failed stage requests retry")
        .expect("retry channel open");
    assert_eq!((retry_id.as_str(), delay), (id.as_str(), 5));

    armed.store(false, Ordering::SeqCst);
    engine.manager.plugin_request_retry(&retry_id, delay).await;
    let scheduled = tokio::time::timeout(Duration::from_secs(10), retry_rx.recv())
        .await
        .expect("retry must be scheduled despite max_auto_retries == 0")
        .expect("retry channel open");
    assert_eq!(scheduled, id);
    engine.manager.resume_task_auto(&scheduled).await;

    let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
        .await
        .expect("retry run finishes")
        .expect("done channel open");
    engine.manager.on_task_done(&done).await;
    let task = engine.db.load_task_by_id(&id).await.unwrap().unwrap();
    assert_eq!(task.status, 3, "retry failed: {}", task.error_message);

    cleanup(&work).await;
}

/// 拦截器入口有语法错误：安装期即以编译失败拒绝，而不是等到下载时才出错。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broken_interceptor_script_is_rejected_at_install() {
    let work = std::env::temp_dir().join(format!("fluxdown-intercept-compile-{}", uniq()));
    let dir = work.join("plugin_src");
    tokio::fs::create_dir_all(&dir).await.expect("mkdir plugin");
    let manifest = r#"{
      "identity": "tester@broken",
      "name": "Broken Interceptor",
      "version": "1.0.0",
      "permissions": ["intercept"],
      "interceptors": { "entry": "intercept.js", "stages": ["prepare"], "match": { "urls": ["*"] } }
    }"#;
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    tokio::fs::write(dir.join("intercept.js"), "globalThis.intercept = (;")
        .await
        .expect("write intercept.js");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm installed");
    let err = pm.install_dev(&dir).await.expect_err("broken script");
    assert!(
        matches!(err, fluxdown_engine::plugin::PluginError::CompileFailed(_)),
        "got {err:?}"
    );
    cleanup(&work).await;
}

// ---------------------------------------------------------------------------
// C：`maxRangeBytes` —— 只接受小区间的直链（网盘 CDN）
// ---------------------------------------------------------------------------

/// 服务器接受的最大 Range 跨度，与 [`SCRIPT_RANGE_CAP`] 里的 `maxRangeBytes` 一致。
const RANGE_CAP: usize = 262_144;

const SCRIPT_RANGE_CAP: &str = r#"
globalThis.intercept = async (ctx) => {
  if (ctx.stage === "prepare") {
    return { segments: 4, maxRangeBytes: 262144 };
  }
  return null;
};
"#;

#[derive(Default)]
struct CapServerState {
    /// 无 Range、开放式 Range 或跨度超过 [`RANGE_CAP`] 的数据请求数。
    violations: AtomicUsize,
    max_span: AtomicUsize,
    data_gets: AtomicUsize,
}

/// 对 HEAD / 无 Range / 开放式 Range / 超跨度 Range 一律 403 的服务器；`armed` 时全部 403。
fn spawn_capped_server(body: Arc<Vec<u8>>, armed: Arc<AtomicBool>) -> (u16, Arc<CapServerState>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let state = Arc::new(CapServerState::default());
    let shared = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let state = shared.clone();
            let body = body.clone();
            let armed = armed.clone();
            std::thread::spawn(move || serve_capped(&mut stream, &state, &body, &armed));
        }
    });
    (port, state)
}

fn serve_capped(stream: &mut TcpStream, state: &CapServerState, body: &[u8], armed: &AtomicBool) {
    const FORBIDDEN: &[u8] =
        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let Some(req) = read_request(stream) else {
        return;
    };
    if req.method == "HEAD" || armed.load(Ordering::SeqCst) {
        write_ignoring_disconnect(stream, FORBIDDEN);
        return;
    }
    let bounds = req
        .headers
        .get("range")
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
    let Some((start, end)) = bounds else {
        state.violations.fetch_add(1, Ordering::SeqCst);
        write_ignoring_disconnect(stream, FORBIDDEN);
        return;
    };
    let span = end.saturating_sub(start) + 1;
    if span > RANGE_CAP || start >= body.len() {
        state.violations.fetch_add(1, Ordering::SeqCst);
        write_ignoring_disconnect(stream, FORBIDDEN);
        return;
    }
    let end = end.min(body.len() - 1);
    if span > 1 {
        state.data_gets.fetch_add(1, Ordering::SeqCst);
    }
    state.max_span.fetch_max(end - start + 1, Ordering::SeqCst);
    let head = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
        end - start + 1,
        body.len()
    );
    if write_ignoring_disconnect(stream, head.as_bytes()) {
        write_ignoring_disconnect(stream, &body[start..=end]);
    }
}

/// `prepare` 返回的 `maxRangeBytes` 随每次运行重放：首次运行（服务器全 403）失败后，
/// 恢复的第二次运行仍只发 ≤ 上限的有界 Range，并完整下完。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prepare_max_range_bytes_applies_on_every_run() {
    let work = std::env::temp_dir().join(format!("fluxdown-intercept-range-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let body: Arc<Vec<u8>> = Arc::new(
        (0..3 * 1024 * 1024 + 4_567)
            .map(|i| (i % 249) as u8)
            .collect(),
    );
    let armed = Arc::new(AtomicBool::new(true));
    let (port, server) = spawn_capped_server(body.clone(), armed.clone());

    let mut engine = make_engine(&work).await;
    install_plugin(
        &engine,
        &work.join("plugin_src"),
        "tester@rangecap",
        r#"["prepare"]"#,
        SCRIPT_RANGE_CAP,
    )
    .await;
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");

    let id = engine
        .manager
        .create_task(NewTaskSpec {
            url: format!("http://127.0.0.1:{port}/file.bin"),
            save_dir: work.to_string_lossy().into_owned(),
            file_name: "file.bin".to_string(),
            segments: 1,
            ..Default::default()
        })
        .await
        .expect("create task");

    let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
        .await
        .expect("first run finishes")
        .expect("done channel open");
    engine.manager.on_task_done(&done).await;
    let task = engine.db.load_task_by_id(&id).await.unwrap().unwrap();
    assert_eq!(task.status, 4, "first run must fail under 403");

    armed.store(false, Ordering::SeqCst);
    engine.manager.resume_task(&id).await;
    let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
        .await
        .expect("second run finishes")
        .expect("done channel open");
    engine.manager.on_task_done(&done).await;
    let task = engine.db.load_task_by_id(&id).await.unwrap().unwrap();
    assert_eq!(task.status, 3, "second run failed: {}", task.error_message);
    let bytes = tokio::fs::read(work.join(&task.file_name))
        .await
        .expect("read output");
    assert!(
        bytes == *body,
        "downloaded content differs from server body"
    );

    assert_eq!(
        server.violations.load(Ordering::SeqCst),
        0,
        "engine sent a request the capped endpoint rejects"
    );
    assert!(server.max_span.load(Ordering::SeqCst) <= RANGE_CAP);
    assert!(
        server.data_gets.load(Ordering::SeqCst) >= body.len().div_ceil(RANGE_CAP),
        "download was not split into windows"
    );

    let notes = plugin_notes(&engine, &id).await;
    assert!(
        notes.iter().any(|n| n.contains("Range 跨度上限")),
        "activity log must name the changed field: {notes:?}"
    );

    cleanup(&work).await;
}

/// 用 `route` 建单段任务并跑到完成收尾，返回最终任务记录。
async fn run_routed_task(
    work: &Path,
    script: &str,
    file_name: &str,
    route: fluxdown_engine::category_route::CategoryRoute,
) -> (Engine, fluxdown_engine::model::TaskInfo, Arc<Vec<u8>>) {
    run_routed_payload(work, script, file_name, route, Arc::new(vec![0x5a; 4096])).await
}

async fn run_routed_payload(
    work: &Path,
    script: &str,
    file_name: &str,
    route: fluxdown_engine::category_route::CategoryRoute,
    body: Arc<Vec<u8>>,
) -> (Engine, fluxdown_engine::model::TaskInfo, Arc<Vec<u8>>) {
    tokio::fs::create_dir_all(work).await.expect("mkdir work");
    let armed = Arc::new(AtomicBool::new(false));
    let (port, _server) = spawn_range_server(body.clone(), armed);
    let mut engine = make_engine(work).await;
    install_archive_plugin(&engine, &work.join("archive_plugin"), script).await;
    let mut done_rx = engine.manager.take_done_rx().expect("done channel");
    let task_id = engine
        .manager
        .create_task(NewTaskSpec {
            url: format!("http://127.0.0.1:{port}/episode"),
            save_dir: work.to_string_lossy().into_owned(),
            file_name: file_name.to_string(),
            segments: 1,
            category_route: Some(route),
            ..Default::default()
        })
        .await
        .expect("create task");
    let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
        .await
        .expect("task finishes")
        .expect("done channel open");
    engine.manager.on_task_done(&done).await;
    let task = engine.db.load_task_by_id(&task_id).await.unwrap().unwrap();
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(task.file_name, file_name, "archive routing must not rename");
    (engine, task, body)
}

fn mkv_route(category_dir: &Path) -> fluxdown_engine::category_route::CategoryRoute {
    fluxdown_engine::category_route::CategoryRoute {
        rules: vec![fluxdown_engine::category_route::CategoryRule {
            match_mode: "extension".to_string(),
            extensions: vec!["mkv".to_string()],
            save_dir: category_dir.to_string_lossy().into_owned(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_plugin_nests_download_under_matched_category_without_renaming() {
    let work = std::env::temp_dir().join(format!("fluxdown-archive-route-{}", uniq()));
    let category_dir = work.join("Videos");
    let file_name = "The Expanse S02E03.mkv";
    let (engine, task, body) =
        run_routed_task(&work, ARCHIVE_ANIME, file_name, mkv_route(&category_dir)).await;
    assert_eq!(
        Path::new(&task.save_dir),
        category_dir.join("Anime Title").as_path(),
        "archive folder must remain nested below the FD category"
    );
    assert_eq!(
        tokio::fs::read(category_dir.join("Anime Title").join(file_name))
            .await
            .expect("read archived file"),
        *body
    );
    assert!(
        !work.join(file_name).exists(),
        "file must not be left in the unclassified root"
    );
    assert!(
        plugin_notes(&engine, &task.task_id)
            .await
            .iter()
            .any(|n| n.contains("Anime Title")),
        "the chosen folder is shown in task activity"
    );
    cleanup(&work).await;
}

/// 插件抛错与返回不安全目录名都不影响下载：文件留在分类目录，原因写进任务活动。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_failures_fall_back_to_category_dir_with_activity_note() {
    for (label, script, note) in [
        (
            "throws",
            r#"globalThis.archive = () => { throw new Error("no match"); };"#,
            "no match",
        ),
        (
            "unsafe",
            r#"globalThis.archive = () => ({ folderName: "../escape" });"#,
            "不合法",
        ),
    ] {
        let work = std::env::temp_dir().join(format!("fluxdown-archive-{label}-{}", uniq()));
        let category_dir = work.join("Videos");
        let file_name = "Show 01.mkv";
        let (engine, task, _) =
            run_routed_task(&work, script, file_name, mkv_route(&category_dir)).await;
        assert_eq!(Path::new(&task.save_dir), category_dir.as_path(), "{label}");
        assert!(category_dir.join(file_name).exists(), "{label}");
        assert!(!work.join("escape").exists(), "{label}: no path escape");
        let notes = plugin_notes(&engine, &task.task_id).await;
        assert!(
            notes.iter().any(|n| n.contains(note)),
            "{label}: activity must explain the fallback, got {notes:?}"
        );
        cleanup(&work).await;
    }
}

/// 「其他」兜底目录也是分类目录：归档子目录挂在它下面。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_applies_under_other_fallback_dir() {
    let work = std::env::temp_dir().join(format!("fluxdown-archive-other-{}", uniq()));
    let other_dir = work.join("Other");
    let route = fluxdown_engine::category_route::CategoryRoute {
        other_dir: other_dir.to_string_lossy().into_owned(),
        ..mkv_route(&work.join("Videos"))
    };
    let (_engine, task, _) = run_routed_task(&work, ARCHIVE_ANIME, "Show 01.ass", route).await;
    assert_eq!(
        Path::new(&task.save_dir),
        other_dir.join("Anime Title").as_path()
    );
    cleanup(&work).await;
}

/// 完成回调无结果时保留已保存的选择，并随分类目录变化迁移。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saved_archive_choice_is_retained_when_completion_declines() {
    let work = std::env::temp_dir().join(format!("fluxdown-archive-saved-{}", uniq()));
    let category_dir = work.join("Videos");
    let route = fluxdown_engine::category_route::CategoryRoute {
        archive: Some(fluxdown_engine::category_route::ArchiveRoute {
            category_dir: work.join("OldVideos").to_string_lossy().into_owned(),
            folder_name: "Saved Title".to_string(),
            directories: Vec::new(),
            base_dir: String::new(),
        }),
        ..mkv_route(&category_dir)
    };
    let script = r#"globalThis.archive = ctx => ctx.phase === "complete" ? null : {folderName:"Anime Title"};"#;
    let (engine, task, _) = run_routed_task(&work, script, "Show 01.mkv", route).await;
    assert_eq!(
        Path::new(&task.save_dir),
        category_dir.join("Saved Title").as_path()
    );
    assert!(!category_dir.join("Anime Title").exists());
    let stored = engine
        .db
        .load_task_category_route(&task.task_id)
        .await
        .expect("load route");
    let stored: fluxdown_engine::category_route::CategoryRoute =
        serde_json::from_str(&stored).expect("stored route json");
    assert_eq!(
        stored.archive,
        Some(fluxdown_engine::category_route::ArchiveRoute {
            category_dir: category_dir.to_string_lossy().into_owned(),
            folder_name: "Saved Title".to_string(),
            directories: Vec::new(),
            base_dir: String::new(),
        }),
        "persisted archive follows the new category dir"
    );
    cleanup(&work).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_plugin_content_facts_override_cached_prediction_without_renaming() {
    use fluxdown_engine::category_route::CategoryRoute;

    let work = std::env::temp_dir().join(format!("fluxdown-archive-content-{}", uniq()));
    let script = r#"globalThis.archive = ctx => {
        if (ctx.phase === "start") {
            if (ctx.detectedType) throw new Error("unobserved start type");
            return {directories:["predicted"]};
        }
        const type = ctx.detectedType;
        if (ctx.phase !== "complete" || ctx.headerHex !== undefined ||
            !type || type.kind !== "image" || type.mime !== "image/png" ||
            type.extension !== "png" || !/^\d{4}-\d{2}-\d{2}$/.test(ctx.createdDate))
            throw new Error("missing or unsafe final content facts");
        return {directories:[ctx.createdDate, type.kind]};
    };"#;
    let body = Arc::new(b"\x89PNG\r\n\x1a\n\0\0\0\0".to_vec());
    let (engine, task, body) =
        run_routed_payload(&work, script, "photo.txt", CategoryRoute::default(), body).await;
    let created = chrono::DateTime::from_timestamp(task.created_at.parse().unwrap(), 0)
        .unwrap()
        .format("%Y-%m-%d")
        .to_string();
    let expected = work.join(created).join("image");
    assert_eq!(Path::new(&task.save_dir), expected.as_path());
    assert_eq!(
        tokio::fs::read(expected.join("photo.txt")).await.unwrap(),
        *body
    );
    assert!(!work.join("predicted/photo.txt").exists());
    let stored = engine
        .db
        .load_task_category_route(&task.task_id)
        .await
        .unwrap();
    let stored: CategoryRoute = serde_json::from_str(&stored).unwrap();
    assert_eq!(
        stored
            .archive
            .unwrap()
            .directories
            .last()
            .map(String::as_str),
        Some("image")
    );
    cleanup(&work).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_plugin_completion_error_keeps_prediction_and_successful_download() {
    let work = std::env::temp_dir().join(format!("fluxdown-archive-final-error-{}", uniq()));
    let script = r#"globalThis.archive = ctx => {
        if (ctx.phase === "complete") throw new Error("final classification failed");
        return {directories:["prediction"]};
    };"#;
    let (engine, task, body) = run_routed_task(
        &work,
        script,
        "report.txt",
        fluxdown_engine::category_route::CategoryRoute::default(),
    )
    .await;
    assert_eq!(Path::new(&task.save_dir), work.join("prediction").as_path());
    assert_eq!(
        tokio::fs::read(work.join("prediction/report.txt"))
            .await
            .unwrap(),
        *body
    );
    assert!(
        plugin_notes(&engine, &task.task_id)
            .await
            .iter()
            .any(|note| note.contains("final classification failed"))
    );
    cleanup(&work).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_plugin_classifies_final_content_despite_misleading_extensions() {
    let script = std::env::var_os("FLUXDOWN_TEST_ARCHIVE_SCRIPT")
        .map(std::fs::read_to_string)
        .transpose()
        .expect("read optional compiled classifier")
        .unwrap_or_else(|| r#"globalThis.archive = ctx => {
            if (ctx.phase !== "complete") return null;
            const folder = {image:"图片", document:"文档", archive:"压缩包"}[ctx.detectedType?.kind];
            return folder ? {directories:[folder]} : null;
        };"#.to_owned());
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("data.bin", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"ordinary archive contents").unwrap();
    let zip = zip.finish().unwrap().into_inner();
    for (name, bytes, folder) in [
        ("photo.txt", b"\x89PNG\r\n\x1a\n\0\0\0\0".to_vec(), "图片"),
        ("report.bin", b"%PDF-1.7\n%content\n".to_vec(), "文档"),
        ("book.epub", zip, "压缩包"),
    ] {
        let work = std::env::temp_dir().join(format!("fluxdown-archive-types-{}", uniq()));
        let (_, task, body) = run_routed_payload(
            &work,
            &script,
            name,
            fluxdown_engine::category_route::CategoryRoute {
                archive_categories: [
                    (
                        "image".to_owned(),
                        work.join("图片").to_string_lossy().into_owned(),
                    ),
                    (
                        "document".to_owned(),
                        work.join("文档").to_string_lossy().into_owned(),
                    ),
                    (
                        "archive".to_owned(),
                        work.join("压缩包").to_string_lossy().into_owned(),
                    ),
                ]
                .into(),
                ..Default::default()
            },
            Arc::new(bytes),
        )
        .await;
        assert_eq!(
            Path::new(&task.save_dir),
            work.join(folder).as_path(),
            "{name}"
        );
        assert_eq!(
            tokio::fs::read(work.join(folder).join(name)).await.unwrap(),
            *body
        );
        cleanup(&work).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_category_uses_host_directory_and_declines_unconfigured_or_conflicting_outputs() {
    let script =
        r#"globalThis.archive = ctx => ctx.phase === "complete" ? {category:"image"} : null;"#;
    for (configured, output) in [
        (Some("Renamed Photos"), script),
        (Some("Changed Destination"), script),
        (None, script),
        (
            Some("Rejected"),
            r#"globalThis.archive = ctx => ctx.phase === "complete" ? {category:"image",directories:["escape"]} : null;"#,
        ),
        (
            Some("Rejected"),
            r#"globalThis.archive = ctx => ctx.phase === "complete" ? {category:"../outside"} : null;"#,
        ),
    ] {
        let work = std::env::temp_dir().join(format!("fluxdown-archive-category-{}", uniq()));
        let target = configured.map(|name| work.join(name));
        let route = fluxdown_engine::category_route::CategoryRoute {
            archive_categories: target
                .as_ref()
                .map(|path| [("image".to_owned(), path.to_string_lossy().into_owned())].into())
                .unwrap_or_default(),
            ..Default::default()
        };
        let (_, task, body) = run_routed_task(&work, output, "misleading.txt", route).await;
        let expected = if output == script {
            target.unwrap_or_else(|| work.clone())
        } else {
            work.clone()
        };
        assert_eq!(Path::new(&task.save_dir), expected);
        assert_eq!(
            tokio::fs::read(expected.join("misleading.txt"))
                .await
                .expect("downloaded file"),
            *body
        );
        assert!(!work.join("escape").exists());
        cleanup(&work).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_plugin_routes_nested_directories_from_authorized_base() {
    nested_archive_download(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_plugin_nested_directories_preserve_existing_files() {
    nested_archive_download(true).await;
}

async fn nested_archive_download(collision: bool) {
    use fluxdown_engine::category_route::CategoryRoute;

    let work = std::env::temp_dir().join(format!("fluxdown-archive-nested-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    // Minimal ISO BMFF with a video handler: actual content, not extension-only fake video bytes.
    let body =
        Arc::new(b"\0\0\0\x18ftypmp42\0\0\0\0mp42isom\0\0\0\x14hdlr\0\0\0\0\0\0\0\0vide".to_vec());
    let armed = Arc::new(AtomicBool::new(false));
    let (port, _server) = spawn_range_server(body.clone(), armed);
    let mut engine = make_engine(&work).await;
    let archive_js = r#"globalThis.archive = async () => ({directories:["视频","哔哩哔哩","小猪仔步履不停","小猪仔评财报"]});"#;
    install_archive_plugin(&engine, &work.join("archive_plugin"), archive_js).await;
    let mut done_rx = engine.manager.take_done_rx().expect("done channel");
    let name = "clip.mp4";
    let expected = work.join("视频/哔哩哔哩/小猪仔步履不停/小猪仔评财报");
    if collision {
        tokio::fs::create_dir_all(&expected)
            .await
            .expect("create existing destination");
        tokio::fs::write(expected.join(name), b"existing unrelated content")
            .await
            .expect("write destination conflict");
    }
    let task_id = engine
        .manager
        .create_task(NewTaskSpec {
            url: format!("http://127.0.0.1:{port}/episode"),
            save_dir: work.to_string_lossy().into_owned(),
            file_name: name.to_owned(),
            segments: 1,
            category_route: Some(CategoryRoute {
                archive_metadata: Some(fluxdown_engine::model::ArchiveMetadata {
                    platform: "bilibili".to_owned(),
                    media_kind: "post".to_owned(),
                    creator_id: "3707002469485044".to_owned(),
                    creator_name: "小猪仔步履不停".to_owned(),
                    series_id: String::new(),
                    series_title: String::new(),
                    collection_id: "8878655".to_owned(),
                    collection_title: "小猪仔评财报".to_owned(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .expect("create task");
    let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
        .await
        .expect("task finishes")
        .expect("done channel open");
    engine.manager.on_task_done(&done).await;
    let task = engine.db.load_task_by_id(&task_id).await.unwrap().unwrap();
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(Path::new(&task.save_dir), expected.as_path());
    assert_eq!(
        tokio::fs::read(expected.join(&task.file_name))
            .await
            .unwrap(),
        *body
    );
    if collision {
        assert_ne!(task.file_name, name);
        assert_eq!(
            tokio::fs::read(expected.join(name)).await.unwrap(),
            b"existing unrelated content"
        );
    } else {
        assert_eq!(task.file_name, name);
    }
    assert!(!work.join(name).exists());
    cleanup(&work).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explicit_save_directory_bypasses_archive_callback() {
    let work = std::env::temp_dir().join(format!("fluxdown-archive-explicit-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let body = Arc::new(vec![0x7c; 4096]);
    let armed = Arc::new(AtomicBool::new(false));
    let (port, _server) = spawn_range_server(body.clone(), armed);
    let mut engine = make_engine(&work).await;
    install_archive_plugin(
        &engine,
        &work.join("archive_plugin"),
        r#"globalThis.archive = async () => ({directories:["must","not","be","used"]});"#,
    )
    .await;
    let mut done_rx = engine.manager.take_done_rx().expect("done channel");
    let name = "manual.mkv";
    let save_dir = work.join("picked").to_string_lossy().into_owned();
    let task_id = engine
        .manager
        .create_task(NewTaskSpec {
            url: format!("http://127.0.0.1:{port}/episode"),
            save_dir: save_dir.clone(),
            file_name: name.to_owned(),
            segments: 1,
            start_paused: true,
            ..Default::default()
        })
        .await
        .expect("create task");
    // Resolver metadata may be retained without granting permission to redirect a manual task.
    engine.db.set_task_category_route(
        &task_id,
        r#"{"archiveMetadata":{"platform":"bilibili","mediaKind":"post","creatorName":"Manual creator"}}"#,
    ).await.expect("persist metadata without route authorization");
    engine.manager.resume_task(&task_id).await;
    let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
        .await
        .expect("task finishes")
        .expect("done channel open");
    engine.manager.on_task_done(&done).await;
    let task = engine.db.load_task_by_id(&task_id).await.unwrap().unwrap();
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(task.save_dir, save_dir);
    assert_eq!(
        tokio::fs::read(work.join("picked").join(name))
            .await
            .unwrap(),
        *body
    );
    cleanup(&work).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_plugin_unsafe_directories_do_not_escape_base() {
    use fluxdown_engine::category_route::CategoryRoute;

    let work = std::env::temp_dir().join(format!("fluxdown-archive-unsafe-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let body = Arc::new(vec![0x2a; 4096]);
    let armed = Arc::new(AtomicBool::new(false));
    let (port, _server) = spawn_range_server(body.clone(), armed);
    let mut engine = make_engine(&work).await;
    install_archive_plugin(
        &engine,
        &work.join("archive_plugin"),
        r#"globalThis.archive = async () => ({directories:["..","escape"]});"#,
    )
    .await;
    let mut done_rx = engine.manager.take_done_rx().expect("done channel");
    let name = "unsafe.mkv";
    let task_id = engine
        .manager
        .create_task(NewTaskSpec {
            url: format!("http://127.0.0.1:{port}/episode"),
            save_dir: work.to_string_lossy().into_owned(),
            file_name: name.to_owned(),
            segments: 1,
            category_route: Some(CategoryRoute::default()),
            ..Default::default()
        })
        .await
        .expect("create task");
    let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
        .await
        .expect("task finishes")
        .expect("done channel open");
    engine.manager.on_task_done(&done).await;
    let task = engine.db.load_task_by_id(&task_id).await.unwrap().unwrap();
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(Path::new(&task.save_dir), work.as_path());
    assert_eq!(tokio::fs::read(work.join(name)).await.unwrap(), *body);
    assert!(!work.parent().unwrap().join("escape").exists());
    cleanup(&work).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_plugin_group_preserves_member_tree_under_authorized_base() {
    use fluxdown_engine::category_route::CategoryRoute;
    use fluxdown_engine::download_manager::{CreateGroupSpec, GroupItemSpec};
    let work = std::env::temp_dir().join(format!("fluxdown-archive-group-{}", uniq()));
    tokio::fs::create_dir_all(&work)
        .await
        .expect("create group work");
    let body = Arc::new(vec![0x6c; 4096]);
    let (port, _server) = spawn_range_server(body.clone(), Arc::new(AtomicBool::new(false)));
    let mut engine = make_engine(&work).await;
    let script = r#"globalThis.archive = ctx => ({directories:["视频","哔哩哔哩",ctx.metadata.creatorName,ctx.metadata.collectionTitle]});"#;
    install_archive_plugin(&engine, &work.join("archive_plugin"), script).await;
    let mut done_rx = engine.manager.take_done_rx().expect("done channel");
    let group_id = engine
        .manager
        .create_task_group(CreateGroupSpec {
            source_url: format!("http://127.0.0.1:{port}/collection"),
            group_name: "Original collection".to_owned(),
            base_save_dir: work.to_string_lossy().into_owned(),
            segments: 1,
            category_route: Some(CategoryRoute::default()),
            archive_metadata: Some(fluxdown_engine::model::ArchiveMetadata {
                platform: "bilibili".to_owned(),
                media_kind: "post".to_owned(),
                creator_name: "小猪仔步履不停".to_owned(),
                collection_title: "小猪仔评财报".to_owned(),
                ..Default::default()
            }),
            items: vec![
                GroupItemSpec {
                    file_name: "one.mp4".to_owned(),
                    rel_path: "chapter/one".to_owned(),
                    ..Default::default()
                },
                GroupItemSpec {
                    file_name: "two.mp4".to_owned(),
                    rel_path: "chapter/two".to_owned(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        })
        .await
        .expect("create group");
    let expected = work.join("视频/哔哩哔哩/小猪仔步履不停/小猪仔评财报");
    let group = engine
        .db
        .load_group_by_id(&group_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(Path::new(&group.save_dir), expected.as_path());
    for _ in 0..2 {
        let done = tokio::time::timeout(Duration::from_secs(60), done_rx.recv())
            .await
            .expect("group member finishes")
            .expect("done channel open");
        engine.manager.on_task_done(&done).await;
    }
    assert_eq!(
        tokio::fs::read(expected.join("chapter/one/one.mp4"))
            .await
            .unwrap(),
        *body
    );
    assert_eq!(
        tokio::fs::read(expected.join("chapter/two/two.mp4"))
            .await
            .unwrap(),
        *body
    );
    assert!(!work.join("Original collection").exists());
    cleanup(&work).await;
}
