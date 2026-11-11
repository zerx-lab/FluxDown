//! BitTorrent v2 / 混合种子端到端：引擎下载到完成、完成时逐片复核（v2 = 逐文件 merkle，纯 v2
//! 种子每个文件从分片边界开始、末片较短）、落地内容逐字节一致；只选部分文件时（parts 边车 +
//! 分片对齐的布局）同样正确；之后的完整性校验也按 v2 判定为完好。

use fluxdown_engine::bt_verify::BtIntegrityStatus;
use librqbit::TorrentVersion;

use super::swarm::{Downloader, PIECE_LEN, Seeder, Tracker, patterned};

/// 大文件长度不是整片（v2 末片较短 / 混合种子补 padding），另有一个小于一片的文件
/// （v2 只按 pieces root 校验）与子目录。
fn pack() -> Vec<(&'static str, Vec<u8>)> {
    let piece = PIECE_LEN as usize;
    vec![
        ("a.bin", patterned(9 * piece + 100, 1)),
        ("sub/b.bin", patterned(3 * piece + 7, 2)),
        ("c.txt", patterned(3 * 1024 + 5, 3)),
    ]
}

async fn assert_intact(leech: &Downloader, task_id: &str) {
    let check = leech
        .engine
        .manager
        .bt_integrity_check_future(task_id)
        .await
        .expect("integrity check starts");
    let report = check.await.expect("integrity check runs");
    assert_eq!(report.status, BtIntegrityStatus::Intact, "{report:?}");
    assert_eq!(report.bad_pieces, 0, "{report:?}");
    assert!(report.verified_pieces > 0, "{report:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v2_and_hybrid_torrents_download_verify_and_land_byte_identical() {
    for version in [TorrentVersion::V2, TorrentVersion::Hybrid] {
        let tracker = Tracker::start().await;
        let seeder = Seeder::start_versioned(&pack(), &tracker, None, version).await;
        let mut leech = Downloader::start(&tracker).await;
        let id = leech.add(&seeder, &[], false).await;
        let task = leech.wait_done(&id).await;
        assert_eq!(
            (task.status, task.error_message.as_str()),
            (3, ""),
            "{version:?}"
        );
        for (rel, bytes) in seeder
            .files
            .iter()
            .filter(|(rel, _)| !rel.starts_with(".pad"))
        {
            assert_eq!(
                leech.landed(&task, rel).await.as_deref(),
                Some(bytes.as_slice()),
                "{version:?} {rel}"
            );
        }
        assert_intact(&leech, &id).await;
        leech.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v2_and_hybrid_partial_selection_lands_only_the_chosen_file() {
    for version in [TorrentVersion::V2, TorrentVersion::Hybrid] {
        let tracker = Tracker::start().await;
        let seeder = Seeder::start_versioned(&pack(), &tracker, None, version).await;
        let b = seeder.index_of("sub/b.bin");
        let mut leech = Downloader::start(&tracker).await;
        let id = leech.add(&seeder, &[b], false).await;
        let task = leech.wait_done(&id).await;
        assert_eq!(
            (task.status, task.error_message.as_str()),
            (3, ""),
            "{version:?}"
        );
        assert_eq!(
            leech.landed(&task, "sub/b.bin").await.as_deref(),
            Some(seeder.content_of("sub/b.bin")),
            "{version:?}"
        );
        for rel in ["a.bin", "c.txt"] {
            assert_eq!(leech.landed(&task, rel).await, None, "{version:?} {rel}");
        }
        assert_intact(&leech, &id).await;
        leech.shutdown().await;
    }
}

/// v2 磁力（纯 v2 只有 `btmh`、混合两个 `xt` 都带）：元数据经 BEP 9 取得并按磁力里的哈希核对，
/// 分片层再向对端要，下载完成落地一致；分享出去的磁力与种子的哈希一致。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v2_and_hybrid_magnets_download_and_share_both_hashes() {
    for version in [TorrentVersion::V2, TorrentVersion::Hybrid] {
        let tracker = Tracker::start().await;
        let seeder = Seeder::start_versioned(&pack(), &tracker, None, version).await;
        let share = fluxdown_engine::bt_local_seed::torrent_share(&seeder.torrent_bytes, &[])
            .expect("share");
        assert_eq!(
            share.magnet.contains("xt=urn:btih:"),
            version == TorrentVersion::Hybrid,
            "{version:?}: {}",
            share.magnet
        );
        assert!(
            share.magnet.contains("xt=urn:btmh:1220"),
            "{}",
            share.magnet
        );
        let mut leech = Downloader::start(&tracker).await;
        let id = leech.add_magnet(&share.magnet).await;
        let task = leech.wait_done(&id).await;
        assert_eq!(
            (task.status, task.error_message.as_str()),
            (3, ""),
            "{version:?}"
        );
        for (rel, bytes) in seeder
            .files
            .iter()
            .filter(|(rel, _)| !rel.starts_with(".pad"))
        {
            assert_eq!(
                leech.landed(&task, rel).await.as_deref(),
                Some(bytes.as_slice()),
                "{version:?} {rel}"
            );
        }
        let status = leech
            .engine
            .manager
            .bt_status(&id)
            .await
            .expect("bt status");
        assert_eq!(status.info_hash_v2, share.info_hash_v2, "{version:?}");
        assert_intact(&leech, &id).await;
        leech.shutdown().await;
    }
}
