//! 本机主机端到端：经 FFI 对外的公开 API（`FluxCore` → `open_local` → `HostSession`）
//! 驱动真实进程内 daemon + 嵌入式 agent，下载 loopback HTTP 服务器上的文件。
//! 全程离线：服务器是测试内的 tokio `TcpListener`，引擎默认不做 CDN / 多网卡探测。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fluxdown_mobile::{
    CreateTaskRequestDto, ErrorCodeDto, FluxCore, FluxError, HostEventDto, HostSession,
    HostSignalDto, HostSnapshotDto, ImportBatchDto, ImportItemDto, ImportUpdateDto,
    LocalHostConfig, TaskDto,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const STATUS_DOWNLOADING: i32 = 1;
const STATUS_PAUSED: i32 = 2;
const STATUS_COMPLETED: i32 = 3;

/// 单个等待步骤的上限（含 daemon 首次启动的数据库迁移）。
const STEP: Duration = Duration::from_secs(60);

const FAST_LEN: u64 = 2 * 1024 * 1024;
const SLOW_LEN: u64 = 8 * 1024 * 1024;
/// 慢资源：每个响应块之间的间隔，保证暂停 / 恢复时下载一定还没结束。
const SLOW_CHUNK: usize = 32 * 1024;
const SLOW_DELAY: Duration = Duration::from_millis(40);

/// 偏移 → 字节：不重复的伪随机内容，截断 / 错位都会被逐字节比对发现。
fn byte_at(offset: u64) -> u8 {
    offset
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(offset >> 7)
        .to_le_bytes()[1]
}

fn body(len: u64) -> Vec<u8> {
    (0..len).map(byte_at).collect()
}

/// 支持 HEAD / GET / `Range` 的 loopback 服务器。`/fast.bin` 全速，`/slow.bin` 限速。
struct HttpServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl HttpServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind http");
        let addr = listener.local_addr().expect("http addr");
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    if let Err(error) = serve(stream).await {
                        // 客户端中途断开（暂停 / 删除）属正常。
                        eprintln!("test http connection ended: {error}");
                    }
                });
            }
        });
        Self { addr, task }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}/{path}", self.addr)
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_head(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        if buffer.len() > 16 * 1024 {
            return Err(std::io::Error::other("request head too large"));
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::other("connection closed in request head"));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// `bytes=a-b` / `bytes=a-` → 闭区间 `[a, b]`。
fn parse_range(head: &str, len: u64) -> Option<(u64, u64)> {
    let value = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("range")
            .then(|| value.trim().to_owned())
    })?;
    let spec = value.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end = match end.trim() {
        "" => len - 1,
        end => end.parse::<u64>().ok()?.min(len - 1),
    };
    (start <= end).then_some((start, end))
}

async fn serve(mut stream: TcpStream) -> std::io::Result<()> {
    let head = read_head(&mut stream).await?;
    let request_line = head.lines().next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let (len, throttled) = match path {
        "/fast.bin" => (FAST_LEN, false),
        "/slow.bin" => (SLOW_LEN, true),
        _ => {
            return stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        }
    };
    let range = parse_range(&head, len);
    let (status, start, end) = match range {
        Some((start, end)) => ("206 Partial Content", start, end),
        None => ("200 OK", 0, len - 1),
    };
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n",
        end - start + 1
    );
    if range.is_some() {
        response.push_str(&format!("Content-Range: bytes {start}-{end}/{len}\r\n"));
    }
    response.push_str("\r\n");
    stream.write_all(response.as_bytes()).await?;
    if method.eq_ignore_ascii_case("HEAD") {
        return Ok(());
    }
    let mut offset = start;
    while offset <= end {
        let chunk_len = if throttled {
            SLOW_CHUNK as u64
        } else {
            64 * 1024
        };
        let upto = (offset + chunk_len - 1).min(end);
        let chunk: Vec<u8> = (offset..=upto).map(byte_at).collect();
        stream.write_all(&chunk).await?;
        offset = upto + 1;
        if throttled {
            tokio::time::sleep(SLOW_DELAY).await;
        }
    }
    stream.shutdown().await
}

struct Dirs {
    root: PathBuf,
}

impl Dirs {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "fluxdown_mobile_e2e_{label}_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        Self { root }
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn save(&self) -> PathBuf {
        self.root.join("downloads")
    }

    fn config(&self) -> LocalHostConfig {
        LocalHostConfig {
            data_dir: self.data().display().to_string(),
            external_log_dir: None,
            save_dir: self.save().display().to_string(),
            platform: "test".to_owned(),
            device_name: None,
        }
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!("test cleanup failed for {}: {error}", self.root.display());
        }
    }
}

fn create_request(url: String) -> CreateTaskRequestDto {
    CreateTaskRequestDto {
        url,
        file_name: String::new(),
        save_dir: String::new(),
        segments: 0,
        queue_id: String::new(),
        start_paused: false,
        cookies: String::new(),
        referrer: String::new(),
        user_agent: String::new(),
        proxy_url: String::new(),
        checksum: String::new(),
        ignore_tls_errors: false,
        headers: HashMap::new(),
        http_user: String::new(),
        http_password: String::new(),
        save_site_auth: false,
        unattended: false,
        offered_save_dir: None,
    }
}

async fn next(session: &HostSession) -> HostSignalDto {
    tokio::time::timeout(STEP, session.next_signal())
        .await
        .expect("timed out waiting for a host signal")
        .expect("host session closed unexpectedly")
}

/// 首个任务表完整的快照：快照头（`tasks_loading`）先到，随后是载入完成的完整快照。
async fn first_snapshot(session: &HostSession) -> HostSnapshotDto {
    loop {
        match next(session).await {
            HostSignalDto::Snapshot { snapshot } if snapshot.tasks_loading => {
                assert!(
                    snapshot.tasks.is_empty(),
                    "a loading snapshot carries no half table"
                );
            }
            HostSignalDto::Snapshot { snapshot } => return snapshot,
            other => panic!("expected a Snapshot while bootstrapping, got {other:?}"),
        }
    }
}

/// 状态观察：任何会带来任务状态的信号（事件 / 重同步快照）都折叠成 `(status, file, bytes)`。
fn observed(signal: &HostSignalDto, task_id: &str) -> Option<TaskObservation> {
    match signal {
        HostSignalDto::Snapshot { snapshot } => snapshot
            .tasks
            .iter()
            .find(|task| task.task_id == task_id)
            .map(TaskObservation::from_task),
        HostSignalDto::Event {
            event: HostEventDto::TaskChanged { task },
        } if task.task_id == task_id => Some(TaskObservation::from_task(task)),
        HostSignalDto::Event {
            event:
                HostEventDto::TaskProgress {
                    task_id: id,
                    status,
                    downloaded_bytes,
                    total_bytes,
                    error_message,
                    ..
                },
        } if id == task_id => Some(TaskObservation {
            status: *status,
            downloaded: *downloaded_bytes,
            total: *total_bytes,
            error: error_message.clone(),
        }),
        HostSignalDto::Fatal { error } => panic!("session went fatal: {error:?}"),
        _ => None,
    }
}

#[derive(Debug)]
struct TaskObservation {
    status: i32,
    downloaded: i64,
    total: i64,
    error: String,
}

impl TaskObservation {
    fn from_task(task: &TaskDto) -> Self {
        Self {
            status: task.status,
            downloaded: task.downloaded_bytes,
            total: task.total_bytes,
            error: task.error_message.clone(),
        }
    }
}

/// 读信号直到该任务满足 `done`，期间不允许出现失败状态（status 4）。
async fn wait_task<F>(session: &HostSession, task_id: &str, what: &str, done: F) -> TaskObservation
where
    F: Fn(&TaskObservation) -> bool,
{
    let result = tokio::time::timeout(STEP, async {
        loop {
            let signal = tokio::time::timeout(STEP, session.next_signal())
                .await
                .unwrap_or_else(|_| panic!("no signal while waiting for {what}"))
                .unwrap_or_else(|| panic!("session closed while waiting for {what}"));
            let Some(seen) = observed(&signal, task_id) else {
                continue;
            };
            assert_ne!(
                seen.status, 4,
                "task failed while waiting for {what}: {} ({seen:?})",
                seen.error
            );
            if done(&seen) {
                return seen;
            }
        }
    })
    .await;
    result.unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

fn assert_file(path: &Path, expected: &[u8]) {
    let actual = std::fs::read(path).unwrap_or_else(|error| {
        panic!("downloaded file {} unreadable: {error}", path.display());
    });
    assert_eq!(actual.len(), expected.len(), "downloaded size");
    assert!(
        actual == expected,
        "downloaded bytes differ from the served body"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_host_downloads_then_pauses_resumes_and_deletes() {
    let dirs = Dirs::new("flow");
    let server = HttpServer::start().await;
    let core = FluxCore::new().expect("core");

    let session = core.open_local(dirs.config()).await.expect("open local");

    // 首个信号 = 快照：daemon 已连上、内置分类在。
    let snapshot = first_snapshot(&session).await;
    assert!(snapshot.daemon_connected, "daemon must be connected");
    assert!(
        !snapshot.categories.is_empty(),
        "categories must be present"
    );
    assert!(snapshot.tasks.is_empty(), "fresh data dir has no tasks");
    assert!(
        snapshot
            .config
            .get("default_save_dir")
            .is_some_and(|dir| { Path::new(dir) == dirs.save().as_path() }),
        "default_save_dir is seeded from LocalHostConfig.save_dir: {:?}",
        snapshot.config.get("default_save_dir")
    );

    // 快速任务：完整下载，字节逐一相等。
    let fast_id = session
        .create_task(create_request(server.url("fast.bin")))
        .await
        .expect("create fast task");
    assert!(!fast_id.is_empty());
    let done = wait_task(&session, &fast_id, "fast task completion", |seen| {
        seen.status == STATUS_COMPLETED
    })
    .await;
    assert_eq!(done.total, i64::try_from(FAST_LEN).expect("len"));
    assert_file(&dirs.save().join("fast.bin"), &body(FAST_LEN));

    // 慢任务：下载中 → 暂停 → 恢复 → 删除（连同文件）。
    let slow_id = session
        .create_task(create_request(server.url("slow.bin")))
        .await
        .expect("create slow task");
    let running = wait_task(
        &session,
        &slow_id,
        "slow task to start downloading",
        |seen| seen.status == STATUS_DOWNLOADING && seen.downloaded > 0,
    )
    .await;
    assert!(running.downloaded < i64::try_from(SLOW_LEN).expect("len"));

    session.pause(slow_id.clone()).await.expect("pause");
    wait_task(&session, &slow_id, "slow task to pause", |seen| {
        seen.status == STATUS_PAUSED
    })
    .await;

    session.resume(slow_id.clone()).await.expect("resume");
    wait_task(&session, &slow_id, "slow task to resume", |seen| {
        seen.status == STATUS_DOWNLOADING
    })
    .await;

    session
        .delete(slow_id.clone(), true)
        .await
        .expect("delete slow task");
    tokio::time::timeout(STEP, async {
        loop {
            match session.next_signal().await {
                Some(HostSignalDto::Event {
                    event: HostEventDto::TaskDeleted { task_id },
                }) if task_id == slow_id => return,
                Some(HostSignalDto::Event {
                    event:
                        HostEventDto::TaskProgress {
                            task_id,
                            status: 4,
                            error_message,
                            ..
                        },
                }) if task_id == slow_id && error_message == "deleted" => return,
                Some(HostSignalDto::Snapshot { snapshot })
                    if !snapshot.tasks_loading
                        && snapshot.tasks.iter().all(|task| task.task_id != slow_id) =>
                {
                    return;
                }
                Some(HostSignalDto::Fatal { error }) => panic!("session went fatal: {error:?}"),
                Some(_) => {}
                None => panic!("session closed while waiting for the delete"),
            }
        }
    })
    .await
    .expect("timed out waiting for the slow task to be deleted");

    // 配置写入：乐观并发。正确的 revision 生效并回推；过期的 revision → Conflict。
    let patch = HashMap::from([("max_auto_retries".to_owned(), "5".to_owned())]);
    session
        .patch_config(snapshot.config_revision, patch.clone())
        .await
        .expect("patch config");
    let revision = tokio::time::timeout(STEP, async {
        loop {
            match session.next_signal().await {
                Some(HostSignalDto::Event {
                    event: HostEventDto::ConfigChanged { values, revision },
                }) if values.get("max_auto_retries").is_some_and(|v| v == "5") => {
                    return revision;
                }
                Some(HostSignalDto::Snapshot { snapshot })
                    if snapshot
                        .config
                        .get("max_auto_retries")
                        .is_some_and(|v| v == "5") =>
                {
                    return snapshot.config_revision;
                }
                Some(HostSignalDto::Fatal { error }) => panic!("session went fatal: {error:?}"),
                Some(_) => {}
                None => panic!("session closed while waiting for the config change"),
            }
        }
    })
    .await
    .expect("timed out waiting for the config change");
    assert!(revision > snapshot.config_revision, "revision must advance");
    match session.patch_config(snapshot.config_revision, patch).await {
        Err(FluxError::Rpc {
            code: ErrorCodeDto::Conflict,
            ..
        }) => {}
        other => panic!("a stale config revision must be a Conflict, got {other:?}"),
    }

    // 已完成的任务落库：整体重启后快照仍带它，且文件还在。
    session.disconnect();
    core.shutdown_local().await;
    let reopened = core
        .open_local(dirs.config())
        .await
        .expect("reopen after shutdown");
    let snapshot = first_snapshot(&reopened).await;
    assert!(snapshot.daemon_connected);
    let persisted = snapshot
        .tasks
        .iter()
        .find(|task| task.task_id == fast_id)
        .expect("completed task survives a restart");
    assert_eq!(persisted.status, STATUS_COMPLETED);
    assert!(
        snapshot.tasks.iter().all(|task| task.task_id != slow_id),
        "deleted task stays deleted"
    );
    assert!(
        !dirs.save().join("slow.bin").exists(),
        "delete_files removes the partial file"
    );
    assert_file(&dirs.save().join("fast.bin"), &body(FAST_LEN));

    reopened.disconnect();
    core.shutdown_local().await;
}

/// 真实本机主机：`start_import` 跨两块（256 + 44）经 `daemon.task.createMany` 建任务，单条非法校验值被逐条拒绝，
/// 其余全部落库并经事件到达 UI。任务以「暂停」创建，不触发下载。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_host_imports_chunked_batches_with_per_item_rejection() {
    let dirs = Dirs::new("import");
    let server = HttpServer::start().await;
    let core = FluxCore::new().expect("core");
    let session = core.open_local(dirs.config()).await.expect("open local");
    first_snapshot(&session).await;

    let mut common = create_request(String::new());
    common.start_paused = true;
    let items: Vec<ImportItemDto> = (0..300)
        .map(|index| ImportItemDto {
            url: server.url(&format!("import-{index}.bin")),
            file_name: String::new(),
            // 第 7 条的校验值非法：只拒绝它，不影响同块其余条目。
            checksum: if index == 7 {
                "not-a-checksum".to_owned()
            } else {
                String::new()
            },
        })
        .collect();
    let handle = session.start_import(vec![ImportBatchDto { common, items }]);
    let summary = tokio::time::timeout(STEP, async {
        loop {
            match handle.next_update().await.expect("update before Finished") {
                ImportUpdateDto::Progress { progress } => assert_eq!(progress.total, 300),
                ImportUpdateDto::Finished { summary } => break summary,
            }
        }
    })
    .await
    .expect("import finished in time");
    assert_eq!(summary.total, 300);
    assert_eq!((summary.succeeded, summary.failed), (299, 1), "{summary:?}");
    assert_eq!(summary.not_submitted, 0);
    assert!(!summary.cancelled);
    let error = summary.first_error.expect("rejection is reported");
    assert_eq!(error.code, ErrorCodeDto::InvalidArgument);
    assert_eq!(error.message, "checksum");
    assert_eq!(
        summary.unfinished,
        [7],
        "only the rejected item is left over"
    );
    assert!(handle.next_update().await.is_none());

    // 事件 / 重同步快照最终覆盖全部 299 个新任务。
    let mut seen = std::collections::HashSet::new();
    tokio::time::timeout(STEP, async {
        while seen.len() < 299 {
            match next(&session).await {
                HostSignalDto::Snapshot { snapshot } => {
                    seen.extend(snapshot.tasks.into_iter().map(|task| task.task_id));
                }
                HostSignalDto::Event {
                    event: HostEventDto::TaskChanged { task },
                } => {
                    seen.insert(task.task_id);
                }
                HostSignalDto::Fatal { error } => panic!("session went fatal: {error:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("all created tasks reach the UI");
    assert_eq!(seen.len(), 299);

    session.disconnect();
    core.shutdown_local().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_local_releases_the_data_dir_for_another_core() {
    let dirs = Dirs::new("locks");
    let first = FluxCore::new().expect("first core");
    let second = FluxCore::new().expect("second core");

    let session = first.open_local(dirs.config()).await.expect("open local");
    assert!(first_snapshot(&session).await.daemon_connected);

    // 同一数据目录被占用时，另一个核心必须报错，而不是共享 / 破坏数据库。
    match second.open_local(dirs.config()).await {
        Err(FluxError::Transport { detail }) => {
            eprintln!("second open refused as expected: {detail}");
        }
        Err(other) => panic!("expected a Transport error for a held data dir, got {other:?}"),
        Ok(_) => panic!("a second core must not open a data dir that is in use"),
    }

    session.disconnect();
    assert!(
        tokio::time::timeout(STEP, session.next_signal())
            .await
            .expect("next_signal after disconnect")
            .is_none(),
        "a disconnected session ends its signal stream"
    );
    tokio::time::timeout(STEP, first.shutdown_local())
        .await
        .expect("shutdown_local timeout");

    // 锁已释放：另一个核心能接手同一目录。
    let taken_over = second
        .open_local(dirs.config())
        .await
        .expect("open after the first host shut down");
    let snapshot = first_snapshot(&taken_over).await;
    assert!(snapshot.daemon_connected);
    assert!(!snapshot.categories.is_empty());

    taken_over.disconnect();
    tokio::time::timeout(STEP, second.shutdown_local())
        .await
        .expect("shutdown_local timeout");

    // 空的 shutdown 是幂等的。
    second.shutdown_local().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_local_rejects_an_empty_data_dir() {
    let core = FluxCore::new().expect("core");
    let result = core
        .open_local(LocalHostConfig {
            data_dir: "   ".to_owned(),
            external_log_dir: None,
            save_dir: String::new(),
            platform: "test".to_owned(),
            device_name: None,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(FluxError::Rpc {
                code: ErrorCodeDto::InvalidArgument,
                ..
            })
        ),
        "empty data_dir must be refused, got {:?}",
        result.map(|_| ())
    );
}
