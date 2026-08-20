//! Windows sparse 存储包装的端到端集成测试（真实 librqbit Session，无网络）。
//!
//! 单测已覆盖 `mark_sparse` 的文件级行为。这里守的是**只有真跑一遍
//! add_torrent 才能证明**的三条线：
//!
//! 1. 生产 add 路径（`build_add_torrent_options`）注入的 sparse 工厂能通过
//!    JSON session 持久化的 TypeId 白名单——冒名失效时 add_torrent 整体
//!    失败（"storages other than FilesystemStorageFactory are not
//!    supported"），BT 下载全线不可用；
//! 2. 初检后 `set_len` 到完整大小的文件带 sparse 属性且**没有**物理预留
//!    全部簇（实际分配远小于逻辑大小）——这是「即点即下、无 VDL 零填充」
//!    的直接证据；
//! 3. `{hash}.bitv` fast-resume 位图照常由持久化层写出——冒名工厂不影响
//!    BitVFactory 面。

#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use fluxdown_engine::bt_downloader::{BtSelectionStrategy, build_add_torrent_options};
use librqbit::{
    AddTorrent, AddTorrentResponse, CreateTorrentOptions, Session, SessionOptions, create_torrent,
};

fn unique_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fluxdown_sparse_add_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// 伪随机可复现内容，避免全零数据让空洞侥幸通过哈希。
fn patterned(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;

/// 从 torrent 字节解析出各文件的相对路径（librqbit 打开文件时相对
/// output_folder 使用的就是这些路径）。
fn relative_paths(torrent_bytes: &[u8]) -> Vec<PathBuf> {
    let parsed = librqbit::torrent_from_bytes(torrent_bytes).unwrap();
    parsed
        .info
        .data
        .validate()
        .unwrap()
        .iter_file_details()
        .map(|fd| fd.filename.to_pathbuf())
        .collect()
}

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn file_attributes(path: &Path) -> u32 {
    let w = wide(path);
    // SAFETY: w 以 NUL 结尾且在调用期间有效。
    unsafe { windows_sys::Win32::Storage::FileSystem::GetFileAttributesW(w.as_ptr()) }
}

/// 文件的实际磁盘占用（已分配簇）。sparse 文件未写区域不占簇，
/// 该值远小于逻辑大小即证明没有整体物理预留。
fn allocated_size(path: &Path) -> u64 {
    let w = wide(path);
    let mut high: u32 = 0;
    // SAFETY: w 以 NUL 结尾；high 指向栈上有效 u32。
    let low = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW(w.as_ptr(), &mut high)
    };
    (u64::from(high) << 32) | u64::from(low)
}

async fn local_session(root: &Path) -> (std::sync::Arc<Session>, PathBuf) {
    let session_dir = root.join("session_out");
    let persist_dir = root.join("session_persist");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::create_dir_all(&persist_dir).unwrap();
    let session = Session::new_with_opts(
        session_dir,
        SessionOptions {
            dht: None,
            // 与生产一致开 JSON 持久化 + fastresume：update_db 的 TypeId
            // 白名单与 BitVFactory 面都必须被真实执行到。
            persistence: Some(librqbit::SessionPersistenceConfig::Json {
                folder: Some(persist_dir.clone()),
            }),
            fastresume: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (session, persist_dir)
}

/// 全新任务：空 staging 起步，初检后文件被 set_len 到完整大小——
/// 必须带 sparse 属性且几乎不占实际磁盘。
#[tokio::test(flavor = "multi_thread")]
async fn fresh_add_creates_sparse_files_without_physical_reservation() {
    let root = unique_dir("fresh");
    let content_dir = root.join("content");
    let stage_dir = root.join("stage");
    std::fs::create_dir_all(&content_dir).unwrap();
    std::fs::create_dir_all(&stage_dir).unwrap();

    let payload_len: usize = 8 * 1024 * 1024;
    std::fs::write(content_dir.join("payload.bin"), patterned(payload_len, 7)).unwrap();
    let torrent = create_torrent(
        &content_dir,
        CreateTorrentOptions {
            name: Some("sparse-fresh"),
            piece_length: Some(16 * 1024),
        },
    )
    .await
    .unwrap();
    let torrent_bytes = torrent.as_bytes().unwrap();
    // 相对路径以 torrent 元数据为准（create_torrent 产物不含种子名目录）。
    let staged = stage_dir.join(relative_paths(&torrent_bytes).pop().unwrap());

    let (session, persist_dir) = local_session(&root).await;

    // 生产 add 选项构建器：Windows 上注入 sparse 工厂。paused 避免无 peer
    // 的 Live 空转，初检（含 set_len）照常执行。
    let mut opts = build_add_torrent_options(
        &BtSelectionStrategy::All,
        stage_dir.to_string_lossy().into_owned(),
        0,
    );
    opts.paused = true;
    let response = session
        .add_torrent(AddTorrent::from_bytes(torrent_bytes), Some(opts))
        .await
        .expect("add_torrent must pass the persistence TypeId whitelist with the sparse factory");
    let handle = match response {
        AddTorrentResponse::Added(_, h) | AddTorrentResponse::AlreadyManaged(_, h) => h,
        AddTorrentResponse::ListOnly(_) => panic!("unexpected list-only response"),
    };

    tokio::time::timeout(Duration::from_secs(120), handle.wait_until_initialized())
        .await
        .expect("initial check timed out")
        .expect("initial check failed");

    assert_eq!(
        std::fs::metadata(&staged).unwrap().len(),
        payload_len as u64,
        "logical size must be set to full length after init"
    );
    assert_ne!(
        file_attributes(&staged) & FILE_ATTRIBUTE_SPARSE_FILE,
        0,
        "staging file must carry the sparse attribute"
    );
    let allocated = allocated_size(&staged);
    assert!(
        allocated < 1024 * 1024,
        "fresh sparse file must not physically reserve the full size (allocated {allocated} bytes)"
    );

    // fast-resume 位图照常由持久化层写出（冒名工厂不影响 BitVFactory）。
    let bitv = persist_dir.join(format!("{}.bitv", handle.info_hash().as_string()));
    assert!(
        bitv.exists(),
        "initial check must persist the {{hash}}.bitv bitfield"
    );

    session.stop().await;
    drop(session);
    let _ = std::fs::remove_dir_all(&root);
}

/// 数据齐全的重添加（跨重启恢复形态）：既有文件被幂等打上 sparse 标记，
/// 初检完成 finished。
#[tokio::test(flavor = "multi_thread")]
async fn readd_with_complete_data_marks_existing_files_and_finishes() {
    let root = unique_dir("readd");
    let content_dir = root.join("content");
    let stage_dir = root.join("stage");
    std::fs::create_dir_all(&content_dir).unwrap();

    let a = patterned(96 * 1024, 1);
    let b = patterned(48 * 1024, 2);
    std::fs::write(content_dir.join("a.bin"), &a).unwrap();
    std::fs::write(content_dir.join("b.bin"), &b).unwrap();
    let torrent = create_torrent(
        &content_dir,
        CreateTorrentOptions {
            name: Some("sparse-readd"),
            piece_length: Some(16 * 1024),
        },
    )
    .await
    .unwrap();
    let torrent_bytes = torrent.as_bytes().unwrap();

    // 预置完整数据到 staging，路径以 torrent 元数据为准。
    let rels = relative_paths(&torrent_bytes);
    let by_name = |name: &str| {
        stage_dir.join(
            rels.iter()
                .find(|p| p.file_name().is_some_and(|n| n == name))
                .unwrap(),
        )
    };
    for (name, data) in [("a.bin", &a), ("b.bin", &b)] {
        let dst = by_name(name);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::write(&dst, data).unwrap();
    }

    let (session, _persist_dir) = local_session(&root).await;

    let mut opts = build_add_torrent_options(
        &BtSelectionStrategy::All,
        stage_dir.to_string_lossy().into_owned(),
        0,
    );
    opts.paused = true;
    let response = session
        .add_torrent(AddTorrent::from_bytes(torrent_bytes), Some(opts))
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
        "complete pre-placed data must verify as finished ({}/{})",
        stats.progress_bytes, stats.total_bytes
    );
    for name in ["a.bin", "b.bin"] {
        assert_ne!(
            file_attributes(&by_name(name)) & FILE_ATTRIBUTE_SPARSE_FILE,
            0,
            "existing file {name} must be idempotently marked sparse"
        );
    }

    session.stop().await;
    drop(session);
    let _ = std::fs::remove_dir_all(&root);
}
