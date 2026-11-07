//! 全局代理热切换（#924）：运行中、跟随全局代理的 HTTP 任务在全局出口变化后
//! 必须自动重建连接走新代理；任务级固定代理与「出口未变」的重复保存不得打断任务。
//!
//! 两个假代理都是本地 raw-tokio HTTP/1.1 服务：按 absolute-form 请求直接限速应答
//! （不真正转发），并记录请求行。源站用 `.invalid` 主机，只有经代理才可达。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::download_manager::NewTaskSpec;
use fluxdown_engine::proxy_config::{ProxyConfig, ProxyMode, ProxyType};
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::Receiver;

const ORIGIN_HOST: &str = "origin.fluxdown-proxy-switch.invalid";
/// 足够大且限速，保证切换代理时任务仍在下载。
const BODY_LEN: usize = 64 * 1024 * 1024;
const CHUNK: usize = 16 * 1024;
const CHUNK_DELAY: Duration = Duration::from_millis(20);

/// 限速假代理：记录收到的请求行，对任意目标按 Range 应答固定字节。
struct FakeProxy {
    port: u16,
    log: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeProxy {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let shared = log.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let log = shared.clone();
                tokio::spawn(async move {
                    if let Err(error) = conn(stream, log).await {
                        assert!(
                            matches!(
                                error.kind(),
                                std::io::ErrorKind::BrokenPipe
                                    | std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::ConnectionAborted
                                    | std::io::ErrorKind::UnexpectedEof
                            ),
                            "fake proxy connection failed: {error}"
                        );
                    }
                });
            }
        });
        Self { port, log, task }
    }

    /// 发往源站的请求数。
    fn origin_requests(&self) -> usize {
        self.log
            .lock()
            .expect("log lock")
            .iter()
            .filter(|line| line.contains(ORIGIN_HOST))
            .count()
    }

    fn config(&self) -> ProxyConfig {
        ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Http,
            host: "127.0.0.1".into(),
            port: self.port,
            ..ProxyConfig::default()
        }
    }
}

impl Drop for FakeProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn conn(mut stream: TcpStream, log: Arc<Mutex<Vec<String>>>) -> std::io::Result<()> {
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
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    log.lock().expect("log lock").push(request_line.clone());
    let method = request_line.split_whitespace().next().unwrap_or_default();
    let mut range: Option<(usize, Option<usize>)> = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("range")
            && let Some(v) = value.trim().strip_prefix("bytes=")
            && let Some((s, e)) = v.split_once('-')
            && let Ok(start) = s.trim().parse::<usize>()
        {
            range = Some((start, e.trim().parse::<usize>().ok()));
        }
    }
    let total = BODY_LEN;
    let (status, start, end) = match range {
        Some((s, _)) if s >= total => {
            let head = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).await?;
            return stream.shutdown().await;
        }
        Some((s, e)) => (
            "206 Partial Content",
            s,
            e.unwrap_or(total - 1).min(total - 1),
        ),
        None => ("200 OK", 0, total - 1),
    };
    let len = end - start + 1;
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nETag: \"switch-v1\"\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n"
    );
    if range.is_some() {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    if method != "HEAD" {
        let chunk = [0x5au8; CHUNK];
        let mut left = len;
        while left > 0 {
            let n = left.min(CHUNK);
            stream.write_all(&chunk[..n]).await?;
            left -= n;
            tokio::time::sleep(CHUNK_DELAY).await;
        }
    }
    stream.shutdown().await
}

struct Harness {
    engine: Engine,
    done_rx: Receiver<fluxdown_engine::download_manager::TaskDone>,
    work: std::path::PathBuf,
}

impl Harness {
    async fn new(tag: &str, global: ProxyConfig) -> Self {
        let work = std::env::temp_dir().join(format!(
            "fluxdown_proxy_switch_{tag}_{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&work).await.expect("mkdir work");
        let config = EngineConfig {
            max_concurrent: 5,
            speed_limit_bps: 0,
            upload_limit_bps: 0,
            default_save_dir: work.to_string_lossy().to_string(),
            app_data_dir: work.to_string_lossy().to_string(),
            bt_config: BtConfig::default(),
            proxy_config: global,
            user_agent: String::new(),
            data_dir_override: Some(work.clone()),
            database_url: None,
        };
        let mut engine = Engine::new(config, Arc::new(NoopSink), Arc::new(NoopSelection))
            .await
            .expect("engine");
        if let Some(mut rx) = engine.manager.take_progress_rx() {
            tokio::spawn(async move { while rx.recv().await.is_some() {} });
        }
        let done_rx = engine.manager.take_done_rx().expect("done receiver");
        Self {
            engine,
            done_rx,
            work,
        }
    }

    async fn create(&mut self, proxy_url: String) -> String {
        self.engine
            .manager
            .create_task(NewTaskSpec {
                url: format!("http://{ORIGIN_HOST}/file.bin"),
                save_dir: self.work.to_string_lossy().to_string(),
                file_name: "file.bin".to_string(),
                segments: 2,
                proxy_url,
                ..Default::default()
            })
            .await
            .expect("create_task")
    }

    /// 与生产 actor 一样把 writer 退出信号交回 manager（暂停后挂起的恢复在此起飞）。
    async fn pump_done(&mut self, task_id: &str) {
        let done = tokio::time::timeout(Duration::from_secs(10), self.done_rx.recv())
            .await
            .expect("paused downloader should stop")
            .expect("done channel should stay open");
        assert_eq!(done.task_id, task_id);
        self.engine.manager.on_task_done(&done).await;
    }

    /// `window` 内没有任何 writer 退出（任务未被打断）。
    async fn assert_not_interrupted(&mut self, window: Duration) {
        let done = tokio::time::timeout(window, self.done_rx.recv()).await;
        assert!(done.is_err(), "task must keep its running downloader");
    }

    async fn status(&self, task_id: &str) -> i32 {
        self.engine
            .db
            .load_task_by_id(task_id)
            .await
            .expect("load task")
            .map_or(-1, |task| task.status)
    }

    async fn finish(mut self, task_id: &str) {
        self.engine.manager.pause_task(task_id).await;
        if let Ok(Some(done)) =
            tokio::time::timeout(Duration::from_secs(10), self.done_rx.recv()).await
        {
            self.engine.manager.on_task_done(&done).await;
        }
        let work = self.work.clone();
        drop(self);
        if let Err(error) = tokio::fs::remove_dir_all(&work).await {
            eprintln!("proxy switch test directory removal failed: {error}");
        }
    }
}

async fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !ok() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_task_following_global_proxy_reconnects_through_new_proxy() {
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("follow", old.config()).await;
    let task_id = h.create(String::new()).await;
    wait_until("download starts via the old proxy", || {
        old.origin_requests() > 0
    })
    .await;
    assert_eq!(h.status(&task_id).await, 1, "task is downloading");

    // 出口未变的重复保存（设置页整页保存）不打断任务。
    h.engine
        .manager
        .set_proxy_config(old.config())
        .await
        .expect("reapply same proxy");
    h.assert_not_interrupted(Duration::from_secs(2)).await;
    assert_eq!(new.origin_requests(), 0);

    h.engine
        .manager
        .set_proxy_config(new.config())
        .await
        .expect("switch proxy");
    h.pump_done(&task_id).await;
    wait_until("running task reconnects via the new proxy", || {
        new.origin_requests() > 0
    })
    .await;
    assert_eq!(h.status(&task_id).await, 1, "task keeps downloading");
    h.finish(&task_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_task_with_fixed_proxy_ignores_global_switch() {
    let fixed = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("fixed", fixed.config()).await;
    let task_id = h.create(format!("http://127.0.0.1:{}", fixed.port)).await;
    wait_until("download starts via the task proxy", || {
        fixed.origin_requests() > 0
    })
    .await;

    h.engine
        .manager
        .set_proxy_config(new.config())
        .await
        .expect("switch proxy");
    h.assert_not_interrupted(Duration::from_secs(2)).await;
    assert_eq!(
        new.origin_requests(),
        0,
        "fixed-proxy task stays on its proxy"
    );
    assert_eq!(h.status(&task_id).await, 1, "task keeps downloading");
    h.finish(&task_id).await;
}
