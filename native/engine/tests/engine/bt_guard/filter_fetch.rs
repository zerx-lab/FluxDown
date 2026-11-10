//! 过滤文件订阅的拉取：本机假源站，覆盖失败容错、大小上限、近期成功跳过、周期 / 手动 / 配置变更触发。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluxdown_engine::bt_guard::{
    BtGuard, BtGuardConfig, HostTimings, MAX_FILTER_BODY_BYTES, refresh_sources,
};
use fluxdown_engine::proxy_config::ProxyConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::swarm::{HostFixture, Seeder, fast_timings, wait_for};

const WITHIN: Duration = Duration::from_secs(8);

#[derive(Clone)]
enum Reply {
    Body(String),
    Status(u16),
    /// 声明一个超大的 Content-Length，不发正文。
    DeclaredHuge,
    /// 不带长度、持续发零字节直到客户端放弃。
    Endless,
}

struct Origin {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    reply: Arc<Mutex<Reply>>,
}

impl Origin {
    async fn start(reply: Reply) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind origin");
        let addr = listener.local_addr().expect("origin addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let reply = Arc::new(Mutex::new(reply));
        let (hits_task, reply_task) = (Arc::clone(&hits), Arc::clone(&reply));
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                hits_task.fetch_add(1, Ordering::SeqCst);
                let reply = reply_task.lock().expect("reply lock").clone();
                tokio::spawn(async move {
                    let mut request = [0u8; 4096];
                    let mut read = 0;
                    while read < request.len() {
                        match stream.read(&mut request[read..]).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => read += n,
                        }
                        if request[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let head = |status: &str, extra: &str| {
                        format!("HTTP/1.1 {status}\r\n{extra}Connection: close\r\n\r\n")
                    };
                    match reply {
                        Reply::Body(text) => {
                            let response =
                                head("200 OK", &format!("Content-Length: {}\r\n", text.len()))
                                    + &text;
                            if let Err(error) = stream.write_all(response.as_bytes()).await {
                                eprintln!("origin write failed: {error}");
                            }
                        }
                        Reply::Status(code) => {
                            let response = head(&format!("{code} Nope"), "Content-Length: 0\r\n");
                            if let Err(error) = stream.write_all(response.as_bytes()).await {
                                eprintln!("origin write failed: {error}");
                            }
                        }
                        Reply::DeclaredHuge => {
                            let response = head(
                                "200 OK",
                                &format!("Content-Length: {}\r\n", MAX_FILTER_BODY_BYTES * 4),
                            );
                            if stream.write_all(response.as_bytes()).await.is_err() {
                                return;
                            }
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                        Reply::Endless => {
                            if stream
                                .write_all(head("200 OK", "").as_bytes())
                                .await
                                .is_err()
                            {
                                return;
                            }
                            let chunk = vec![b'#'; 256 * 1024];
                            // 超过上限的两倍还没被放弃就认为客户端没有设上限。
                            for _ in 0..(MAX_FILTER_BODY_BYTES * 2 / chunk.len()) {
                                if stream.write_all(&chunk).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        Self { addr, hits, reply }
    }

    fn url(&self) -> String {
        format!("http://{}/list.txt", self.addr)
    }

    fn set(&self, reply: Reply) {
        *self.reply.lock().expect("reply lock") = reply;
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn guard_for(urls: &[String]) -> Arc<BtGuard> {
    Arc::new(BtGuard::new(BtGuardConfig {
        filter_urls: urls.join("\n"),
        skip_lan: false,
        ..BtGuardConfig::default()
    }))
}

const LIST: &str = "# community list\n203.0.113.0/24\n198.51.100.7\n";

fn banned(guard: &BtGuard, ip: [u8; 4]) -> bool {
    guard.is_banned(IpAddr::V4(Ipv4Addr::from(ip)))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_good_source_replaces_its_entries_and_a_failing_one_keeps_the_old_ones() {
    let origin = Origin::start(Reply::Body(LIST.to_owned())).await;
    let guard = guard_for(&[origin.url()]);
    let proxy = ProxyConfig::default();

    let sources = refresh_sources(&guard, &proxy, None).await;
    assert_eq!(sources.len(), 1);
    assert!(sources[0].ok, "{sources:?}");
    assert_eq!(sources[0].entries, 2);
    assert!(banned(&guard, [203, 0, 113, 77]));
    assert!(banned(&guard, [198, 51, 100, 7]));
    assert_eq!(guard.counts().file, 2);

    // 站点故障：状态记错误，旧条目保留。
    origin.set(Reply::Status(500));
    let sources = refresh_sources(&guard, &proxy, None).await;
    assert!(!sources[0].ok);
    assert!(
        sources[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("500")),
        "{sources:?}"
    );
    assert!(banned(&guard, [203, 0, 113, 77]), "old entries are kept");

    // 空正文 / HTML 错误页都算失败，不会清空旧条目。
    for body in ["", "   \n\n", "<html><body>oops</body></html>"] {
        origin.set(Reply::Body(body.to_owned()));
        let sources = refresh_sources(&guard, &proxy, None).await;
        assert!(!sources[0].ok, "{body:?}: {sources:?}");
        assert!(banned(&guard, [203, 0, 113, 77]), "{body:?}");
    }

    // 只有注释的文件是合法的空订阅：来源被整体替换为空。
    origin.set(Reply::Body("# nothing to block today\n".to_owned()));
    let sources = refresh_sources(&guard, &proxy, None).await;
    assert!(sources[0].ok, "{sources:?}");
    assert_eq!(sources[0].entries, 0);
    assert!(!banned(&guard, [203, 0, 113, 77]));
}

#[tokio::test(flavor = "multi_thread")]
async fn sources_are_independent_and_bad_urls_are_rejected_without_a_request() {
    let good = Origin::start(Reply::Body(LIST.to_owned())).await;
    let bad = Origin::start(Reply::Status(404)).await;
    let guard = guard_for(&[
        bad.url(),
        "ftp://example.invalid/list.txt".to_owned(),
        "not a url".to_owned(),
        good.url(),
    ]);
    let sources = refresh_sources(&guard, &ProxyConfig::default(), None).await;
    assert_eq!(sources.len(), 4);
    assert!(!sources[0].ok);
    assert!(
        sources[1]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("scheme")),
        "{sources:?}"
    );
    assert!(!sources[2].ok);
    assert!(sources[3].ok, "{sources:?}");
    assert!(
        banned(&guard, [203, 0, 113, 1]),
        "the good source still applied"
    );
    assert_eq!(bad.hits(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_responses_are_abandoned() {
    let huge = Origin::start(Reply::DeclaredHuge).await;
    let endless = Origin::start(Reply::Endless).await;
    let guard = guard_for(&[huge.url(), endless.url()]);
    let sources = refresh_sources(&guard, &ProxyConfig::default(), None).await;
    assert!(!sources[0].ok);
    assert!(
        sources[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("too large")),
        "{sources:?}"
    );
    assert!(!sources[1].ok);
    assert!(
        sources[1]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("exceeds")),
        "{sources:?}"
    );
    assert_eq!(guard.counts().file, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn recently_fetched_sources_are_skipped_unless_forced() {
    let origin = Origin::start(Reply::Body(LIST.to_owned())).await;
    let guard = guard_for(&[origin.url()]);
    let proxy = ProxyConfig::default();
    refresh_sources(&guard, &proxy, Some(3600)).await;
    assert_eq!(origin.hits(), 1);
    refresh_sources(&guard, &proxy, Some(3600)).await;
    assert_eq!(origin.hits(), 1, "fresh source is not downloaded again");
    refresh_sources(&guard, &proxy, None).await;
    assert_eq!(origin.hits(), 2, "a forced refresh always fetches");

    // 失败过的源即使「年龄」未到也要重试。
    origin.set(Reply::Status(503));
    refresh_sources(&guard, &proxy, None).await;
    origin.set(Reply::Body(LIST.to_owned()));
    refresh_sources(&guard, &proxy, Some(3600)).await;
    assert_eq!(origin.hits(), 4);
    assert!(guard.filter_sources()[0].ok);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_urls_means_no_client_and_no_traffic() {
    let guard = guard_for(&[]);
    assert!(
        refresh_sources(&guard, &ProxyConfig::default(), None)
            .await
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn session_start_fetches_then_the_interval_and_config_changes_refetch() {
    let origin = Origin::start(Reply::Body(LIST.to_owned())).await;
    let fx = HostFixture::with(
        BtGuardConfig {
            filter_urls: origin.url(),
            skip_lan: false,
            ..BtGuardConfig::default()
        },
        HostTimings {
            filter_interval: Duration::from_millis(150),
            ..fast_timings()
        },
    )
    .await;
    assert_eq!(origin.hits(), 0, "nothing is fetched without a BT session");
    let seed = Seeder::start(&fx.host).await;

    wait_for("initial fetch", || origin.hits() >= 1, WITHIN).await;
    wait_for("entries", || fx.host.guard().counts().file == 2, WITHIN).await;
    wait_for("periodic refresh", || origin.hits() >= 3, WITHIN).await;
    wait_for("changed event", || fx.sink.changed() >= 1, WITHIN).await;

    // 新增订阅源立即拉取（不等周期）。
    let second = Origin::start(Reply::Body("192.0.2.0/24\n".to_owned())).await;
    fx.host.set_config(BtGuardConfig {
        filter_urls: format!("{}\n{}", origin.url(), second.url()),
        skip_lan: false,
        ..BtGuardConfig::default()
    });
    wait_for("new source fetched", || second.hits() >= 1, WITHIN).await;
    wait_for(
        "both applied",
        || fx.host.guard().counts().file == 3,
        WITHIN,
    )
    .await;

    // 会话结束后不再有订阅流量。
    seed.closing.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let settled = (origin.hits(), second.hits());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!((origin.hits(), second.hits()), settled);
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_refresh_works_without_a_session_and_removed_urls_drop_their_entries() {
    let origin = Origin::start(Reply::Body(LIST.to_owned())).await;
    let fx = HostFixture::new(BtGuardConfig {
        filter_urls: origin.url(),
        skip_lan: false,
        ..BtGuardConfig::default()
    })
    .await;
    let sources = fx.host.refresh_filters_now().await;
    assert!(sources[0].ok, "{sources:?}");
    assert!(
        fx.host
            .guard()
            .is_banned(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)))
    );

    // 手动刷新无视「近期成功」。
    fx.host.refresh_filters_now().await;
    assert_eq!(origin.hits(), 2);

    // 订阅被移除 → 条目随之丢弃（配置热更新，无需会话）。
    fx.host.set_config(BtGuardConfig {
        skip_lan: false,
        ..BtGuardConfig::default()
    });
    assert!(
        !fx.host
            .guard()
            .is_banned(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)))
    );
    assert!(fx.host.refresh_filters_now().await.is_empty());
}
