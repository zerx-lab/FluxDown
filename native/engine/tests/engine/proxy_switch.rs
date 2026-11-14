//! 全局代理热切换（#924）：运行中、跟随全局代理且能按已有分段续传的 HTTP 任务在全局
//! 出口变化后必须自动重建连接走新代理，并从已下载偏移续传；任务级固定代理、源站不支持
//! Range 的单流任务与「出口未变」的重复保存不得打断任务；Boost 不得因此被取消。
//!
//! 假代理都是本地 raw-tokio HTTP/1.1 服务：按 absolute-form 请求直接限速应答
//! （不真正转发），并记录请求行与 Range 起点。源站用 `.invalid` 主机，只有经代理才可达。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::download_manager::NewTaskSpec;
use fluxdown_engine::events::{EngineEvent, EventSink};
use fluxdown_engine::proxy_config::{ProxyConfig, ProxyMode, ProxyType};
use fluxdown_engine::proxy_rules::ProxyRuleSet;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::Receiver;

const ORIGIN_HOST: &str = "origin.fluxdown-proxy-switch.invalid";
/// 足够大且限速，保证切换代理时任务仍在下载。
const BODY_LEN: usize = 64 * 1024 * 1024;
const CHUNK: usize = 16 * 1024;
const CHUNK_DELAY: Duration = Duration::from_millis(20);

/// 假代理收到的一条发往源站的请求。
#[derive(Clone)]
struct Logged {
    method: String,
    /// `Range: bytes=<start>-` 的起点；无 Range 头为 `None`。
    range_start: Option<usize>,
    headers: std::collections::HashMap<String, String>,
}

/// 限速假代理：记录收到的请求；`ranged = false` 时模拟不支持 Range 的源站
/// （忽略 Range、不发 Accept-Ranges，恒 200 全量）。
struct FakeProxy {
    port: u16,
    log: Arc<Mutex<Vec<Logged>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeProxy {
    async fn start() -> Self {
        Self::spawn(true).await
    }

    async fn start_without_range() -> Self {
        Self::spawn(false).await
    }

    async fn spawn(ranged: bool) -> Self {
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
                    if let Err(error) = conn(stream, log, ranged).await {
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

    /// 发往源站的请求。
    fn requests(&self) -> Vec<Logged> {
        self.log.lock().expect("log lock").clone()
    }

    fn origin_requests(&self) -> usize {
        self.log.lock().expect("log lock").len()
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

async fn conn(
    mut stream: TcpStream,
    log: Arc<Mutex<Vec<Logged>>>,
    ranged: bool,
) -> std::io::Result<()> {
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
    let method = request_line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    let mut range: Option<(usize, Option<usize>)> = None;
    let mut headers = std::collections::HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("range")
            && let Some(v) = value.trim().strip_prefix("bytes=")
            && let Some((s, e)) = v.split_once('-')
            && let Ok(start) = s.trim().parse::<usize>()
        {
            range = Some((start, e.trim().parse::<usize>().ok()));
        }
    }
    if request_line.contains(ORIGIN_HOST) {
        log.lock().expect("log lock").push(Logged {
            method: method.clone(),
            range_start: range.map(|(start, _)| start),
            headers,
        });
    }
    let range = if ranged { range } else { None };
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
        "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nETag: \"switch-v1\"\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n"
    );
    if ranged {
        head.push_str("Accept-Ranges: bytes\r\n");
    }
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

/// 记录引擎发出的 `queue.*` 语义事件（Webhook / 云端推送 / 提示音的共同来源）。
#[derive(Default)]
struct QueueEdgeSink {
    edges: Mutex<Vec<String>>,
}

impl EventSink for QueueEdgeSink {
    fn emit(&self, event: EngineEvent) {
        if let EngineEvent::TaskNotice(notice) = event
            && notice.event.starts_with("queue.")
        {
            self.edges.lock().unwrap().push(notice.event);
        }
    }
}

struct Harness {
    engine: Engine,
    done_rx: Receiver<fluxdown_engine::download_manager::TaskDone>,
    work: std::path::PathBuf,
    sink: Arc<QueueEdgeSink>,
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
        let sink = Arc::new(QueueEdgeSink::default());
        let mut engine = Engine::new(config, sink.clone(), Arc::new(NoopSelection))
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
            sink,
        }
    }

    fn queue_edges(&self) -> Vec<String> {
        self.sink.edges.lock().unwrap().clone()
    }

    async fn create(&mut self, file_name: &str, proxy_url: String) -> String {
        self.create_in(file_name, proxy_url, String::new()).await
    }

    async fn create_in(&mut self, file_name: &str, proxy_url: String, queue_id: String) -> String {
        self.engine
            .manager
            .create_task(NewTaskSpec {
                url: format!("http://{ORIGIN_HOST}/{file_name}"),
                save_dir: self.work.to_string_lossy().to_string(),
                file_name: file_name.to_string(),
                segments: 2,
                proxy_url,
                queue_id,
                ..Default::default()
            })
            .await
            .expect("create_task")
    }

    /// 新建并发上限为 `max_concurrent` 的队列，返回其 ID。
    async fn create_queue(&mut self, name: &str, max_concurrent: i32) -> String {
        self.engine
            .manager
            .create_queue(
                name.to_string(),
                0,
                0,
                max_concurrent,
                String::new(),
                0,
                String::new(),
            )
            .await;
        self.engine
            .db
            .load_all_queues()
            .await
            .expect("load queues")
            .into_iter()
            .find(|queue| queue.name == name)
            .expect("queue created")
            .queue_id
    }

    /// 回灌 writer 退出信号，直到 `task_ids` 每个都收到过一条（到达顺序不定）。
    async fn pump_all(&mut self, task_ids: &[&str]) {
        let mut pending: Vec<&str> = task_ids.to_vec();
        while !pending.is_empty() {
            let done = tokio::time::timeout(Duration::from_secs(10), self.done_rx.recv())
                .await
                .expect("paused downloader should stop")
                .expect("done channel should stay open");
            pending.retain(|task_id| *task_id != done.task_id);
            self.engine.manager.on_task_done(&done).await;
        }
    }

    /// 与生产 actor 一样把 writer 退出信号交回 manager（暂停后挂起的恢复在此起飞），
    /// 直到收到 `task_id` 的那一条。
    async fn pump_done(&mut self, task_id: &str) {
        loop {
            let done = tokio::time::timeout(Duration::from_secs(10), self.done_rx.recv())
                .await
                .expect("paused downloader should stop")
                .expect("done channel should stay open");
            let matched = done.task_id == task_id;
            self.engine.manager.on_task_done(&done).await;
            if matched {
                return;
            }
        }
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

    /// 等到任务的新一代下载器起跑（库写回 downloading）。
    async fn wait_downloading(&self, task_id: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while self.status(task_id).await != 1 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: task {task_id} back to downloading"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 等到任务已有分段行且每段续传起点都 > 0（首段已落库进度），重建连接后不应再有 0 起点。
    async fn wait_segment_progress(&self, task_id: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let segments = self
                .engine
                .db
                .load_segments(task_id)
                .await
                .expect("load segments");
            if !segments.is_empty()
                && segments
                    .iter()
                    .all(|s| s.start_byte + s.downloaded_bytes > 0)
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: segment progress persisted, segments={:?}",
                segments
                    .iter()
                    .map(|s| (s.start_byte, s.end_byte, s.downloaded_bytes))
                    .collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn finish(mut self, task_ids: &[&str]) {
        for task_id in task_ids {
            self.engine.manager.pause_task(task_id).await;
        }
        while let Ok(Some(done)) =
            tokio::time::timeout(Duration::from_secs(3), self.done_rx.recv()).await
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

/// 新代理上的下载请求全部是从已下载偏移起的 Range 续传，没有从 0 重下。
fn assert_resumed_from_offsets(proxy: &FakeProxy) {
    let gets: Vec<Logged> = proxy
        .requests()
        .into_iter()
        .filter(|r| r.method == "GET")
        .collect();
    assert!(!gets.is_empty(), "new proxy must carry the download");
    for request in gets {
        assert!(
            request.range_start.is_some_and(|start| start > 0),
            "reconnect must resume from the downloaded offset, got range start {:?}",
            request.range_start
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_task_following_global_proxy_reconnects_through_new_proxy() {
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("follow", old.config()).await;
    let task_id = h.create("file.bin", String::new()).await;
    h.wait_segment_progress(&task_id).await;
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
    assert_resumed_from_offsets(&new);
    h.finish(&[&task_id]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_task_with_fixed_proxy_ignores_global_switch() {
    let fixed = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("fixed", fixed.config()).await;
    let task_id = h
        .create("file.bin", format!("http://127.0.0.1:{}", fixed.port))
        .await;
    h.wait_segment_progress(&task_id).await;

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
    h.finish(&[&task_id]).await;
}

/// 源站不支持 Range：恢复只能从 0 重下，切换代理不得打断它（保持旧连接）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_task_without_range_support_keeps_old_connection() {
    let old = FakeProxy::start_without_range().await;
    let new = FakeProxy::start_without_range().await;
    let mut h = Harness::new("norange", old.config()).await;
    let task_id = h.create("file.bin", String::new()).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let task = h
            .engine
            .db
            .load_task_by_id(&task_id)
            .await
            .expect("load task")
            .expect("task row");
        if task.status == 1 && task.downloaded_bytes > 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out: single-stream download progress persisted"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    h.engine
        .manager
        .set_proxy_config(new.config())
        .await
        .expect("switch proxy");
    h.assert_not_interrupted(Duration::from_secs(2)).await;
    assert_eq!(
        new.origin_requests(),
        0,
        "non-resumable task must not restart from byte 0 through the new proxy"
    );
    assert_eq!(h.status(&task_id).await, 1, "task keeps downloading");
    h.finish(&[&task_id]).await;
}

/// Boost 中切换代理：优先任务按新出口续传，Boost 保留，让位任务继续暂停。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_switch_keeps_boost() {
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("boost", old.config()).await;
    let boosted = h.create("boosted.bin", String::new()).await;
    let yielded = h.create("yielded.bin", String::new()).await;
    h.wait_segment_progress(&boosted).await;
    h.wait_segment_progress(&yielded).await;

    h.engine
        .manager
        .set_priority_task(boosted.clone())
        .await
        .expect("boost");
    h.pump_done(&yielded).await;
    assert_eq!(h.status(&yielded).await, 2, "boost pauses the other task");

    h.engine
        .manager
        .set_proxy_config(new.config())
        .await
        .expect("switch proxy");
    h.pump_done(&boosted).await;
    wait_until("boosted task reconnects via the new proxy", || {
        new.origin_requests() > 0
    })
    .await;
    assert_resumed_from_offsets(&new);

    let boost_state = h
        .engine
        .db
        .get_config("boost_state")
        .await
        .expect("read boost_state")
        .expect("boost must survive the proxy switch");
    assert!(boost_state.contains(&boosted), "boost target unchanged");
    assert_eq!(
        h.status(&boosted).await,
        1,
        "boosted task keeps downloading"
    );
    assert_eq!(h.status(&yielded).await, 2, "yielded task stays paused");
    h.finish(&[&boosted, &yielded]).await;
}

/// 并发占满且有排队任务时切换代理：重建连接的两个任务在旧 writer 收尾前仍占着自己的
/// 槽位，先收尾的那个恢复后不得把另一个的槽位让给排队任务。
async fn assert_switch_keeps_queue_order(mut h: Harness, new: &FakeProxy, ids: [String; 3]) {
    let [a, b, c] = ids;
    h.wait_segment_progress(&a).await;
    h.wait_segment_progress(&b).await;
    assert_eq!(h.status(&c).await, 0, "third task waits in the queue");

    h.engine
        .manager
        .set_proxy_config(new.config())
        .await
        .expect("switch proxy");
    h.pump_all(&[&a, &b]).await;
    // 新一代下载器起跑后才把库写回 downloading；排队任务若被放进来，被挤掉的那个会停在 0。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let states = (h.status(&a).await, h.status(&b).await, h.status(&c).await);
        if states.0 == 1 && states.1 == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "reconnected tasks keep their slots, got (a, b, c) = {states:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        h.status(&c).await,
        0,
        "the queued task keeps waiting behind the reconnected tasks"
    );
    wait_until("running tasks reconnect via the new proxy", || {
        new.origin_requests() > 0
    })
    .await;
    assert_resumed_from_offsets(new);
    h.finish(&[&a, &b, &c]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_switch_keeps_global_slots_ahead_of_queue() {
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("global_slots", old.config()).await;
    h.engine.manager.set_max_concurrent(2).await;
    let a = h.create("a.bin", String::new()).await;
    let b = h.create("b.bin", String::new()).await;
    let c = h.create("c.bin", String::new()).await;
    assert_switch_keeps_queue_order(h, &new, [a, b, c]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_switch_keeps_queue_slots_ahead_of_queue() {
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("queue_slots", old.config()).await;
    let queue = h.create_queue("proxy-switch", 2).await;
    let a = h.create_in("a.bin", String::new(), queue.clone()).await;
    let b = h.create_in("b.bin", String::new(), queue.clone()).await;
    let c = h.create_in("c.bin", String::new(), queue).await;
    assert_switch_keeps_queue_order(h, &new, [a, b, c]).await;
}

/// 换全局出口 / 改线程数的内部「暂停 → 恢复」不是队列忙闲变化：队列唯一在途任务重建
/// 连接前后不得出现 `queue.drained` 或额外的 `queue.started`（误报的 Webhook / 云端推送 /
/// 「队列完成」提示音）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_emits_no_queue_edges() {
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("queue_edges", old.config()).await;
    let task_id = h.create("file.bin", String::new()).await;
    h.wait_segment_progress(&task_id).await;
    assert_eq!(h.queue_edges(), ["queue.started"]);

    h.engine
        .manager
        .set_proxy_config(new.config())
        .await
        .expect("switch proxy");
    assert_eq!(h.queue_edges(), ["queue.started"], "no drained on pause");
    h.pump_done(&task_id).await;
    wait_until("running task reconnects via the new proxy", || {
        new.origin_requests() > 0
    })
    .await;
    h.wait_downloading(&task_id).await;
    assert_eq!(h.queue_edges(), ["queue.started"], "proxy switch: no edges");

    assert!(
        h.engine
            .manager
            .set_task_segments(&task_id, 4)
            .await
            .expect("set segments")
    );
    assert_eq!(h.queue_edges(), ["queue.started"], "no drained on pause");
    h.pump_done(&task_id).await;
    h.wait_downloading(&task_id).await;
    assert_eq!(
        h.queue_edges(),
        ["queue.started"],
        "segment change: no edges"
    );

    // 用户真的暂停唯一在途任务：队列清空照常发 drained。
    h.engine.manager.pause_task(&task_id).await;
    assert_eq!(h.queue_edges(), ["queue.started", "queue.drained"]);
    h.finish(&[&task_id]).await;
}

/// 站点规则钉住出口的任务（跟随全局、规则首条命中）：全局出口变化不改变它的实际出口，
/// 不得打断。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rule_pinned_task_ignores_global_switch() {
    let pinned = FakeProxy::start().await;
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let raw = format!(
        r#"[{{"id":"r1","enabled":true,"name":"","match":"domain","pattern":"{ORIGIN_HOST}","action":"custom","proxy":"http://127.0.0.1:{}"}}]"#,
        pinned.port
    );
    let rules = Arc::new(ProxyRuleSet::compile(&raw, &ProxyConfig::default()));
    assert_eq!(rules.len(), 1, "the rule compiles");
    let with_rules = |config: ProxyConfig| ProxyConfig {
        rules: Some(Arc::clone(&rules)),
        ..config
    };
    let mut h = Harness::new("rule", with_rules(old.config())).await;
    let task_id = h.create("file.bin", String::new()).await;
    h.wait_segment_progress(&task_id).await;
    assert!(
        pinned.origin_requests() > 0,
        "the rule carries the download"
    );
    assert_eq!(old.origin_requests(), 0, "global proxy is bypassed");

    h.engine
        .manager
        .set_proxy_config(with_rules(new.config()))
        .await
        .expect("switch proxy");
    h.assert_not_interrupted(Duration::from_secs(2)).await;
    assert_eq!(
        new.origin_requests(),
        0,
        "rule-pinned task stays on its proxy"
    );
    assert_eq!(h.status(&task_id).await, 1, "task keeps downloading");
    h.finish(&[&task_id]).await;
}

/// A task override must replace live connections now, not only after a host restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_proxy_edit_reconnects_immediately_and_clear_returns_to_global() {
    let global = FakeProxy::start().await;
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("task_edit", global.config()).await;
    let id = h
        .create("edit.bin", format!("http://127.0.0.1:{}", old.port))
        .await;
    h.wait_segment_progress(&id).await;
    let proxy = format!("http://127.0.0.1:{}", new.port);
    h.engine
        .manager
        .set_task_proxy(&id, &proxy)
        .await
        .expect("edit proxy");
    h.pump_done(&id).await;
    wait_until("task edit reaches new proxy", || new.origin_requests() > 0).await;
    h.wait_downloading(&id).await;
    assert_resumed_from_offsets(&new);
    assert_eq!(
        h.engine
            .db
            .load_task_by_id(&id)
            .await
            .unwrap()
            .unwrap()
            .proxy_url,
        proxy
    );
    h.engine
        .manager
        .set_task_proxy(&id, &proxy)
        .await
        .expect("same proxy");
    h.assert_not_interrupted(Duration::from_millis(250)).await;
    h.engine
        .manager
        .set_task_proxy(&id, "")
        .await
        .expect("follow global");
    h.pump_done(&id).await;
    wait_until("clear override reaches global proxy", || {
        global.origin_requests() > 0
    })
    .await;
    assert_resumed_from_offsets(&global);
    h.finish(&[&id]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_proxy_edit_reconnects_non_resumable_transfer_too() {
    let old = FakeProxy::start_without_range().await;
    let new = FakeProxy::start_without_range().await;
    let mut h = Harness::new("task_edit_single", old.config()).await;
    let id = h.create("single.bin", String::new()).await;
    wait_until("old transfer started", || {
        old.requests().iter().any(|r| r.method == "GET")
    })
    .await;
    h.engine
        .manager
        .set_task_proxy(&id, &format!("http://127.0.0.1:{}", new.port))
        .await
        .expect("switch non-resumable proxy");
    h.pump_done(&id).await;
    wait_until("single-stream task uses new proxy", || {
        new.requests().iter().any(|r| r.method == "GET")
    })
    .await;
    h.finish(&[&id]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_proxy_edit_preserves_paused_and_queued_states() {
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("task_edit_states", old.config()).await;
    let queue = h.create_queue("limited", 1).await;
    let first = h.create_in("first.bin", String::new(), queue.clone()).await;
    h.wait_segment_progress(&first).await;
    let queued = h.create_in("queued.bin", String::new(), queue).await;
    let proxy = format!("http://127.0.0.1:{}", new.port);
    assert_eq!(h.status(&queued).await, 0);
    h.engine
        .manager
        .set_task_proxy(&queued, &proxy)
        .await
        .expect("queued proxy");
    assert_eq!(h.status(&queued).await, 0);
    assert_eq!(new.origin_requests(), 0);
    h.engine.manager.pause_task(&first).await;
    h.pump_done(&first).await;
    wait_until("queued task starts on edited proxy", || {
        new.origin_requests() > 0
    })
    .await;
    h.engine
        .manager
        .set_task_proxy(&first, &proxy)
        .await
        .expect("paused proxy");
    assert_eq!(h.status(&first).await, 2);
    for bad in [
        "http://host:bad",
        "http://host:0",
        "socks5://host",
        "http://host:8080/path",
        "file://host:8080",
    ] {
        assert_eq!(
            h.engine.manager.set_task_proxy(&first, bad).await,
            Err("invalid-proxy".into())
        );
    }
    assert_eq!(
        h.engine
            .db
            .load_task_by_id(&first)
            .await
            .unwrap()
            .unwrap()
            .proxy_url,
        proxy
    );
    h.finish(&[&first, &queued]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_proxy_edit_retries_failed_task_without_manual_resume() {
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("task_edit_failed", old.config()).await;
    let id = h.create("failed.bin", String::new()).await;
    h.wait_segment_progress(&id).await;
    h.engine.manager.pause_task(&id).await;
    h.pump_done(&id).await;
    h.engine
        .db
        .update_task_status(&id, 4, "old proxy unavailable")
        .await
        .unwrap();
    h.engine
        .manager
        .set_task_proxy(&id, &format!("http://127.0.0.1:{}", new.port))
        .await
        .expect("retry using new proxy");
    wait_until("failed task immediately retries on new proxy", || {
        new.origin_requests() > 0
    })
    .await;
    h.wait_downloading(&id).await;
    assert_resumed_from_offsets(&new);
    h.finish(&[&id]).await;
}

impl Harness {
    async fn apply_edit(
        &mut self,
        patch: fluxdown_engine::task_edit::EditTask,
    ) -> Result<(), String> {
        use fluxdown_engine::download_manager::TaskEditStep;
        let id = patch.task_id.clone();
        let mut step = self.engine.manager.begin_task_edit(patch).await?;
        loop {
            step = match step {
                TaskEditStep::Applied => return Ok(()),
                TaskEditStep::Waiting(session) => {
                    self.pump_done(&id).await;
                    self.engine.manager.continue_task_edit(session).await?
                }
                TaskEditStep::Started(job) => {
                    return self.engine.manager.finish_task_edit(job.await).await;
                }
            };
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_edit_relocates_and_reconnects_with_protected_credentials() {
    use fluxdown_engine::task_edit::{EditTask, snapshot};
    let old = FakeProxy::start().await;
    let new = FakeProxy::start().await;
    let mut h = Harness::new("unified_edit", old.config()).await;
    let id = h.create("original.bin", String::new()).await;
    h.wait_segment_progress(&id).await;
    let before = snapshot(&h.engine.db, &id).await.unwrap();
    let site = fluxdown_engine::auth::site_key(&format!("http://{ORIGIN_HOST}/")).unwrap();
    let reference = format!("editorref::{site}");
    fluxdown_engine::auth::save(
        &h.engine.db,
        &fluxdown_engine::auth::AuthProfile {
            auth_ref: reference.clone(),
            plugin_id: "editorref".into(),
            site,
            kind: "headers".into(),
            headers: std::collections::HashMap::from([(
                "X-Api-Key".into(),
                "referenced-key".into(),
            )]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let destination = h.work.join("目录 with spaces");
    h.apply_edit(EditTask {
        task_id: id.clone(),
        expected_revision: before.revision,
        save_dir: Some(destination.to_string_lossy().into_owned()),
        file_name: Some("renamed 文档.bin".into()),
        proxy_url: Some(format!("http://127.0.0.1:{}", new.port)),
        user_agent: Some("TaskEditor/1".into()),
        auth_ref: Some(reference),
        cookies: Some("session=private-value".into()),
        headers: Some(std::collections::HashMap::from([
            ("Authorization".into(), "Bearer private-token".into()),
            ("X-Task".into(), "edited".into()),
        ])),
        category: Some("archive".into()),
        tags: Some(vec!["work".into(), "任务".into()]),
        download_limit_bps: Some(512 * 1024),
        ..Default::default()
    })
    .await
    .unwrap();
    wait_until("new requests with new credentials", || {
        new.requests().iter().any(|r| {
            r.method == "GET"
                && r.headers
                    .get("x-api-key")
                    .is_some_and(|v| v == "referenced-key")
                && r.headers
                    .get("user-agent")
                    .is_some_and(|v| v == "TaskEditor/1")
                && r.headers
                    .get("cookie")
                    .is_some_and(|v| v == "session=private-value")
                && r.headers
                    .get("authorization")
                    .is_some_and(|v| v == "Bearer private-token")
                && r.headers.get("x-task").is_some_and(|v| v == "edited")
        })
    })
    .await;
    assert_resumed_from_offsets(&new);
    let after = snapshot(&h.engine.db, &id).await.unwrap();
    assert!(after.cookies_configured);
    assert_eq!(
        after.headers.get("authorization").map(String::as_str),
        Some("********")
    );
    assert_eq!(
        (after.file_name.as_str(), after.properties.category.as_str()),
        ("renamed 文档.bin", "archive")
    );
    assert_eq!(after.properties.tags, ["work", "任务"]);
    assert_eq!(after.properties.download_limit_bps, 512 * 1024);
    let wire = serde_json::to_string(&after).unwrap();
    assert!(!wire.contains("private-value") && !wire.contains("private-token"));
    assert!(
        !h.work
            .join(format!(
                "original.bin{}",
                fluxdown_engine::downloader::TEMP_EXT
            ))
            .exists()
    );
    assert!(
        destination
            .join(format!(
                "renamed 文档.bin{}",
                fluxdown_engine::downloader::TEMP_EXT
            ))
            .exists()
    );
    // An unrelated edit must not erase write-only credentials represented by masks.
    h.apply_edit(EditTask {
        task_id: id.clone(),
        expected_revision: after.revision,
        headers: Some(after.headers),
        tags: Some(vec!["retained".into()]),
        ..Default::default()
    })
    .await
    .unwrap();
    h.assert_not_interrupted(Duration::from_millis(100)).await;
    assert!(
        snapshot(&h.engine.db, &id)
            .await
            .unwrap()
            .cookies_configured
    );
    h.finish(&[&id]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_edit_validation_and_stale_revision_leave_running_task_unchanged() {
    use fluxdown_engine::task_edit::{EditTask, snapshot};
    let proxy = FakeProxy::start().await;
    let mut h = Harness::new("unified_invalid", proxy.config()).await;
    let id = h.create("keep.bin", String::new()).await;
    h.wait_segment_progress(&id).await;
    let before = snapshot(&h.engine.db, &id).await.unwrap();
    let destination = h.work.join("must-not-be-created");
    let rejected = h
        .apply_edit(EditTask {
            task_id: id.clone(),
            expected_revision: before.revision.clone(),
            save_dir: Some(destination.to_string_lossy().into_owned()),
            category: Some("changed".into()),
            headers: Some(std::collections::HashMap::from([(
                "X-Header".into(),
                "bad\r\ninjection".into(),
            )])),
            ..Default::default()
        })
        .await;
    assert!(rejected.is_err());
    assert!(!destination.exists());
    let unchanged = snapshot(&h.engine.db, &id).await.unwrap();
    assert_eq!(unchanged.revision, before.revision);
    h.assert_not_interrupted(Duration::from_millis(100)).await;
    h.apply_edit(EditTask {
        task_id: id.clone(),
        expected_revision: before.revision.clone(),
        tags: Some(vec!["first".into()]),
        ..Default::default()
    })
    .await
    .unwrap();
    let stale = h
        .apply_edit(EditTask {
            task_id: id.clone(),
            expected_revision: before.revision,
            file_name: Some("stale.bin".into()),
            ..Default::default()
        })
        .await;
    assert_eq!(stale, Err("edit-conflict".into()));
    assert_eq!(
        snapshot(&h.engine.db, &id).await.unwrap().file_name,
        "keep.bin"
    );
    h.assert_not_interrupted(Duration::from_millis(100)).await;
    h.finish(&[&id]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_edit_future_start_pauses_and_due_tick_resumes_once() {
    use fluxdown_engine::task_edit::{EditTask, snapshot};
    let proxy = FakeProxy::start().await;
    let mut h = Harness::new("unified_schedule", proxy.config()).await;
    let id = h.create("scheduled.bin", String::new()).await;
    h.wait_segment_progress(&id).await;
    let before = snapshot(&h.engine.db, &id).await.unwrap();
    h.apply_edit(EditTask {
        task_id: id.clone(),
        expected_revision: before.revision,
        scheduled_at: Some(chrono::Utc::now().timestamp() + 3600),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(h.status(&id).await, 2);
    let paused = snapshot(&h.engine.db, &id).await.unwrap();
    assert!(paused.properties.scheduled_at > chrono::Utc::now().timestamp());
    h.apply_edit(EditTask {
        task_id: id.clone(),
        expected_revision: paused.revision,
        scheduled_at: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(
        h.status(&id).await,
        2,
        "removing a schedule is not a resume command"
    );
    let cleared = snapshot(&h.engine.db, &id).await.unwrap();
    h.apply_edit(EditTask {
        task_id: id.clone(),
        expected_revision: cleared.revision,
        scheduled_at: Some(chrono::Utc::now().timestamp() - 1),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(h.status(&id).await, 2);
    h.engine.manager.tick_task_schedules().await;
    h.wait_downloading(&id).await;
    assert_eq!(
        snapshot(&h.engine.db, &id)
            .await
            .unwrap()
            .properties
            .scheduled_at,
        0
    );
    h.engine.manager.tick_task_schedules().await;
    h.assert_not_interrupted(Duration::from_millis(100)).await;
    h.finish(&[&id]).await;
}

#[cfg(feature = "bt")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_edit_bt_tree_is_atomic_on_conflict_database_failure_and_cancel() {
    use fluxdown_engine::{
        download_manager::TaskEditStep,
        task_edit::{EditTask, snapshot},
    };
    let mut h = Harness::new("unified_bt_tree", ProxyConfig::default()).await;
    let id = h
        .engine
        .manager
        .create_task(NewTaskSpec {
            url: "magnet:?xt=urn:btih:0123456789012345678901234567890123456789".into(),
            file_name: "种子文件夹".into(),
            save_dir: h.work.to_string_lossy().into_owned(),
            start_paused: true,
            ..Default::default()
        })
        .await
        .unwrap();
    h.engine.db.update_task_status(&id, 3, "").await.unwrap();
    let source = h.work.join("种子文件夹");
    tokio::fs::create_dir_all(source.join("sub")).await.unwrap();
    tokio::fs::write(source.join("sub/资料.txt"), b"owned payload")
        .await
        .unwrap();
    let before = snapshot(&h.engine.db, &id).await.unwrap();
    let destination = h.work.join("new location");
    tokio::fs::create_dir_all(destination.join("种子文件夹"))
        .await
        .unwrap();
    tokio::fs::write(destination.join("种子文件夹/keep.txt"), b"unrelated")
        .await
        .unwrap();
    let patch = || EditTask {
        task_id: id.clone(),
        expected_revision: before.revision.clone(),
        save_dir: Some(destination.to_string_lossy().into_owned()),
        tags: Some(vec!["moved".into()]),
        download_limit_bps: Some(48_000),
        ..Default::default()
    };
    assert_eq!(h.apply_edit(patch()).await, Err("target-exists".into()));
    assert_eq!(
        tokio::fs::read(destination.join("种子文件夹/keep.txt"))
            .await
            .unwrap(),
        b"unrelated"
    );
    assert_eq!(
        snapshot(&h.engine.db, &id).await.unwrap().revision,
        before.revision
    );
    tokio::fs::remove_dir_all(&destination).await.unwrap();
    let pool = sqlx::AnyPool::connect(&format!(
        "sqlite://{}",
        h.work.join("flux_down.db").display()
    ))
    .await
    .unwrap();
    sqlx::query("CREATE TRIGGER reject_task_edit BEFORE UPDATE OF edit_properties ON tasks BEGIN SELECT RAISE(ABORT, 'injected edit failure'); END")
        .execute(&pool).await.unwrap();
    assert!(
        h.apply_edit(patch())
            .await
            .unwrap_err()
            .contains("injected edit failure")
    );
    assert_eq!(
        tokio::fs::read(source.join("sub/资料.txt")).await.unwrap(),
        b"owned payload"
    );
    assert!(!destination.join("种子文件夹").exists());
    assert_eq!(
        snapshot(&h.engine.db, &id).await.unwrap().revision,
        before.revision
    );
    sqlx::query("DROP TRIGGER reject_task_edit")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let TaskEditStep::Started(job) = h.engine.manager.begin_task_edit(patch()).await.unwrap()
    else {
        panic!("paused BT directory save must produce off-actor file work");
    };
    h.engine.manager.pause_task(&id).await;
    assert!(h.engine.manager.finish_task_edit(job.await).await.is_err());
    assert_eq!(
        tokio::fs::read(source.join("sub/资料.txt")).await.unwrap(),
        b"owned payload"
    );
    assert_eq!(
        snapshot(&h.engine.db, &id).await.unwrap().revision,
        before.revision
    );
    h.apply_edit(patch()).await.unwrap();
    assert!(!source.exists());
    assert_eq!(
        tokio::fs::read(destination.join("种子文件夹/sub/资料.txt"))
            .await
            .unwrap(),
        b"owned payload"
    );
    let task = h.engine.db.load_task_by_id(&id).await.unwrap().unwrap();
    assert_eq!(task.bt_download_limit_bps, 48_000);
    assert_eq!(task.properties.tags, ["moved"]);
    h.finish(&[&id]).await;
}
