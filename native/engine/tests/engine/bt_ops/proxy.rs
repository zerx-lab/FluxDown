//! 「BT 使用手动代理」端到端：tracker 与对端连接经代理出站；关掉「节点连接也走代理」后
//! 只有 tracker 走代理；代理不通时不回退直连。

use std::time::{Duration, Instant};

use fluxdown_engine::proxy_config::{ProxyMode, ProxyType};

use super::swarm::{Downloader, PIECE_LEN, Seeder, Socks5, Tracker, patterned};

fn pack() -> Vec<(&'static str, Vec<u8>)> {
    vec![("a.bin", patterned(8 * PIECE_LEN as usize, 1))]
}

/// 下载端：BT 走 `proxy_port` 上的手动 SOCKS5 服务器；全局代理模式保持「不使用代理」
/// （BT 是否走代理只看自己的开关）。
async fn downloader_behind(tracker: &Tracker, proxy_port: u16, peers_too: bool) -> Downloader {
    Downloader::start_with(tracker, |bt, proxy| {
        bt.proxy_enabled = true;
        bt.proxy_peers = peers_too;
        proxy.mode = ProxyMode::None;
        proxy.proxy_type = ProxyType::Socks5;
        proxy.host = "127.0.0.1".to_owned();
        proxy.port = proxy_port;
    })
    .await
}

/// 每条到 tracker 的连接都是代理建的隧道。两边各自计数，收尾时的汇报可能正在途中，
/// 所以等两个数对上；有一次直连它们就永远对不上。
async fn assert_every_announce_was_tunnelled(tracker: &Tracker, proxy: &Socks5) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (hits, tunnels) = (tracker.hits(), proxy.tunnels_to(tracker.addr));
        if hits >= 1 && hits == tunnels {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "tracker saw {hits} connection(s) but the proxy made {tunnels} tunnel(s) to it"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tracker_and_peer_traffic_goes_through_the_manual_proxy() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, None).await;
    let proxy = Socks5::start().await;
    let mut leech = downloader_behind(&tracker, proxy.addr.port(), true).await;

    let id = leech.add(&seeder, &[], false).await;
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(
        leech.landed(&task, "a.bin").await.as_deref(),
        Some(seeder.content_of("a.bin"))
    );
    assert_every_announce_was_tunnelled(&tracker, &proxy).await;
    assert!(
        proxy.tunnels_to(seeder.addr) >= 1,
        "peer connections go through the proxy"
    );
    leech.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peers_connect_directly_when_only_trackers_are_proxied() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, None).await;
    let proxy = Socks5::start().await;
    let mut leech = downloader_behind(&tracker, proxy.addr.port(), false).await;

    let id = leech.add(&seeder, &[], false).await;
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_every_announce_was_tunnelled(&tracker, &proxy).await;
    assert_eq!(
        proxy.tunnels_to(seeder.addr),
        0,
        "peer data bypasses the proxy when asked to"
    );
    leech.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_proxy_is_never_bypassed() {
    let tracker = Tracker::start().await;
    let seeder = Seeder::start(&pack(), &tracker, None).await;
    // 一个没人监听的端口：绑定后立即释放。
    let dead_port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("ephemeral port")
        .local_addr()
        .expect("local addr")
        .port();
    let mut leech = downloader_behind(&tracker, dead_port, true).await;

    let id = leech.add(&seeder, &[], false).await;
    // 直连的话这个 128 KiB 的种子几十毫秒就下完；给足时间后仍然什么都没发生。
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        tracker.hits(),
        0,
        "no announce may reach the tracker directly"
    );
    let task = leech.task(&id).await;
    assert_ne!(task.status, 3, "nothing can have been downloaded");
    assert_eq!(task.downloaded_bytes, 0);
    leech.shutdown().await;
}
