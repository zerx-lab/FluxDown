//! RSS 订阅端到端管线集成测试（本地 HTTP，无外网依赖）。
//!
//! 单测已覆盖纯函数（`rss::filter`）、解析（`rss::parser`）与状态机
//! （`rss::apply_fetch`）。这里补的是**只有真正跑一遍才能证明**的那一段：
//!
//! ```text
//! Engine::new 装载订阅 → tick_rss_sources() 判定到期 → off-actor HTTP 抓取
//!   → feed 解析 → 规则过滤 → mpsc 回流 → on_rss_event → create_task 落库
//!   → 任务 rss_source_id 溯源 + 条目回链 task_id
//! ```
//!
//! 即宿主 actor 那两条接线（tick + 回流）与「任务创建收敛到 create_task」这条
//! 不变式的活体验证——任何一环断了，本测试立刻红。
//!
//! 订阅配 `start_paused`：任务以 paused 落库、不真的开跑下载，测试只关心
//! 「有没有正确地建出这个任务」。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::rss::RssRefreshOutcome;
use fluxdown_engine::rss::model::{RssItemStatus, RssSourceInfo};
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

/// 首轮 feed：两个历史条目。
const FEED_ROUND1: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel>
  <title>Mikan Project - 我的番组</title>
  <link>https://mikanani.me/</link>
  <item>
    <guid isPermaLink="false">ep01</guid>
    <title>[ANi] 幼女战记 2 - 01 [1080P][Baha][WEB-DL]</title>
    <link>http://HOST/page/ep01</link>
    <description><![CDATA[<img src="../cover.png">]]></description>
    <enclosure type="application/octet-stream" length="1024" url="http://HOST/dl/ep01.bin"/>
  </item>
  <item>
    <guid isPermaLink="false">ep02-720</guid>
    <title>[桜都字幕组] 幼女战记 2 - 02 [720P][简体内嵌]</title>
    <link>http://HOST/page/ep02-720</link>
    <enclosure type="application/octet-stream" length="1024" url="http://HOST/dl/ep02-720.bin"/>
  </item>
</channel></rss>"#;

/// 次轮 feed：保留首轮两条 + 两个新条目（一个命中规则、一个被排除词拦下）。
const FEED_ROUND2: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel>
  <title>Mikan Project - 我的番组</title>
  <link>https://mikanani.me/</link>
  <item>
    <guid isPermaLink="false">ep01</guid>
    <title>[ANi] 幼女战记 2 - 01 [1080P][Baha][WEB-DL]</title>
    <link>http://HOST/page/ep01</link>
    <enclosure type="application/octet-stream" length="1024" url="http://HOST/dl/ep01.bin"/>
  </item>
  <item>
    <guid isPermaLink="false">ep02-720</guid>
    <title>[桜都字幕组] 幼女战记 2 - 02 [720P][简体内嵌]</title>
    <link>http://HOST/page/ep02-720</link>
    <enclosure type="application/octet-stream" length="1024" url="http://HOST/dl/ep02-720.bin"/>
  </item>
  <item>
    <guid isPermaLink="false">ep03</guid>
    <title>[ANi] 幼女战记 2 - 03 [1080P][Baha][WEB-DL]</title>
    <link>http://HOST/page/ep03</link>
    <enclosure type="application/octet-stream" length="2048" url="http://HOST/dl/ep03.bin"/>
  </item>
  <item>
    <guid isPermaLink="false">ep04-720</guid>
    <title>[某组] 幼女战记 2 - 04 [720P][简体]</title>
    <link>http://HOST/page/ep04-720</link>
    <enclosure type="application/octet-stream" length="2048" url="http://HOST/dl/ep04-720.bin"/>
  </item>
</channel></rss>"#;

/// Mikan 形态的 feed：enclosure 是 `.torrent` 直链，且 `length` 声明的是
/// **番剧内容**的大小（342 MB）而非种子文件本身的几 KB——这正是真实站点的
/// 行为，也是曾经把 `hint_file_size` 毒死的那个坑。
#[cfg(feature = "bt")]
const FEED_TORRENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel>
  <title>Mikan Project - 欺诈游戏</title>
  <link>https://mikanani.me/</link>
  <item>
    <guid isPermaLink="false">bt01</guid>
    <title>[LoliHouse] LIAR GAME - 01 [WebRip 1080p HEVC-10bit AAC]</title>
    <link>http://HOST/Home/Episode/bt01</link>
    <enclosure type="application/x-bittorrent" length="358612992"
               url="http://HOST/Download/20260412/bt01.torrent"/>
  </item>
</channel></rss>"#;

/// 最小 bencode 字典（`d…e`），足以通过种子抓取的形态校验。
const TORRENT_BODY: &str = "d4:name6:sample12:piece lengthi16384ee";

/// 本地 feed 服务器：`/feed.xml` 按被请求的**次数**依次吐出 round1 / round2…
/// 之后固定停在最后一份，模拟「站点又更新了几条」。其余路径返回 1KB 占位内容，
/// 让 `create_task` 的后台元数据探测（HEAD）有东西可探。
/// `make_rounds` 接收 `host:port`（bind 之后才知道）用于把 feed 里的
/// `HOST` 占位符换成真实地址——先 bind 拿端口，再造 body，避免二次启动。
fn spawn_feed_server(make_rounds: impl FnOnce(&str) -> Vec<String>) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let rounds = make_rounds(&format!("127.0.0.1:{port}"));
    let feed_hits = Arc::new(AtomicUsize::new(0));
    let hits = feed_hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let is_feed = req.contains("/feed.xml");
            let is_torrent = req.contains(".torrent");
            let body = if is_feed {
                let i = hits.fetch_add(1, Ordering::SeqCst);
                rounds[i.min(rounds.len() - 1)].clone()
            } else if is_torrent {
                // 最小合法 bencode 字典：`fetch_torrent` 只校验首字节是 'd'，
                // 真正的解析由 librqbit 在任务启动时做（本测试 start_paused）。
                TORRENT_BODY.to_string()
            } else {
                "x".repeat(1024)
            };
            let ctype = if is_feed {
                "application/rss+xml"
            } else if is_torrent {
                "application/x-bittorrent"
            } else {
                "application/octet-stream"
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                body.len()
            );

            stream.write_all(resp.as_bytes()).unwrap_or_else(|error| {
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                    ),
                    "test server response failed: {error}"
                );
            });
            // HEAD 不回 body（否则 reqwest 会读到多余字节）。
            if !req.starts_with("HEAD ") {
                stream.write_all(body.as_bytes()).unwrap_or_else(|error| {
                    assert!(
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ),
                        "test server response failed: {error}"
                    );
                });
            }

            stream.flush().unwrap_or_else(|error| {
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                    ),
                    "test server response failed: {error}"
                );
            });
        }
    });
    (port, feed_hits)
}

fn uniq() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), n)
}

async fn make_engine(work: &std::path::Path) -> Engine {
    let cfg = EngineConfig {
        max_concurrent: 4,
        speed_limit_bps: 0,
        upload_limit_bps: 0,
        default_save_dir: work.to_string_lossy().into_owned(),
        app_data_dir: work.to_string_lossy().into_owned(),
        bt_config: BtConfig::default(),
        proxy_config: ProxyConfig::default(),
        user_agent: String::new(),
        data_dir_override: Some(work.to_path_buf()),
        database_url: None,
    };
    Engine::new(cfg, Arc::new(NoopSink), Arc::new(NoopSelection))
        .await
        .expect("engine")
}

/// 排空一次 RSS 回流：等 off-actor 抓取把结果送回来，再交给 actor 侧消化。
/// 宿主 actor 里这一段就是 `Some(ev) = rss_rx.recv() => on_rss_event(ev).await`。
async fn drain_one_rss_event(
    engine: &mut Engine,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<fluxdown_engine::rss::RssEvent>,
) {
    let ev = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("rss fetch timed out")
        .expect("rss channel closed");
    engine.manager.on_rss_event(ev).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rss_pipeline_seeds_then_downloads_only_matching_new_items() {
    let work = std::env::temp_dir().join(format!("fluxdown-rss-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let (port, feed_hits) = spawn_feed_server(|host| {
        vec![
            FEED_ROUND1.replace("HOST", host),
            FEED_ROUND2.replace("HOST", host),
        ]
    });
    let host = format!("127.0.0.1:{port}");
    let feed_url = format!("http://{host}/feed.xml");

    let mut engine = make_engine(&work).await;
    let mut rss_rx = engine
        .manager
        .rss
        .take_event_rx()
        .expect("rss receiver available exactly once");

    // 排除 720P、每轮上限 10；新任务 paused 落库（测试不真的下载）。
    let source_id = engine
        .manager
        .rss
        .create_source(RssSourceInfo {
            url: feed_url.clone(),
            exclude_pattern: "720P".to_string(),
            start_paused: true,
            interval_minutes: 1,
            max_per_fetch: 10,
            ..Default::default()
        })
        .await
        .expect("subscribe");

    // ── 第一轮：新订阅立刻到期 → 历史条目只标记不下载 ──────────────────
    engine.manager.tick_rss_sources();
    drain_one_rss_event(&mut engine, &mut rss_rx).await;

    assert_eq!(feed_hits.load(Ordering::SeqCst), 1, "fetched exactly once");
    let items = engine
        .db
        .load_rss_items(&source_id, 100)
        .await
        .expect("items");
    assert_eq!(items.len(), 2, "both history items recorded");
    assert_eq!(
        items
            .iter()
            .find(|item| item.guid == "ep01")
            .and_then(|item| item.image_urls.as_deref()),
        Some([format!("http://{host}/cover.png")].as_slice()),
    );
    assert!(
        items.iter().all(|i| i.status == RssItemStatus::SeedSkipped),
        "first round must never auto-download history: {items:?}"
    );
    assert!(
        engine.db.load_all_tasks().await.expect("tasks").is_empty(),
        "no download task may exist after the seeding round"
    );
    assert!(
        engine
            .manager
            .rss
            .source(&source_id)
            .expect("source")
            .seeded
    );

    // ── 第二轮：手动刷新（绕开 due 判定）→ 只下新增且命中规则的那一条 ──
    assert_eq!(
        engine.manager.refresh_rss_source(&source_id),
        RssRefreshOutcome::Started
    );
    drain_one_rss_event(&mut engine, &mut rss_rx).await;

    assert_eq!(feed_hits.load(Ordering::SeqCst), 2);
    let items = engine
        .db
        .load_rss_items(&source_id, 100)
        .await
        .expect("items");
    assert_eq!(items.len(), 4, "two new items appended, old ones untouched");

    let by_guid = |guid: &str| {
        items
            .iter()
            .find(|i| i.guid == guid)
            .cloned()
            .unwrap_or_else(|| panic!("item {guid} missing"))
    };
    let downloaded = by_guid("ep03");
    assert_eq!(downloaded.status, RssItemStatus::Downloaded);
    assert!(!downloaded.task_id.is_empty(), "item must link its task");
    assert_eq!(by_guid("ep04-720").status, RssItemStatus::Filtered);
    assert_eq!(by_guid("ep04-720").reason, "excluded");
    // 已入库的老条目不因二次抓取被改写（guid 即身份）。
    assert_eq!(by_guid("ep01").status, RssItemStatus::SeedSkipped);

    // ── 任务确实建出来了，且带上了溯源指针 ────────────────────────────
    let tasks = engine.db.load_all_tasks().await.expect("tasks");
    assert_eq!(tasks.len(), 1, "exactly one task for the one matching item");
    let task = &tasks[0];
    assert_eq!(task.url, format!("http://{host}/dl/ep03.bin"));
    assert_eq!(task.status, 2, "start_paused → paused on insert");
    assert_eq!(
        task.rss_source_id, source_id,
        "task must be traceable back to the subscription"
    );
    assert_eq!(
        task.task_id, downloaded.task_id,
        "both link directions agree"
    );

    // ── 第三轮：feed 内容没变 → 不重复建任务 ─────────────────────────
    assert_eq!(
        engine.manager.refresh_rss_source(&source_id),
        RssRefreshOutcome::Started
    );
    drain_one_rss_event(&mut engine, &mut rss_rx).await;
    assert_eq!(
        engine.db.load_all_tasks().await.expect("tasks").len(),
        1,
        "an unchanged feed must not create the same download twice"
    );

    // ── 删除订阅：条目清空、任务保留、溯源指针清空 ───────────────────
    assert!(engine.manager.rss.delete_source(&source_id).await);
    assert!(
        engine
            .db
            .load_rss_items(&source_id, 100)
            .await
            .expect("items")
            .is_empty()
    );
    let tasks = engine.db.load_all_tasks().await.expect("tasks");
    assert_eq!(tasks.len(), 1, "deleting a feed must keep its downloads");
    assert!(
        tasks[0].rss_source_id.is_empty(),
        "dangling pointer cleared"
    );

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

/// 订阅这个动作本身就是「我要这个源的内容」：点完「订阅」必须立刻有条目，
/// 而不是等下一次分钟级 tick，更不该逼用户自己再按一次「立即抓取」。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribing_fetches_immediately_without_waiting_for_a_tick() {
    let work = std::env::temp_dir().join(format!("fluxdown-rss-now-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let (port, feed_hits) = spawn_feed_server(|host| vec![FEED_ROUND1.replace("HOST", host)]);
    let mut engine = make_engine(&work).await;
    let mut rss_rx = engine.manager.rss.take_event_rx().expect("rss receiver");

    let source_id = engine
        .manager
        .create_rss_source(RssSourceInfo {
            url: format!("http://127.0.0.1:{port}/feed.xml"),
            start_paused: true,
            // 30 分钟：如果抓取只靠 due 判定，这条订阅这辈子都等不到本测试结束。
            interval_minutes: 30,
            ..Default::default()
        })
        .await
        .expect("subscribe");

    // 注意：全程**没有** tick_rss_sources()。
    drain_one_rss_event(&mut engine, &mut rss_rx).await;

    assert_eq!(
        feed_hits.load(Ordering::SeqCst),
        1,
        "subscribe → fetch once"
    );
    let items = engine
        .db
        .load_rss_items(&source_id, 100)
        .await
        .expect("items");
    assert_eq!(items.len(), 2, "首轮历史条目应当已经在库里");
    assert!(
        engine
            .manager
            .rss
            .source(&source_id)
            .expect("source")
            .seeded,
        "首轮抓完即播种完成"
    );

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rss_fetch_failure_is_recorded_and_backs_off_without_disabling() {
    let work = std::env::temp_dir().join(format!("fluxdown-rss-err-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let mut engine = make_engine(&work).await;
    let mut rss_rx = engine.manager.rss.take_event_rx().expect("rss receiver");

    // 指向一个没人监听的端口：连接被拒 → 走失败分支。
    let dead = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = dead.local_addr().expect("addr").port();
    drop(dead);

    let source_id = engine
        .manager
        .rss
        .create_source(RssSourceInfo {
            url: format!("http://127.0.0.1:{port}/feed.xml"),
            interval_minutes: 30,
            ..Default::default()
        })
        .await
        .expect("subscribe");

    engine.manager.tick_rss_sources();
    drain_one_rss_event(&mut engine, &mut rss_rx).await;

    let source = engine
        .manager
        .rss
        .source(&source_id)
        .expect("source")
        .clone();
    assert_eq!(source.fail_count, 1);
    assert!(!source.last_error.is_empty(), "failure reason must surface");
    assert!(source.enabled, "a failed fetch must not disable the feed");
    assert!(!source.seeded, "a failed first round stays unseeded");
    // 退避生效：30min × 2^1 = 1h，此刻绝不该再次到期。
    assert_eq!(fluxdown_engine::rss::effective_interval_secs(&source), 3600);
    engine.manager.tick_rss_sources();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), rss_rx.recv())
            .await
            .is_err(),
        "a backed-off source must not be re-fetched on the next tick"
    );

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

/// 回归：Mikan 这类「enclosure 是 `.torrent` 直链」的 feed 必须建出**真正的
/// BT 任务**，而不是把种子文件当普通 HTTP 文件下下来。
///
/// 这条测试同时钉死曾经的两个真实故障：
/// 1. `.torrent` 直链走 HTTP 分段下载 → 订阅只攒下一堆种子文件，番剧一集不下；
/// 2. 把 `<enclosure length>`（番剧 342 MB）当 `hint_file_size` → 引擎按
///    342 MB 规划 16 段并发并跳过 probe，首段 truncated、后续段全 416，还把
///    站点误学成「只支持 2 连接」污染 24h 的域名策略缓存。
#[cfg(feature = "bt")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn torrent_enclosures_become_real_bt_tasks_without_a_bogus_size_hint() {
    let work = std::env::temp_dir().join(format!("fluxdown-rss-bt-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    // 首轮播种给空 feed，第二轮才放出 BT 条目。
    let (port, _hits) = spawn_feed_server(|host| {
        vec![
            FEED_ROUND1
                .replace("HOST", host)
                .replace("<item>", "<!--")
                .replace("</item>", "-->"),
            FEED_TORRENT.replace("HOST", host),
        ]
    });
    let feed_url = format!("http://127.0.0.1:{port}/feed.xml");

    let mut engine = make_engine(&work).await;
    let mut rss_rx = engine.manager.rss.take_event_rx().expect("rss receiver");
    let source_id = engine
        .manager
        .rss
        .create_source(RssSourceInfo {
            url: feed_url,
            start_paused: true,
            interval_minutes: 1,
            ..Default::default()
        })
        .await
        .expect("subscribe");

    // 第一轮播种（feed 里的条目全被注释掉，等价于空 feed）。
    engine.manager.tick_rss_sources();
    drain_one_rss_event(&mut engine, &mut rss_rx).await;
    assert!(engine.db.load_all_tasks().await.expect("tasks").is_empty());

    // 第二轮：BT 条目出现 → 先抓 feed（回流一次），再抓 .torrent（再回流一次）。
    assert_eq!(
        engine.manager.refresh_rss_source(&source_id),
        RssRefreshOutcome::Started
    );
    drain_one_rss_event(&mut engine, &mut rss_rx).await; // Fetched
    assert!(
        engine.db.load_all_tasks().await.expect("tasks").is_empty(),
        "任务必须等种子字节到手才建，不能先建个 HTTP 任务占位"
    );
    drain_one_rss_event(&mut engine, &mut rss_rx).await; // TorrentReady

    let tasks = engine.db.load_all_tasks().await.expect("tasks");
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert_eq!(
        task.url, "torrent-file://local",
        "必须是 BT 任务（torrent-file 哨兵），而不是 .torrent 的 HTTP 直链"
    );
    assert!(
        task.origin_url.ends_with("/Download/20260412/bt01.torrent"),
        "哨兵 url 必须有真实来源兜底,否则右键「复制下载链接」复制到的是 \
         `torrent-file://local` 这种对任何工具都无意义的字符串,实得 {:?}",
        task.origin_url
    );
    assert_eq!(
        task.total_bytes, 0,
        "绝不能把 enclosure length（番剧大小）当成文件大小 hint"
    );
    assert_eq!(task.rss_source_id, source_id);
    let bytes = engine
        .db
        .load_torrent_file_bytes(&task.task_id)
        .await
        .expect("query")
        .expect("torrent bytes persisted");
    assert_eq!(bytes, TORRENT_BODY.as_bytes(), "种子内容原样落库");

    // 无人值守：文件选择必须**预先**落库为「全部文件」（`Some([])`），否则
    // `do_start_task` 会走 HostSelection 弹出「选择要下载的文件」对话框——
    // 自动订阅半夜抓到 5 集就弹 5 次，而且用户点「取消」后条目已被标记
    // 「已下载」，状态就撒谎了。
    assert_eq!(
        engine
            .db
            .load_bt_selected_files(&task.task_id)
            .await
            .expect("query"),
        Some(Vec::new()),
        "RSS 建的 BT 任务必须预置「全选」，绝不弹文件选择框"
    );

    let items = engine
        .db
        .load_rss_items(&source_id, 100)
        .await
        .expect("items");
    let item = items.iter().find(|i| i.guid == "bt01").expect("item");
    assert_eq!(item.status, RssItemStatus::Downloaded);
    assert_eq!(item.task_id, task.task_id);

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

/// 「立即抓取」撞上在途抓取是幂等成功（AlreadyRunning），只有订阅不存在才是 NotFound。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_while_fetching_is_idempotent_and_unknown_source_is_not_found() {
    let work = std::env::temp_dir().join(format!("fluxdown-rss-refresh-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let (port, _feed_hits) = spawn_feed_server(|host| vec![FEED_ROUND1.replace("HOST", host)]);
    let mut engine = make_engine(&work).await;
    let mut rss_rx = engine.manager.rss.take_event_rx().expect("rss receiver");
    let source_id = engine
        .manager
        .rss
        .create_source(RssSourceInfo {
            url: format!("http://127.0.0.1:{port}/feed.xml"),
            start_paused: true,
            ..Default::default()
        })
        .await
        .expect("subscribe");

    assert_eq!(
        engine.manager.refresh_rss_source(&source_id),
        RssRefreshOutcome::Started
    );
    assert_eq!(
        engine.manager.refresh_rss_source(&source_id),
        RssRefreshOutcome::AlreadyRunning
    );
    assert_eq!(
        engine.manager.refresh_rss_source("no-such-source"),
        RssRefreshOutcome::NotFound
    );
    drain_one_rss_event(&mut engine, &mut rss_rx).await;
    // 抓取回流后在途标记解除，可再次派发。
    assert_eq!(
        engine.manager.refresh_rss_source(&source_id),
        RssRefreshOutcome::Started
    );

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}
