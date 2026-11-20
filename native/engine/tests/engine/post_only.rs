//! 只收 POST 的下载端点（OneDrive `*.svc.ms/transform/zip` 打包下载，#970）：
//!
//! - 浏览器 POST 接管的任务按原 method + body 下载完成，且表单只向服务端提交一次；
//! - 同一引擎内恢复 / 重试这类任务时按内存里的原请求事务重放，从 0 重下，不拼接上一次的残片；
//! - 引擎重启后请求体已不在（不落库），恢复必须明确失败，且不得改发 HEAD / GET；
//! - 换源后任务按新地址的 GET 续传，不再当作 POST 任务；
//! - 以 GET 建的任务撞上 `405 + Allow: OPTIONS, POST`，给出可读错误、不做无意义的重试。
//!
//! 运行：`cargo test -p fluxdown_engine --test engine post_only -- --ignored`

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::download_manager::{FileExistsBehavior, NewTaskSpec};
use fluxdown_engine::downloader::{
    CapturedRequestBody, POST_ONLY_ENDPOINT_MESSAGE, POST_ONLY_RESUME_MESSAGE,
};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const ZIP_BODY: &[u8] = b"PK\x03\x04 fake zip payload for the POST-only endpoint";
const FORM: &str = "zipFileName=OneDrive.zip&files=%7B%22items%22%3A%5B%5D%7D";
const ZIP_PATH: &str = "/transform/zip?cs=abc";
/// 换源用的普通直链：GET 返回同一份内容。
const PLAIN_PATH: &str = "/plain/OneDrive.zip";
/// 表单端点：POST + 表单 → 303 到 [`SIGNED_PATH`]（服务端按表单签出直链的形态）。
const FORM_PATH: &str = "/form/download";
const SIGNED_PATH: &str = "/signed/OneDrive.zip";
const HTML_PATH: &str = "/share";
const DIRECT_HTML_PATH: &str = "/page.html";
const HTML_BODY: &[u8] = b"<!doctype html><p>Shared file</p>";
const CHECKSUM: &str = "sha256=0000000000000000000000000000000000000000000000000000000000000000";

/// Read the request line, complete body, and headers.
async fn read_request(stream: &mut TcpStream) -> Option<(String, String, String, String)> {
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut request_line = head.split_whitespace();
    let method = request_line.next()?.to_string();
    let path = request_line.next()?.to_string();
    let content_length = head
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    Some((method, path, body, head))
}

fn zip_response(body: &[u8]) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\n\
         Content-Disposition: attachment; filename=\"OneDrive.zip\"\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        ZIP_BODY.len()
    )
    .into_bytes();
    head.extend_from_slice(body);
    head
}

/// 模拟端点：打包地址只认 POST + 表单 → 200 zip，其余方法 → 405 `Allow: OPTIONS, POST`；
/// [`FORM_PATH`] 的 POST + 表单 → 303 到 [`SIGNED_PATH`]（GET → 200 zip）；[`PLAIN_PATH`]
/// 的 GET → 200 zip。`truncate_next_zip` 置位时下一个 zip 响应只发一半就断开。
async fn serve(mut stream: TcpStream, server: Arc<Server>) {
    let Some((method, path, body, head)) = read_request(&mut stream).await else {
        return;
    };
    server.log.lock().unwrap().push(format!("{method} {path}"));
    let zip = || {
        if server.truncate_next_zip.swap(false, Ordering::SeqCst) {
            zip_response(&ZIP_BODY[..ZIP_BODY.len() / 2])
        } else {
            zip_response(ZIP_BODY)
        }
    };
    let response = if matches!(path.as_str(), HTML_PATH | DIRECT_HTML_PATH)
        && (matches!(method.as_str(), "HEAD" | "GET") || (method == "POST" && body == FORM))
        && head
            .lines()
            .any(|line| line.eq_ignore_ascii_case("cookie: session=fixture"))
        && head
            .lines()
            .any(|line| line.eq_ignore_ascii_case("authorization: Bearer fixture"))
    {
        let (status, headers, body) = server.share_response.lock().unwrap().unwrap_or((
            "200 OK",
            "Content-Type: Text/HTML; charset=utf-8\r\n",
            HTML_BODY,
        ));
        let mut response = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        if method != "HEAD" {
            response.extend_from_slice(body);
        }
        response
    } else if path == PLAIN_PATH && method == "GET" {
        zip_response(ZIP_BODY)
    } else if (path == ZIP_PATH && method == "POST" && body == FORM)
        || (path == SIGNED_PATH && method == "GET")
    {
        zip()
    } else if path == FORM_PATH && method == "POST" && body == FORM {
        format!(
            "HTTP/1.1 303 See Other\r\nLocation: {SIGNED_PATH}\r\nContent-Length: 0\r\n\
             Connection: close\r\n\r\n"
        )
        .into_bytes()
    } else {
        b"HTTP/1.1 405 Method Not Allowed\r\nAllow: OPTIONS, POST\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec()
    };
    let _ignored = stream.write_all(&response).await;
}

type ShareResponse = (&'static str, &'static str, &'static [u8]);

#[derive(Default)]
struct Server {
    log: Mutex<Vec<String>>,
    truncate_next_zip: AtomicBool,
    share_response: Mutex<Option<ShareResponse>>,
}

struct Harness {
    engine: Engine,
    done_rx: tokio::sync::mpsc::Receiver<fluxdown_engine::download_manager::TaskDone>,
    addr: SocketAddr,
    server: Arc<Server>,
    work_dir: std::path::PathBuf,
}

async fn open_engine(
    work_dir: &std::path::Path,
) -> (
    Engine,
    tokio::sync::mpsc::Receiver<fluxdown_engine::download_manager::TaskDone>,
) {
    let dir = work_dir.to_string_lossy().to_string();
    let config = EngineConfig {
        max_concurrent: 5,
        speed_limit_bps: 0,
        upload_limit_bps: 0,
        default_save_dir: dir.clone(),
        app_data_dir: dir,
        bt_config: BtConfig::default(),
        proxy_config: ProxyConfig::default(),
        user_agent: String::new(),
        data_dir_override: Some(work_dir.to_path_buf()),
        database_url: None,
    };
    let mut engine = Engine::new(config, Arc::new(NoopSink), Arc::new(NoopSelection))
        .await
        .expect("engine");
    if let Some(mut rx) = engine.manager.take_progress_rx() {
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
    }
    let done_rx = engine.manager.take_done_rx().expect("done receiver");
    (engine, done_rx)
}

impl Harness {
    async fn new(name: &str) -> Self {
        let work_dir =
            std::env::temp_dir().join(format!("fluxdown_post_only_{name}_{}", std::process::id()));
        if let Err(error) = tokio::fs::remove_dir_all(&work_dir).await {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        }
        tokio::fs::create_dir_all(&work_dir).await.unwrap();

        let server = Arc::new(Server::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        {
            let server = server.clone();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serve(stream, server.clone()));
                }
            });
        }

        let (engine, done_rx) = open_engine(&work_dir).await;
        Self {
            engine,
            done_rx,
            addr,
            server,
            work_dir,
        }
    }

    /// 关掉引擎再在同一数据目录重开：只剩落库的任务事实，内存里的请求事务随之消失。
    /// 旧引擎必须先整体释放（含数据目录写入租约），新引擎才能打开。
    async fn restart_engine(self) -> Self {
        let Self {
            mut engine,
            done_rx,
            addr,
            server,
            work_dir,
        } = self;
        engine.manager.shutdown().await;
        drop(done_rx);
        drop(engine);
        let (engine, done_rx) = open_engine(&work_dir).await;
        Self {
            engine,
            done_rx,
            addr,
            server,
            work_dir,
        }
    }

    fn spec(&self, method: Option<&str>, start_paused: bool) -> NewTaskSpec {
        NewTaskSpec {
            url: format!("http://{}{ZIP_PATH}", self.addr),
            save_dir: self.work_dir.to_string_lossy().to_string(),
            file_name: "OneDrive.zip".to_string(),
            segments: 4,
            method: method.map(str::to_string),
            body: method.map(|_| CapturedRequestBody::Urlencoded {
                raw: FORM.to_string(),
            }),
            start_paused,
            ..Default::default()
        }
    }

    fn share_spec(&self, method: Option<&str>) -> NewTaskSpec {
        let mut spec = self.spec(method, false);
        spec.url = format!("http://{}{HTML_PATH}", self.addr);
        spec.cookies = "session=fixture".into();
        spec.extra_headers
            .insert("Authorization".into(), "Bearer fixture".into());
        spec.checksum = CHECKSUM.into();
        spec
    }

    async fn finish(&mut self, task_id: &str) -> fluxdown_engine::model::TaskInfo {
        let done = tokio::time::timeout(Duration::from_secs(30), self.done_rx.recv())
            .await
            .expect("download should settle")
            .expect("done channel open");
        self.engine.manager.on_task_done(&done).await;
        self.engine
            .db
            .load_task_by_id(task_id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn output(&self, task: &fluxdown_engine::model::TaskInfo) -> Vec<u8> {
        tokio::fs::read(self.work_dir.join(&task.file_name))
            .await
            .unwrap()
    }

    /// 打包地址收到的请求（`METHOD path` 去掉主机）。
    fn requests(&self) -> Vec<String> {
        self.server.log.lock().unwrap().clone()
    }

    fn clear_requests(&self) {
        self.server.log.lock().unwrap().clear();
    }

    async fn cleanup(self) {
        let work_dir = self.work_dir.clone();
        drop(self);
        if let Err(error) = tokio::fs::remove_dir_all(&work_dir).await {
            eprintln!("cleanup {}: {error}", work_dir.display());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn captured_post_downloads_with_a_single_submission() {
    let mut h = Harness::new("captured").await;
    let spec = h.spec(Some("POST"), false);
    let task_id = h.engine.manager.create_task(spec).await.expect("create");
    let task = h.finish(&task_id).await;
    assert_eq!(task.status, 3, "error={}", task.error_message);
    assert_eq!(h.output(&task).await, ZIP_BODY);
    // 探测发出的就是那唯一一次提交：服务端（OneDrive 会为每次提交重新打包）只处理一遍。
    assert_eq!(h.requests(), [format!("POST {ZIP_PATH}")]);
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn redownloading_a_cleared_post_task_requires_a_new_browser_request() {
    let mut h = Harness::new("redownload_cleared").await;
    let spec = h.spec(Some("POST"), false);
    let task_id = h.engine.manager.create_task(spec).await.expect("create");
    let completed = h.finish(&task_id).await;
    assert_eq!(completed.status, 3, "error={}", completed.error_message);
    h.engine.manager.clear_task_file(&task_id).await.unwrap();
    assert!(!h.work_dir.join(&completed.file_name).exists());

    h.clear_requests();
    for restart_engine in [false, true] {
        if restart_engine {
            h = h.restart_engine().await;
        }
        let error = h
            .engine
            .manager
            .restart_missing_task_file(&task_id)
            .await
            .expect_err("a completed POST must not be replayed, even in the same engine");
        assert_eq!(error.to_string(), POST_ONLY_RESUME_MESSAGE);
        let retained = h
            .engine
            .db
            .load_task_by_id(&task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.status, 3);
        assert!(retained.file_missing);
        assert_eq!(retained.downloaded_bytes, completed.downloaded_bytes);
        assert_eq!(retained.completed_at, completed.completed_at);
        assert!(h.requests().is_empty());
    }
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn retrying_a_post_task_replays_the_request_from_scratch() {
    let mut h = Harness::new("retry").await;
    h.server.truncate_next_zip.store(true, Ordering::SeqCst);
    let spec = h.spec(Some("POST"), false);
    let task_id = h.engine.manager.create_task(spec).await.expect("create");
    let first = h.finish(&task_id).await;
    assert_eq!(first.status, 4, "a truncated response must fail the run");

    h.engine.manager.resume_task(&task_id).await;
    let task = h.finish(&task_id).await;
    assert_eq!(task.status, 3, "error={}", task.error_message);
    // 从 0 重下：上一次的半截数据不得留在文件头部。
    assert_eq!(h.output(&task).await, ZIP_BODY);
    assert_eq!(
        h.requests(),
        [format!("POST {ZIP_PATH}"), format!("POST {ZIP_PATH}")],
        "the retry must replay the original POST, never HEAD/GET"
    );
    h.cleanup().await;
}

/// 表单 POST 被 303 带到签出的直链（数据来自 GET）：重试不能对上一跳的直链续传
/// （每次提交签出的地址 / 内容都可能不同），而是从表单起重放整条跳转链、从 0 重下。
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn retrying_a_post_redirected_to_get_replays_the_whole_chain() {
    let mut h = Harness::new("redirect").await;
    h.server.truncate_next_zip.store(true, Ordering::SeqCst);
    let spec = NewTaskSpec {
        url: format!("http://{}{FORM_PATH}", h.addr),
        ..h.spec(Some("POST"), false)
    };
    let task_id = h.engine.manager.create_task(spec).await.expect("create");
    let first = h.finish(&task_id).await;
    assert_eq!(first.status, 4, "a truncated response must fail the run");

    h.engine.manager.resume_task(&task_id).await;
    let task = h.finish(&task_id).await;
    assert_eq!(task.status, 3, "error={}", task.error_message);
    assert_eq!(h.output(&task).await, ZIP_BODY);
    let hop = [format!("POST {FORM_PATH}"), format!("GET {SIGNED_PATH}")];
    assert_eq!(h.requests(), [hop.clone(), hop].concat());
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn paused_post_task_resumes_in_the_same_engine() {
    let mut h = Harness::new("paused").await;
    let spec = h.spec(Some("POST"), true);
    let task_id = h.engine.manager.create_task(spec).await.expect("create");
    // 稍后下载会在后台探测一次元数据（HEAD，不提交表单）；等它结束再清空请求日志。
    tokio::time::sleep(Duration::from_millis(500)).await;
    h.clear_requests();

    h.engine.manager.resume_task(&task_id).await;
    let task = h.finish(&task_id).await;
    assert_eq!(task.status, 3, "error={}", task.error_message);
    assert_eq!(h.output(&task).await, ZIP_BODY);
    assert_eq!(h.requests(), [format!("POST {ZIP_PATH}")]);
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn resuming_a_post_task_after_restart_fails_clearly_without_sending_get() {
    let mut h = Harness::new("restart").await;
    let spec = h.spec(Some("post"), true);
    let task_id = h.engine.manager.create_task(spec).await.expect("create");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut h = h.restart_engine().await;
    h.clear_requests();

    h.engine.manager.resume_task(&task_id).await;
    let task = h
        .engine
        .db
        .load_task_by_id(&task_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.status, 4);
    assert_eq!(task.error_message, POST_ONLY_RESUME_MESSAGE);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        h.requests().is_empty(),
        "resume must not replay the request as GET/HEAD: {:?}",
        h.requests()
    );
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn changing_the_url_of_a_post_task_downloads_the_new_link_with_get() {
    let mut h = Harness::new("change_url").await;
    let spec = h.spec(Some("POST"), true);
    let task_id = h.engine.manager.create_task(spec).await.expect("create");
    tokio::time::sleep(Duration::from_millis(500)).await;
    h.engine
        .manager
        .change_task_url(&task_id, &format!("http://{}{PLAIN_PATH}", h.addr))
        .await
        .expect("change url");
    h.clear_requests();

    h.engine.manager.resume_task(&task_id).await;
    let task = h.finish(&task_id).await;
    assert_eq!(task.status, 3, "error={}", task.error_message);
    assert_eq!(h.output(&task).await, ZIP_BODY);
    let requests = h.requests();
    assert!(
        requests
            .iter()
            .all(|r| r.ends_with(PLAIN_PATH) && !r.starts_with("POST")),
        "the replaced link must be fetched with GET only: {requests:?}"
    );
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --ignored"]
async fn get_task_on_post_only_endpoint_reports_readable_error() {
    let mut h = Harness::new("get").await;
    let spec = h.spec(None, false);
    let task_id = h.engine.manager.create_task(spec).await.expect("create");
    let task = h.finish(&task_id).await;
    assert_eq!(task.status, 4);
    assert_eq!(task.error_message, POST_ONLY_ENDPOINT_MESSAGE);
    // 一轮探测（HEAD ∥ ranged GET，必要时 plain GET）即停，不按 405 反复重试。
    let requests = h.requests();
    assert!(
        requests.len() <= 3 && requests.iter().all(|r| !r.starts_with("POST")),
        "expected a single probe round: {requests:?}"
    );
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
async fn save_html_keeps_original_and_existing_file_and_rejects_changed_response() {
    let mut h = Harness::new("save_html_get").await;
    let page_body = b"<!-- share page --><section><h2>Shared file</h2></section>";
    let headers = "Content-Type: application/octet-stream\r\n\
                   Content-Disposition: inline; filename=share.html\r\n";
    *h.server.share_response.lock().unwrap() = Some(("200 OK", headers, page_body));
    let mut spec = h.share_spec(None);
    spec.url = format!("http://{}{DIRECT_HTML_PATH}", h.addr);
    let original = h.engine.manager.create_task(spec).await.unwrap();
    let failed = h.finish(&original).await;
    assert_eq!(failed.status, 4);
    assert!(
        failed
            .error_message
            .contains("instead of the expected file")
    );
    assert!(!h.work_dir.join("OneDrive.zip").exists());
    let existing_page = h.work_dir.join("OneDrive.html");
    tokio::fs::write(&existing_page, b"keep").await.unwrap();
    h.engine
        .manager
        .set_file_exists_behavior(FileExistsBehavior::Overwrite);
    h.clear_requests();
    let saved = h
        .engine
        .manager
        .save_html_response(&original)
        .await
        .unwrap();
    let page = h.finish(&saved).await;
    assert_ne!(saved, original);
    assert_eq!(page.status, 3, "{}", page.error_message);
    assert!(page.file_name.ends_with(".html"));
    assert!(page.checksum.is_empty());
    assert_eq!(h.output(&page).await, page_body);
    assert_eq!(
        tokio::fs::read(&existing_page).await.unwrap().as_slice(),
        b"keep"
    );
    assert_eq!(h.requests(), [format!("GET {DIRECT_HTML_PATH}")]);
    assert_eq!(
        h.engine.db.load_task_by_id(&original).await.unwrap(),
        Some(failed)
    );

    assert!(h.engine.manager.save_html_response(&saved).await.is_err());
    let credential_key = format!("plugin.task-{saved}@fluxdown.auth.http://{}", h.addr);
    assert!(
        h.engine
            .db
            .get_config(&credential_key)
            .await
            .unwrap()
            .is_some()
    );
    h.engine.manager.delete_task(&saved, false).await.unwrap();
    assert!(
        h.engine
            .db
            .get_config(&credential_key)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(h.output(&page).await, page_body);

    *h.server.share_response.lock().unwrap() = Some(("200 OK", headers, ZIP_BODY));
    let changed = h
        .engine
        .manager
        .save_html_response(&original)
        .await
        .unwrap();
    let changed = h.finish(&changed).await;
    assert_ne!(changed.task_id, saved);
    assert_eq!(changed.status, 4);
    assert!(
        changed
            .error_message
            .contains("did not return a complete HTML page")
    );
    assert!(!h.work_dir.join(changed.file_name).exists());
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
async fn unnamed_direct_html_and_html_attachments_download_normally() {
    let mut h = Harness::new("normal_html").await;
    for (path, headers, expected_name, hint) in [
        (
            DIRECT_HTML_PATH,
            "Content-Type: text/html\r\n",
            "page.html",
            0,
        ),
        (
            HTML_PATH,
            "Content-Type: application/octet-stream\r\n\
             Content-Disposition: attachment; filename=report.html\r\n",
            "report.html",
            0,
        ),
        (
            HTML_PATH,
            "Content-Type: text/html\r\n\
             Content-Disposition: Attachment; filename=hinted.html\r\n",
            "hinted.html",
            -1,
        ),
    ] {
        *h.server.share_response.lock().unwrap() = Some(("200 OK", headers, HTML_BODY));
        let mut spec = h.share_spec(None);
        spec.url = format!("http://{}{path}", h.addr);
        spec.file_name.clear();
        spec.checksum.clear();
        spec.hint_file_size = hint;
        let task = h.engine.manager.create_task(spec).await.unwrap();
        let page = h.finish(&task).await;
        assert_eq!(page.status, 3, "{}", page.error_message);
        assert_eq!(page.file_name, expected_name);
        assert_eq!(h.output(&page).await, HTML_BODY);
    }
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
async fn html_url_suffix_does_not_override_explicit_binary_target() {
    let mut h = Harness::new("binary_at_html_url").await;
    *h.server.share_response.lock().unwrap() = Some((
        "200 OK",
        "Content-Type: application/octet-stream\r\n",
        ZIP_BODY,
    ));
    for (hint, file_name) in [(0, "probed.zip"), (-1, "unprobed.zip")] {
        let mut spec = h.share_spec(None);
        spec.url = format!("http://{}{DIRECT_HTML_PATH}", h.addr);
        spec.file_name = file_name.into();
        spec.hint_file_size = hint;
        spec.checksum.clear();
        let task = h.engine.manager.create_task(spec).await.unwrap();
        let task = h.finish(&task).await;
        assert_eq!(task.status, 3, "{}", task.error_message);
        assert_eq!(task.file_name, file_name);
        assert_eq!(h.output(&task).await, ZIP_BODY);
    }
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
async fn html_response_name_is_evidence_not_download_intent() {
    let mut h = Harness::new("html_response_filename").await;
    for (headers, hint, filename) in [
        (
            "Content-Type: application/octet-stream\r\n\
             Content-Disposition: attachment; filename=login.html\r\n",
            0,
            "archive.zip",
        ),
        (
            "Content-Disposition: attachment; filename=login.html\r\n",
            -1,
            "archive.zip",
        ),
        (
            "Content-Disposition: inline; filename=login.html\r\n",
            0,
            "",
        ),
    ] {
        *h.server.share_response.lock().unwrap() = Some(("200 OK", headers, HTML_BODY));
        let mut spec = h.share_spec(None);
        spec.file_name = filename.into();
        spec.hint_file_size = hint;
        let original = h.engine.manager.create_task(spec).await.unwrap();
        let failed = h.finish(&original).await;
        assert_eq!(failed.status, 4);
        assert!(failed.error_message.contains("HTML response filename"));
        assert!(!h.work_dir.join("archive.zip").exists());
        assert!(!h.work_dir.join("login.html").exists());

        let saved = h
            .engine
            .manager
            .save_html_response(&original)
            .await
            .unwrap();
        let page = h.finish(&saved).await;
        assert_eq!(page.status, 3, "{}", page.error_message);
        assert_eq!(h.output(&page).await, HTML_BODY);
    }

    let mut spec = h.share_spec(None);
    spec.file_name = "explicit.html".into();
    spec.checksum.clear();
    let task = h.engine.manager.create_task(spec).await.unwrap();
    let page = h.finish(&task).await;
    assert_eq!(page.status, 3, "{}", page.error_message);
    assert_eq!(page.file_name, "explicit.html");
    assert_eq!(h.output(&page).await, HTML_BODY);
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
async fn html_login_status_is_reported_but_cannot_be_saved() {
    const HTML_HEADERS: &str = "Content-Type: text/html\r\n";
    const CHALLENGE_HEADERS: &str = "Content-Type: text/html\r\ncf-mitigated: challenge\r\n";
    let mut h = Harness::new("html_login_status").await;
    for (status, method, hint, headers) in [
        ("403 Forbidden", None, 0, HTML_HEADERS),
        ("401 Unauthorized", Some("POST"), 0, HTML_HEADERS),
        ("403 Forbidden", None, -1, HTML_HEADERS),
        ("403 Forbidden", None, 0, CHALLENGE_HEADERS),
    ] {
        *h.server.share_response.lock().unwrap() = Some((status, headers, HTML_BODY));
        let mut spec = h.share_spec(method);
        spec.hint_file_size = hint;
        let original = h.engine.manager.create_task(spec).await.unwrap();
        let failed = h.finish(&original).await;
        assert_eq!(failed.status, 4);
        assert!(
            !failed
                .error_message
                .starts_with("server returned HTML page")
        );
        let (code, _) = status.split_once(' ').unwrap();
        assert!(failed.error_message.contains(code));
        if headers == CHALLENGE_HEADERS {
            assert!(failed.error_message.contains("bot-challenge"));
        }
        assert!(!h.work_dir.join(failed.file_name).exists());
        h.clear_requests();
        assert_eq!(
            h.engine.manager.save_html_response(&original).await,
            Err("not-an-html-failure".into())
        );
        assert!(h.requests().is_empty());
    }
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
async fn unsuccessful_or_incomplete_html_replays_are_not_published() {
    const HTML_HEADERS: &str = "Content-Type: text/html\r\n";
    let mut h = Harness::new("incomplete_html").await;
    let spec = h.share_spec(None);
    let original = h.engine.manager.create_task(spec).await.unwrap();
    assert_eq!(h.finish(&original).await.status, 4);
    for (status, headers, body) in [
        ("403 Forbidden", HTML_HEADERS, HTML_BODY),
        ("200 OK", HTML_HEADERS, b"".as_slice()),
        (
            "206 Partial Content",
            "Content-Type: text/html\r\nContent-Range: bytes 0-3/100\r\n",
            b"<htm".as_slice(),
        ),
    ] {
        *h.server.share_response.lock().unwrap() = Some((status, headers, body));
        let saved = h
            .engine
            .manager
            .save_html_response(&original)
            .await
            .unwrap();
        let replay = h.finish(&saved).await;
        assert_eq!(replay.status, 4);
        if status == "403 Forbidden" {
            assert!(replay.error_message.contains("403"));
        } else {
            assert!(
                replay
                    .error_message
                    .contains("did not return a complete HTML page")
            );
        }
        assert!(!h.work_dir.join(replay.file_name).exists());
    }
    h.cleanup().await;
}

#[cfg(feature = "plugins")]
#[tokio::test(flavor = "current_thread")]
async fn save_html_after_restart_uses_stored_url_without_resolve_or_prepare() {
    let mut h = Harness::new("save_html_bound_resolver").await;
    let spec = h.share_spec(None);
    let original = h.engine.manager.create_task(spec).await.unwrap();
    let failed = h.finish(&original).await;
    assert_eq!(failed.status, 4);
    assert!(
        failed
            .error_message
            .starts_with("server returned HTML page")
    );

    let mut h = h.restart_engine().await;
    let dir = h.work_dir.join("plugin_src");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let manifest = r#"{
      "identity": "tester@html-replay",
      "name": "HTML Replay Test",
      "version": "1.0.0",
      "permissions": ["intercept"],
      "resolvers": [{ "match": { "urls": ["http://127.0.0.1*"] }, "entry": "resolve.js" }],
      "interceptors": {
        "match": { "urls": ["http://127.0.0.1*"] },
        "entry": "intercept.js",
        "stages": ["prepare"]
      }
    }"#;
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .unwrap();
    let rewrite = format!(
        r#"async () => ({{ url: "http://{}{PLAIN_PATH}" }})"#,
        h.addr
    );
    tokio::fs::write(
        dir.join("resolve.js"),
        format!("globalThis.resolve = {rewrite};"),
    )
    .await
    .unwrap();
    tokio::fs::write(
        dir.join("intercept.js"),
        format!("globalThis.intercept = {rewrite};"),
    )
    .await
    .unwrap();
    let pm = h.engine.manager.plugin_manager().unwrap();
    let id = pm.install_dev(&dir).await.unwrap();
    pm.grant_consent(&id, &["intercept".to_string()], &[])
        .await
        .unwrap();
    pm.set_enabled(&id, true).await.unwrap();
    h.engine.db.set_task_resolver(&original, &id).await.unwrap();
    h.engine
        .db
        .set_task_resolver_item(&original, "fixture")
        .await
        .unwrap();

    let mut resolve_rx = h.engine.manager.take_resolve_rx().unwrap();
    h.clear_requests();
    let saved = h
        .engine
        .manager
        .save_html_response(&original)
        .await
        .unwrap();
    let page = tokio::select! {
        biased;
        _ = resolve_rx.recv() => Err("saving HTML entered resolve"),
        page = h.finish(&saved) => Ok(page),
    }
    .expect("HTML save must complete without resolving");
    assert_eq!(page.status, 3, "{}", page.error_message);
    assert_eq!(h.output(&page).await, HTML_BODY);
    assert_eq!(h.requests(), [format!("GET {HTML_PATH}")]);
    h.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
async fn save_html_replays_post_and_refuses_to_guess_after_restart() {
    let mut h = Harness::new("save_html_post").await;
    let mut spec = h.share_spec(Some("POST"));
    spec.hint_file_size = -1;
    let original = h.engine.manager.create_task(spec).await.unwrap();
    let failed = h.finish(&original).await;
    assert_eq!(failed.status, 4);
    assert!(failed.error_message.contains("for a non-HTML download"));
    assert_eq!(h.requests(), [format!("POST {HTML_PATH}")]);

    h.clear_requests();
    let saved = h
        .engine
        .manager
        .save_html_response(&original)
        .await
        .unwrap();
    let page = h.finish(&saved).await;
    assert_eq!(page.status, 3, "{}", page.error_message);
    assert_eq!(h.output(&page).await, HTML_BODY);
    assert_eq!(h.requests(), [format!("POST {HTML_PATH}")]);

    let mut h = h.restart_engine().await;
    h.clear_requests();
    assert_eq!(
        h.engine
            .manager
            .save_html_response(&original)
            .await
            .unwrap_err(),
        "browser-request-unavailable"
    );
    assert!(h.requests().is_empty());
    assert_eq!(h.engine.db.load_all_tasks().await.unwrap().len(), 2);
    h.cleanup().await;
}
