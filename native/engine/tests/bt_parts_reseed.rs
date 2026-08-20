//! BT 部分选择续种的端到端集成测试（真实 librqbit Session，无网络依赖）。
//!
//! 单测已覆盖 parts 边车的格式与边界区间数学。这里补的是**只有真跑一遍
//! librqbit 初始校验才能证明**的那一段：
//!
//! ```text
//! 多文件 torrent 只选一个文件 → 完成后扁平重命名落盘 → 写 parts 边车
//!   → 以 PartsSeedStorageFactory 重新 add（paused，模拟重启续种）
//!   → librqbit 全量初检通过（stats.finished）
//!   → save_dir 里不出现任何未选文件（不再重建 0 字节占位）
//! ```
//!
//! 初检会逐 piece 重哈希：选中文件的字节来自边车映射的最终路径（重命名
//! 后的名字，与 torrent 内部名不同），跨文件边界 piece 中未选文件的字节
//! 来自边车 blob——任何一环断了（重建占位文件、边界字节丢失、路径映射
//! 失效），`finished` 都到不了 true 或目录断言立刻红。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use fluxdown_engine::bt_partfile::{
    PartsFileMeta, SidecarWriteRequest, load_seed_factory, write_sidecar,
};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, CreateTorrentOptions, Session,
    SessionOptions, create_torrent,
};

fn unique_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fluxdown_parts_reseed_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// 伪随机可复现内容，避免全零文件让「零填充空洞」侥幸通过哈希。
fn patterned(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn reseed_with_parts_sidecar_passes_check_without_recreating_files() {
    let root = unique_dir("e2e");
    let content_dir = root.join("content");
    let save_dir = root.join("save");
    let session_dir = root.join("session_out");
    std::fs::create_dir_all(&content_dir).unwrap();
    std::fs::create_dir_all(&save_dir).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();

    // 三个文件跨越 16 KiB piece 边界：b.bin 的首尾 piece 分别与 a.bin 尾部、
    // c.bin 头部共享。
    let a = patterned(20_000, 1);
    let b = patterned(30_000, 2);
    let c = patterned(10_000, 3);
    std::fs::write(content_dir.join("a.bin"), &a).unwrap();
    std::fs::write(content_dir.join("b.bin"), &b).unwrap();
    std::fs::write(content_dir.join("c.bin"), &c).unwrap();

    let piece_length: u32 = 16 * 1024;
    let torrent = create_torrent(
        &content_dir,
        CreateTorrentOptions {
            name: Some("parts-e2e"),
            trackers: Vec::new(),
            piece_length: Some(piece_length),
        },
        &librqbit::spawn_utils::BlockingSpawner::new(1),
    )
    .await
    .unwrap();
    let torrent_bytes = torrent.as_bytes().unwrap();

    // 从 torrent 元数据（而非源目录遍历顺序）重建 file_id 顺序与偏移。
    let parsed = librqbit::torrent_from_bytes(&torrent_bytes).unwrap();
    let info = parsed.info.data.validate().unwrap();
    let mut files: Vec<PartsFileMeta> = Vec::new();
    let mut offset: u64 = 0;
    let pl = u64::from(piece_length);
    for fd in info.iter_file_details() {
        let len = fd.len;
        let piece_range = if len > 0 {
            u32::try_from(offset / pl).unwrap()..u32::try_from((offset + len - 1) / pl + 1).unwrap()
        } else {
            let p = u32::try_from(offset / pl).unwrap();
            p..p
        };
        files.push(PartsFileMeta {
            relative_path: fd.filename.to_pathbuf(),
            len,
            offset_in_torrent: offset,
            piece_range,
            padding: fd.attrs().padding,
        });
        offset += len;
    }
    let total_length = offset;
    // create_torrent follows the filesystem iterator order, which is not stable
    // across platforms. Select the metadata's middle file so it always has two
    // neighbours and therefore exercises both boundary pieces.
    let selected_id = files.len() / 2;
    let selected_source = content_dir.join(&files[selected_id].relative_path);

    // Simulate completion by flattening and renaming the selected file. Keep the
    // staging byproducts so the sidecar can extract neighbouring boundary bytes.
    let final_path = save_dir.join("selected_renamed.bin");
    std::fs::copy(&selected_source, &final_path).unwrap();

    let sidecar = root.join("task-e2e.parts");
    let segments = write_sidecar(&SidecarWriteRequest {
        sidecar_path: sidecar.clone(),
        info_hash_hex: String::new(),
        piece_length,
        total_length,
        files: files.clone(),
        selected: vec![(selected_id, final_path.clone())],
        save_dir: save_dir.clone(),
        stage_dir: content_dir.clone(),
    })
    .unwrap();
    // The selected middle file shares one boundary piece with each neighbour.
    assert_eq!(segments, 2, "expected boundary bytes from both neighbours");

    let factory = load_seed_factory(&sidecar, &save_dir).unwrap().unwrap();

    // 与 readd_for_seeding 相同的 add 选项（paused + only_files + 自定义
    // storage），Session 本地化：无 DHT、无监听。持久化与生产一致开 JSON
    // ——它的 update_db 按 TypeId 白名单只认 FilesystemStorageFactory，
    // 自定义 storage 必须靠 is_type_id 冒名才能通过，本测试即守这条线
    //（persistence: None 会漏掉该拒绝路径）。
    let persist_dir = root.join("session_persist");
    std::fs::create_dir_all(&persist_dir).unwrap();
    let session = Session::new_with_opts(
        session_dir.clone(),
        SessionOptions {
            dht: None,
            persistence: Some(librqbit::SessionPersistenceConfig::Json {
                folder: Some(persist_dir),
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let response = session
        .add_torrent(
            AddTorrent::from_bytes(torrent_bytes.clone()),
            Some(AddTorrentOptions {
                overwrite: true,
                paused: true,
                output_folder: Some(save_dir.to_string_lossy().into_owned()),
                only_files: Some(vec![selected_id]),
                storage_factory: Some(factory.into_boxed()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let handle = match response {
        AddTorrentResponse::Added(_, h) | AddTorrentResponse::AlreadyManaged(_, h) => h,
        AddTorrentResponse::ListOnly(_) => panic!("unexpected list-only response"),
    };

    tokio::time::timeout(Duration::from_secs(120), handle.wait_until_initialized())
        .await
        .expect("initial check timed out")
        .expect("initial check failed");

    let stats = handle.stats();
    assert!(
        stats.finished,
        "selected data must verify as complete via parts sidecar (progress {}/{})",
        stats.progress_bytes, stats.total_bytes
    );

    // 核心断言：save_dir 里只有重命名后的选中文件，未选文件没有被重建。
    let entries: HashSet<String> = std::fs::read_dir(&save_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        HashSet::from(["selected_renamed.bin".to_string()]),
        "unselected files must not be recreated in save_dir"
    );

    session.stop().await;
    drop(session);
    let _ = std::fs::remove_dir_all(&root);
}
