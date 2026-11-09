//! 大任务量下的调度热路径行为测试：排队计数与全量重算一致、满额队列的 drain 不逐项
//! 搬运且保持 FIFO、重名预订在并发下编号最小可用且互不冲突。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    ActiveTaskEntry, BtConfig, Db, DownloadManager, DownloadManagerConfig, EngineEvent, EventSink,
    MAIN_QUEUE_ID, NewTaskSpec, PendingQueue, ProxyConfig, QueuedTask, dedup_filename_sync,
    reserve_deduped_name,
};
use crate::file_exists::OverwritePolicy;

struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: EngineEvent) {}
}

fn queued(task_id: &str, url: &str, queue_id: &str, torrent: bool) -> QueuedTask {
    QueuedTask {
        task_id: task_id.to_string(),
        url: url.to_string(),
        save_dir: String::new(),
        file_name: String::new(),
        segments: 0,
        is_resume: false,
        cookies: String::new(),
        referrer: String::new(),
        hint_file_size: 0,
        torrent_file_bytes: if torrent { vec![1, 2, 3] } else { Vec::new() },
        proxy_url: String::new(),
        user_agent: String::new(),
        queue_id: queue_id.to_string(),
        checksum: String::new(),
        ignore_tls_errors: false,
        extra_headers: std::collections::HashMap::new(),
        selected_file_indices: Vec::new(),
        method: None,
        body: None,
        audio_url: None,
        resolver_plugin_id: String::new(),
        resolved: false,
        range_supported: false,
        resolver_item: String::new(),
        source_url: String::new(),
        resolver_patch: None,
    }
}

fn ids(queue: &PendingQueue) -> Vec<&str> {
    queue.iter().map(|item| item.task_id.as_str()).collect()
}

#[test]
fn pending_counts_match_recount_after_every_mutation() {
    const MAGNET: &str = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
    let mut queue = PendingQueue::default();
    queue.push_back(queued("a", "http://x/a", "main", false));
    queue.push_back(queued("b", MAGNET, "main", false));
    queue.push_back(queued("c", "http://x/c", "slow", false));
    // 携带 .torrent 字节但 URL 不是 BT 链接：算 BT 项，不算 BT 链接项。
    queue.push_front(queued("d", "http://x/d", "slow", true));
    assert_eq!(ids(&queue), ["d", "a", "b", "c"]);
    assert!(queue.counts_match_recount());
    assert_eq!(queue.queued_in("main"), 2);
    assert_eq!(queue.queued_in("slow"), 2);
    assert!(queue.has_bt() && queue.has_bt_url());

    // 删除 a（队列中间）。
    assert_eq!(queue.remove(1).map(|item| item.task_id), Some("a".into()));
    assert!(queue.counts_match_recount());
    assert_eq!(queue.queued_in("main"), 1);

    // 越界删除不改变计数。
    assert!(queue.remove(99).is_none());
    assert!(queue.counts_match_recount());

    // 迁移 b 到 slow。
    assert!(queue.set_task_queue("b", "slow"));
    assert!(!queue.set_task_queue("missing", "slow"));
    assert!(queue.counts_match_recount());
    assert_eq!(queue.queued_in("main"), 0);
    assert_eq!(queue.queued_in("slow"), 3);
    assert_eq!(queue.queue_ids().count(), 1, "empty queues must not linger");

    // 整队改归主队列（删除队列）。
    queue.reassign_queue("slow", "main");
    assert!(queue.counts_match_recount());
    assert_eq!(queue.queued_in("main"), 3);
    assert_eq!(queue.queued_in("slow"), 0);

    // 条件保留。
    queue.retain(|item| item.task_id != "c");
    assert!(queue.counts_match_recount());
    assert_eq!(ids(&queue), ["d", "b"]);

    // 整体摘出 BT 项后队列清空，所有计数归零。
    queue.push_back(queued("e", "http://x/e", "main", false));
    let taken = queue.drain_bt();
    assert_eq!(
        taken
            .iter()
            .map(|item| item.task_id.as_str())
            .collect::<Vec<_>>(),
        ["d", "b"]
    );
    assert_eq!(ids(&queue), ["e"]);
    assert!(queue.counts_match_recount());
    assert!(!queue.has_bt() && !queue.has_bt_url());

    queue.clear();
    assert!(queue.counts_match_recount());
    assert!(queue.is_empty());
    assert_eq!(queue.queue_ids().count(), 0);
}

async fn fixture(max_concurrent: usize) -> (DownloadManager, PathBuf) {
    let dir = std::env::temp_dir().join(format!("fluxdown-scaling-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("isolated download directory");
    let db = Db::connect("sqlite::memory:")
        .await
        .expect("memory database");
    db.seed_builtin_queues().await.expect("builtin queues");
    let manager = DownloadManager::new(
        db,
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
        Arc::new(crate::NoopSelection),
    )
    .expect("download manager");
    (manager, dir)
}

fn occupy(manager: &mut DownloadManager, id: &str, queue_id: &str) {
    manager.active_tasks.insert(
        id.to_string(),
        ActiveTaskEntry {
            token: CancellationToken::new(),
            generation: 0,
            handle: None,
            is_bt: false,
            queue_id: queue_id.to_string(),
        },
    );
}

async fn create(
    manager: &mut DownloadManager,
    dir: &std::path::Path,
    name: &str,
    queue_id: &str,
) -> String {
    manager
        .create_task(NewTaskSpec {
            // 端口 1 无人监听：起跑的任务立即失败，测试不触碰网络。
            url: format!("http://127.0.0.1:1/{name}"),
            save_dir: dir.to_string_lossy().into_owned(),
            file_name: name.to_string(),
            hint_file_size: 1,
            queue_id: queue_id.to_string(),
            ..Default::default()
        })
        .await
        .expect("create task")
}

async fn slow_queue(manager: &mut DownloadManager) -> String {
    manager
        .create_queue("slow".into(), 0, 0, 1, String::new(), 0, String::new())
        .await;
    manager
        .queues
        .iter()
        .find(|(_, queue)| queue.name == "slow")
        .map(|(id, _)| id.clone())
        .expect("slow queue exists")
}

/// 入队 / 暂停 / 删除 / 迁移队列 / 起跑 / 完成之后，增量计数与全量重算一致，
/// `queue.drained` 判定所用的占用集与真实占用一致。
#[tokio::test]
async fn pending_counts_stay_consistent_through_pause_delete_move_start_and_finish() {
    let (mut manager, dir) = fixture(5).await;
    for slot in 0..5 {
        occupy(&mut manager, &format!("occupied-{slot}"), MAIN_QUEUE_ID);
    }
    let slow = slow_queue(&mut manager).await;

    let s0 = create(&mut manager, &dir, "s0.bin", &slow).await;
    let m0 = create(&mut manager, &dir, "m0.bin", MAIN_QUEUE_ID).await;
    let s1 = create(&mut manager, &dir, "s1.bin", &slow).await;
    let m1 = create(&mut manager, &dir, "m1.bin", MAIN_QUEUE_ID).await;
    let s2 = create(&mut manager, &dir, "s2.bin", &slow).await;
    let m2 = create(&mut manager, &dir, "m2.bin", MAIN_QUEUE_ID).await;
    assert_eq!(manager.pending_queue.len(), 6);
    assert!(manager.pending_queue.counts_match_recount());
    assert_eq!(manager.pending_queue.queued_in(&slow), 3);
    assert_eq!(manager.pending_queue.queued_in(MAIN_QUEUE_ID), 3);

    manager.pause_task(&s1).await;
    assert_eq!(manager.pending_queue.len(), 5);
    assert!(manager.pending_queue.counts_match_recount());
    assert_eq!(manager.pending_queue.queued_in(&slow), 2);

    manager.delete_task(&m2, false).await.expect("delete task");
    assert_eq!(manager.pending_queue.len(), 4);
    assert!(manager.pending_queue.counts_match_recount());
    assert_eq!(manager.pending_queue.queued_in(MAIN_QUEUE_ID), 2);

    manager.move_task_to_queue(m0.clone(), slow.clone()).await;
    assert!(manager.pending_queue.counts_match_recount());
    assert_eq!(manager.pending_queue.queued_in(&slow), 3);
    assert_eq!(manager.pending_queue.queued_in(MAIN_QUEUE_ID), 1);

    // 全局槽位腾空后 drain：slow 队列并发上限 1，只起 s0；其余 slow 项原位保留。
    manager.active_tasks.clear();
    manager.drain_queue().await;
    assert!(manager.active_tasks.contains_key(&s0));
    assert!(manager.active_tasks.contains_key(&m1));
    assert!(manager.pending_queue.counts_match_recount());
    assert_eq!(
        ids(&manager.pending_queue),
        [m0.as_str(), s2.as_str()],
        "skipped items keep their FIFO order"
    );
    assert_eq!(manager.pending_queue.queued_in(&slow), 2);
    assert_eq!(manager.pending_queue.queued_in(MAIN_QUEUE_ID), 0);

    // s0 完成（离开活跃集）：slow 队列腾出名额，m0 起跑，s2 继续等待。
    manager.active_tasks.remove(&s0);
    manager.drain_queue().await;
    assert!(manager.active_tasks.contains_key(&m0));
    assert_eq!(ids(&manager.pending_queue), [s2.as_str()]);
    assert!(manager.pending_queue.counts_match_recount());

    // 占用集与真实占用（活跃 ∪ 排队）一致。
    manager.sync_queue_occupancy();
    let mut expected: HashSet<String> = manager
        .active_tasks
        .values()
        .map(|entry| entry.queue_id.clone())
        .collect();
    expected.extend(
        manager
            .pending_queue
            .iter()
            .map(|item| item.queue_id.clone()),
    );
    assert_eq!(manager.occupied_queues, expected);

    manager.shutdown().await;
    std::fs::remove_dir_all(dir).expect("remove isolated download directory");
}

/// 队列级并发上限已满时，drain 在 O(队列数) 内早退：不弹出、不搬运任何排队项，
/// 名额释放后才按 FIFO 启动。
#[tokio::test]
async fn saturated_queue_drain_leaves_the_waiting_buffer_untouched() {
    let (mut manager, dir) = fixture(5).await;
    let slow = slow_queue(&mut manager).await;
    occupy(&mut manager, "occupied-slow", &slow);
    for slot in 0..4 {
        occupy(
            &mut manager,
            &format!("occupied-main-{slot}"),
            MAIN_QUEUE_ID,
        );
    }
    let s0 = create(&mut manager, &dir, "s0.bin", &slow).await;
    let m0 = create(&mut manager, &dir, "m0.bin", MAIN_QUEUE_ID).await;
    let s1 = create(&mut manager, &dir, "s1.bin", &slow).await;
    let m1 = create(&mut manager, &dir, "m1.bin", MAIN_QUEUE_ID).await;
    assert_eq!(manager.pending_queue.len(), 4);

    // 主队列占用者离场：m0 / m1 起跑，被 slow 上限拦住的 s0 / s1 原位保留。
    for slot in 0..4 {
        manager
            .active_tasks
            .remove(&format!("occupied-main-{slot}"));
    }
    manager.drain_queue().await;
    assert!(manager.active_tasks.contains_key(&m0));
    assert!(manager.active_tasks.contains_key(&m1));
    assert_eq!(ids(&manager.pending_queue), [s0.as_str(), s1.as_str()]);
    assert!(manager.pending_queue.counts_match_recount());

    // 只剩被拦住的 slow 项：再次 drain 不触碰缓冲区。
    let storage = manager.pending_queue.as_slices().0.as_ptr();
    manager.drain_queue().await;
    assert_eq!(manager.pending_queue.as_slices().0.as_ptr(), storage);
    assert_eq!(ids(&manager.pending_queue), [s0.as_str(), s1.as_str()]);

    // slow 名额释放：只起队首 s0，s1 继续等待。
    manager.active_tasks.remove("occupied-slow");
    manager.drain_queue().await;
    assert!(manager.active_tasks.contains_key(&s0));
    assert_eq!(ids(&manager.pending_queue), [s1.as_str()]);
    assert!(manager.pending_queue.counts_match_recount());

    manager.shutdown().await;
    std::fs::remove_dir_all(dir).expect("remove isolated download directory");
}

fn touch(dir: &std::path::Path, name: &str) {
    std::fs::write(dir.join(name), b"x").expect("write fixture file");
}

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fluxdown-scaling-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

/// 保存目录里有大量文件时，探测式重名去重与旧「整目录扫描」算法一致：编号最小可用，
/// 临时文件与在途预订都算占用，绝不覆盖既有条目。
#[test]
fn dedup_filename_sync_matches_directory_scan_with_many_entries() {
    let dir = unique_dir("sync-many");
    touch(&dir, "video.mp4");
    for index in 1..=700u32 {
        if index != 500 {
            touch(&dir, &format!("video ({index}).mp4"));
        }
    }
    for index in 0..300u32 {
        touch(&dir, &format!("unrelated-{index}.bin"));
    }
    let never = OverwritePolicy::Never;
    let check = |reserved: &HashSet<PathBuf>, expected: &str| {
        let actual = dedup_filename_sync(&dir, "video.mp4", reserved, &never, false);
        let reference = crate::downloader::dedup_reference_scan(
            &dir,
            "video.mp4",
            reserved,
            &HashSet::new(),
            false,
        );
        assert_eq!(actual, reference, "must match the directory-scan algorithm");
        assert_eq!(actual, expected);
    };

    check(&HashSet::new(), "video (500).mp4");
    // 500 被另一个在途任务预订 → 701。
    let reserved = HashSet::from([dir.join("video (500).mp4.fdownloading")]);
    check(&reserved, "video (701).mp4");
    // 原名空闲时原样返回（目录里的无关文件不影响）。
    assert_eq!(
        dedup_filename_sync(&dir, "fresh.bin", &HashSet::new(), &never, false),
        "fresh.bin"
    );
    std::fs::remove_dir_all(dir).expect("remove fixture dir");
}

/// 同名任务并发预订：所有结果互不相同、不与磁盘条目冲突，并恰为紧随既有编号的
/// 连续最小可用段（每个预订都在锁内核对，冲突自动重试）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reservations_pick_distinct_smallest_free_names() {
    let dir = unique_dir("reserve");
    touch(&dir, "a.bin");
    for index in 1..=40u32 {
        touch(&dir, &format!("a ({index}).bin"));
    }
    let reserved: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut handles = Vec::new();
    for _ in 0..24 {
        let reserved = Arc::clone(&reserved);
        let dir = dir.clone();
        handles.push(tokio::spawn(async move {
            reserve_deduped_name(&reserved, &dir, "a.bin", &OverwritePolicy::Never, false)
                .await
                .expect("reservation must not panic")
        }));
    }
    let mut names = Vec::new();
    for handle in handles {
        let (name, temp) = handle.await.expect("reservation task");
        assert_eq!(temp, dir.join(format!("{name}.fdownloading")));
        names.push(name);
    }
    let distinct: HashSet<&String> = names.iter().collect();
    assert_eq!(distinct.len(), names.len(), "reservations must be unique");
    let expected: HashSet<String> = (41..=64u32)
        .map(|index| format!("a ({index}).bin"))
        .collect();
    assert_eq!(names.iter().cloned().collect::<HashSet<_>>(), expected);
    assert!(names.iter().all(|name| !dir.join(name).exists()));
    assert_eq!(
        reserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        24
    );
    std::fs::remove_dir_all(dir).expect("remove fixture dir");
}
