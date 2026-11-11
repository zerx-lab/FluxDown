//! BT 存储操作端到端：重命名文件（会话里 / 不在会话里）、强制重新校验（下载中 / 离线）、
//! 移动数据（下载中 / 做种中 / 中断后下次启动接着搬）。

use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use fluxdown_engine::bt_move::{MoveJournal, journal_key};
use fluxdown_engine::bt_seeding::SEEDING_STATUS_ACTIVE;
use fluxdown_engine::bt_task_files::{BtTaskFiles, BtTaskOpError};

use super::swarm::{Downloader, PIECE_LEN, Seeder, TORRENT_NAME, Tracker, patterned};

/// 做种端限速（B/s）：每秒三块，下载慢到测试能在途中动手。
const SLOW: u32 = 48 * 1024;
const PIECE: u64 = PIECE_LEN as u64;

fn pack() -> Vec<(&'static str, Vec<u8>)> {
    let big = 24 * PIECE_LEN as usize;
    vec![
        ("a.bin", patterned(big, 1)),
        ("sub/b.bin", patterned(big, 2)),
        ("c.txt", patterned(3 * 1024 + 5, 3)),
    ]
}

fn path_of(files: &BtTaskFiles, index: u32) -> &str {
    files
        .files
        .iter()
        .find(|file| file.index == index)
        .unwrap_or_else(|| panic!("file {index} is not listed: {files:?}"))
        .path
        .as_str()
}

fn downloaded(files: &BtTaskFiles, index: u32) -> u64 {
    files
        .files
        .iter()
        .find(|file| file.index == index)
        .unwrap_or_else(|| panic!("file {index} is not listed: {files:?}"))
        .downloaded
}

/// 把整个文件写成零、长度不变（模拟磁盘上的数据坏了）。哪些片已下完取决于选片顺序，
/// 整个文件写坏才能保证命中已校验的片。
fn zero_file(path: &Path) {
    let len = std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()))
        .len();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    file.seek(SeekFrom::Start(0)).expect("seek");
    file.write_all(&vec![0u8; usize::try_from(len).expect("fits")])
        .expect("zero the file");
    file.sync_all().expect("sync");
}

async fn wait_until<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(60);
    while !cond().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renamed_files_land_under_their_new_names() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, Some(SLOW)).await;
    let (a, b, c) = (
        seeder.index_of("a.bin"),
        seeder.index_of("sub/b.bin"),
        seeder.index_of("c.txt"),
    );
    let mut leech = Downloader::start(&tracker).await;
    let id = leech.add(&seeder, &[], false).await;
    leech
        .wait_for_files(&id, "a.bin to make progress", |files| {
            files.editable && downloaded(files, a) >= PIECE
        })
        .await;

    // 会话里：librqbit 当场挪文件。
    let manager = &mut leech.engine.manager;
    manager
        .rename_bt_file(&id, b, "Renamed\\B.bin")
        .await
        .expect("rename b.bin while downloading");
    assert_eq!(path_of(&leech.files(&id).await, b), "Renamed/B.bin");
    let manager = &mut leech.engine.manager;
    for bad in ["../escape.bin", "", "x/./y"] {
        assert!(
            matches!(
                manager.rename_bt_file(&id, c, bad).await,
                Err(BtTaskOpError::InvalidArgument(_))
            ),
            "{bad:?}"
        );
    }
    // 与别的文件（含刚改过名的）撞名，不区分大小写。
    assert!(matches!(
        manager.rename_bt_file(&id, c, "renamed/b.bin").await,
        Err(BtTaskOpError::InvalidArgument(_))
    ));
    assert!(matches!(
        manager.rename_bt_file("missing", c, "x").await,
        Err(BtTaskOpError::NotFound)
    ));

    // 不在会话里（暂停后重启）：直接挪 staging 里的文件，下次启动带上重命名表。
    leech.engine.manager.pause_task(&id).await;
    let paused = leech.wait_done(&id).await;
    assert_eq!(paused.status, 2, "{}", paused.error_message);
    let mut leech = leech.restart(&tracker).await;
    assert_eq!(path_of(&leech.files(&id).await, b), "Renamed/B.bin");
    leech
        .engine
        .manager
        .rename_bt_file(&id, c, "docs/说明 c.txt")
        .await
        .expect("rename c.txt while the task is not running");
    let stage = leech.stage_dir(&id).await;
    assert!(
        stage.join("docs/说明 c.txt").is_file() && !stage.join("c.txt").exists(),
        "the staged file moved"
    );
    // 改回原名就是撤销。
    leech
        .engine
        .manager
        .rename_bt_file(&id, c, "c.txt")
        .await
        .expect("rename back");
    leech
        .engine
        .manager
        .rename_bt_file(&id, c, "docs/说明 c.txt")
        .await
        .expect("rename again");

    leech.engine.manager.resume_task(&id).await;
    seeder.unthrottle();
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(task.file_name, TORRENT_NAME);
    assert_eq!(
        leech.landed(&task, "a.bin").await.as_deref(),
        Some(seeder.content_of("a.bin"))
    );
    assert_eq!(
        leech.landed(&task, "Renamed/B.bin").await.as_deref(),
        Some(seeder.content_of("sub/b.bin"))
    );
    assert_eq!(
        leech.landed(&task, "docs/说明 c.txt").await.as_deref(),
        Some(seeder.content_of("c.txt"))
    );
    assert_eq!(leech.landed(&task, "sub/b.bin").await, None);
    assert_eq!(leech.landed(&task, "c.txt").await, None);
    // 已完成的任务不能再改名。
    assert!(matches!(
        leech.engine.manager.rename_bt_file(&id, a, "z.bin").await,
        Err(BtTaskOpError::StateConflict(_))
    ));
    leech.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn force_recheck_while_running_downloads_the_damaged_piece_again() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, Some(SLOW)).await;
    let a = seeder.index_of("a.bin");
    let mut leech = Downloader::start(&tracker).await;
    let id = leech.add(&seeder, &[], false).await;
    leech
        .wait_for_files(&id, "a.bin to have pieces", |files| {
            downloaded(files, a) >= 3 * PIECE
        })
        .await;

    // 把已校验的数据写坏。新加的任务完成时不再全量重哈希（位图都来自读盘 / 写盘校验），
    // 不重新校验的话坏数据会原样落盘。
    zero_file(&leech.stage_dir(&id).await.join("a.bin"));
    leech
        .engine
        .manager
        .recheck_bt_task(&id)
        .await
        .expect("recheck while running");
    seeder.unthrottle();
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    for rel in ["a.bin", "sub/b.bin", "c.txt"] {
        assert_eq!(
            leech.landed(&task, rel).await.as_deref(),
            Some(seeder.content_of(rel)),
            "{rel}"
        );
    }
    assert!(matches!(
        leech.engine.manager.recheck_bt_task(&id).await,
        Err(BtTaskOpError::StateConflict(_))
    ));
    assert!(matches!(
        leech.engine.manager.recheck_bt_task("missing").await,
        Err(BtTaskOpError::NotFound)
    ));
    leech.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn force_recheck_of_a_paused_task_saves_the_checked_progress() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, Some(SLOW)).await;
    let a = seeder.index_of("a.bin");
    let mut leech = Downloader::start(&tracker).await;
    let id = leech.add(&seeder, &[], false).await;
    leech
        .wait_for_files(&id, "a.bin to have pieces", |files| {
            downloaded(files, a) >= 3 * PIECE
        })
        .await;
    leech.engine.manager.pause_task(&id).await;
    let paused = leech.wait_done(&id).await;
    assert_eq!(paused.status, 2, "{}", paused.error_message);

    // 重启后任务不在会话里：以暂停态加入会话校验，校验出的进度落库（坏掉的片不算），仍是暂停。
    let mut leech = leech.restart(&tracker).await;
    let before = downloaded(&leech.files(&id).await, a);
    assert!(
        before >= 3 * PIECE,
        "a.bin has pieces before the damage: {before}"
    );
    zero_file(&leech.stage_dir(&id).await.join("a.bin"));
    leech
        .engine
        .manager
        .recheck_bt_task(&id)
        .await
        .expect("recheck a paused task");
    let db_progress = || {
        let db = leech.engine.db.clone();
        let id = id.clone();
        async move {
            db.load_task_by_id(&id)
                .await
                .expect("load")
                .expect("task")
                .downloaded_bytes
        }
    };
    let files_total = |files: &BtTaskFiles| {
        files
            .files
            .iter()
            .map(|file| i64::try_from(file.downloaded).expect("fits"))
            .sum::<i64>()
    };
    // 校验结束后进度落库一次：库里的值等于句柄校验出的各文件之和；写坏的 a.bin 一片不剩。
    let mut checked = leech.files(&id).await;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !(downloaded(&checked, a) == 0 && files_total(&checked) == db_progress().await) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the checked progress to be saved: {checked:?}, saved {}",
            db_progress().await
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        checked = leech.files(&id).await;
    }
    assert_eq!(leech.task(&id).await.status, 2, "still paused");
    // 校验中 / 校验后再来一次也行（会话里的暂停句柄当场重新校验）。
    leech
        .engine
        .manager
        .recheck_bt_task(&id)
        .await
        .expect("recheck the paused handle again");

    leech.engine.manager.resume_task(&id).await;
    seeder.unthrottle();
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    for rel in ["a.bin", "sub/b.bin", "c.txt"] {
        assert_eq!(
            leech.landed(&task, rel).await.as_deref(),
            Some(seeder.content_of(rel)),
            "{rel}"
        );
    }
    leech.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moving_a_download_and_a_seed_keeps_them_going() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, Some(SLOW)).await;
    let a = seeder.index_of("a.bin");
    let mut leech = Downloader::start(&tracker).await;
    let id = leech.add(&seeder, &[], false).await;
    leech
        .wait_for_files(&id, "a.bin to make progress", |files| {
            downloaded(files, a) >= PIECE
        })
        .await;
    let old_dir = leech.work.clone();
    let old_stage = leech.stage_dir(&id).await;

    // 拒绝：相对路径、搬进自己的数据里。目标与当前相同什么都不做。
    for bad in ["relative/dir".into(), old_stage.join("inside")] {
        assert!(
            matches!(
                leech.move_storage(&id, &bad).await,
                Err(BtTaskOpError::InvalidArgument(_))
            ),
            "{bad:?}"
        );
    }
    leech
        .move_storage(&id, &old_dir)
        .await
        .expect("same folder");
    assert_eq!(leech.task(&id).await.save_dir, old_dir.to_string_lossy());

    // 下载中：暂停、搬走 staging、在新目录继续下载。
    let moved_dir = old_dir.join("moved");
    leech
        .move_storage(&id, &moved_dir)
        .await
        .expect("move a running download");
    let moved = leech.task(&id).await;
    assert_eq!(moved.save_dir, moved_dir.to_string_lossy());
    assert!(!old_stage.exists(), "nothing left at the old place");
    assert!(leech.stage_dir(&id).await.join("a.bin").is_file());
    seeder.unthrottle();
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(task.save_dir, moved_dir.to_string_lossy());
    for rel in ["a.bin", "sub/b.bin", "c.txt"] {
        assert_eq!(
            leech.landed(&task, rel).await.as_deref(),
            Some(seeder.content_of(rel)),
            "{rel}"
        );
    }

    // 做种中：目标里已有同名条目时什么都不动。
    let seed_dir = old_dir.join("seed here");
    std::fs::create_dir_all(seed_dir.join(TORRENT_NAME)).expect("taken name");
    assert!(matches!(
        leech.move_storage(&id, &seed_dir).await,
        Err(BtTaskOpError::Conflict(_))
    ));
    assert!(moved_dir.join(TORRENT_NAME).join("a.bin").is_file());
    std::fs::remove_dir(seed_dir.join(TORRENT_NAME)).expect("free the name");

    let seeding = |leech: &Downloader| {
        let db = leech.engine.db.clone();
        let id = id.clone();
        async move {
            db.load_task_by_id(&id)
                .await
                .expect("load")
                .expect("task")
                .seeding_status
                == SEEDING_STATUS_ACTIVE
        }
    };
    wait_until("the task to seed", || seeding(&leech)).await;
    leech
        .move_storage(&id, &seed_dir)
        .await
        .expect("move a seeding task");
    let task = leech.task(&id).await;
    assert_eq!(task.save_dir, seed_dir.to_string_lossy());
    assert!(!moved_dir.join(TORRENT_NAME).exists());
    for rel in ["a.bin", "sub/b.bin", "c.txt"] {
        assert_eq!(
            leech.landed(&task, rel).await.as_deref(),
            Some(seeder.content_of(rel)),
            "{rel}"
        );
    }
    // 搬完重新挂载做种（本地数据校验通过后）。
    wait_until("the moved task to seed again", || seeding(&leech)).await;
    leech.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupted_move_is_finished_on_the_next_start() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, Some(SLOW)).await;
    let a = seeder.index_of("a.bin");
    let mut leech = Downloader::start(&tracker).await;
    let id = leech.add(&seeder, &[], false).await;
    leech
        .wait_for_files(&id, "a.bin to make progress", |files| {
            downloaded(files, a) >= PIECE
        })
        .await;
    leech.engine.manager.pause_task(&id).await;
    let paused = leech.wait_done(&id).await;
    assert_eq!(paused.status, 2, "{}", paused.error_message);
    let have = downloaded(&leech.files(&id).await, a);

    // 模拟搬到一半进程退出：日志已写，staging 里的一个文件已在新目录。
    let from = leech.work.clone();
    let to = from.join("target");
    let stage_name = format!(".bt_stage_{id}");
    std::fs::create_dir_all(to.join(&stage_name)).expect("target stage");
    std::fs::rename(
        from.join(&stage_name).join("c.txt"),
        to.join(&stage_name).join("c.txt"),
    )
    .expect("half of the move");
    let journal = MoveJournal {
        from: from.to_string_lossy().into_owned(),
        to: to.to_string_lossy().into_owned(),
        names: vec![stage_name.clone()],
        reseed: false,
    };
    leech
        .engine
        .db
        .set_config(&journal_key(&id), &journal.encode())
        .await
        .expect("write journal");

    let mut leech = leech.restart(&tracker).await;
    leech.engine.manager.load_and_send_all_tasks().await;
    let task = leech.task(&id).await;
    assert_eq!(task.save_dir, to.to_string_lossy());
    assert!(!from.join(&stage_name).exists());
    assert!(to.join(&stage_name).join("a.bin").is_file());
    assert!(to.join(&stage_name).join("c.txt").is_file());
    assert_eq!(
        leech
            .engine
            .db
            .get_config(&journal_key(&id))
            .await
            .expect("read"),
        None,
        "the journal is gone"
    );
    assert_eq!(
        downloaded(&leech.files(&id).await, a),
        have,
        "the progress survived the move"
    );

    leech.engine.manager.resume_task(&id).await;
    seeder.unthrottle();
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    for rel in ["a.bin", "sub/b.bin", "c.txt"] {
        assert_eq!(
            leech.landed(&task, rel).await.as_deref(),
            Some(seeder.content_of(rel)),
            "{rel}"
        );
    }
    leech.shutdown().await;
}
