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
    Fixture {
        mgr,
        db,
        done_rx,
        dir,
        base_url: hang_server().await,
    }
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
    assert!(fx.mgr.has_unowned_existing_path(&id).await);

    // 重新下载重新占名，不删被采纳的用户文件。
    fx.mgr
        .restart_task(&id)
        .await
        .expect("restart adopted task");
    assert_eq!(std::fs::read(fx.dir.join("a.bin")).expect("kept"), b"mine");
    let renamed_partial = fx.dir.join(format!("a (1).bin{}", downloader::TEMP_EXT));
    fx.pump_until("new output name reserved", |mgr| {
        lock_reserved(&mgr.reserved_temp_paths).contains(&renamed_partial)
    })
    .await;

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
async fn pr1241_remaining_matching_checksum_does_not_override_rename() {
    let (selector, _) = FakeSelector::new(true);
    let mut fx = fixture("checksum_rename", 1, selector, FileExistsBehavior::Rename).await;
    let (url, server) = checksum_fixture_server();
    fx.base_url = url;
    fx.write_existing("match.bin", b"abc");
    let mut spec = fx.spec("match.bin");
    spec.checksum = CHECKSUM.into();
    let id = fx.mgr.create_task(spec).await.expect("create task");
    fx.pump_until_status(&id, 3).await;
    server.abort();
    assert_eq!(fx.name(&id).await, "match (1).bin");
    assert_eq!(std::fs::read(fx.dir.join("match (1).bin")).unwrap(), b"abc");
    assert_eq!(std::fs::read(fx.dir.join("match.bin")).unwrap(), b"abc");
    assert!(!fx.mgr.has_unowned_existing_path(&id).await);
}

#[tokio::test]
async fn checksum_match_adopts_without_prompt_and_mismatch_cannot_skip() {
    let (selector, answers) = FakeSelector::new(true);
    let mut fx = fixture("checksum", 1, selector.clone(), FileExistsBehavior::Ask).await;
    let (url, server) = checksum_fixture_server();
    fx.base_url = url;
    fx.write_existing("match.bin", b"abc");
    fx.write_existing("mismatch.bin", b"abd");

    let mut matched = fx.spec("match.bin");
    matched.checksum = CHECKSUM.into();
    let matched = fx.mgr.create_task(matched).await.expect("create match");
    fx.pump_until_status(&matched, 3).await;
    let task = fx
        .db
        .load_task_by_id(&matched)
        .await
        .expect("load")
        .expect("task");
    assert_eq!(task.file_hash, CHECKSUM);
    assert!(fx.mgr.has_unowned_existing_path(&matched).await);
    assert_eq!(selector.asked_count(), 0);

    let mut mismatch = fx.spec("mismatch.bin");
    mismatch.checksum = CHECKSUM.into();
    let mismatch = fx.mgr.create_task(mismatch).await.expect("create mismatch");
    fx.pump_until("mismatch prompts", |mgr| parked(mgr, &mismatch))
        .await;
    assert!(!selector.asked.lock().expect("asked")[0].allow_skip);
    assert_ne!(fx.status(&mismatch).await, 3);
    answers.send(FileExistsChoice::Cancel).expect("cancel");
    fx.pump_until_status(&mismatch, 2).await;

    fx.write_existing("match.bin", b"locally edited");
    let sibling_partial = format!("match.bin{}", downloader::TEMP_EXT);
    fx.write_existing(&sibling_partial, b"another task's partial");
    fx.mgr
        .set_file_exists_behavior(FileExistsBehavior::Overwrite);
    fx.mgr
        .restart_task(&matched)
        .await
        .expect("restart checksum match");
    fx.pump_until_status(&matched, 3).await;
    let new_name = fx.name(&matched).await;
    assert_ne!(new_name, "match.bin");
    assert_eq!(
        std::fs::read(fx.dir.join(new_name)).expect("new file"),
        b"abc"
    );
    assert_eq!(
        std::fs::read(fx.dir.join("match.bin")).expect("old file"),
        b"locally edited"
    );
    assert_eq!(
        std::fs::read(fx.dir.join(sibling_partial)).expect("sibling partial"),
        b"another task's partial"
    );
    server.abort();
    match server.await {
        Err(error) if error.is_cancelled() => {}
        result => panic!("checksum server stopped unexpectedly: {result:?}"),
    }
}

#[tokio::test]
async fn failed_restart_reset_leaves_adoption_protected() {
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture("restart_rollback", 1, selector, FileExistsBehavior::Skip).await;
    fx.write_existing("adopted.bin", b"abc");
    let mut spec = fx.spec("adopted.bin");
    spec.checksum = CHECKSUM.into();
    let id = fx.mgr.create_task(spec).await.expect("create");
    fx.pump_until_status(&id, 3).await;
    sqlx::query(
        "CREATE TRIGGER reject_restart BEFORE UPDATE OF downloaded_bytes ON tasks
         WHEN NEW.downloaded_bytes = 0 BEGIN SELECT RAISE(FAIL, 'reject restart'); END",
    )
    .execute(fx.db.pool())
    .await
    .expect("inject restart failure");
    fx.mgr
        .restart_task(&id)
        .await
        .expect_err("trigger rejects reset");
    assert_eq!(fx.status(&id).await, 3);
    fx.mgr
        .delete_task(&id, true)
        .await
        .expect("delete adopted task");
    assert_eq!(
        std::fs::read(fx.dir.join("adopted.bin")).expect("protected file"),
        b"abc"
    );
}

#[tokio::test]
async fn checksum_worker_timeout_preserves_existing_file_and_next_tasks_partial() {
    async fn delete_during_checksum(batch: bool) {
        let (selector, _answers) = FakeSelector::new(false);
        let mut fx = fixture("checksum_timeout", 1, selector, FileExistsBehavior::Rename).await;
        fx.write_existing("candidate.bin", b"existing");
        let mut spec = fx.spec("candidate.bin");
        spec.start_paused = true;
        spec.checksum = CHECKSUM.into();
        let id = fx.mgr.create_task(spec).await.expect("create");
        fx.db
            .update_task_status(&id, 5, "")
            .await
            .expect("preparing");
        // Model a blocking disk read in the start prelude: worker alive, no
        // path reserved, no progress and no completion before the delete deadline.
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
        if batch {
            let result = fx
                .mgr
                .delete_tasks_batch(std::slice::from_ref(&id), true)
                .await
                .expect("batch delete");
            assert_eq!(result.deleted, 1);
        } else {
            fx.mgr.delete_task(&id, true).await.expect("single delete");
        }
        let (url, server) = checksum_fixture_server();
        fx.base_url = url;
        let mut next = fx.spec("candidate.bin");
        next.start_paused = true;
        let next = fx.mgr.create_task(next).await.expect("create next task");
        let partial = format!("candidate.bin{}", downloader::TEMP_EXT);
        let audio_partial = format!("candidate.audio.m4a{}", downloader::TEMP_EXT);
        fx.write_existing(&partial, b"a");
        fx.write_existing(&audio_partial, b"unrelated audio");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(
            std::fs::read(fx.dir.join("candidate.bin")).expect("existing file"),
            b"existing"
        );
        assert_eq!(
            std::fs::read(fx.dir.join(&partial)).expect("next partial"),
            b"a"
        );
        assert_eq!(
            std::fs::read(fx.dir.join(&audio_partial)).expect("unrelated audio partial"),
            b"unrelated audio"
        );
        fx.mgr.resume_task(&next).await;
        fx.pump_until_status(&next, 3).await;
        let name = fx.name(&next).await;
        assert_eq!(
            std::fs::read(fx.dir.join(name)).expect("next output"),
            b"abc"
        );
        server.abort();
        match server.await {
            Err(error) if error.is_cancelled() => {}
            result => panic!("checksum server stopped unexpectedly: {result:?}"),
        }
    }

    tokio::join!(delete_during_checksum(false), delete_during_checksum(true));
}

#[tokio::test]
async fn batch_delete_uses_the_name_claimed_when_the_worker_exits() {
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture(
        "delete_during_restart_claim",
        1,
        selector,
        FileExistsBehavior::Rename,
    )
    .await;
    let mut spec = fx.spec("candidate.bin");
    spec.start_paused = true;
    let id = fx
        .mgr
        .create_task(spec)
        .await
        .expect("create restarting task");
    fx.db
        .set_exists_decision(&id, ExistsDecision::RestartRename.as_db())
        .await
        .expect("protect the old path");

    let old_partial = format!("candidate.bin{}", downloader::TEMP_EXT);
    let new_partial = fx
        .dir
        .join(format!("candidate (1).bin{}", downloader::TEMP_EXT));
    fx.write_existing(&old_partial, b"another task's partial");
    let token = CancellationToken::new();
    let cancelled = token.clone();
    let db = fx.db.clone();
    let task_id = id.clone();
    let path = new_partial.clone();
    let handle = tokio::spawn(async move {
        // A name write already in flight can commit as the worker shuts down.
        cancelled.cancelled().await;
        db.claim_restart_file_name(&task_id, "candidate (1).bin")
            .await
            .expect("finish claiming the new name");
        tokio::fs::write(path, b"own partial")
            .await
            .expect("write new partial");
        db.insert_segments(&task_id, &[(0, 0, 10)])
            .await
            .expect("persist the new path's checkpoint");
    });
    fx.mgr.active_tasks.insert(
        id.clone(),
        ActiveTaskEntry {
            token,
            generation: 1,
            handle: Some(handle),
            is_bt: false,
            queue_id: String::new(),
        },
    );

    let outcome = fx
        .mgr
        .delete_tasks_batch(&[id], true)
        .await
        .expect("batch delete");
    assert_eq!(outcome.deleted, 1);
    assert_eq!(
        std::fs::read(fx.dir.join(old_partial)).expect("old partial"),
        b"another task's partial"
    );
    assert!(!new_partial.exists(), "the claimed partial must be deleted");
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

#[tokio::test]
async fn never_started_task_does_not_resume_another_tasks_partial() {
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture("retained_partial", 1, selector, FileExistsBehavior::Rename).await;
    let mut old = fx.spec("same.bin");
    old.start_paused = true;
    let old_id = fx.mgr.create_task(old).await.expect("create old task");
    fx.db
        .insert_segments(&old_id, &[(0, 0, 1), (1, 2, 2)])
        .await
        .expect("save old segment layout");
    let partial = format!("same.bin{}", downloader::TEMP_EXT);
    fx.write_existing(&partial, b"x\0\0");
    fx.mgr
        .delete_task(&old_id, false)
        .await
        .expect("delete old record");

    let (url, server) = checksum_fixture_server();
    fx.base_url = url;
    let mut next = fx.spec("same.bin");
    next.start_paused = true;
    next.segments = 1;
    let id = fx.mgr.create_task(next).await.expect("create new task");
    fx.mgr.resume_task(&id).await;
    fx.pump_until_status(&id, 3).await;
    assert_eq!(
        std::fs::read(fx.dir.join(fx.name(&id).await)).expect("new file"),
        b"abc"
    );
    assert_eq!(
        std::fs::read(fx.dir.join(partial)).expect("retained partial"),
        b"x\0\0"
    );
    server.abort();
    match server.await {
        Err(error) if error.is_cancelled() => {}
        result => panic!("fixture server stopped unexpectedly: {result:?}"),
    }
}

#[tokio::test]
async fn never_started_task_deletion_preserves_another_tasks_partial() {
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture(
        "delete_retained_partial",
        1,
        selector,
        FileExistsBehavior::Rename,
    )
    .await;
    let partial = format!("same.bin{}", downloader::TEMP_EXT);
    let mut spec = fx.spec("same.bin");
    spec.start_paused = true;
    let old = fx.mgr.create_task(spec.clone()).await.expect("old task");
    fx.db
        .insert_segments(&old, &[(0, 0, 2)])
        .await
        .expect("save old checkpoint");
    fx.write_existing(&partial, b"x\0\0");
    fx.mgr
        .delete_task(&old, false)
        .await
        .expect("remove old record only");

    for batch in [false, true] {
        let id = fx.mgr.create_task(spec.clone()).await.expect("new task");
        if batch {
            let outcome = fx
                .mgr
                .delete_tasks_batch(std::slice::from_ref(&id), true)
                .await
                .expect("delete new record and files in batch");
            assert_eq!(outcome.deleted, 1);
            assert_eq!(outcome.failed, 0);
        } else {
            fx.mgr
                .delete_task(&id, true)
                .await
                .expect("delete new record and files");
        }
        assert!(
            fx.db
                .load_task_by_id(&id)
                .await
                .expect("load task")
                .is_none()
        );
        assert_eq!(
            std::fs::read(fx.dir.join(&partial)).expect("old partial"),
            b"x\0\0"
        );
    }
    std::fs::remove_dir_all(fx.dir).expect("remove fixture");
}

#[tokio::test]
async fn never_started_task_restart_preserves_a_running_download() {
    assert_restart_preserves_a_running_download(false).await;
}

#[tokio::test]
async fn completed_task_restart_preserves_a_running_download() {
    assert_restart_preserves_a_running_download(true).await;
}

async fn gated_download(
    prefix_len: usize,
) -> (
    String,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    let tail = b"remaining";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind slow download");
    let url = format!("http://{}", listener.local_addr().expect("local address"));
    let (release, released) = tokio::sync::oneshot::channel();
    let slow_server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept download");
        let mut socket = BufReader::new(socket);
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(socket.read_line(&mut line).await.expect("request line"), 0);
            if line == "\r\n" {
                break;
            }
        }
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            prefix_len + tail.len()
        );
        let socket = socket.get_mut();
        socket.write_all(header.as_bytes()).await.expect("headers");
        socket
            .write_all(&vec![b'a'; prefix_len])
            .await
            .expect("prefix");
        released.await.expect("release response");
        socket.write_all(tail).await.expect("tail");
    });
    (url, release, slow_server)
}

async fn wait_for_partial(path: &Path, min_size: u64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match tokio::fs::metadata(path).await {
                Ok(meta) if meta.len() >= min_size => break,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("inspect the running partial: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("task opened its partial");
}

async fn assert_restart_preserves_a_running_download(completed: bool) {
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture(
        "restart_same_name",
        2,
        selector,
        FileExistsBehavior::Overwrite,
    )
    .await;
    let (url, server) = checksum_fixture_server();
    fx.base_url = url;
    let mut second = fx.spec("same.bin");
    second.start_paused = true;
    second.segments = 1;
    let second = fx.mgr.create_task(second).await.expect("new paused task");
    if completed {
        fx.mgr.resume_task(&second).await;
        fx.pump_until_status(&second, 3).await;
        std::fs::remove_file(fx.dir.join("same.bin")).expect("remove old output only");
    }
    let prefix_len = downloader::BUF_WRITER_CAPACITY * 2;
    let (url, release, slow_server) = gated_download(prefix_len).await;
    fx.base_url = url;

    let mut first = fx.spec("same.bin");
    first.segments = 1;
    let first = fx.mgr.create_task(first).await.expect("start first task");
    let partial = fx.dir.join(format!("same.bin{}", downloader::TEMP_EXT));
    wait_for_partial(&partial, 1).await;
    assert_eq!(fx.status(&first).await, 1);

    fx.mgr
        .restart_task(&second)
        .await
        .expect("restart second task");
    fx.pump_until_status(&second, 3).await;
    let output = fx.dir.join(fx.name(&second).await);
    assert_ne!(output, fx.dir.join("same.bin"));
    assert_eq!(std::fs::read(output).expect("second output"), b"abc");
    assert!(
        partial.exists(),
        "the first task still owns its open partial"
    );

    release.send(()).expect("finish the first response");
    slow_server.await.expect("join slow server");
    fx.pump_until_status(&first, 3).await;
    let mut expected = vec![b'a'; prefix_len];
    expected.extend_from_slice(b"remaining");
    assert_eq!(
        std::fs::read(fx.dir.join("same.bin")).expect("first output"),
        expected
    );
    server.abort();
    match server.await {
        Err(error) if error.is_cancelled() => {}
        result => panic!("fixture server stopped unexpectedly: {result:?}"),
    }
    fx.mgr.shutdown().await;
    std::fs::remove_dir_all(fx.dir).expect("remove fixture");
}

#[tokio::test]
async fn zero_byte_single_stream_partial_is_deleted() {
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture("empty_partial", 1, selector, FileExistsBehavior::Rename).await;
    for batch in [false, true] {
        let (url, release, server) = gated_download(0).await;
        fx.base_url = url;
        let mut spec = fx.spec("same.bin");
        spec.segments = 1;
        let id = fx.mgr.create_task(spec).await.expect("start single stream");
        let path = fx.dir.join(format!("same.bin{}", downloader::TEMP_EXT));
        wait_for_partial(&path, 0).await;
        assert_eq!(std::fs::metadata(&path).expect("partial exists").len(), 0);
        if batch {
            let outcome = fx
                .mgr
                .delete_tasks_batch(&[id], true)
                .await
                .expect("batch delete");
            assert_eq!(outcome.deleted, 1);
            assert_eq!(outcome.failed, 0);
        } else {
            fx.mgr.delete_task(&id, true).await.expect("single delete");
        }
        assert!(!path.exists(), "delete must remove its own empty partial");
        server.abort();
        assert!(server.await.expect_err("abort gated server").is_cancelled());
        drop(release);
    }
    fx.mgr.shutdown().await;
    std::fs::remove_dir_all(fx.dir).expect("remove fixture");
}

#[tokio::test]
async fn hls_delete_only_removes_registered_audio_outputs() {
    let (selector, _answers) = FakeSelector::new(false);
    let mut fx = fixture("audio_cleanup", 1, selector, FileExistsBehavior::Rename).await;
    for batch in [false, true] {
        for registered in [false, true] {
            let mut spec = fx.spec("clip.ts");
            spec.url = format!("{}/clip.m3u8", fx.base_url);
            spec.start_paused = true;
            let id = fx.mgr.create_task(spec).await.expect("create HLS task");
            fx.db
                .update_task_status(&id, 3, "")
                .await
                .expect("completed");
            let audio = "clip.audio.m4a";
            let partial = format!("{audio}{}", downloader::TEMP_EXT);
            fx.write_existing("clip.ts", b"video");
            fx.write_existing(audio, b"audio");
            fx.write_existing(&partial, b"partial");
            if registered {
                fx.db
                    .record_audio_output(&id, audio)
                    .await
                    .expect("register audio");
                fx.db
                    .record_audio_partial(&id, &partial)
                    .await
                    .expect("register partial");
            }
            // When a different task is using the registered basename, keep both files.
            let other = if registered && batch {
                let mut spec = fx.spec(audio);
                spec.start_paused = true;
                Some(fx.mgr.create_task(spec).await.expect("create audio task"))
            } else {
                None
            };
            if batch {
                let outcome = fx
                    .mgr
                    .delete_tasks_batch(&[id], true)
                    .await
                    .expect("batch delete");
                assert_eq!(outcome.deleted, 1);
            } else {
                fx.mgr.delete_task(&id, true).await.expect("single delete");
            }
            assert!(!fx.dir.join("clip.ts").exists());
            if registered && other.is_none() {
                assert!(!fx.dir.join(audio).exists());
                assert!(!fx.dir.join(&partial).exists());
            } else {
                assert_eq!(
                    std::fs::read(fx.dir.join(audio)).expect("kept audio"),
                    b"audio"
                );
                assert_eq!(
                    std::fs::read(fx.dir.join(&partial)).expect("kept partial"),
                    b"partial"
                );
            }
        }
    }
    std::fs::remove_dir_all(fx.dir).expect("remove fixture");
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
