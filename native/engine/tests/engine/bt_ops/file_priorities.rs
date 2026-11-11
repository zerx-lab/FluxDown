//! 文件优先级端到端：下载途中改选择 / 级别后，完成收尾按新的选择落盘；不在会话里的任务
//! 改动只落库、下次启动带上；重启后的文件表按快速恢复位图给出进度。

use fluxdown_engine::bt_task_files::{BtTaskFiles, BtTaskOpError};

use super::swarm::{Downloader, PIECE_LEN, Seeder, Tracker, patterned};

/// 做种端限速（B/s）：每秒三块，下载慢到测试能在途中动手。
const SLOW: u32 = 48 * 1024;
const PIECE: u64 = PIECE_LEN as u64;

/// 两个大文件（其一在子目录里）+ 一个长度不是整片的小文件。
fn pack() -> Vec<(&'static str, Vec<u8>)> {
    let big = 24 * PIECE_LEN as usize;
    vec![
        ("a.bin", patterned(big, 1)),
        ("sub/b.bin", patterned(big, 2)),
        ("c.txt", patterned(3 * 1024 + 5, 3)),
    ]
}

fn priority(files: &BtTaskFiles, index: u32) -> u8 {
    files
        .files
        .iter()
        .find(|file| file.index == index)
        .unwrap_or_else(|| panic!("file {index} is not listed: {files:?}"))
        .priority
}

fn downloaded(files: &BtTaskFiles, index: u32) -> u64 {
    files
        .files
        .iter()
        .find(|file| file.index == index)
        .unwrap_or_else(|| panic!("file {index} is not listed: {files:?}"))
        .downloaded
}

fn selection(mut indices: Vec<u32>) -> Option<Vec<i32>> {
    indices.sort_unstable();
    Some(
        indices
            .into_iter()
            .map(|index| i32::try_from(index).expect("file index"))
            .collect(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raising_a_skipped_file_mid_download_lands_it_on_disk() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, Some(SLOW)).await;
    let (a, b, c) = (
        seeder.index_of("a.bin"),
        seeder.index_of("sub/b.bin"),
        seeder.index_of("c.txt"),
    );
    let mut leech = Downloader::start(&tracker).await;
    let id = leech.add(&seeder, &[a], false).await;

    let before = leech
        .wait_for_files(&id, "a.bin to make progress", |files| {
            files.editable && downloaded(files, a) >= PIECE
        })
        .await;
    assert_eq!(
        (
            priority(&before, a),
            priority(&before, b),
            priority(&before, c)
        ),
        (1, 0, 0)
    );

    leech
        .engine
        .manager
        .set_task_file_priorities(&id, &[(b, 6)])
        .await
        .expect("raise b.bin");
    assert_eq!(priority(&leech.files(&id).await, b), 6);
    assert_eq!(
        leech.engine.db.load_bt_selected_files(&id).await.unwrap(),
        selection(vec![a, b])
    );

    seeder.unthrottle();
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(
        leech.landed(&task, "a.bin").await.as_deref(),
        Some(seeder.content_of("a.bin"))
    );
    assert_eq!(
        leech.landed(&task, "sub/b.bin").await.as_deref(),
        Some(seeder.content_of("sub/b.bin")),
        "the file raised mid-download is moved out of staging with the rest"
    );
    assert_eq!(leech.landed(&task, "c.txt").await, None);

    // 完成后只读：文件表不可改，改动被拒且不动选择。
    let done = leech.files(&id).await;
    assert!(!done.editable);
    assert_eq!(downloaded(&done, b), 24 * PIECE);
    assert!(matches!(
        leech
            .engine
            .manager
            .set_task_file_priorities(&id, &[(c, 1)])
            .await,
        Err(BtTaskOpError::StateConflict(_))
    ));
    assert_eq!(
        leech.engine.db.load_bt_selected_files(&id).await.unwrap(),
        selection(vec![a, b])
    );
    leech.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lowering_a_file_to_skip_mid_download_finishes_without_it() {
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
        .wait_for_files(&id, "the download to make progress", |files| {
            files.editable && downloaded(files, a) + downloaded(files, b) >= PIECE
        })
        .await;
    leech
        .engine
        .manager
        .set_task_file_priorities(&id, &[(b, 0)])
        .await
        .expect("skip b.bin");
    assert_eq!(priority(&leech.files(&id).await, b), 0);
    assert_eq!(
        leech.engine.db.load_bt_selected_files(&id).await.unwrap(),
        selection(vec![a, c])
    );

    seeder.unthrottle();
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(
        leech.landed(&task, "a.bin").await.as_deref(),
        Some(seeder.content_of("a.bin"))
    );
    assert_eq!(
        leech.landed(&task, "c.txt").await.as_deref(),
        Some(seeder.content_of("c.txt"))
    );
    assert_eq!(
        leech.landed(&task, "sub/b.bin").await,
        None,
        "a file skipped mid-download does not land, even partially"
    );
    leech.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changes_to_a_task_that_is_not_running_apply_on_its_next_start() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, Some(SLOW)).await;
    let (a, b, c) = (
        seeder.index_of("a.bin"),
        seeder.index_of("sub/b.bin"),
        seeder.index_of("c.txt"),
    );
    let (len_a, len_c) = (
        seeder.content_of("a.bin").len(),
        seeder.content_of("c.txt").len(),
    );
    let mut leech = Downloader::start(&tracker).await;
    let id = leech.add(&seeder, &[], true).await;

    // 还没启动过：文件表来自落库的种子，全部「普通」、进度为 0、可改。
    let idle = leech.files(&id).await;
    assert!(idle.editable);
    assert_eq!(idle.files.len(), 3);
    for file in &idle.files {
        assert_eq!((file.priority, file.downloaded), (1, 0), "{file:?}");
    }
    assert_eq!(
        idle.files
            .iter()
            .find(|file| file.index == b)
            .map(|file| file.path.as_str()),
        Some("sub/b.bin")
    );

    // 无效改动整批拒绝。
    let manager = &leech.engine.manager;
    for bad in [
        vec![(c, 7), (99, 1)],
        vec![(a, 8)],
        vec![(a, 0), (b, 0), (c, 0)],
    ] {
        assert!(
            matches!(
                manager.set_task_file_priorities(&id, &bad).await,
                Err(BtTaskOpError::InvalidArgument(_))
            ),
            "{bad:?}"
        );
    }
    assert_eq!(priority(&leech.files(&id).await, c), 1);
    assert!(matches!(
        manager.set_task_file_priorities("missing", &[(a, 6)]).await,
        Err(BtTaskOpError::NotFound)
    ));
    assert!(matches!(
        manager.bt_task_files("missing").await,
        Err(BtTaskOpError::NotFound)
    ));

    manager
        .set_task_file_priorities(&id, &[(b, 0), (c, 7)])
        .await
        .expect("skip b.bin and raise c.txt");
    let changed = leech.files(&id).await;
    assert_eq!(
        (
            priority(&changed, a),
            priority(&changed, b),
            priority(&changed, c)
        ),
        (1, 0, 7)
    );
    assert_eq!(
        leech.task(&id).await.total_bytes,
        i64::try_from(len_a + len_c).unwrap(),
        "the task size follows the selection"
    );

    // 启动后：会话里的句柄带着落库的选择与级别。
    leech.engine.manager.resume_task(&id).await;
    let live = leech
        .wait_for_files(&id, "a.bin to make progress", |files| {
            downloaded(files, a) >= PIECE
        })
        .await;
    assert_eq!(
        (priority(&live, a), priority(&live, b), priority(&live, c)),
        (1, 0, 7)
    );

    seeder.unthrottle();
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(
        leech.landed(&task, "a.bin").await.as_deref(),
        Some(seeder.content_of("a.bin"))
    );
    assert_eq!(
        leech.landed(&task, "c.txt").await.as_deref(),
        Some(seeder.content_of("c.txt"))
    );
    assert_eq!(leech.landed(&task, "sub/b.bin").await, None);
    leech.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_progress_survives_a_restart_without_resuming() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, Some(SLOW)).await;
    let (a, b) = (seeder.index_of("a.bin"), seeder.index_of("sub/b.bin"));
    let mut leech = Downloader::start(&tracker).await;
    let id = leech.add(&seeder, &[a], false).await;
    leech
        .wait_for_files(&id, "two pieces of a.bin", |files| {
            downloaded(files, a) >= 2 * PIECE
        })
        .await;
    leech.engine.manager.pause_task(&id).await;
    let paused = leech.wait_done(&id).await;
    assert_eq!(
        paused.status, 2,
        "paused, not failed: {}",
        paused.error_message
    );

    // 重启后任务不在会话里：进度来自快速恢复位图，选择与可改状态来自库。
    let leech = leech.restart(&tracker).await;
    let stored = leech.files(&id).await;
    assert!(stored.editable);
    assert_eq!((priority(&stored, a), priority(&stored, b)), (1, 0));
    let have = downloaded(&stored, a);
    assert!(
        (2 * PIECE..24 * PIECE).contains(&have),
        "progress of a.bin read from the bitfield: {have}"
    );
    assert_eq!(have % PIECE, 0, "whole pieces only");
    assert_eq!(downloaded(&stored, b), 0);
    leech.shutdown().await;
}
