use fluxdown_engine::bt_guard::{
    BtGuard, EntryFilter, FilterAction, GuardEvent, IpGroup, NewEntry, Verdict,
};
use fluxdown_engine::db::{
    BT_GUARD_EVENT_RING, BtGuardBatch, BtGuardEntryOp, BtGuardEntryQuery, BtGuardEntryRow,
    BtGuardEventQuery, BtGuardEventRow, BtGuardLedgerRow, BtGuardPrune, BtGuardStrikeRow, Db,
};

use super::support::{
    DAY, T0, bits, cloud_doc, cloud_entry, ctx_for, deliver_piece, disconnect, enforce_config,
    ev_request, ev_upload, guard_with,
};

async fn mem_db() -> Db {
    Db::connect("sqlite::memory:").await.expect("connect")
}

fn entry_row(
    id: i64,
    layer: &str,
    cidr: &str,
    rule: &str,
    source: &str,
    expires: Option<i64>,
) -> BtGuardEntryRow {
    BtGuardEntryRow {
        id,
        layer: layer.to_owned(),
        cidr: cidr.to_owned(),
        action: "ban".to_owned(),
        rule: rule.to_owned(),
        reason: format!("reason {id}"),
        evidence: String::new(),
        source: source.to_owned(),
        created_at: T0,
        expires_at: expires,
    }
}

fn event_row(kind: &str, rule: &str, detail: &str) -> BtGuardEventRow {
    BtGuardEventRow {
        id: 0,
        at: T0,
        kind: kind.to_owned(),
        ip_group: "1.2.3.4".to_owned(),
        info_hash: "ab".repeat(20),
        rule: rule.to_owned(),
        mode: "enforce".to_owned(),
        action: "ban".to_owned(),
        detail: detail.to_owned(),
    }
}

fn entry_query(layer: Option<&str>, rule: Option<&str>, text: Option<&str>) -> BtGuardEntryQuery {
    BtGuardEntryQuery {
        layer: layer.map(str::to_owned),
        rule: rule.map(str::to_owned),
        query: text.map(str::to_owned),
        active_at: None,
        offset: 0,
        limit: 1000,
    }
}

fn ids(rows: &[BtGuardEntryRow]) -> Vec<i64> {
    rows.iter().map(|r| r.id).collect()
}

// ------------------------------------------------------------------ Db 层

#[tokio::test]
async fn schema_is_created_and_empty_batch_is_a_noop() {
    let db = mem_db().await;
    assert!(
        db.bt_guard_load_entries(T0)
            .await
            .expect("entries")
            .is_empty()
    );
    assert!(
        db.bt_guard_load_strikes()
            .await
            .expect("strikes")
            .is_empty()
    );
    assert!(db.bt_guard_load_ledger(0).await.expect("ledger").is_empty());
    assert_eq!(db.bt_guard_max_entry_id().await.expect("max"), 0);
    let batch = BtGuardBatch::default();
    assert!(batch.is_empty());
    db.bt_guard_apply_batch(&batch).await.expect("noop");
    let (total, items) = db
        .bt_guard_list_events(&BtGuardEventQuery {
            limit: 10,
            ..BtGuardEventQuery::default()
        })
        .await
        .expect("events");
    assert_eq!((total, items.len()), (0, 0));
    assert_eq!(BT_GUARD_EVENT_RING, 5000);
}

#[tokio::test]
async fn entry_round_trip_filters_and_paging() {
    let db = mem_db().await;
    let mut with_evidence = entry_row(
        3,
        "auto",
        "2001:db8::/64",
        "excessive",
        "auto",
        Some(T0 + 100),
    );
    with_evidence.evidence = "{\"ratioMilli\":2100}".to_owned();
    let batch = BtGuardBatch {
        entry_ops: vec![
            BtGuardEntryOp::Upsert(entry_row(1, "manual", "1.2.3.4/32", "manual", "user", None)),
            BtGuardEntryOp::Upsert(entry_row(2, "allow", "9.9.9.9/32", "manual", "user", None)),
            BtGuardEntryOp::Upsert(with_evidence.clone()),
            BtGuardEntryOp::Upsert(entry_row(
                4,
                "cloud",
                "5.5.5.0/24",
                "cloud",
                "cloud#1#99",
                Some(T0 - 5),
            )),
            BtGuardEntryOp::Upsert(entry_row(
                5,
                "file",
                "7.7.7.0/24",
                "file",
                "https://x.example/100%_list",
                None,
            )),
        ],
        ..BtGuardBatch::default()
    };
    db.bt_guard_apply_batch(&batch).await.expect("apply");

    // 启动加载只返回未过期条目，按 id 升序，字段完整往返。
    let loaded = db.bt_guard_load_entries(T0).await.expect("load");
    assert_eq!(ids(&loaded), vec![1, 2, 3, 5]);
    assert_eq!(loaded[2], with_evidence);
    assert_eq!(loaded[0].expires_at, None);
    assert_eq!(db.bt_guard_max_entry_id().await.expect("max"), 5);

    let list = |q: BtGuardEntryQuery| {
        let db = db.clone();
        async move { db.bt_guard_list_entries(&q).await.expect("list") }
    };
    let (total, rows) = list(entry_query(None, None, None)).await;
    assert_eq!(
        (total, ids(&rows)),
        (5, vec![5, 4, 3, 2, 1]),
        "descending id"
    );
    assert_eq!(list(entry_query(Some("auto"), None, None)).await.0, 1);
    assert_eq!(list(entry_query(None, Some("cloud"), None)).await.0, 1);
    assert_eq!(
        list(entry_query(Some("manual"), Some("manual"), None))
            .await
            .0,
        1
    );
    // 子串匹配：不区分大小写，覆盖 cidr / reason / source。
    assert_eq!(
        ids(&list(entry_query(None, None, Some("2001:DB8"))).await.1),
        vec![3]
    );
    assert_eq!(
        ids(&list(entry_query(None, None, Some("REASON 2"))).await.1),
        vec![2]
    );
    assert_eq!(
        ids(&list(entry_query(None, None, Some("x.example"))).await.1),
        vec![5]
    );
    // LIKE 通配符按字面匹配：`%` / `_` 不是通配。
    assert_eq!(
        ids(&list(entry_query(None, None, Some("100%_list"))).await.1),
        vec![5]
    );
    assert_eq!(list(entry_query(None, None, Some("%"))).await.0, 1);
    assert_eq!(list(entry_query(None, None, Some("_"))).await.0, 1);
    assert_eq!(list(entry_query(None, None, Some("nothing"))).await.0, 0);
    // 到期过滤：`expires_at > now` 才算有效。
    let mut active = entry_query(None, None, None);
    active.active_at = Some(T0);
    assert_eq!(ids(&list(active.clone()).await.1), vec![5, 3, 2, 1]);
    active.active_at = Some(T0 + 100);
    assert_eq!(ids(&list(active).await.1), vec![5, 2, 1]);
    // 分页：total 不受 offset / limit 影响。
    let mut page = entry_query(None, None, None);
    page.offset = 1;
    page.limit = 2;
    let (total, rows) = list(page).await;
    assert_eq!((total, ids(&rows)), (5, vec![4, 3]));
}

#[tokio::test]
async fn entry_ops_upsert_delete_and_replace_layer() {
    let db = mem_db().await;
    let apply = |ops: Vec<BtGuardEntryOp>| {
        let db = db.clone();
        async move {
            db.bt_guard_apply_batch(&BtGuardBatch {
                entry_ops: ops,
                ..BtGuardBatch::default()
            })
            .await
            .expect("apply");
        }
    };
    let all = || {
        let db = db.clone();
        async move {
            ids(&db
                .bt_guard_list_entries(&entry_query(None, None, None))
                .await
                .expect("list")
                .1)
        }
    };
    apply(vec![BtGuardEntryOp::Upsert(entry_row(
        1,
        "manual",
        "1.1.1.1/32",
        "manual",
        "user",
        None,
    ))])
    .await;
    // 同 id 再次 upsert：覆盖而非重复（失败重试幂等）。
    let mut changed = entry_row(1, "manual", "1.1.1.2/32", "manual", "user", Some(T0 + 5));
    changed.reason = "updated".to_owned();
    apply(vec![BtGuardEntryOp::Upsert(changed.clone())]).await;
    assert_eq!(
        db.bt_guard_load_entries(T0).await.expect("load"),
        vec![changed]
    );

    // 删除：存在的删除，不存在的忽略。
    apply(vec![BtGuardEntryOp::Delete(1), BtGuardEntryOp::Delete(404)]).await;
    assert!(all().await.is_empty());

    // 按来源整体替换 file 层：只动该来源。
    apply(vec![
        BtGuardEntryOp::Upsert(entry_row(10, "file", "7.0.0.0/24", "file", "A", None)),
        BtGuardEntryOp::Upsert(entry_row(11, "file", "7.0.1.0/24", "file", "A", None)),
        BtGuardEntryOp::Upsert(entry_row(12, "file", "8.0.0.0/24", "file", "B", None)),
        BtGuardEntryOp::Upsert(entry_row(20, "manual", "9.0.0.0/24", "manual", "A", None)),
    ])
    .await;
    apply(vec![BtGuardEntryOp::ReplaceLayer {
        layer: "file".to_owned(),
        source: Some("A".to_owned()),
        rows: vec![entry_row(13, "file", "7.0.2.0/24", "file", "A", None)],
    }])
    .await;
    assert_eq!(
        all().await,
        vec![20, 13, 12],
        "A replaced; B and the manual layer untouched"
    );
    // 不带来源：整层替换（空 rows = 清层）。
    apply(vec![BtGuardEntryOp::ReplaceLayer {
        layer: "file".to_owned(),
        source: None,
        rows: Vec::new(),
    }])
    .await;
    assert_eq!(all().await, vec![20]);
    // 一个批次内的操作按顺序生效。
    apply(vec![
        BtGuardEntryOp::Upsert(entry_row(
            30,
            "manual",
            "3.3.3.3/32",
            "manual",
            "user",
            None,
        )),
        BtGuardEntryOp::Delete(30),
        BtGuardEntryOp::Upsert(entry_row(
            31,
            "manual",
            "3.3.3.4/32",
            "manual",
            "user",
            None,
        )),
    ])
    .await;
    assert_eq!(all().await, vec![31, 20]);
}

fn ledger_row(hash: &str, group: &str, bytes: i64, last_seen: i64) -> BtGuardLedgerRow {
    BtGuardLedgerRow {
        info_hash: hash.to_owned(),
        ip_group: group.to_owned(),
        delivered_bytes: bytes,
        dup_bytes: bytes / 2,
        delivered_bits: (0..=255u8).collect(),
        claimed_bits: vec![0x00, 0xff, 0x80, 0x01],
        rerequests: 3,
        probed_at: 77,
        first_seen: T0 - 10,
        last_seen,
        lag_cleared_at: 0,
        excess_baseline: 0,
    }
}

#[tokio::test]
async fn ledger_and_strike_round_trip_including_binary_blobs() {
    let db = mem_db().await;
    let hash = "cd".repeat(20);
    let row = ledger_row(&hash, "1.2.3.4", 5_000_000_000, T0);
    let big = BtGuardBatch {
        ledgers: vec![row.clone(), ledger_row(&hash, "2001:db8::/64", 1, T0 - 100)],
        strikes: vec![BtGuardStrikeRow {
            ip_group: "1.2.3.4".to_owned(),
            strikes: 3,
            last_strike_at: T0,
            last_ban_until: 0,
        }],
        ..BtGuardBatch::default()
    };
    db.bt_guard_apply_batch(&big).await.expect("apply");
    let mut loaded = db.bt_guard_load_ledger(0).await.expect("load");
    loaded.sort_by(|a, b| a.ip_group.cmp(&b.ip_group));
    assert_eq!(loaded.len(), 2);
    let read = loaded
        .iter()
        .find(|r| r.ip_group == "1.2.3.4")
        .expect("row");
    assert_eq!(
        *read, row,
        "BIGINT byte counters (> 4 GiB) and blobs survive"
    );
    // since 过滤。
    assert_eq!(
        db.bt_guard_load_ledger(T0 - 50).await.expect("since").len(),
        1
    );
    // upsert 更新。
    let mut updated = row.clone();
    updated.delivered_bytes = 9;
    updated.claimed_bits = Vec::new();
    db.bt_guard_apply_batch(&BtGuardBatch {
        ledgers: vec![updated.clone()],
        strikes: vec![BtGuardStrikeRow {
            ip_group: "1.2.3.4".to_owned(),
            strikes: 4,
            last_strike_at: T0 + 1,
            last_ban_until: T0 + 7,
        }],
        ..BtGuardBatch::default()
    })
    .await
    .expect("update");
    let loaded = db.bt_guard_load_ledger(T0).await.expect("load");
    assert_eq!(loaded, vec![updated]);
    let strikes = db.bt_guard_load_strikes().await.expect("strikes");
    assert_eq!(
        (
            strikes[0].strikes,
            strikes[0].last_strike_at,
            strikes[0].last_ban_until
        ),
        (4, T0 + 1, T0 + 7)
    );
    // 删除。
    db.bt_guard_apply_batch(&BtGuardBatch {
        ledger_deletes: vec![
            (hash.clone(), "1.2.3.4".to_owned()),
            ("nope".to_owned(), "x".to_owned()),
        ],
        strike_deletes: vec!["1.2.3.4".to_owned()],
        ..BtGuardBatch::default()
    })
    .await
    .expect("delete");
    assert_eq!(db.bt_guard_load_ledger(0).await.expect("load").len(), 1);
    assert!(
        db.bt_guard_load_strikes()
            .await
            .expect("strikes")
            .is_empty()
    );
}

#[tokio::test]
async fn event_ring_keeps_the_newest_5000() {
    let db = mem_db().await;
    for chunk in 0..2 {
        let events = (0..3000)
            .map(|i| event_row("enforced", "excessive", &(chunk * 3000 + i).to_string()))
            .collect();
        db.bt_guard_apply_batch(&BtGuardBatch {
            events,
            ..BtGuardBatch::default()
        })
        .await
        .expect("apply");
    }
    let (total, newest) = db
        .bt_guard_list_events(&BtGuardEventQuery {
            limit: 5000,
            ..BtGuardEventQuery::default()
        })
        .await
        .expect("list");
    assert_eq!(total, 5000);
    assert_eq!(newest.len(), 5000);
    assert_eq!(newest[0].detail, "5999");
    assert_eq!(newest[4999].detail, "1000", "oldest 1000 were trimmed");
    assert!(newest.windows(2).all(|w| w[0].id > w[1].id));
}

#[tokio::test]
async fn event_listing_filters_and_pages() {
    let db = mem_db().await;
    db.bt_guard_apply_batch(&BtGuardBatch {
        events: vec![
            event_row("observed", "excessive", "1"),
            event_row("enforced", "excessive", "2"),
            event_row("probe", "lag", "3"),
            event_row("probe_cleared", "lag", "4"),
            event_row("entry_added", "", "5"),
        ],
        ..BtGuardBatch::default()
    })
    .await
    .expect("apply");
    let q = |rule: Option<&str>, kind: Option<&str>, offset, limit| BtGuardEventQuery {
        rule: rule.map(str::to_owned),
        kind: kind.map(str::to_owned),
        ip_group: None,
        offset,
        limit,
    };
    let list = |query: BtGuardEventQuery| {
        let db = db.clone();
        async move { db.bt_guard_list_events(&query).await.expect("list") }
    };
    let (total, rows) = list(q(Some("lag"), None, 0, 10)).await;
    assert_eq!(
        (
            total,
            rows.iter().map(|r| r.detail.as_str()).collect::<Vec<_>>()
        ),
        (2, vec!["4", "3"])
    );
    assert_eq!(list(q(None, Some("probe"), 0, 10)).await.0, 1);
    assert_eq!(
        list(q(Some("lag"), Some("probe_cleared"), 0, 10)).await.0,
        1
    );
    assert_eq!(list(q(Some("excessive"), Some("probe"), 0, 10)).await.0, 0);
    let (total, rows) = list(q(None, None, 1, 2)).await;
    assert_eq!((total, rows.len()), (5, 2));
    assert_eq!(rows[0].detail, "4");
    // 字段完整往返。
    let first = &list(q(None, Some("observed"), 0, 1)).await.1[0];
    assert_eq!(first.ip_group, "1.2.3.4");
    assert_eq!(first.info_hash, "ab".repeat(20));
    assert_eq!(
        (first.mode.as_str(), first.action.as_str(), first.at),
        ("enforce", "ban", T0)
    );
}

#[tokio::test]
async fn prune_removes_expired_entries_and_stale_rows() {
    let db = mem_db().await;
    let hash = "cd".repeat(20);
    db.bt_guard_apply_batch(&BtGuardBatch {
        entry_ops: vec![
            BtGuardEntryOp::Upsert(entry_row(
                1,
                "auto",
                "1.1.1.1/32",
                "x",
                "auto",
                Some(T0 - 1),
            )),
            BtGuardEntryOp::Upsert(entry_row(2, "auto", "1.1.1.2/32", "x", "auto", Some(T0))),
            BtGuardEntryOp::Upsert(entry_row(
                3,
                "auto",
                "1.1.1.3/32",
                "x",
                "auto",
                Some(T0 + 1),
            )),
            BtGuardEntryOp::Upsert(entry_row(4, "manual", "1.1.1.4/32", "x", "user", None)),
        ],
        strikes: vec![
            BtGuardStrikeRow {
                ip_group: "a".to_owned(),
                strikes: 1,
                last_strike_at: T0 - 100,
                last_ban_until: 0,
            },
            BtGuardStrikeRow {
                ip_group: "b".to_owned(),
                strikes: 1,
                last_strike_at: T0 - 99,
                last_ban_until: 0,
            },
            // 最近 strike 很旧，但最近封禁的到期时刻仍在截止线之后 → 保留。
            BtGuardStrikeRow {
                ip_group: "c".to_owned(),
                strikes: 4,
                last_strike_at: T0 - 500,
                last_ban_until: T0 - 50,
            },
        ],
        ledgers: vec![
            ledger_row(&hash, "old", 1, T0 - 100),
            ledger_row(&hash, "edge", 1, T0 - 50),
        ],
        ..BtGuardBatch::default()
    })
    .await
    .expect("seed");
    db.bt_guard_apply_batch(&BtGuardBatch {
        prune: Some(BtGuardPrune {
            now: T0,
            ledger_cutoff: T0 - 50,
            strike_cutoff: T0 - 99,
        }),
        ..BtGuardBatch::default()
    })
    .await
    .expect("prune");
    let rows = db
        .bt_guard_list_entries(&entry_query(None, None, None))
        .await
        .expect("list")
        .1;
    assert_eq!(ids(&rows), vec![4, 3], "expires_at <= now is gone");
    let strikes = db.bt_guard_load_strikes().await.expect("strikes");
    let mut kept: Vec<&str> = strikes.iter().map(|s| s.ip_group.as_str()).collect();
    kept.sort_unstable();
    assert_eq!(kept, vec!["b", "c"]);
    let ledgers = db.bt_guard_load_ledger(0).await.expect("ledger");
    assert_eq!(
        ledgers
            .iter()
            .map(|l| l.ip_group.as_str())
            .collect::<Vec<_>>(),
        vec!["edge"]
    );
}

// ------------------------------------------------------ BtGuard ↔ Db

fn group(text: &str) -> IpGroup {
    text.parse().expect("group")
}

fn manual_ban(g: &BtGuard, cidr: &str, secs: Option<i64>) -> i64 {
    g.add_entry(NewEntry {
        cidr: cidr.to_owned(),
        action: FilterAction::Ban,
        duration_secs: secs,
        reason: "m".to_owned(),
    })
    .expect("add")
    .id
}

async fn stored_entries(db: &Db) -> Vec<BtGuardEntryRow> {
    db.bt_guard_list_entries(&entry_query(None, None, None))
        .await
        .expect("list")
        .1
}

async fn stored_events(db: &Db, kind: &str) -> usize {
    db.bt_guard_list_events(&BtGuardEventQuery {
        kind: Some(kind.to_owned()),
        limit: 1000,
        ..BtGuardEventQuery::default()
    })
    .await
    .expect("events")
    .0 as usize
}

#[tokio::test]
async fn flush_then_load_restores_everything() {
    let db = mem_db().await;
    let (g1, _c1) = guard_with(enforce_config());
    // auto 封禁 + strike（rerequest）。
    let banned = ctx_for("1.2.3.4", true);
    g1.on_event_at(&banned, ev_request(1, true), T0);
    assert_eq!(
        g1.on_event_at(&banned, ev_request(2, true), T0),
        Verdict::Disconnect
    );
    // 账本：向 3.3.3.3 送满 100 片并以全 1 位图断开。
    let seeded = ctx_for("3.3.3.3", true);
    for p in 0..100 {
        deliver_piece(&g1, &seeded, p, T0);
    }
    disconnect(&g1, &seeded, Some(&bits(0..100)), T0);
    manual_ban(&g1, "9.9.9.0/24", Some(3600));
    g1.add_entry(NewEntry {
        cidr: "8.8.4.4".to_owned(),
        action: FilterAction::Allow,
        duration_secs: None,
        reason: String::new(),
    })
    .expect("allow");
    g1.apply_cloud(&cloud_doc(
        7,
        T0 + DAY,
        vec![
            cloud_entry("20.0.0.1/32", "ban"),
            cloud_entry("20.0.0.2/32", "choke"),
        ],
    ))
    .expect("cloud");
    g1.apply_filter_file("https://f.example/a", "7.7.7.0/24\n");
    assert!(g1.needs_flush());
    g1.flush(&db).await.expect("flush");
    assert!(!g1.needs_flush(), "flush drains the pending queue");

    let (g2, _c2) = guard_with(enforce_config());
    let summary = g2.load(&db).await.expect("load");
    assert_eq!(summary.entries, 6);
    assert_eq!(summary.skipped_entries, 0);
    assert_eq!(summary.ledgers, 2);
    assert_eq!(summary.strikes, 1);
    let (a, b) = (g1.counts(), g2.counts());
    assert_eq!(
        (a.allow, a.manual, a.auto, a.cloud, a.file),
        (b.allow, b.manual, b.auto, b.cloud, b.file)
    );
    assert_eq!(g2.strikes_of(&group("1.2.3.4")), 1);
    for (ip, banned) in [
        ("1.2.3.4", true),
        ("9.9.9.9", true),
        ("20.0.0.1", true),
        ("7.7.7.7", true),
        ("8.8.4.4", false),
        ("20.0.0.2", false),
    ] {
        assert_eq!(!g2.admit(ip.parse().expect("ip")), banned, "{ip}");
    }
    // 账本位集完整恢复：重连丢 8% 仍被 rewind 识别。
    let b = bits(8..100);
    assert_eq!(
        g2.on_event_at(&seeded, GuardEvent::Bitfield { bits: &b }, T0 + 5),
        Verdict::Disconnect
    );
    assert_eq!(
        g2.group_delivered_bytes(&seeded.info_hash, &group("3.3.3.3")),
        Some(100 * 1024 * 1024)
    );
    // 云名单 sequence / 有效期从条目来源恢复；clientRules 不持久化（由 agent 重放缓存）。
    assert_eq!(g2.cloud_sequence(), 7);
    let status = g2.cloud_status();
    assert_eq!(
        (status.expires_at, status.entries, status.client_rules),
        (T0 + DAY, 2, 0)
    );
    assert!(matches!(
        g2.apply_cloud(&cloud_doc(6, T0 + DAY, vec![])),
        Err(fluxdown_engine::bt_guard::CloudReject::Rollback { current: 7, got: 6 })
    ));
    assert!(
        g2.apply_cloud(&cloud_doc(
            7,
            T0 + DAY,
            vec![cloud_entry("20.0.0.1/32", "ban")]
        ))
        .is_ok()
    );
    // 加载后继续分配的 id 不与已存条目冲突。
    let max = db.bt_guard_max_entry_id().await.expect("max");
    let fresh = manual_ban(&g2, "6.6.6.6", None);
    assert!(fresh > max, "{fresh} must exceed {max}");
}

#[tokio::test]
async fn idle_flush_is_a_noop_and_ordering_is_preserved() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    g.flush(&db).await.expect("idle flush");
    assert!(stored_entries(&db).await.is_empty());
    // 同一次 flush 前新增又删除：净效果为空。
    let id = manual_ban(&g, "5.5.5.5", None);
    g.remove_entries(&[id]).expect("remove");
    g.flush(&db).await.expect("flush");
    assert!(stored_entries(&db).await.is_empty());
    // 已落库的条目删除后再 flush 即移除。
    let keep = manual_ban(&g, "5.5.5.6", None);
    let gone = manual_ban(&g, "5.5.5.7", None);
    g.flush(&db).await.expect("flush");
    assert_eq!(ids(&stored_entries(&db).await), vec![gone, keep]);
    g.remove_entries(&[gone]).expect("remove");
    g.flush(&db).await.expect("flush");
    assert_eq!(ids(&stored_entries(&db).await), vec![keep]);
    let (g2, _c2) = guard_with(enforce_config());
    g2.load(&db).await.expect("load");
    assert!(!g2.admit("5.5.5.6".parse().expect("ip")));
    assert!(g2.admit("5.5.5.7".parse().expect("ip")));
}

#[tokio::test]
async fn expired_entries_and_stale_strikes_are_pruned_at_flush() {
    let db = mem_db().await;
    let (g, c) = guard_with(enforce_config());
    manual_ban(&g, "5.5.5.5", Some(100));
    let banned = ctx_for("1.2.3.4", true);
    g.on_event_at(&banned, ev_request(1, true), T0);
    g.on_event_at(&banned, ev_request(2, true), T0);
    g.flush(&db).await.expect("flush");
    assert_eq!(stored_entries(&db).await.len(), 2);
    assert_eq!(db.bt_guard_load_strikes().await.expect("strikes").len(), 1);
    // 100s 后手动条目到期；24h 后 auto 封禁到期；strike 仍在衰减期内。
    c.set(T0 + 100);
    g.flush(&db).await.expect("flush");
    assert_eq!(stored_entries(&db).await.len(), 1);
    c.set(T0 + 25 * 3600);
    g.flush(&db).await.expect("flush");
    assert!(stored_entries(&db).await.is_empty());
    assert_eq!(db.bt_guard_load_strikes().await.expect("strikes").len(), 1);
    assert_eq!(g.counts().auto + g.counts().manual, 0);
    // 衰减从封禁到期（T0+24h）起算：到期后 30 天无新 strike → 行被清掉。
    c.set(T0 + 24 * 3600 + 30 * DAY + 1);
    g.flush(&db).await.expect("flush");
    assert!(
        db.bt_guard_load_strikes()
            .await
            .expect("strikes")
            .is_empty()
    );
    assert_eq!(g.strikes_of(&group("1.2.3.4")), 0);
}

#[tokio::test]
async fn ledger_retention_is_fourteen_days_and_spares_live_connections() {
    let db = mem_db().await;
    let (g, c) = guard_with(enforce_config());
    let gone = ctx_for("1.2.3.4", true);
    deliver_piece(&g, &gone, 0, T0);
    disconnect(&g, &gone, None, T0);
    let live = ctx_for("5.6.7.8", true);
    deliver_piece(&g, &live, 0, T0); // 连接保持，不断开
    g.flush_all(&db).await.expect("flush");
    assert_eq!(db.bt_guard_load_ledger(0).await.expect("ledger").len(), 2);
    // 刚好 14 天（last_seen == cutoff）：保留。
    c.set(T0 + 14 * DAY);
    g.flush(&db).await.expect("flush");
    assert_eq!(g.ledger_group_count(&gone.info_hash), 2);
    // 超过 14 天：无连接的组被清，有存活连接的保留。
    c.set(T0 + 14 * DAY + 1);
    g.flush(&db).await.expect("flush");
    assert_eq!(g.ledger_group_count(&gone.info_hash), 1);
    assert_eq!(
        g.group_delivered_bytes(&gone.info_hash, &group("1.2.3.4")),
        None
    );
    assert!(
        g.group_delivered_bytes(&live.info_hash, &group("5.6.7.8"))
            .is_some()
    );
    let rows = db.bt_guard_load_ledger(0).await.expect("ledger");
    assert_eq!(
        rows.iter().map(|r| r.ip_group.as_str()).collect::<Vec<_>>(),
        vec!["5.6.7.8"]
    );
}

#[tokio::test]
async fn ledger_is_capped_at_4096_groups_per_torrent_evicting_the_oldest() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    let ip = |i: u32| format!("21.{}.{}.{}", i / 65536, (i / 256) % 256, i % 256);
    let hash = ctx_for("21.0.0.0", true).info_hash;
    for i in 0..4100u32 {
        let ctx = ctx_for(&ip(i), true);
        g.on_event_at(&ctx, ev_upload(0, 16384), T0 + i64::from(i));
    }
    assert_eq!(g.ledger_group_count(&hash), 4096);
    for i in 0..4 {
        assert_eq!(
            g.group_delivered_bytes(&hash, &group(&ip(i))),
            None,
            "oldest {i} evicted"
        );
    }
    assert_eq!(g.group_delivered_bytes(&hash, &group(&ip(4))), Some(16384));
    assert_eq!(
        g.group_delivered_bytes(&hash, &group(&ip(4099))),
        Some(16384)
    );
    g.flush_all(&db).await.expect("flush");
    assert_eq!(
        db.bt_guard_load_ledger(0).await.expect("ledger").len(),
        4100,
        "evicted-but-dirty ledgers are persisted, not dropped"
    );
    // 另一个种子有自己的上限。
    let mut other = ctx_for("21.0.0.0", true);
    other.info_hash = [9; 20];
    g.on_event_at(&other, ev_upload(0, 16384), T0);
    assert_eq!(g.ledger_group_count(&other.info_hash), 1);
}

#[tokio::test]
async fn evicted_but_already_persisted_ledgers_are_deleted_from_the_db() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    let ip = |i: u32| format!("21.0.{}.{}", i / 256, i % 256);
    let hash = ctx_for("21.0.0.0", true).info_hash;
    g.on_event_at(&ctx_for(&ip(0), true), ev_upload(0, 16384), T0);
    g.flush_all(&db).await.expect("flush");
    assert_eq!(db.bt_guard_load_ledger(0).await.expect("ledger").len(), 1);
    for i in 1..4097u32 {
        g.on_event_at(
            &ctx_for(&ip(i), true),
            ev_upload(0, 16384),
            T0 + i64::from(i),
        );
    }
    assert_eq!(g.group_delivered_bytes(&hash, &group(&ip(0))), None);
    g.flush_all(&db).await.expect("flush");
    let rows = db.bt_guard_load_ledger(0).await.expect("ledger");
    assert_eq!(rows.len(), 4096);
    assert!(
        rows.iter().all(|r| r.ip_group != ip(0)),
        "evicted row must not resurrect on reload"
    );
}

#[tokio::test]
async fn decisions_and_admin_actions_are_logged_as_events() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    // entry_added / entry_removed
    let id = manual_ban(&g, "5.5.5.5", None);
    g.remove_entries(&[id]).expect("remove");
    // cloud_applied / cloud_rejected
    g.apply_cloud(&cloud_doc(
        5,
        T0 + DAY,
        vec![cloud_entry("20.0.0.1/32", "ban")],
    ))
    .expect("cloud");
    assert!(g.apply_cloud(&cloud_doc(4, T0 + DAY, vec![])).is_err());
    // filter_applied
    g.apply_filter_file("https://f.example/a", "7.7.7.0/24\n");
    // enforced（ban）
    let banned = ctx_for("1.2.3.4", true);
    g.on_event_at(&banned, ev_request(1, true), T0);
    g.on_event_at(&banned, ev_request(2, true), T0);
    // probe → probe_cleared（lag 往返）
    let lag = ctx_for("4.4.4.4", true);
    for p in 0..4 {
        deliver_piece(&g, &lag, p, T0);
    }
    assert_eq!(
        g.on_event_at(&lag, ev_request(60, false), T0 + 200),
        Verdict::Disconnect
    );
    disconnect(&g, &lag, None, T0 + 201);
    let b = bits(0..4);
    g.on_event_at(&lag, GuardEvent::Bitfield { bits: &b }, T0 + 210);
    g.flush(&db).await.expect("flush");
    for kind in [
        "entry_added",
        "entry_removed",
        "cloud_applied",
        "cloud_rejected",
        "filter_applied",
        "enforced",
        "probe",
        "probe_cleared",
    ] {
        assert!(stored_events(&db, kind).await >= 1, "missing {kind} event");
    }
    // 事件携带规则 / 动作 / 组 / infohash（hex）。
    let (_, rows) = db
        .bt_guard_list_events(&BtGuardEventQuery {
            kind: Some("enforced".to_owned()),
            limit: 10,
            ..BtGuardEventQuery::default()
        })
        .await
        .expect("events");
    let e = &rows[0];
    assert_eq!(
        (e.rule.as_str(), e.action.as_str(), e.mode.as_str()),
        ("rerequest", "ban", "enforce")
    );
    assert_eq!(e.ip_group, "1.2.3.4");
    assert_eq!(e.info_hash, "01".repeat(20));
    let detail: serde_json::Value = serde_json::from_str(&e.detail).expect("json detail");
    assert_eq!(detail["strikes"], 1);
    assert!(detail.get("torrentSize").is_some());
    // 已落库的事件不会重复写。
    g.flush(&db).await.expect("flush again");
    assert_eq!(stored_events(&db, "enforced").await, 1);
}

#[tokio::test]
async fn observed_events_are_logged_without_actions() {
    let db = mem_db().await;
    let (g, _c) = guard_with(Default::default());
    let ctx = ctx_for("1.2.3.4", true);
    g.on_event_at(&ctx, ev_request(1, true), T0);
    g.on_event_at(&ctx, ev_request(2, true), T0);
    g.flush(&db).await.expect("flush");
    assert_eq!(stored_events(&db, "observed").await, 1);
    assert_eq!(stored_events(&db, "enforced").await, 0);
    assert!(stored_entries(&db).await.is_empty());
}

#[tokio::test]
async fn file_and_cloud_layers_are_replaced_not_duplicated_in_the_db() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    g.apply_filter_file("https://f.example/a", "7.7.7.0/24\n7.7.8.0/24\n");
    g.apply_filter_file("https://f.example/b", "9.9.9.9\n");
    g.apply_cloud(&cloud_doc(
        1,
        T0 + DAY,
        vec![
            cloud_entry("20.0.0.1/32", "ban"),
            cloud_entry("20.0.0.2/32", "ban"),
        ],
    ))
    .expect("cloud");
    g.flush(&db).await.expect("flush");
    assert_eq!(stored_entries(&db).await.len(), 5);
    // 重新应用：A 缩为 1 条，云名单换成 1 条。
    g.apply_filter_file("https://f.example/a", "7.7.7.0/24\n");
    g.apply_cloud(&cloud_doc(
        2,
        T0 + DAY,
        vec![cloud_entry("20.0.0.3/32", "ban")],
    ))
    .expect("cloud");
    g.flush(&db).await.expect("flush");
    let rows = stored_entries(&db).await;
    assert_eq!(rows.len(), 3, "A(1) + B(1) + cloud(1)");
    let (g2, _c2) = guard_with(enforce_config());
    g2.load(&db).await.expect("load");
    let c = g2.counts();
    assert_eq!((c.file, c.cloud), (2, 1));
    assert!(g2.admit("7.7.8.1".parse().expect("ip")));
    assert!(!g2.admit("7.7.7.1".parse().expect("ip")));
    assert!(!g2.admit("20.0.0.3".parse().expect("ip")));
    assert!(g2.admit("20.0.0.1".parse().expect("ip")));
    // 条目 id 在重新应用后保持唯一。
    let all = g2
        .list_entries(&EntryFilter {
            limit: 100,
            ..EntryFilter::default()
        })
        .1;
    let mut seen: Vec<i64> = all.iter().map(|e| e.id).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), all.len());
}

#[tokio::test]
async fn load_replaces_prior_memory_state() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    manual_ban(&g, "5.5.5.5", None);
    g.flush(&db).await.expect("flush");
    let (g2, _c2) = guard_with(enforce_config());
    manual_ban(&g2, "6.6.6.6", None); // 未落库、加载后应被丢弃
    g2.load(&db).await.expect("load");
    assert!(!g2.admit("5.5.5.5".parse().expect("ip")));
    assert!(g2.admit("6.6.6.6".parse().expect("ip")));
}

// ---------------------------------------------------- 账本内存上限

fn big_ctx(ip: &str, num_pieces: u32) -> fluxdown_engine::bt_guard::GuardCtx {
    let mut ctx = ctx_for(ip, true);
    ctx.num_pieces = num_pieces;
    ctx.total_length = u64::from(num_pieces) * u64::from(super::support::PIECE_LEN);
    ctx
}

#[test]
fn ledger_group_cap_formula() {
    use fluxdown_engine::bt_guard::ledger_group_cap;
    assert_eq!(ledger_group_cap(0), 4096);
    assert_eq!(ledger_group_cap(100), 4096);
    // 8 MiB / (2 × ceil(n/8) + 256)：恰好 2048 字节/条 → 4096。
    assert_eq!(ledger_group_cap(7168), 4096);
    assert_eq!(ledger_group_cap(7169), 4092);
    assert_eq!(ledger_group_cap(10_000), 3043);
    assert_eq!(ledger_group_cap(100_000), 332);
    // 至少 64。
    assert_eq!(ledger_group_cap(1_000_000), 64);
    assert_eq!(ledger_group_cap(u32::MAX), 64);
}

#[tokio::test]
async fn large_torrents_get_a_smaller_cap_with_lru_order() {
    let (g, _c) = guard_with(enforce_config());
    let ip = |i: u32| format!("22.0.{}.{}", i / 256, i % 256);
    let hash = big_ctx("22.0.0.0", 100_000).info_hash;
    for i in 0..332u32 {
        g.on_event_at(
            &big_ctx(&ip(i), 100_000),
            ev_upload(0, 16384),
            T0 + i64::from(i),
        );
    }
    assert_eq!(g.ledger_group_count(&hash), 332);
    // 最旧的组 0 被再次看到后变新；新增一个组应淘汰组 1 而不是组 0。
    g.on_event_at(&big_ctx(&ip(0), 100_000), ev_upload(0, 16384), T0 + 1000);
    g.on_event_at(&big_ctx(&ip(332), 100_000), ev_upload(0, 16384), T0 + 1001);
    assert_eq!(g.ledger_group_count(&hash), 332);
    assert!(
        g.group_delivered_bytes(&hash, &group(&ip(0))).is_some(),
        "recently seen survives"
    );
    assert_eq!(
        g.group_delivered_bytes(&hash, &group(&ip(1))),
        None,
        "oldest evicted first"
    );
    assert!(g.group_delivered_bytes(&hash, &group(&ip(2))).is_some());
    // 连续淘汰按 last_seen 升序。
    g.on_event_at(&big_ctx(&ip(333), 100_000), ev_upload(0, 16384), T0 + 1002);
    assert_eq!(g.group_delivered_bytes(&hash, &group(&ip(2))), None);
    assert!(g.group_delivered_bytes(&hash, &group(&ip(3))).is_some());
}

#[tokio::test]
async fn cap_never_drops_below_64_groups() {
    let (g, _c) = guard_with(enforce_config());
    let hash = big_ctx("22.1.0.0", 1_000_000).info_hash;
    for i in 0..80u32 {
        g.on_event_at(
            &big_ctx(&format!("22.1.0.{i}"), 1_000_000),
            ev_upload(0, 16384),
            T0 + i64::from(i),
        );
    }
    assert_eq!(g.ledger_group_count(&hash), 64);
}

#[tokio::test]
async fn evicting_a_dirty_ledger_keeps_it_for_persistence_and_clean_ones_are_deleted() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    let ip = |i: u32| format!("22.2.{}.{}", i / 256, i % 256);
    let ctx = |i: u32| big_ctx(&ip(i), 100_000);
    let hash = ctx(0).info_hash;
    // 先填满 332 组并落库（全部变干净）。
    for i in 0..332u32 {
        g.on_event_at(&ctx(i), ev_upload(0, 16384), T0 + i64::from(i));
    }
    g.flush_all(&db).await.expect("flush");
    assert_eq!(db.bt_guard_load_ledger(0).await.expect("ledger").len(), 332);
    // 淘汰两个干净账本（组 0、1）→ 随 flush 从库删除；再把组 2 弄脏后淘汰它 → 保留落库。
    g.on_event_at(&ctx(332), ev_upload(0, 16384), T0 + 1000);
    g.on_event_at(&ctx(333), ev_upload(0, 16384), T0 + 1001);
    assert_eq!(g.group_delivered_bytes(&hash, &group(&ip(0))), None);
    assert_eq!(g.group_delivered_bytes(&hash, &group(&ip(1))), None);
    g.on_event_at(&ctx(2), ev_upload(1, 32768), T0 + 1002); // 组 2 变脏且变新
    for i in 334..666u32 {
        g.on_event_at(&ctx(i), ev_upload(0, 16384), T0 + 2000 + i64::from(i));
    }
    // 组 2 在这些新增中被淘汰（它的 last_seen 早于 334.. 之后的全部）。
    assert_eq!(g.group_delivered_bytes(&hash, &group(&ip(2))), None);
    g.flush_all(&db).await.expect("flush");
    let rows = db.bt_guard_load_ledger(0).await.expect("ledger");
    assert!(
        rows.iter()
            .all(|r| r.ip_group != ip(0) && r.ip_group != ip(1)),
        "clean evicted rows deleted"
    );
    let kept = rows
        .iter()
        .find(|r| r.ip_group == ip(2))
        .expect("dirty evicted row persisted");
    assert_eq!(
        kept.delivered_bytes,
        16384 + 32768,
        "its unflushed progress was not lost"
    );
}

#[tokio::test]
async fn unflushed_evictions_are_all_persisted_and_survive_reload() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    let ip = |i: u32| format!("22.3.{}.{}", i / 256, i % 256);
    let hash = big_ctx("22.3.0.0", 100_000).info_hash;
    for i in 0..340u32 {
        g.on_event_at(
            &big_ctx(&ip(i), 100_000),
            ev_upload(0, 16384),
            T0 + i64::from(i),
        );
    }
    assert_eq!(g.ledger_group_count(&hash), 332);
    g.flush_all(&db).await.expect("flush");
    assert_eq!(db.bt_guard_load_ledger(0).await.expect("ledger").len(), 340);
    let (g2, _c2) = guard_with(enforce_config());
    assert_eq!(g2.load(&db).await.expect("load").ledgers, 340);
}

// ---------------------------------------------------- 云文档元数据持久化

fn xl_rules_doc(seq: i64, expires: i64) -> fluxdown_engine::bt_guard::CloudDocument {
    let mut doc = cloud_doc(seq, expires, vec![cloud_entry("20.0.0.1/32", "ban")]);
    doc.client_rules = vec![fluxdown_engine::bt_guard::CloudClientRule {
        id: "xl0019".to_owned(),
        field: "peerIdPrefix".to_owned(),
        op: "prefix".to_owned(),
        value: "-XL0019-".to_owned(),
        action: "choke".to_owned(),
        seeding_only: true,
        reason: "xunlei".to_owned(),
    }];
    doc.attribution = vec!["BTN-Collected-Rules by PBH-BTN, CC BY 4.0".to_owned()];
    doc
}

fn handshake_xl() -> GuardEvent<'static> {
    GuardEvent::Handshake {
        peer_id: *b"-XL0019-abcdefghijkl",
        reserved: 0,
        incoming: false,
    }
}

#[tokio::test]
async fn cloud_meta_and_client_rules_survive_restart() {
    let db = mem_db().await;
    assert_eq!(db.bt_guard_load_cloud_meta().await.expect("meta"), None);
    let (g1, _c1) = guard_with(enforce_config());
    g1.apply_cloud(&xl_rules_doc(7, T0 + DAY)).expect("apply");
    g1.flush(&db).await.expect("flush");
    assert!(db.bt_guard_load_cloud_meta().await.expect("meta").is_some());
    assert!(!g1.needs_flush());

    let (g2, _c2) = guard_with(enforce_config());
    // 重启后、加载前没有任何规则。
    let ctx = ctx_for("1.2.3.4", true);
    assert_eq!(g2.on_event_at(&ctx, handshake_xl(), T0), Verdict::Allow);
    g2.clear_connections();
    g2.load(&db).await.expect("load");
    // fingerprint 规则仍命中（不依赖 agent 重放）。
    assert_eq!(g2.on_event_at(&ctx, handshake_xl(), T0), Verdict::Choke);
    let status = g2.cloud_status();
    assert_eq!(status.sequence, 7);
    assert_eq!(status.updated_at, T0);
    assert_eq!(status.expires_at, T0 + DAY);
    assert_eq!((status.entries, status.client_rules), (1, 1));
    assert_eq!(
        status.attribution,
        vec!["BTN-Collected-Rules by PBH-BTN, CC BY 4.0"]
    );
    assert!(!status.stale);
    // sequence 防回滚仍然生效；相同 sequence 幂等接受。
    assert!(g2.apply_cloud(&xl_rules_doc(6, T0 + DAY)).is_err());
    assert!(g2.apply_cloud(&xl_rules_doc(7, T0 + DAY)).is_ok());
    assert!(!g2.admit("20.0.0.1".parse().expect("ip")));
}

#[tokio::test]
async fn expired_cloud_document_is_stale_after_restore() {
    let db = mem_db().await;
    let (g1, _c1) = guard_with(enforce_config());
    g1.apply_cloud(&xl_rules_doc(3, T0 + 1000)).expect("apply");
    g1.flush(&db).await.expect("flush");
    let (g2, c2) = guard_with(enforce_config());
    c2.set(T0 + 5000);
    g2.load(&db).await.expect("load");
    let status = g2.cloud_status();
    assert!(status.stale);
    assert_eq!(status.client_rules, 1, "rules are kept but inactive");
    assert!(
        g2.admit("20.0.0.1".parse().expect("ip")),
        "stale layer is disabled"
    );
    let ctx = ctx_for("1.2.3.4", true);
    assert_eq!(
        g2.on_event_at(&ctx, handshake_xl(), T0 + 5000),
        Verdict::Allow
    );
    // 过期文档同样不能被拿来绕过防回滚。
    assert!(g2.apply_cloud(&xl_rules_doc(2, T0 + 9000)).is_err());
    assert!(g2.apply_cloud(&xl_rules_doc(4, T0 + 9000)).is_ok());
}

#[tokio::test]
async fn newer_cloud_meta_replaces_the_stored_row() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    g.apply_cloud(&xl_rules_doc(1, T0 + DAY)).expect("apply");
    g.flush(&db).await.expect("flush");
    let mut doc = xl_rules_doc(2, T0 + 2 * DAY);
    doc.client_rules.clear();
    g.apply_cloud(&doc).expect("apply");
    g.flush(&db).await.expect("flush");
    let (g2, _c2) = guard_with(enforce_config());
    g2.load(&db).await.expect("load");
    let status = g2.cloud_status();
    assert_eq!((status.sequence, status.client_rules), (2, 0));
    assert_eq!(status.expires_at, T0 + 2 * DAY);
}

#[tokio::test]
async fn legacy_databases_without_meta_still_recover_sequence_from_entries() {
    let db = mem_db().await;
    db.bt_guard_apply_batch(&BtGuardBatch {
        entry_ops: vec![BtGuardEntryOp::Upsert(entry_row(
            1,
            "cloud",
            "20.0.0.1/32",
            "cloud",
            &format!("cloud#9#{}", T0 + DAY),
            Some(T0 + DAY),
        ))],
        ..BtGuardBatch::default()
    })
    .await
    .expect("seed");
    let (g, _c) = guard_with(enforce_config());
    g.load(&db).await.expect("load");
    assert_eq!(g.cloud_sequence(), 9);
    assert_eq!(g.cloud_status().expires_at, T0 + DAY);
    assert!(!g.admit("20.0.0.1".parse().expect("ip")));
}

#[tokio::test]
async fn malformed_stored_meta_is_ignored_not_fatal() {
    let db = mem_db().await;
    db.bt_guard_apply_batch(&BtGuardBatch {
        cloud_meta: Some("{not json".to_owned()),
        ..BtGuardBatch::default()
    })
    .await
    .expect("seed");
    let (g, _c) = guard_with(enforce_config());
    g.load(&db).await.expect("load must tolerate bad meta");
    assert_eq!(g.cloud_sequence(), 0);
}

// ------------------------------------- 账本淘汰后重建：删除不得吞掉新账本

#[tokio::test]
async fn apply_batch_deletes_before_writing_so_a_rebuilt_ledger_survives() {
    let db = mem_db().await;
    let hash = "cd".repeat(20);
    db.bt_guard_apply_batch(&BtGuardBatch {
        ledgers: vec![ledger_row(&hash, "1.2.3.4", 100, T0)],
        ..BtGuardBatch::default()
    })
    .await
    .expect("seed");
    // 同一批里既删除旧行又写同键新行：新行必须留下。
    db.bt_guard_apply_batch(&BtGuardBatch {
        ledgers: vec![ledger_row(&hash, "1.2.3.4", 999, T0 + 5)],
        ledger_deletes: vec![(hash.clone(), "1.2.3.4".to_owned())],
        ..BtGuardBatch::default()
    })
    .await
    .expect("batch");
    let rows = db.bt_guard_load_ledger(0).await.expect("ledger");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].delivered_bytes, 999);
}

#[tokio::test]
async fn a_ledger_evicted_then_rebuilt_is_not_deleted_by_the_same_flush() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    let ip = |i: u32| format!("23.0.{}.{}", i / 256, i % 256);
    let ctx = |i: u32| big_ctx(&ip(i), 100_000);
    let hash = ctx(0).info_hash;
    // 填满 332 组并落库（全部干净）。
    for i in 0..332u32 {
        g.on_event_at(&ctx(i), ev_upload(0, 16384), T0 + i64::from(i));
    }
    g.flush_all(&db).await.expect("flush");
    // 组 0（最旧、干净）被淘汰 → 排队「从库中删除」；随后它又回来重建账本。
    g.on_event_at(&ctx(332), ev_upload(0, 16384), T0 + 1000);
    assert_eq!(g.group_delivered_bytes(&hash, &group(&ip(0))), None);
    g.on_event_at(&ctx(0), ev_upload(0, 32768), T0 + 1001);
    assert_eq!(
        g.group_delivered_bytes(&hash, &group(&ip(0))),
        Some(32768),
        "rebuilt from zero"
    );
    g.flush_all(&db).await.expect("flush");
    let rows = db.bt_guard_load_ledger(0).await.expect("ledger");
    let rebuilt = rows
        .iter()
        .find(|r| r.ip_group == ip(0))
        .expect("the rebuilt ledger must be in the db");
    assert_eq!(rebuilt.delivered_bytes, 32768);
    // 被它顶掉的最旧组（组 1）才是真正被删除的。
    assert!(rows.iter().all(|r| r.ip_group != ip(1)));
}
