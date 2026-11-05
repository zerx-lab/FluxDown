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
    fx2.mgr.delete_task(&id2, true).await;
    assert_eq!(
        std::fs::read(fx2.dir.join("b.bin")).expect("adopted file survives"),
        b"keep me"
    );
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
        dedup_filename_sync(&dir, "clip.ts", &none, &OverwritePolicy::Never, true),
        "clip (1).ts"
    );
    assert_eq!(
        dedup_filename_sync(&dir, "clip.ts", &none, &OverwritePolicy::Never, false),
        "clip.ts"
    );
    assert_eq!(
        dedup_filename_sync(&dir, "clip.ts", &none, &OverwritePolicy::Any, true),
        "clip.ts"
    );
    std::fs::write(dir.join("clip (1).mp4"), b"old").expect("write numbered mp4");
    assert_eq!(
        dedup_filename_sync(&dir, "clip.ts", &none, &OverwritePolicy::Never, true),
        "clip (2).ts"
    );
    std::fs::remove_dir_all(&dir).expect("cleanup");
}
