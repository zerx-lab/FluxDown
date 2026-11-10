//! 端到端：真实 librqbit 做种会话 + `BtGuardHost` + 手写线协议的吸血端假对端。

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use fluxdown_engine::bt_guard::{
    BtGuardConfig, CloudEntry, FilterAction, HostTimings, Layer, NewEntry, RuleMode,
};
use fluxdown_engine::db::BtGuardEventQuery;

use super::support::cloud_doc;
use super::swarm::{
    BLOCK, FakePeer, HostFixture, Leecher, Seeder, fast_timings, loopback_config, peer_id, wait_for,
};

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const WITHIN: Duration = Duration::from_secs(8);

fn ban_entry() -> NewEntry {
    NewEntry {
        cidr: "127.0.0.1/32".to_owned(),
        action: FilterAction::Ban,
        duration_secs: Some(3600),
        reason: "e2e".to_owned(),
    }
}

async fn events(fx: &HostFixture) -> Vec<fluxdown_engine::db::BtGuardEventRow> {
    fx.host.flush_now().await.expect("flush");
    fx.db
        .bt_guard_list_events(&BtGuardEventQuery {
            rule: None,
            kind: None,
            ip_group: None,
            offset: 0,
            limit: 200,
        })
        .await
        .expect("events")
        .1
}

/// 吸血端的典型动作：声明拥有全部分片，却反复请求其中不同的分片。
async fn leech_repeat_requests(peer: &mut FakePeer) {
    peer.bitfield(&[0b1111_0000]).await.expect("bitfield");
    peer.interested().await.expect("interested");
    peer.request(0, 0, BLOCK).await.expect("request 0");
    peer.request(1, 0, BLOCK).await.expect("request 1");
}

#[tokio::test(flavor = "multi_thread")]
async fn enforce_bans_the_repeat_requester_and_refuses_its_reconnect() {
    let fx = HostFixture::new(BtGuardConfig {
        report_enabled: true,
        ..loopback_config(RuleMode::Enforce)
    })
    .await;
    let seed = Seeder::start(&fx.host).await;
    let guard = fx.host.guard();

    // 同一 IP 的另一条空闲连接：封禁后必须被泵经 disconnect_peers_where 断开。
    let mut bystander = FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0001-"))
        .await
        .expect("bystander");
    let mut leech = FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0002-"))
        .await
        .expect("leech");
    leech_repeat_requests(&mut leech).await;

    assert!(
        leech.closed_within(WITHIN).await.is_some(),
        "the offending connection is disconnected by the verdict"
    );
    assert!(
        bystander.closed_within(WITHIN).await.is_some(),
        "the ban reaches the other live connections of the group"
    );
    assert!(guard.is_banned(LOOPBACK));
    assert!(
        FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0003-"))
            .await
            .is_err(),
        "a banned address is refused before the handshake"
    );
    assert!(guard.stats().blocked_connections >= 1);
    assert!(guard.stats().enforced >= 1);

    // 条目：auto 层、/32、封禁、规则 rerequest。
    let (total, items) = guard.list_entries(&fluxdown_engine::bt_guard::EntryFilter {
        layer: Some(Layer::Auto),
        rule: None,
        query: None,
        contains_ip: None,
        offset: 0,
        limit: 10,
    });
    assert_eq!(total, 1);
    assert_eq!(items[0].cidr(), "127.0.0.1/32");
    assert_eq!(items[0].action, FilterAction::Ban);
    assert_eq!(&*items[0].rule, "rerequest");

    // 事件：引擎事件（合并后）与证据事件、落库的事件记录。
    wait_for("changed event", || fx.sink.changed() >= 1, WITHIN).await;
    wait_for(
        "evidence event",
        || !fx.sink.evidence_counts().is_empty(),
        WITHIN,
    )
    .await;
    let drained = guard.drain_evidence();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].rule, "rerequest");
    assert_eq!(drained[0].ip_group, "127.0.0.1");
    let rows = events(&fx).await;
    assert!(
        rows.iter().any(|r| r.rule == "rerequest"),
        "the ban is recorded in the event log: {rows:?}"
    );
    assert_eq!(
        fx.db.bt_guard_load_entries(0).await.expect("entries").len(),
        1,
        "the auto ban survives in the entry table"
    );

    // 解除后立即可以重新连接。
    let ids: Vec<i64> = items.iter().map(|e| e.id).collect();
    guard.remove_entries(&ids).expect("remove");
    wait_for("unban", || !guard.is_banned(LOOPBACK), WITHIN).await;
    FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0004-"))
        .await
        .expect("reconnect after the ban is lifted");
}

#[tokio::test(flavor = "multi_thread")]
async fn observe_only_records_and_leaves_the_connection_alone() {
    let fx = HostFixture::new(loopback_config(RuleMode::Observe)).await;
    let seed = Seeder::start(&fx.host).await;
    let guard = fx.host.guard();

    let mut leech = FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0005-"))
        .await
        .expect("leech");
    leech_repeat_requests(&mut leech).await;
    leech
        .expect_pieces(2, WITHIN)
        .await
        .expect("observe mode keeps serving the peer");
    assert!(leech.stays_open(Duration::from_millis(300)).await);

    wait_for("observed stat", || guard.stats().observed == 1, WITHIN).await;
    assert_eq!(guard.stats().enforced, 0);
    assert!(!guard.is_banned(LOOPBACK));
    let counts = guard.counts();
    assert_eq!(
        (counts.auto, counts.manual, counts.cloud, counts.file),
        (0, 0, 0, 0),
        "observe never writes a ban entry"
    );
    wait_for("changed event", || fx.sink.changed() >= 1, WITHIN).await;
    let rows = events(&fx).await;
    let observed = rows
        .iter()
        .find(|r| r.kind == "observed" && r.rule == "rerequest")
        .unwrap_or_else(|| panic!("an observed rerequest is recorded: {rows:?}"));
    assert_eq!(observed.mode, "observe");
    assert_eq!(
        observed.action, "ban",
        "records what enforce would have done"
    );
    FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0006-"))
        .await
        .expect("a fresh connection is still welcome");
}

#[tokio::test(flavor = "multi_thread")]
async fn bitmaps_cross_the_adapter_and_drive_the_rewind_rule() {
    let fx = HostFixture::new(BtGuardConfig {
        rule_rewind: RuleMode::Enforce,
        ..loopback_config(RuleMode::Off)
    })
    .await;
    let seed = Seeder::start(&fx.host).await;
    let guard = fx.host.guard();

    // 第一次连接：位图声明 0、1、2，HAVE 3，并完整取走第 0 片（两个 16 KiB 块）。
    // rewind 至少要丢 4 片才成立，这个 4 片的种子里只有「全部丢光」才够。
    let mut first = FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0007-"))
        .await
        .expect("first");
    first.bitfield(&[0b1110_0000]).await.expect("bitfield");
    first.have(3).await.expect("have");
    first.interested().await.expect("interested");
    first.request(0, 0, BLOCK).await.expect("block 0");
    first.request(0, BLOCK, BLOCK).await.expect("block 1");
    first.expect_pieces(2, WITHIN).await.expect("piece 0");
    drop(first);

    // Disconnected 事件携带最终位图 {0,1,2,3}，随会话事件落库。
    let ledger = loop {
        let rows = fx.db.bt_guard_load_ledger(0).await.expect("ledger");
        if let Some(row) = rows.into_iter().next() {
            break row;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(ledger.info_hash, hex::encode(seed.info_hash));
    assert_eq!(ledger.ip_group, "127.0.0.1");
    assert_eq!(ledger.claimed_bits, vec![0b1111_0000]);
    assert_eq!(ledger.delivered_bits, vec![0b1000_0000]);
    assert_eq!(ledger.delivered_bytes, i64::from(2 * BLOCK));

    // 重连后交出空位图（同一客户端）：此前持有的 4 片全部丢失 → rewind → 封禁。
    let mut second = FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0007-"))
        .await
        .expect("second");
    second.bitfield(&[0]).await.expect("empty bitfield");
    assert!(
        second.closed_within(WITHIN).await.is_some(),
        "an emptied bitfield after receiving data is a rewind"
    );
    assert!(guard.is_banned(LOOPBACK));
    let rows = events(&fx).await;
    assert!(rows.iter().any(|r| r.rule == "rewind"), "{rows:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_ban_disconnects_live_peers_and_allow_list_overrides_it() {
    let fx = HostFixture::new(loopback_config(RuleMode::Off)).await;
    let seed = Seeder::start(&fx.host).await;
    let guard = fx.host.guard();

    let mut idle = FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0008-"))
        .await
        .expect("idle peer");
    let ban = guard.add_entry(ban_entry()).expect("ban");
    assert!(
        idle.closed_within(WITHIN).await.is_some(),
        "adding a ban closes the live connections of that address"
    );
    assert!(
        FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0009-"))
            .await
            .is_err()
    );

    // 白名单优先于手动封禁。
    guard
        .add_entry(NewEntry {
            action: FilterAction::Allow,
            duration_secs: None,
            ..ban_entry()
        })
        .expect("allow");
    FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0010-"))
        .await
        .expect("allow list wins");

    // 删除封禁条目本身同样可行。
    guard.remove_entries(&[ban.id]).expect("remove ban");
    wait_for("changed", || fx.sink.changed() >= 1, WITHIN).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn config_updates_are_hot_and_keep_the_session_and_its_connections() {
    let fx = HostFixture::new(loopback_config(RuleMode::Observe)).await;
    let seed = Seeder::start(&fx.host).await;
    let before = std::sync::Arc::as_ptr(&seed.session);

    assert_eq!(
        seed.session.throttled_upload_bps().map(|v| v.get()),
        Some(32 * 1024),
        "the default throttle rate is pushed down on attach"
    );
    let mut peer = FakePeer::connect(seed.addr, &seed.info_hash, &peer_id(b"-FK0011-"))
        .await
        .expect("peer");

    fx.host.set_config(BtGuardConfig {
        throttle_kib: 64,
        rule_rerequest: RuleMode::Enforce,
        ..loopback_config(RuleMode::Observe)
    });
    assert_eq!(
        seed.session.throttled_upload_bps().map(|v| v.get()),
        Some(64 * 1024)
    );
    assert_eq!(fx.host.config().rule_rerequest, RuleMode::Enforce);
    assert!(fx.host.session_active());
    assert!(!seed.closing.is_cancelled(), "the session is not rebuilt");
    assert_eq!(std::sync::Arc::as_ptr(&seed.session), before);
    assert!(
        peer.stays_open(Duration::from_millis(300)).await,
        "existing connections survive a config change"
    );

    // 新规则模式对新事件立即生效。
    leech_repeat_requests(&mut peer).await;
    assert!(peer.closed_within(WITHIN).await.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn cloud_refresh_is_requested_at_session_start_and_then_on_the_interval() {
    let fx = HostFixture::with(
        BtGuardConfig {
            cloud_mode: RuleMode::Observe,
            ..BtGuardConfig::default()
        },
        HostTimings {
            cloud_interval: Duration::from_millis(40),
            ..fast_timings()
        },
    )
    .await;
    // 已应用文档的序号随请求带出。
    let expires = chrono::Utc::now().timestamp() + 86_400;
    fx.host
        .guard()
        .apply_cloud(&cloud_doc(
            7,
            expires,
            vec![CloudEntry {
                cidr: "203.0.113.0/24".to_owned(),
                category: "community".to_owned(),
                action: "ban".to_owned(),
                reason: String::new(),
                expires_at: Some(expires),
            }],
        ))
        .expect("cloud doc");
    let seed = Seeder::start(&fx.host).await;

    wait_for(
        "first request",
        || !fx.sink.cloud_requests().is_empty(),
        WITHIN,
    )
    .await;
    assert_eq!(fx.sink.cloud_requests()[0], 7);
    wait_for(
        "interval requests",
        || fx.sink.cloud_requests().len() >= 4,
        WITHIN,
    )
    .await;

    // 云名单模式关闭 → 不再请求；重新开启 → 立即请求并恢复节拍。
    fx.host.set_config(BtGuardConfig {
        cloud_mode: RuleMode::Off,
        ..BtGuardConfig::default()
    });
    tokio::time::sleep(Duration::from_millis(120)).await;
    let frozen = fx.sink.cloud_requests().len();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(fx.sink.cloud_requests().len(), frozen);
    fx.host.set_config(BtGuardConfig {
        cloud_mode: RuleMode::Observe,
        ..BtGuardConfig::default()
    });
    wait_for(
        "request after re-enable",
        || fx.sink.cloud_requests().len() > frozen,
        WITHIN,
    )
    .await;

    // 云名单开关关闭同样静默。
    fx.host.set_config(BtGuardConfig {
        cloud_enabled: false,
        ..BtGuardConfig::default()
    });
    tokio::time::sleep(Duration::from_millis(120)).await;
    let frozen = fx.sink.cloud_requests().len();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(fx.sink.cloud_requests().len(), frozen);
    drop(seed);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_cloud_request_when_the_guard_or_the_cloud_is_off_at_session_start() {
    for config in [
        BtGuardConfig {
            enabled: false,
            ..BtGuardConfig::default()
        },
        BtGuardConfig {
            cloud_enabled: false,
            ..BtGuardConfig::default()
        },
        BtGuardConfig {
            cloud_mode: RuleMode::Off,
            ..BtGuardConfig::default()
        },
    ] {
        let fx = HostFixture::new(config).await;
        let _seed = Seeder::start(&fx.host).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(fx.sink.cloud_requests().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_session_stops_every_timer_and_flushes_pending_state() {
    // 泵的合并窗口设得很长：落库只能来自「会话关闭」这个点。
    let fx = HostFixture::with(
        BtGuardConfig {
            cloud_mode: RuleMode::Observe,
            ..BtGuardConfig::default()
        },
        HostTimings {
            coalesce: Duration::from_secs(60),
            cloud_interval: Duration::from_millis(30),
            readmit_interval: Duration::from_millis(30),
            ..fast_timings()
        },
    )
    .await;
    let seed = Seeder::start(&fx.host).await;
    wait_for(
        "timer running",
        || fx.sink.cloud_requests().len() >= 3,
        WITHIN,
    )
    .await;

    fx.host.guard().add_entry(ban_entry()).expect("entry");
    assert!(
        fx.db
            .bt_guard_load_entries(0)
            .await
            .expect("entries")
            .is_empty(),
        "nothing is written before a persistence point"
    );
    seed.closing.cancel();
    let entries = loop {
        let rows = fx.db.bt_guard_load_entries(0).await.expect("entries");
        if !rows.is_empty() {
            break rows;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(entries[0].cidr, "127.0.0.1/32");
    assert!(!fx.host.session_active());

    tokio::time::sleep(Duration::from_millis(100)).await;
    let settled = fx.sink.cloud_requests().len();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        fx.sink.cloud_requests().len(),
        settled,
        "no cloud timer survives the session"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_shutdown_flushes_without_waiting_for_the_coalesce_window() {
    let fx = HostFixture::with(
        loopback_config(RuleMode::Off),
        HostTimings {
            coalesce: Duration::from_secs(60),
            ..fast_timings()
        },
    )
    .await;
    fx.host.guard().add_entry(ban_entry()).expect("entry");
    assert!(
        fx.db
            .bt_guard_load_entries(0)
            .await
            .expect("rows")
            .is_empty()
    );
    fx.host.shutdown().await;
    assert_eq!(fx.db.bt_guard_load_entries(0).await.expect("rows").len(), 1);
    // 幂等。
    fx.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn state_is_restored_from_the_database_on_the_next_start() {
    let fx = HostFixture::new(loopback_config(RuleMode::Off)).await;
    fx.host.guard().add_entry(ban_entry()).expect("entry");
    fx.host.flush_now().await.expect("flush");

    // 同一份库、全新的 guard：条目恢复，封禁立即生效。
    let sink = std::sync::Arc::new(super::swarm::RecordingSink::default());
    let restarted = fluxdown_engine::bt_guard::BtGuardHost::with_timings(
        std::sync::Arc::new(fluxdown_engine::bt_guard::BtGuard::new(loopback_config(
            RuleMode::Off,
        ))),
        fx.db.clone(),
        sink,
        fluxdown_engine::proxy_config::ProxyConfig::default(),
        fast_timings(),
    );
    let summary = restarted.load().await.expect("load");
    assert_eq!(summary.entries, 1);
    assert!(restarted.guard().is_banned(LOOPBACK));
}

#[tokio::test(flavor = "multi_thread")]
async fn readmit_retries_do_not_count_as_blocked_connections_and_unban_redials() {
    let fx = HostFixture::new(loopback_config(RuleMode::Off)).await;
    let guard = fx.host.guard();
    let seed = Seeder::start_plain().await;
    let ban = guard.add_entry(ban_entry()).expect("ban");
    let leech = Leecher::start(&fx.host, &seed).await;

    wait_for(
        "the first refused dial",
        || guard.stats().blocked_connections >= 1,
        WITHIN,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let settled = guard.stats().blocked_connections;
    // readmit_interval 为 50ms：这段时间里会话任务反复重试被拒的对端，但重试只看封禁状态、不计数。
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(guard.stats().blocked_connections, settled);
    assert!(!leech.completed(Duration::from_millis(300)).await);

    // 解封后被拒的出站对端被重新拨号，下载完成。
    guard.remove_entries(&[ban.id]).expect("unban");
    assert!(
        leech.completed(WITHIN).await,
        "the refused peer is dialed again once the ban is lifted"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_startup_load_is_retried_when_the_next_bt_session_starts() {
    let dir = super::swarm::unique_dir("loadretry");
    std::fs::create_dir_all(&dir).expect("dir");
    let url = format!(
        "sqlite:{}?mode=rwc",
        dir.join("guard.db").to_string_lossy().replace('\\', "/")
    );
    let db = fluxdown_engine::db::Db::connect(&url).await.expect("db");
    // 库里已有一条手动封禁。
    let seeding = fluxdown_engine::bt_guard::BtGuard::new(loopback_config(RuleMode::Off));
    seeding.add_entry(ban_entry()).expect("add");
    seeding.flush(&db).await.expect("seed");

    // 另开一条连接破坏 meta 表，使启动 load 失败。
    let raw = sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("raw connection");
    sqlx::query("DROP TABLE bt_guard_cloud_meta")
        .execute(&raw)
        .await
        .expect("break the meta table");
    let fx = HostFixture::with_db(loopback_config(RuleMode::Off), fast_timings(), db.clone()).await;
    assert!(fx.host.load().await.is_err());
    assert!(fx.host.guard().load_failed());
    assert!(
        !fx.host.guard().is_banned(LOOPBACK),
        "nothing was loaded: the stored ban is not in memory yet"
    );

    // 库恢复；下一次 BT 会话启动时宿主重试 load，状态改以库为准。
    sqlx::query("CREATE TABLE bt_guard_cloud_meta (id INTEGER PRIMARY KEY, data TEXT NOT NULL)")
        .execute(&raw)
        .await
        .expect("repair");
    let _seed = Seeder::start(&fx.host).await;
    wait_for("retried load", || !fx.host.guard().load_failed(), WITHIN).await;
    assert!(
        fx.host.guard().is_banned(LOOPBACK),
        "the stored ban is loaded by the retry"
    );
    std::fs::remove_dir_all(&dir).expect("cleanup");
}
