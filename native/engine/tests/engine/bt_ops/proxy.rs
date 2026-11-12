//! 「BT 使用代理」端到端：tracker 与对端连接经代理出站；关掉「节点连接也走代理」后
//! 只有 tracker 走代理；代理不通时不回退直连；运行中改代理设置，重建会话后任务在新出口上
//! 继续下完（无需重启应用）。

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

/// 运行中把 BT 从直连切到代理：daemon 写设置后走的就是 `set_bt_config` +
/// `invalidate_bt_session`。在跑的任务被接到新会话上继续，之后的 tracker 与对端连接都经代理。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switching_to_the_proxy_mid_download_carries_the_running_task_over() {
    let tracker = Tracker::start().await;
    // 限速做种：切换落地时任务还在下载。
    let seeder = Seeder::start(&pack(), &tracker, Some(16 * 1024)).await;
    let proxy = Socks5::start().await;
    let proxy_port = proxy.addr.port();
    let mut base = None;
    let mut leech = Downloader::start_with(&tracker, |bt, proxy| {
        proxy.mode = ProxyMode::None;
        proxy.proxy_type = ProxyType::Socks5;
        proxy.host = "127.0.0.1".to_owned();
        proxy.port = proxy_port;
        base = Some(bt.clone());
    })
    .await;
    let mut switched = base.expect("BT config captured");

    let id = leech.add(&seeder, &[], false).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let task = leech.task(&id).await;
        assert_ne!(
            task.status, 3,
            "finished before the switch; throttle too loose"
        );
        if task.downloaded_bytes > 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no direct progress before the switch"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(proxy.tunnels_to(seeder.addr), 0, "direct before the switch");

    switched.proxy_enabled = true;
    switched.proxy_peers = true;
    leech.engine.manager.set_bt_config(switched);
    leech.engine.manager.invalidate_bt_session().await;
    seeder.unthrottle();

    // 旧会话上被打断的那一轮也会交回一次 TaskDone，等到真正下完为止。
    let mut task = leech.wait_done(&id).await;
    for _ in 0..3 {
        if task.status == 3 {
            break;
        }
        task = leech.wait_done(&id).await;
    }
    assert_eq!(task.status, 3, "download failed: {}", task.error_message);
    assert_eq!(
        leech.landed(&task, "a.bin").await.as_deref(),
        Some(seeder.content_of("a.bin"))
    );
    assert!(
        proxy.tunnels_to(seeder.addr) >= 1,
        "after the switch peer connections go through the proxy"
    );
    assert!(
        proxy.tunnels_to(tracker.addr) >= 1,
        "the rebuilt session announces through the proxy"
    );
    leech.shutdown().await;
}
