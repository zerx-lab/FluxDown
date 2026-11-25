//! 「文件已存在」询问流程的行为测试：假宿主选择器驱动真实的 manager 序幕、
//! 挂起/答复/重入状态机。下载目标指向只接受不应答的本机端口，任务不会真正落盘。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex as AsyncMutex, mpsc};

use super::*;
use crate::events::{EngineEvent, EventSink};
use crate::file_exists::{ExistsDecision, OverwritePolicy};
use crate::model::{BtFileEntry, HlsQualityOption, ResolveVariantOption};
use crate::selection::{BtFileChoice, FileConflict, FileExistsChoice, SelectionOutcome};

struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: EngineEvent) {}
}

/// 可编排答复的假宿主：`can_prompt` 可切换，答复经 channel 注入，询问记录可查。
struct FakeSelector {
    can_prompt: AtomicBool,
    answers: AsyncMutex<mpsc::UnboundedReceiver<FileExistsChoice>>,
    asked: std::sync::Mutex<Vec<FileConflict>>,
}

impl FakeSelector {
    fn new(can_prompt: bool) -> (Arc<Self>, mpsc::UnboundedSender<FileExistsChoice>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                can_prompt: AtomicBool::new(can_prompt),
                answers: AsyncMutex::new(rx),
                asked: std::sync::Mutex::new(Vec::new()),
            }),
            tx,
        )
    }

    fn asked_count(&self) -> usize {
        self.asked.lock().map(|asked| asked.len()).unwrap_or(0)
    }
}

#[async_trait::async_trait]
impl HostSelection for FakeSelector {
    async fn select_hls_quality(
        &self,
        _task_id: &str,
        _options: &[HlsQualityOption],
        _timeout: Duration,
    ) -> SelectionOutcome<i32> {
        SelectionOutcome::NoSelectorConfigured(0)
    }

    async fn select_bt_files(
        &self,
        _task_id: &str,
        _files: &[BtFileEntry],
        _save_dir: &str,
        _timeout: Option<Duration>,
    ) -> SelectionOutcome<BtFileChoice> {
        SelectionOutcome::NoSelectorConfigured(BtFileChoice::default())
    }

    async fn select_resolve_variant(
        &self,
        _task_id: &str,
        _options: &[ResolveVariantOption],
        default_index: i32,
        _timeout: Duration,
    ) -> SelectionOutcome<i32> {
        SelectionOutcome::NoSelectorConfigured(default_index)
    }

    fn provide_hls_selection(&self, _task_id: &str, _selected_index: i32) {}
    fn provide_bt_selection(&self, _task_id: &str, _selected_indices: Vec<i32>) {}
    fn provide_variant_selection(&self, _task_id: &str, _selected_index: i32) {}

    fn can_prompt(&self) -> bool {
        self.can_prompt.load(Ordering::SeqCst)
    }

    async fn select_file_exists(
        &self,
        _task_id: &str,
        conflict: &FileConflict,
        _timeout: Duration,
    ) -> SelectionOutcome<FileExistsChoice> {
        if let Ok(mut asked) = self.asked.lock() {
            asked.push(conflict.clone());
        }
        match self.answers.lock().await.recv().await {
            Some(choice) => SelectionOutcome::UserChose(choice),
            None => std::future::pending().await,
        }
    }
}

struct Fixture {
    mgr: DownloadManager,
    db: Db,
    done_rx: mpsc::Receiver<TaskDone>,
    dir: PathBuf,
    base_url: String,
}

/// 接受连接但永不应答的本机端口：下载器连上后挂住，任务保持「运行中」。
async fn hang_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind hang server");
    let port = listener.local_addr().expect("local addr").port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    format!("http://127.0.0.1:{port}")
}

async fn fixture(
    tag: &str,
    max_concurrent: usize,
    selector: Arc<dyn HostSelection>,
    behavior: FileExistsBehavior,
) -> Fixture {
    let dir = std::env::temp_dir().join(format!("fluxdown_exists_{tag}_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    let db = Db::connect("sqlite::memory:").await.expect("connect db");
    let (mgr, done_rx) = fixture_manager(&db, &dir, max_concurrent, selector, behavior);
    Fixture {
        mgr,
        db,
        done_rx,
        dir,
        base_url: hang_server().await,
    }
}

fn fixture_manager(
    db: &Db,
    dir: &Path,
    max_concurrent: usize,
    selector: Arc<dyn HostSelection>,
    behavior: FileExistsBehavior,
) -> (DownloadManager, mpsc::Receiver<TaskDone>) {
    let mut mgr = DownloadManager::new(
        db.clone(),
        DownloadManagerConfig {
            max_concurrent,
            speed_limit_bps: 0,
            upload_limit_bps: 0,
            default_save_dir: dir.to_string_lossy().into_owned(),
            app_data_dir: String::new(),
            data_dir: std::env::temp_dir(),
            bt_config: BtConfig::default(),
            proxy_config: ProxyConfig::default(),
            user_agent: String::new(),
        },
        Arc::new(NullSink),
        selector,
    )
    .expect("construct manager");
    mgr.set_file_exists_behavior(behavior);
    let done_rx = mgr.take_done_rx().expect("done receiver");
    (mgr, done_rx)
}

impl Fixture {
    fn spec(&self, name: &str) -> NewTaskSpec {
        NewTaskSpec {
            url: format!("{}/{name}", self.base_url),
            save_dir: self.dir.to_string_lossy().into_owned(),
            file_name: name.to_string(),
            hint_file_size: -1,
            ..Default::default()
        }
    }

    fn write_existing(&self, name: &str, body: &[u8]) {
        std::fs::write(self.dir.join(name), body).expect("write existing file");
    }

    /// 消费 TaskDone 直到 `cond` 成立；超时视为失败。
    async fn pump_until(&mut self, what: &str, cond: impl Fn(&DownloadManager) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !cond(&self.mgr) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for: {what}");
            if let Ok(Some(done)) = tokio::time::timeout(
                remaining.min(Duration::from_millis(50)),
                self.done_rx.recv(),
            )
            .await
            {
                self.mgr.on_task_done(&done).await;
            }
        }
    }

    /// 消费 TaskDone 直到任务 DB 状态达到 `status`；超时视为失败。
    async fn pump_until_status(&mut self, id: &str, status: i32) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while self.status(id).await != status {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for status {status}"
            );
            if let Ok(Some(done)) =
                tokio::time::timeout(Duration::from_millis(50), self.done_rx.recv()).await
            {
                self.mgr.on_task_done(&done).await;
            }
        }
    }

    async fn status(&self, id: &str) -> i32 {
        self.db
            .load_task_by_id(id)
            .await
            .expect("load task")
            .expect("task exists")
            .status
    }

    async fn name(&self, id: &str) -> String {
        self.db
            .load_task_by_id(id)
            .await
            .expect("load task")
            .expect("task exists")
            .file_name
    }

    async fn decision(&self, id: &str) -> String {
        self.db.get_exists_decision(id).await.expect("decision")
    }
}

fn parked(mgr: &DownloadManager, id: &str) -> bool {
    mgr.active_tasks
        .get(id)
        .is_some_and(|entry| mgr.is_awaiting_decision(id, entry.generation))
}

#[tokio::test]
async fn ask_without_subscriber_renames_without_parking() {
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture("nosub", 1, selector.clone(), FileExistsBehavior::Ask).await;
    fx.write_existing("a.bin", b"old");
    let id = fx.mgr.create_task(fx.spec("a.bin")).await.expect("create");
    fx.pump_until("task renamed and running", |mgr| {
        mgr.active_tasks.contains_key(&id) && mgr.awaiting_decision.is_empty()
    })
    .await;
    let name = {
        let mut name = fx.name(&id).await;
        for _ in 0..50 {
            if name != "a.bin" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            name = fx.name(&id).await;
        }
        name
    };
    assert_eq!(name, "a (1).bin");
    assert_eq!(selector.asked_count(), 0);
    assert_eq!(fx.decision(&id).await, "");
}

#[tokio::test]
async fn ask_parks_task_and_releases_slot_for_queued_task() {
    let (selector, _answers) = FakeSelector::new(true);
    let mut fx = fixture("slot", 1, selector.clone(), FileExistsBehavior::Ask).await;
    fx.write_existing("a.bin", b"old");
    let a = fx
        .mgr
        .create_task(fx.spec("a.bin"))
        .await
        .expect("create a");
    let b = fx
        .mgr
        .create_task(fx.spec("b.bin"))
        .await
        .expect("create b");
    assert!(fx.mgr.pending_queue.iter().any(|q| q.task_id == b));
    fx.pump_until("a parked and b started", |mgr| {
        parked(mgr, &a) && mgr.active_tasks.contains_key(&b) && mgr.pending_queue.is_empty()
    })
    .await;
    assert_eq!(fx.status(&a).await, 0, "parked task keeps pending status");
    assert_eq!(selector.asked_count(), 1);
    let asked = selector.asked.lock().expect("asked").clone();
    assert_eq!(asked[0].file_name, "a.bin");
    assert_eq!(asked[0].rename_preview, "a (1).bin");
    assert_eq!(asked[0].existing_size, Some(3));
    assert!(asked[0].allow_skip);
}

#[tokio::test]
async fn overwrite_answer_keeps_name_and_binds_decision() {
    let (selector, answers) = FakeSelector::new(true);
    let mut fx = fixture("overwrite", 1, selector.clone(), FileExistsBehavior::Ask).await;
    fx.write_existing("a.bin", b"old");
    let id = fx.mgr.create_task(fx.spec("a.bin")).await.expect("create");
    fx.pump_until("parked", |mgr| parked(mgr, &id)).await;
    answers.send(FileExistsChoice::Overwrite).expect("answer");
    fx.pump_until("re-entered after answer", |mgr| {
        !parked(mgr, &id) && mgr.active_tasks.contains_key(&id)
    })
    .await;
    assert_eq!(fx.decision(&id).await, "overwrite");
    assert_eq!(fx.name(&id).await, "a.bin");
    assert_eq!(selector.asked_count(), 1, "answer must not be asked twice");
    assert_eq!(
        std::fs::read(fx.dir.join("a.bin")).expect("old file"),
        b"old"
    );
}

#[tokio::test]
async fn skip_answer_adopts_existing_file_and_survives_restart_and_delete() {
    let (selector, answers) = FakeSelector::new(true);
    let mut fx = fixture("skip", 1, selector.clone(), FileExistsBehavior::Ask).await;
    fx.write_existing("a.bin", b"mine");
    let id = fx.mgr.create_task(fx.spec("a.bin")).await.expect("create");
    fx.pump_until("parked", |mgr| parked(mgr, &id)).await;
    answers.send(FileExistsChoice::Skip).expect("answer");
    fx.pump_until_status(&id, 3).await;
    assert_eq!(fx.decision(&id).await, "skip");
    assert!(fx.mgr.skip_adopted(&id).await);

    // 重新下载：不删被采纳的用户文件，并清除决定。
    fx.mgr.restart_task(&id).await;
    assert_eq!(std::fs::read(fx.dir.join("a.bin")).expect("kept"), b"mine");
    assert_eq!(fx.decision(&id).await, "");

    // 再次跳过后删除任务连同文件：采纳的文件同样保留。
    let (selector2, _a2) = FakeSelector::new(false);
    let mut fx2 = fixture("skip_delete", 1, selector2, FileExistsBehavior::Skip).await;
    fx2.write_existing("b.bin", b"keep me");
    let id2 = fx2
        .mgr
        .create_task(fx2.spec("b.bin"))
        .await
        .expect("create");
    fx2.pump_until_status(&id2, 3).await;
    fx2.mgr.delete_task(&id2, true).await.expect("delete task");
    assert_eq!(
        std::fs::read(fx2.dir.join("b.bin")).expect("adopted file survives"),
        b"keep me"
    );
}

fn checksum_fixture_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind checksum server");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let address = listener.local_addr().expect("checksum server address");
    let listener = tokio::net::TcpListener::from_std(listener).expect("register listener");
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

        while let Ok((socket, _)) = listener.accept().await {
            let mut socket = BufReader::new(socket);
            let mut line = String::new();
            let head = match socket.read_line(&mut line).await {
                Ok(0) | Err(_) => continue,
                Ok(_) => line.starts_with("HEAD "),
            };
            loop {
                line.clear();
                match socket.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line == "\r\n" => break,
                    Ok(_) => {}
                }
            }
            let response: &[u8] = if head {
                b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\n"
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc"
            };
            if let Err(error) = socket.get_mut().write_all(response).await {
                tracing::debug!("checksum fixture connection closed: {error}");
            }
        }
    });
    (format!("http://{address}"), server)
}

const CHECKSUM: &str = "sha256=ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[tokio::test]
async fn checksum_match_adopts_without_prompt_and_mismatch_cannot_skip() {
    let (selector, answers) = FakeSelector::new(true);
    let mut fx = fixture("checksum", 1, selector.clone(), FileExistsBehavior::Ask).await;
    fx.write_existing("match.bin", b"abc");
    fx.write_existing("mismatch.bin", b"abd");

    let mut spec = fx.spec("match.bin");
    spec.checksum = CHECKSUM.into();
    let matched = fx.mgr.create_task(spec).await.expect("create match");
    fx.pump_until_status(&matched, 3).await;
    let task = fx
        .db
        .load_task_by_id(&matched)
        .await
        .expect("load")
        .expect("task");
    assert_eq!(task.file_hash, CHECKSUM);
    assert!(fx.mgr.skip_adopted(&matched).await);
    assert_eq!(selector.asked_count(), 0);

    let mut spec = fx.spec("restart.bin");
    spec.checksum = CHECKSUM.into();
    spec.start_paused = true;
    let restart = fx.mgr.create_task(spec).await.expect("create restart");
    fx.mgr.checksum_preludes.insert(
        restart.clone(),
        Arc::new(Mutex::new(ChecksumPartial::Unclaimed)),
    );
    let foreign_partial = format!("restart.bin{}", downloader::TEMP_EXT);
    fx.write_existing(&foreign_partial, b"another task");
    fx.mgr.restart_task(&restart).await;
    assert_eq!(
        std::fs::read(fx.dir.join(foreign_partial)).expect("foreign partial"),
        b"another task"
    );
    fx.mgr.pause_task(&restart).await;

    // A same-sized replacement has different identity even while the old file is open.
    let candidate = fx.dir.join("match.bin");
    let original = std::fs::File::open(&candidate).expect("open candidate");
    let replacement = fx.dir.join("replacement.bin");
    std::fs::write(&replacement, b"abd").expect("write replacement");
    let replacement = std::fs::File::open(replacement).expect("open replacement");
    assert!(!crate::file_hash::same_file(&original, &replacement).expect("compare identity"));
    drop((original, replacement));

    let mut spec = fx.spec("mismatch.bin");
    spec.checksum = CHECKSUM.into();
    spec.start_paused = true;
    let mismatch = fx.mgr.create_task(spec).await.expect("create mismatch");
    fx.db
        .set_exists_decision(&mismatch, ExistsDecision::Skip.as_db())
        .await
        .expect("persist skip");
    fx.mgr.resume_task(&mismatch).await;
    fx.pump_until("mismatch prompts", |mgr| {
        parked(mgr, &mismatch) && selector.asked_count() == 1
    })
    .await;
    assert!(!selector.asked.lock().expect("asked")[0].allow_skip);
    assert_eq!(fx.decision(&mismatch).await, ExistsDecision::Skip.as_db());
    answers.send(FileExistsChoice::Cancel).expect("cancel");
    fx.pump_until_status(&mismatch, 2).await;

    let partial = format!("match.bin{}", downloader::TEMP_EXT);
    fx.write_existing(&partial, b"sibling data");
    fx.mgr
        .delete_task(&matched, true)
        .await
        .expect("delete adoption");
    assert_eq!(
        std::fs::read(fx.dir.join("match.bin")).expect("candidate"),
        b"abc"
    );
    assert_eq!(
        std::fs::read(fx.dir.join(partial)).expect("sibling partial"),
        b"sibling data"
    );
    std::fs::remove_dir_all(&fx.dir).expect("cleanup");
}

#[tokio::test]
async fn matching_checksum_does_not_override_rename_or_overwrite() {
    let (selector, _) = FakeSelector::new(true);
    let mut fx = fixture("checksum_policies", 1, selector, FileExistsBehavior::Rename).await;
    let (url, server) = checksum_fixture_server();
    fx.base_url = url;
    for (behavior, name, expected) in [
        (FileExistsBehavior::Rename, "rename.bin", "rename (1).bin"),
        (
            FileExistsBehavior::Overwrite,
            "overwrite.bin",
            "overwrite.bin",
        ),
    ] {
        fx.mgr.set_file_exists_behavior(behavior);
        fx.write_existing(name, b"abc");
        let mut spec = fx.spec(name);
        spec.checksum = CHECKSUM.into();
        let id = fx.mgr.create_task(spec).await.expect("create");
        fx.pump_until_status(&id, 3).await;
        assert_eq!(fx.name(&id).await, expected);
        assert!(!fx.mgr.skip_adopted(&id).await);
        assert_eq!(
            std::fs::read(fx.dir.join(expected)).expect("written file"),
            b"abc"
        );
    }
    server.abort();
    assert!(server.await.expect_err("server aborted").is_cancelled());
    std::fs::remove_dir_all(&fx.dir).expect("cleanup");
}

#[tokio::test]
async fn checksum_redownload_does_not_adopt_the_replacement_file() {
    let (selector, _) = FakeSelector::new(false);
    let mut fx = fixture("checksum_redownload", 1, selector, FileExistsBehavior::Skip).await;
    let (url, server) = checksum_fixture_server();
    fx.base_url = url;
    fx.write_existing("match.bin", b"abc");
    let mut spec = fx.spec("match.bin");
    spec.checksum = CHECKSUM.into();
    let id = fx.mgr.create_task(spec).await.expect("create");
    fx.pump_until_status(&id, 3).await;
    fx.pump_until("adopted task retired", |mgr| {
        !mgr.active_tasks.contains_key(&id)
    })
    .await;
    std::fs::remove_file(fx.dir.join("match.bin")).expect("remove adopted file");
    assert!(
        fx.db
            .update_task_file_missing(&id, true)
            .await
            .expect("mark missing")
    );
    fx.write_existing("match.bin", b"abc");
    fx.mgr
        .restart_missing_task_file(&id)
        .await
        .expect("restart");
    fx.pump_until_status(&id, 3).await;
    assert_eq!(fx.name(&id).await, "match (1).bin");
    assert!(!fx.mgr.skip_adopted(&id).await);
    assert_eq!(
        std::fs::read(fx.dir.join("match.bin")).expect("replacement"),
        b"abc"
    );
    server.abort();
    assert!(server.await.expect_err("server aborted").is_cancelled());
    std::fs::remove_dir_all(&fx.dir).expect("cleanup");
}

#[tokio::test]
async fn checksum_worker_timeout_keeps_candidate_and_foreign_partial() {
    async fn check(batch: bool) {
        let (selector, _answers) = FakeSelector::new(false);
        let mut fx = fixture("checksum_timeout", 1, selector, FileExistsBehavior::Skip).await;
        let mut spec = fx.spec("candidate.bin");
        spec.start_paused = true;
        spec.checksum = CHECKSUM.into();
        let id = fx.mgr.create_task(spec).await.expect("create");
        fx.db
            .update_task_status(&id, 5, "")
            .await
            .expect("preparing");
        // The read-only prelude is blocked, with no paths or bytes claimed by this task.
        fx.mgr
            .checksum_preludes
            .insert(id.clone(), Arc::new(Mutex::new(ChecksumPartial::Unclaimed)));
        fx.mgr.active_tasks.insert(
            id.clone(),
            ActiveTaskEntry {
                token: CancellationToken::new(),
                generation: 1,
                handle: Some(tokio::spawn(std::future::pending())),
                is_bt: false,
                queue_id: String::new(),
            },
        );
        let partial = format!("candidate.bin{}", downloader::TEMP_EXT);
        fx.write_existing("candidate.bin", b"abc");
        fx.write_existing(&partial, b"sibling data");
        let reserved = fx.dir.join(&partial);
        lock_reserved(&fx.mgr.reserved_temp_paths).insert(reserved.clone());
        if batch {
            let outcome = fx
                .mgr
                .delete_tasks_batch(std::slice::from_ref(&id), true)
                .await
                .expect("batch delete");
            assert_eq!(outcome.deleted, 1);
        } else {
            fx.mgr.delete_task(&id, true).await.expect("delete");
        }
        assert_eq!(
            std::fs::read(&reserved).expect("sibling partial"),
            b"sibling data"
        );
        assert!(lock_reserved(&fx.mgr.reserved_temp_paths).contains(&reserved));
        // The baseline schedules a second name-based sweep two seconds after timeout.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(
            std::fs::read(fx.dir.join("candidate.bin")).expect("candidate"),
            b"abc"
        );
        assert_eq!(
            std::fs::read(&reserved).expect("sibling partial"),
            b"sibling data"
        );
        std::fs::remove_dir_all(&fx.dir).expect("cleanup");
    }
    tokio::join!(check(false), check(true));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn checksum_delete_retries_an_owned_partial_after_timeout() {
    tokio::join!(
        check_delete_retry(false, false),
        check_delete_retry(true, false)
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn checksum_delete_retries_a_partial_unreadable_at_worker_exit() {
    tokio::join!(
        check_delete_retry(false, true),
        check_delete_retry(true, true)
    );
}

#[cfg(any(unix, windows))]
async fn check_delete_retry(batch: bool, worker_exits: bool) {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    // Unix root bypasses the permissions used to force the I/O failure.
    #[cfg(unix)]
    // SAFETY: geteuid takes no arguments and has no memory preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture(
        "checksum_delete_retry",
        1,
        selector,
        FileExistsBehavior::Rename,
    )
    .await;
    let mut spec = fx.spec("owned.bin");
    spec.checksum = CHECKSUM.into();
    spec.start_paused = true;
    let id = fx.mgr.create_task(spec).await.expect("create paused");
    let partial = format!("owned.bin{}", downloader::TEMP_EXT);
    fx.write_existing(&partial, b"owned");
    let path = fx.dir.join(partial);
    lock_reserved(&fx.mgr.reserved_temp_paths).insert(path.clone());
    let claim = Arc::new(Mutex::new(ChecksumPartial::Reserved {
        path: path.clone(),
        hls_http_fallback: false,
    }));
    fx.mgr
        .checksum_preludes
        .insert(id.clone(), Arc::clone(&claim));
    let token = CancellationToken::new();
    let handle = if worker_exits {
        let cancel = token.clone();
        let reserved = Arc::clone(&fx.mgr.reserved_temp_paths);
        let done_tx = fx.mgr.done_tx.clone();
        let id = id.clone();
        let path = path.clone();
        tokio::spawn(async move {
            cancel.cancelled().await;
            let reservation = finish_worker_reservation(Some(&claim), &reserved, Some(path)).await;
            done_tx
                .send(TaskDone::finished(id, 1, reservation))
                .await
                .expect("send completion");
        })
    } else {
        tokio::spawn(std::future::pending())
    };
    fx.mgr.active_tasks.insert(
        id.clone(),
        ActiveTaskEntry {
            token: token.clone(),
            generation: 1,
            handle: Some(handle),
            is_bt: false,
            queue_id: String::new(),
        },
    );

    #[cfg(unix)]
    let blocked_path = if worker_exits {
        path.clone()
    } else {
        fx.dir.clone()
    };
    #[cfg(unix)]
    let original = {
        let permissions = std::fs::metadata(&blocked_path)
            .expect("blocked path")
            .permissions();
        let denied = if worker_exits { 0o444 } else { 0o222 };
        std::fs::set_permissions(
            &blocked_path,
            std::fs::Permissions::from_mode(permissions.mode() & !denied),
        )
        .expect("deny partial read or unlink");
        permissions
    };
    #[cfg(windows)]
    let locked = {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let sharing = if worker_exits {
            0
        } else {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        };
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(sharing)
            .open(&path)
            .expect("deny partial read or unlink")
    };

    // Cover both orders: the manager processes TaskDone before deleting,
    // or it joins the worker itself while processing the delete request.
    if worker_exits && !batch {
        token.cancel();
        fx.pump_until("worker exited", |mgr| !mgr.active_tasks.contains_key(&id))
            .await;
    }
    let first_failed = if batch {
        fx.mgr
            .delete_tasks_batch(std::slice::from_ref(&id), false)
            .await
            .is_ok_and(|outcome| outcome.deleted == 0 && outcome.failed == 1)
    } else {
        matches!(
            fx.mgr.delete_task(&id, false).await,
            Err(DeleteTaskError::Cleanup(_))
        )
    };
    #[cfg(unix)]
    std::fs::set_permissions(blocked_path, original).expect("restore test permissions");
    #[cfg(windows)]
    drop(locked);

    assert!(first_failed, "inaccessible partial must fail the deletion");
    assert!(fx.db.load_task_by_id(&id).await.expect("load").is_some());
    assert_eq!(std::fs::read(&path).expect("partial remains"), b"owned");
    assert_eq!(
        lock_reserved(&fx.mgr.reserved_temp_paths).contains(&path),
        worker_exits,
        "only the unresolved identity should keep the reservation"
    );
    if batch {
        let outcome = fx
            .mgr
            .delete_tasks_batch(std::slice::from_ref(&id), false)
            .await
            .expect("retry batch delete");
        assert_eq!((outcome.deleted, outcome.failed), (1, 0));
    } else {
        fx.mgr.delete_task(&id, false).await.expect("retry delete");
    }
    assert!(!path.exists(), "retry must remove the original partial");
    assert!(!lock_reserved(&fx.mgr.reserved_temp_paths).contains(&path));
    assert!(fx.db.load_task_by_id(&id).await.expect("load").is_none());
    fx.mgr.shutdown().await;
    std::fs::remove_dir_all(&fx.dir).expect("cleanup");
}

#[tokio::test]
async fn checksum_task_with_zero_recorded_bytes_cleans_its_own_partial() {
    async fn check(batch: bool) {
        let (selector, _answers) = FakeSelector::new(false);
        let mut fx = fixture(
            "checksum_zero_bytes",
            1,
            selector,
            FileExistsBehavior::Rename,
        )
        .await;
        // Keep the metadata request inside the startup prelude under test control.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        fx.base_url = format!("http://{}", listener.local_addr().expect("address"));
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.expect("request") > 0);
            accepted_tx.send(()).expect("accepted");
            release_rx.await.expect("release");
            if let Err(error) = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc")
                .await
            {
                tracing::debug!("cancelled probe closed: {error}");
            }
        });
        let mut spec = fx.spec("own");
        spec.start_paused = true;
        spec.checksum = CHECKSUM.into();
        let id = fx.mgr.create_task(spec).await.expect("create");
        // Avoid a creation-time background probe, but enable the resumed prelude's
        // missing-extension probe before it can claim its output name.
        fx.db
            .set_task_range_verified(&id, true)
            .await
            .expect("range state");
        // The previous worker left a partial before any progress was saved.
        let partial = format!("own{}", downloader::TEMP_EXT);
        fx.write_existing(&partial, b"unreported bytes");
        let path = fx.dir.join(&partial);
        let file = std::fs::File::open(&path).expect("open own partial");
        fx.mgr.checksum_preludes.insert(
            id.clone(),
            Arc::new(Mutex::new(ChecksumPartial::Retained {
                path,
                file: Arc::new(file),
                hls_http_fallback: false,
            })),
        );
        // Cancel the resume probe before it writes; the retained partial still belongs to us.
        fx.mgr.resume_task(&id).await;
        tokio::time::timeout(Duration::from_secs(5), accepted_rx)
            .await
            .expect("probe connected")
            .expect("accepted");
        fx.mgr.pause_task(&id).await;
        release_tx.send(()).expect("release probe");
        fx.pump_until("retry stopped", |mgr| {
            !mgr.active_tasks.contains_key(&id) && !mgr.pending_pauses.contains_key(&id)
        })
        .await;
        server.await.expect("server stopped");
        if batch {
            let outcome = fx
                .mgr
                .delete_tasks_batch(std::slice::from_ref(&id), false)
                .await
                .expect("batch delete");
            assert_eq!((outcome.deleted, outcome.failed), (1, 0));
        } else {
            fx.mgr.delete_task(&id, false).await.expect("delete");
        }
        assert!(
            !fx.dir.join(&partial).exists(),
            "owned partial must be cleaned"
        );
        assert!(fx.db.load_task_by_id(&id).await.expect("load").is_none());
        fx.mgr.shutdown().await;
        std::fs::remove_dir_all(&fx.dir).expect("cleanup");
    }
    tokio::join!(check(false), check(true));
}

#[tokio::test]
async fn checksum_task_does_not_reuse_or_remove_a_replaced_partial() {
    async fn check(restart: bool) {
        let (selector, _answers) = FakeSelector::new(false);
        let mut fx = fixture("checksum_replaced", 1, selector, FileExistsBehavior::Rename).await;
        let (url, server) = checksum_fixture_server();
        fx.base_url = url;
        let mut spec = fx.spec("shared.bin");
        spec.start_paused = true;
        spec.checksum = CHECKSUM.into();
        let id = fx.mgr.create_task(spec).await.expect("create paused");
        let partial = format!("shared.bin{}", downloader::TEMP_EXT);
        let path = fx.dir.join(&partial);
        fx.write_existing(&partial, b"abc");
        let old = std::fs::File::open(&path).expect("open original partial");
        fx.mgr.checksum_preludes.insert(
            id.clone(),
            Arc::new(Mutex::new(ChecksumPartial::Retained {
                path: path.clone(),
                file: Arc::new(old),
                hls_http_fallback: false,
            })),
        );
        std::fs::rename(&path, fx.dir.join("old-partial")).expect("move original partial aside");
        fx.write_existing(&partial, b"xyz");

        if restart {
            fx.mgr.restart_task(&id).await;
        } else {
            fx.mgr.resume_task(&id).await;
        }
        fx.pump_until_status(&id, 3).await;
        assert_eq!(fx.name(&id).await, "shared (1).bin");
        assert_eq!(std::fs::read(&path).expect("read sibling partial"), b"xyz");
        assert_eq!(
            std::fs::read(fx.dir.join("shared (1).bin")).expect("read new result"),
            b"abc"
        );
        fx.pump_until("worker retired", |mgr| !mgr.active_tasks.contains_key(&id))
            .await;
        fx.mgr.delete_task(&id, true).await.expect("delete result");
        assert!(!fx.dir.join("shared (1).bin").exists());
        assert_eq!(
            std::fs::read(&path).expect("preserve sibling partial"),
            b"xyz"
        );
        server.abort();
        assert!(server.await.expect_err("server aborted").is_cancelled());
        fx.mgr.shutdown().await;
        std::fs::remove_dir_all(&fx.dir).expect("cleanup");
    }
    tokio::join!(check(false), check(true));
}

#[tokio::test]
async fn checksum_task_without_claim_or_checkpoint_preserves_same_name_partial() {
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture("checksum_no_claim", 1, selector, FileExistsBehavior::Rename).await;
    let (url, server) = checksum_fixture_server();
    fx.base_url = url;

    // Create paused task rows without running their downloaders, as after
    // reloading a database without any in-process partial claims.
    for name in ["resume", "restart", "delete", "batch"] {
        let file_name = format!("{name}.bin");
        let mut spec = fx.spec(&file_name);
        spec.start_paused = true;
        spec.checksum = CHECKSUM.into();
        let id = fx.mgr.create_task(spec).await.expect("create paused");
        let partial = format!("{file_name}{}", downloader::TEMP_EXT);
        fx.write_existing(&partial, b"xyz");

        match name {
            "resume" | "restart" => {
                if name == "resume" {
                    fx.mgr.resume_task(&id).await;
                } else {
                    fx.mgr.restart_task(&id).await;
                }
                fx.pump_until_status(&id, 3).await;
                let chosen = format!("{name} (1).bin");
                assert_eq!(fx.name(&id).await, chosen);
                assert_eq!(
                    std::fs::read(fx.dir.join(chosen)).expect("new file"),
                    b"abc"
                );
                fx.pump_until("worker retired", |mgr| !mgr.active_tasks.contains_key(&id))
                    .await;
            }
            "delete" => fx.mgr.delete_task(&id, true).await.expect("delete"),
            _ => {
                let outcome = fx
                    .mgr
                    .delete_tasks_batch(std::slice::from_ref(&id), true)
                    .await
                    .expect("batch delete");
                assert_eq!((outcome.deleted, outcome.failed), (1, 0));
            }
        }
        assert_eq!(
            std::fs::read(fx.dir.join(partial)).expect("preserved partial"),
            b"xyz"
        );
    }

    // A persisted segment plan still permits cleanup even with zero recorded bytes.
    let mut spec = fx.spec("checkpoint.bin");
    spec.start_paused = true;
    spec.checksum = CHECKSUM.into();
    let id = fx
        .mgr
        .create_task(spec)
        .await
        .expect("create checkpoint task");
    fx.db
        .insert_segments(&id, &[(0, 0, 2)])
        .await
        .expect("persist segment plan");
    let partial = format!("checkpoint.bin{}", downloader::TEMP_EXT);
    fx.write_existing(&partial, b"abc");
    let outcome = fx
        .mgr
        .delete_tasks_batch(&[id], true)
        .await
        .expect("batch delete");
    assert_eq!((outcome.deleted, outcome.failed), (1, 0));
    assert!(!fx.dir.join(partial).exists());

    server.abort();
    assert!(server.await.expect_err("server aborted").is_cancelled());
    fx.mgr.shutdown().await;
    std::fs::remove_dir_all(&fx.dir).expect("cleanup");
}

#[tokio::test]
async fn checksum_hls_fallback_resumes_the_http_partial() {
    check_hls_fallback_resume(false).await;
}

#[tokio::test]
async fn checksum_hls_fallback_resumes_after_manager_restart() {
    check_hls_fallback_resume(true).await;
}

async fn check_hls_fallback_resume(restart: bool) {
    use sha2::{Digest as _, Sha256};
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

    const MIB: usize = 1024 * 1024;
    const TOTAL: usize = 2 * MIB;
    const PREFIX: usize = MIB + MIB / 2;
    let body = vec![b'x'; TOTAL];
    let checksum = format!("sha256={:x}", Sha256::digest(&body));
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture(
        "checksum_hls_fallback",
        1,
        selector,
        FileExistsBehavior::Rename,
    )
    .await;
    fx.write_existing("data.m3u8", b"sibling");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    fx.base_url = format!("http://{}", listener.local_addr().expect("address"));
    let (ranges, mut requests) = mpsc::unbounded_channel();
    let prefix_sent = Arc::new(tokio::sync::Notify::new());
    let notify = Arc::clone(&prefix_sent);
    let server = tokio::spawn(async move {
        let mut plain_gets = 0;
        while let Ok((socket, _)) = listener.accept().await {
            let mut socket = BufReader::new(socket);
            let mut line = String::new();
            if socket.read_line(&mut line).await.expect("request") == 0 {
                continue;
            }
            let head = line.starts_with("HEAD ");
            let mut range = None;
            loop {
                line.clear();
                if socket.read_line(&mut line).await.expect("header") == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("range")
                {
                    let (start, end) = value
                        .trim()
                        .strip_prefix("bytes=")
                        .expect("bytes range")
                        .split_once('-')
                        .expect("range bounds");
                    range = Some((
                        start.parse::<usize>().expect("range start"),
                        end.parse::<usize>().unwrap_or(TOTAL - 1),
                    ));
                }
            }
            let socket = socket.get_mut();
            if let Some((start, end)) = range {
                let len = end - start + 1;
                let headers = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {len}\r\nContent-Range: bytes {start}-{end}/{TOTAL}\r\nConnection: close\r\n\r\n"
                );
                if start != 0 {
                    ranges.send(start).expect("record resume offset");
                }
                // The competing metadata probe may cancel a 0-0 request.
                if socket.write_all(headers.as_bytes()).await.is_ok()
                    && let Err(error) = socket.write_all(&body[start..=end]).await
                {
                    tracing::debug!("test range response closed: {error}");
                }
            } else {
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {TOTAL}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
                );
                if socket.write_all(headers.as_bytes()).await.is_err() || head {
                    continue;
                }
                plain_gets += 1;
                match plain_gets {
                    1 => socket
                        .write_all(&body)
                        .await
                        .expect("non-playlist response"),
                    2 => {
                        socket
                            .write_all(&body[..PREFIX])
                            .await
                            .expect("initial prefix");
                        notify.notify_one();
                        // Do not finish the response; pausing the task closes this socket.
                        assert!(socket.read_u8().await.is_err(), "paused socket must close");
                    }
                    _ => panic!("resume re-fetched the playlist or restarted at byte zero"),
                }
            }
        }
    });

    let mut spec = fx.spec("data.m3u8");
    spec.start_paused = true;
    spec.segments = 1;
    spec.checksum = checksum.clone();
    let id = fx.mgr.create_task(spec).await.expect("create task");
    fx.mgr.resume_task(&id).await;
    let name = "data (1).m3u8";
    let partial = fx.dir.join(format!("{name}{}", downloader::TEMP_EXT));
    tokio::time::timeout(Duration::from_secs(10), async {
        prefix_sent.notified().await;
        while !std::fs::metadata(&partial).is_ok_and(|metadata| metadata.len() >= MIB as u64) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("HTTP fallback wrote a resumable prefix");
    fx.mgr.pause_task(&id).await;
    fx.pump_until("fallback paused", |mgr| {
        !mgr.pending_pauses.contains_key(&id)
    })
    .await;
    assert_eq!(fx.status(&id).await, 2);
    if restart {
        fx.mgr.shutdown().await;
        let (selector, _answers) = FakeSelector::new(false);
        (fx.mgr, fx.done_rx) =
            fixture_manager(&fx.db, &fx.dir, 1, selector, FileExistsBehavior::Rename);
    }

    fx.mgr.resume_task(&id).await;
    fx.pump_until_status(&id, 3).await;
    assert_eq!(requests.try_recv().expect("positive Range offset"), MIB);
    assert_eq!(fx.name(&id).await, name);
    assert!(!partial.exists());
    assert_eq!(
        fx.db
            .load_task_by_id(&id)
            .await
            .expect("load task")
            .expect("task")
            .file_hash,
        checksum
    );
    assert_eq!(
        std::fs::read(fx.dir.join("data.m3u8")).expect("sibling"),
        b"sibling"
    );
    fx.pump_until("fallback retired", |mgr| {
        !mgr.active_tasks.contains_key(&id)
    })
    .await;
    server.abort();
    assert!(server.await.expect_err("server aborted").is_cancelled());
    fx.mgr.shutdown().await;
    std::fs::remove_dir_all(&fx.dir).expect("cleanup");
}

#[tokio::test]
async fn cancel_answer_pauses_and_resume_asks_again() {
    let (selector, answers) = FakeSelector::new(true);
    let mut fx = fixture("cancel", 1, selector.clone(), FileExistsBehavior::Ask).await;
    fx.write_existing("a.bin", b"old");
    let id = fx.mgr.create_task(fx.spec("a.bin")).await.expect("create");
    fx.pump_until("parked", |mgr| parked(mgr, &id)).await;
    answers.send(FileExistsChoice::Cancel).expect("answer");
    fx.pump_until("paused after cancel", |mgr| {
        !mgr.active_tasks.contains_key(&id)
    })
    .await;
    assert_eq!(fx.status(&id).await, 2);
    assert_eq!(fx.decision(&id).await, "");

    fx.mgr.resume_task(&id).await;
    fx.pump_until("parked again", |mgr| parked(mgr, &id)).await;
    assert_eq!(selector.asked_count(), 2);
}

#[tokio::test]
async fn pause_during_wait_ignores_late_answer() {
    let (selector, answers) = FakeSelector::new(true);
    let mut fx = fixture("pause", 1, selector.clone(), FileExistsBehavior::Ask).await;
    fx.write_existing("a.bin", b"old");
    let id = fx.mgr.create_task(fx.spec("a.bin")).await.expect("create");
    fx.pump_until("parked", |mgr| parked(mgr, &id)).await;
    fx.mgr.pause_task(&id).await;
    // 暂停取消了等待中的询问；迟到的答复不会被任何人消费，也不得改动任务。
    answers.send(FileExistsChoice::Overwrite).expect("answer");
    fx.pump_until("pause flushed", |mgr| mgr.pending_pauses.is_empty())
        .await;
    assert_eq!(fx.status(&id).await, 2);
    assert_eq!(fx.decision(&id).await, "");
    assert!(!fx.mgr.active_tasks.contains_key(&id));
}

#[tokio::test]
async fn never_started_task_resumed_honours_global_skip() {
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture("later", 1, selector, FileExistsBehavior::Skip).await;
    fx.write_existing("a.bin", b"mine");
    let mut spec = fx.spec("a.bin");
    spec.start_paused = true;
    let id = fx.mgr.create_task(spec).await.expect("create");
    assert_eq!(fx.status(&id).await, 2);
    fx.mgr.resume_task(&id).await;
    fx.pump_until_status(&id, 3).await;
    assert_eq!(fx.decision(&id).await, ExistsDecision::Skip.as_db());
}

#[test]
fn hls_companion_mp4_counts_as_conflict() {
    let dir = std::env::temp_dir().join(format!("fluxdown_hls_companion_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create dir");
    std::fs::write(dir.join("clip.mp4"), b"old").expect("write mp4");
    let none = HashSet::new();
    assert_eq!(
        dedup_filename_sync(&dir, "clip.ts", &none, &OverwritePolicy::Never, true, false),
        "clip (1).ts"
    );
    assert_eq!(
        dedup_filename_sync(
            &dir,
            "clip.ts",
            &none,
            &OverwritePolicy::Never,
            false,
            false
        ),
        "clip.ts"
    );
    assert_eq!(
        dedup_filename_sync(&dir, "clip.ts", &none, &OverwritePolicy::Any, true, false),
        "clip.ts"
    );
    std::fs::write(dir.join("clip (1).mp4"), b"old").expect("write numbered mp4");
    assert_eq!(
        dedup_filename_sync(&dir, "clip.ts", &none, &OverwritePolicy::Never, true, false),
        "clip (2).ts"
    );
    std::fs::remove_dir_all(&dir).expect("cleanup");
}
