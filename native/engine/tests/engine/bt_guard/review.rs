//! 评审修复回归测试：列表过滤 / 去重、cloud 差异防护、账本落库节奏、冷却与基线持久化、
//! 过滤文件层（共享字符串 / 条目上限 / 内容未变跳过）、热路径与 stats。

use std::net::IpAddr;
use std::sync::Arc;

use fluxdown_engine::bt_guard::{
    BtGuard, EntryFilter, FilterAction, GuardError, GuardEvent, IpGroup, Layer,
    MAX_FILTER_ENTRIES_PER_SOURCE, NewEntry, RuleMode, Verdict,
};
use fluxdown_engine::db::{BtGuardEventQuery, Db};

use super::support::{
    DAY, HOUR, MIB, T0, bits, cloud_doc, cloud_entry, ctx_for, deliver_piece, disconnect,
    enforce_config, ev_request, ev_upload, guard_with,
};

const PEER: &str = "1.2.3.4";

async fn mem_db() -> Db {
    Db::connect("sqlite::memory:").await.expect("connect")
}

fn ip(s: &str) -> IpAddr {
    s.parse().expect("ip")
}

fn group(s: &str) -> IpGroup {
    s.parse().expect("group")
}

fn add(g: &BtGuard, cidr: &str, action: FilterAction, secs: Option<i64>, reason: &str) -> i64 {
    g.add_entry(NewEntry {
        cidr: cidr.to_owned(),
        action,
        duration_secs: secs,
        reason: reason.to_owned(),
    })
    .expect("add")
    .id
}

fn list(g: &BtGuard, filter: EntryFilter) -> (usize, Vec<String>) {
    let (total, items) = g.list_entries(&EntryFilter {
        limit: 1000,
        ..filter
    });
    (total, items.iter().map(|e| e.cidr()).collect())
}

// ------------------------------------------------------------ A. 列表过滤与去重

#[test]
fn contains_ip_returns_entries_whose_cidr_covers_the_address_across_layers() {
    let (g, _c) = guard_with(enforce_config());
    add(&g, "7.7.7.0/24", FilterAction::Choke, None, "narrow");
    add(&g, "7.7.7.7", FilterAction::Ban, None, "exact");
    add(&g, "9.9.9.9", FilterAction::Ban, None, "elsewhere");
    add(&g, "7.7.7.7", FilterAction::Allow, None, "allow");
    g.apply_cloud(&cloud_doc(
        1,
        T0 + DAY,
        vec![cloud_entry("7.7.0.0/16", "throttle")],
    ))
    .expect("cloud");
    g.apply_filter_file("https://f.example/a", "7.0.0.0-7.0.255.255\n8.8.8.0/24\n");

    let (total, mut cidrs) = list(
        &g,
        EntryFilter {
            contains_ip: Some(ip("7.7.7.7")),
            ..EntryFilter::default()
        },
    );
    cidrs.sort();
    assert_eq!(total, 4, "narrow + exact + allow + the wide cloud /16");
    assert_eq!(
        cidrs,
        ["7.7.0.0/16", "7.7.7.0/24", "7.7.7.7/32", "7.7.7.7/32"]
    );

    // 文件层宽网段（/16）也会被命中。
    let (total, cidrs) = list(
        &g,
        EntryFilter {
            contains_ip: Some(ip("7.0.9.9")),
            ..EntryFilter::default()
        },
    );
    assert_eq!((total, cidrs), (1, vec!["7.0.0.0/16".to_owned()]));

    // IPv4-mapped IPv6 归一后同样命中。
    let (total, _) = list(
        &g,
        EntryFilter {
            contains_ip: Some(ip("::ffff:7.7.7.7")),
            ..EntryFilter::default()
        },
    );
    assert_eq!(total, 4);

    // 无覆盖条目 → 空。
    let (total, _) = list(
        &g,
        EntryFilter {
            contains_ip: Some(ip("200.1.1.1")),
            ..EntryFilter::default()
        },
    );
    assert_eq!(total, 0);
}

#[test]
fn contains_ip_stacks_with_layer_rule_query_and_pagination() {
    let (g, _c) = guard_with(enforce_config());
    let narrow = add(&g, "7.7.7.0/24", FilterAction::Choke, None, "narrow net");
    let exact = add(&g, "7.7.7.7", FilterAction::Ban, None, "exact host");
    add(&g, "7.7.7.7", FilterAction::Allow, None, "allow");
    g.apply_cloud(&cloud_doc(
        1,
        T0 + DAY,
        vec![cloud_entry("7.7.0.0/16", "throttle")],
    ))
    .expect("cloud");

    let only = |layer: Option<Layer>, query: Option<&str>| {
        list(
            &g,
            EntryFilter {
                layer,
                query: query.map(str::to_owned),
                contains_ip: Some(ip("7.7.7.7")),
                ..EntryFilter::default()
            },
        )
    };
    assert_eq!(only(Some(Layer::Manual), None).0, 2);
    assert_eq!(only(Some(Layer::Cloud), None).0, 1);
    assert_eq!(only(Some(Layer::Auto), None).0, 0);
    assert_eq!(only(Some(Layer::Manual), Some("exact")).0, 1);
    assert_eq!(only(None, Some("no such text")).0, 0);

    // rule 过滤叠加。
    let (total, _) = list(
        &g,
        EntryFilter {
            rule: Some("cloud".to_owned()),
            contains_ip: Some(ip("7.7.7.7")),
            ..EntryFilter::default()
        },
    );
    assert_eq!(total, 1);

    // 先过滤后分页：total 是过滤后的总数，items 按 id 降序切片。
    let (total, items) = g.list_entries(&EntryFilter {
        layer: Some(Layer::Manual),
        contains_ip: Some(ip("7.7.7.7")),
        offset: 1,
        limit: 1,
        ..EntryFilter::default()
    });
    assert_eq!(total, 2);
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].id, narrow,
        "ids {exact} > {narrow}: second page item"
    );
}

#[test]
fn contains_ip_skips_expired_entries() {
    let (g, c) = guard_with(enforce_config());
    add(&g, "7.7.7.7", FilterAction::Ban, Some(60), "short");
    let f = EntryFilter {
        contains_ip: Some(ip("7.7.7.7")),
        ..EntryFilter::default()
    };
    assert_eq!(list(&g, f.clone()).0, 1);
    c.set(T0 + 61);
    assert_eq!(list(&g, f).0, 0);
}

#[test]
fn normalize_group_matches_the_event_table_form() {
    let (g, _c) = guard_with(enforce_config());
    assert_eq!(g.normalize_group("1.2.3.4").expect("v4"), "1.2.3.4");
    assert_eq!(g.normalize_group("  1.2.3.4 ").expect("trim"), "1.2.3.4");
    assert_eq!(
        g.normalize_group("::ffff:1.2.3.4").expect("mapped"),
        "1.2.3.4"
    );
    assert_eq!(
        g.normalize_group("64:ff9b::102:304").expect("nat64"),
        "1.2.3.4"
    );
    // IPv6 按配置前缀（默认 /64）归组。
    assert_eq!(
        g.normalize_group("2001:db8:0:1:aaaa:bbbb:cccc:dddd")
            .expect("v6"),
        "2001:db8:0:1::/64"
    );
    // 已是组文本：原样掩码。
    assert_eq!(
        g.normalize_group("2001:db8:0:1:ffff::/64").expect("group"),
        "2001:db8:0:1::/64"
    );
    for bad in ["", "nope", "1.2.3.4/24", "2001:db8::/200", "1.2.3"] {
        assert!(
            matches!(g.normalize_group(bad), Err(GuardError::InvalidArgument(_))),
            "{bad:?} must be rejected"
        );
    }
    // 与封禁产生的事件使用同一组文本。
    let ctx = ctx_for("2001:db8:0:1:aaaa::1", true);
    g.on_event_at(&ctx, ev_request(1, true), T0);
    g.on_event_at(&ctx, ev_request(2, true), T0);
    let (_, items) = g.list_entries(&EntryFilter {
        layer: Some(Layer::Auto),
        limit: 10,
        ..EntryFilter::default()
    });
    assert_eq!(
        items[0].cidr(),
        g.normalize_group("2001:db8:0:1:aaaa::1").expect("group"),
    );
}

#[test]
fn add_entry_dedupes_same_layer_cidr_and_action_and_refreshes_it() {
    let (g, c) = guard_with(enforce_config());
    let first = g
        .add_entry(NewEntry {
            cidr: "9.9.9.9/24".to_owned(),
            action: FilterAction::Ban,
            duration_secs: Some(60),
            reason: "first".to_owned(),
        })
        .expect("add");
    assert_eq!(first.cidr(), "9.9.9.0/24");
    c.set(T0 + 30);
    // 同层 + 规范化后相同 cidr（主机位不同）+ 同动作：更新而非新增。
    let second = g
        .add_entry(NewEntry {
            cidr: "9.9.9.1/24".to_owned(),
            action: FilterAction::Ban,
            duration_secs: Some(3600),
            reason: "second".to_owned(),
        })
        .expect("add again");
    assert_eq!(second.id, first.id, "same entry, id is stable");
    assert_eq!(second.expires_at, Some(T0 + 30 + 3600));
    assert_eq!(&*second.reason, "second");
    assert_eq!(second.created_at, first.created_at);
    assert_eq!(g.counts().manual, 1);
    // 永久化也是更新。
    let third = g
        .add_entry(NewEntry {
            cidr: "9.9.9.0/24".to_owned(),
            action: FilterAction::Ban,
            duration_secs: None,
            reason: "forever".to_owned(),
        })
        .expect("permanent");
    assert_eq!((third.id, third.expires_at), (first.id, None));
    assert_eq!(g.counts().manual, 1);

    // 不同动作 / 不同 cidr / 不同层 → 各自独立。
    let choke = add(&g, "9.9.9.0/24", FilterAction::Choke, None, "choke");
    let other = add(&g, "9.9.8.0/24", FilterAction::Ban, None, "other");
    let allow = add(&g, "9.9.9.0/24", FilterAction::Allow, None, "allow");
    assert!(choke != first.id && other != first.id && allow != first.id);
    assert_eq!(g.counts().manual, 3);
    assert_eq!(g.counts().allow, 1);
    // 去重也适用于 allow 层。
    assert_eq!(
        add(&g, "9.9.9.5/24", FilterAction::Allow, Some(10), "x"),
        allow
    );
    assert_eq!(g.counts().allow, 1);
}

#[tokio::test]
async fn deduped_add_persists_a_single_updated_row() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    let id = add(&g, "9.9.9.0/24", FilterAction::Ban, Some(60), "first");
    g.flush(&db).await.expect("flush");
    assert_eq!(
        add(&g, "9.9.9.0/24", FilterAction::Ban, Some(7200), "second"),
        id
    );
    g.flush(&db).await.expect("flush");
    let rows = db.bt_guard_load_entries(T0).await.expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].reason, "second");
    assert_eq!(rows[0].expires_at, Some(T0 + 7200));
}

#[tokio::test]
async fn event_list_filters_by_exact_ip_group() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    for target in ["1.2.3.4", "5.6.7.8", "2001:db8:0:1::5"] {
        let ctx = ctx_for(target, true);
        g.on_event_at(&ctx, ev_request(1, true), T0);
        g.on_event_at(&ctx, ev_request(2, true), T0);
    }
    g.flush(&db).await.expect("flush");
    let query = |ip_group: Option<&str>, rule: Option<&str>| BtGuardEventQuery {
        ip_group: ip_group.map(str::to_owned),
        rule: rule.map(str::to_owned),
        limit: 100,
        ..BtGuardEventQuery::default()
    };
    let (all, _) = db
        .bt_guard_list_events(&query(None, None))
        .await
        .expect("all");
    let (one, rows) = db
        .bt_guard_list_events(&query(Some("1.2.3.4"), None))
        .await
        .expect("one");
    assert!(one >= 1 && one < all, "{one} of {all}");
    assert!(rows.iter().all(|r| r.ip_group == "1.2.3.4"));
    // 精确匹配：前缀 / 片段不命中。
    let (none, _) = db
        .bt_guard_list_events(&query(Some("1.2.3"), None))
        .await
        .expect("prefix");
    assert_eq!(none, 0);
    // v6 组文本。
    let v6 = g.normalize_group("2001:db8:0:1::99").expect("group");
    let (n, rows) = db
        .bt_guard_list_events(&query(Some(&v6), None))
        .await
        .expect("v6");
    assert!(n >= 1 && rows.iter().all(|r| r.ip_group == v6));
    // 与 rule 叠加。
    let (n, _) = db
        .bt_guard_list_events(&query(Some("1.2.3.4"), Some("lag")))
        .await
        .expect("stacked");
    assert_eq!(n, 0);
}

// ----------------------------------------------------- 1. cloud 差异防护

fn numbered(range: std::ops::Range<u32>) -> Vec<fluxdown_engine::bt_guard::CloudEntry> {
    range
        .map(|i| {
            cloud_entry(
                &format!("20.{}.{}.{}/32", i / 65536, (i / 256) % 256, i % 256),
                "ban",
            )
        })
        .collect()
}

#[test]
fn diff_guard_is_skipped_when_the_previous_document_has_expired() {
    let (g, c) = guard_with(enforce_config());
    g.apply_cloud(&cloud_doc(1, T0 + 1000, numbered(0..1000)))
        .expect("baseline");
    // 上一份还有效时，整体替换被拒（对照）。
    assert!(
        g.apply_cloud(&cloud_doc(2, T0 + DAY, numbered(5000..6000)))
            .is_err()
    );
    // 长期离线：上一份已过期 → 新文档与旧文档差异再大也该被接受。
    c.set(T0 + 2000);
    let summary = g
        .apply_cloud(&cloud_doc(2, T0 + 2 * DAY, numbered(5000..6000)))
        .expect("previous expired, diff guard off");
    assert_eq!((summary.added, summary.removed), (1000, 1000));
}

#[test]
fn diff_guard_is_skipped_when_the_sequence_jumps_more_than_one() {
    let (g, _c) = guard_with(enforce_config());
    g.apply_cloud(&cloud_doc(10, T0 + DAY, numbered(0..1000)))
        .expect("baseline");
    // 连续版本（+1）：大差异仍拒绝。
    assert!(
        g.apply_cloud(&cloud_doc(11, T0 + DAY, numbered(5000..6000)))
            .is_err()
    );
    // 间隔 > 1：中间版本我们没见过，差异本来就会很大 → 接受。
    let summary = g
        .apply_cloud(&cloud_doc(12, T0 + DAY, numbered(5000..6000)))
        .expect("gap > 1, diff guard off");
    assert_eq!((summary.added, summary.removed), (1000, 1000));
    // 同一 sequence 的重发仍受防护（视为连续）。
    assert!(
        g.apply_cloud(&cloud_doc(12, T0 + DAY, numbered(0..1000)))
            .is_err()
    );
}

// ------------------------------------------------- 3 / 9. 持久化：冷却与基线

#[tokio::test]
async fn lag_cooldown_survives_a_restart() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    for p in 0..4 {
        deliver_piece(&g, &ctx, p, T0);
    }
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 130),
        Verdict::Disconnect
    );
    disconnect(&g, &ctx, None, T0 + 131);
    let b = bits(0..4);
    g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0 + 140);
    disconnect(&g, &ctx, Some(&b), T0 + 141);
    g.flush(&db).await.expect("flush");
    let rows = db.bt_guard_load_ledger(0).await.expect("ledger");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lag_cleared_at, T0 + 140, "clear time is persisted");
    assert_eq!(rows[0].probed_at, T0 + 130);

    // 重启后：探测配额未恢复（若冷却丢失就会退为 Choke），冷却仍在 → 只观察。
    let (g2, _c2) = guard_with(enforce_config());
    g2.load(&db).await.expect("load");
    let ctx = ctx_for(PEER, true);
    for p in 4..8 {
        deliver_piece(&g2, &ctx, p, T0 + 200);
    }
    assert_eq!(
        g2.on_event_at(&ctx, ev_request(51, false), T0 + 400),
        Verdict::Allow
    );
    assert_eq!(g2.stats().observed, 1);
}

#[tokio::test]
async fn excessive_rebans_only_after_the_increment_past_the_ban_baseline() {
    let db = mem_db().await;
    let (g, c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    // 100 MiB 的种子，阈值 200%：送到第 201 MiB 触发封禁。
    let mut verdict = Verdict::Allow;
    let mut sent = 0u64;
    while verdict == Verdict::Allow {
        verdict = deliver_piece(&g, &ctx, (sent % 100) as u32, T0);
        sent += 1;
    }
    assert_eq!(verdict, Verdict::Disconnect);
    assert_eq!(sent, 201);
    disconnect(&g, &ctx, None, T0);
    g.flush(&db).await.expect("flush");
    let rows = db.bt_guard_load_ledger(0).await.expect("ledger");
    assert_eq!(rows[0].excess_baseline, (201 * MIB) as i64);

    // 封禁到期（24h）后，同一个组再来：历史累计 201 MiB 不能让它的第一个块就再次被封。
    c.set(T0 + 25 * HOUR);
    let t = T0 + 25 * HOUR;
    for i in 0..200u32 {
        assert_eq!(
            deliver_piece(&g, &ctx, i % 100, t),
            Verdict::Allow,
            "increment {} MiB is within 200%",
            i + 1
        );
    }
    assert_eq!(
        deliver_piece(&g, &ctx, 0, t),
        Verdict::Disconnect,
        "the increment passes 200% of the torrent size"
    );
    assert_eq!(g.strikes_of(&group(PEER)), 2, "second offence escalates");

    // 基线随账本持久化：重启后行为一致。
    disconnect(&g, &ctx, None, t);
    g.flush_all(&db).await.expect("flush");
    // 第二次封禁 4×24h = 96h；等它到期（strike 衰减期远未到）。
    let later = t + 5 * DAY;
    let (g2, c2) = guard_with(enforce_config());
    c2.set(later);
    g2.load(&db).await.expect("load");
    let ctx2 = ctx_for(PEER, true);
    assert_eq!(
        deliver_piece(&g2, &ctx2, 0, later),
        Verdict::Allow,
        "reloaded baseline keeps the first block of a returning peer clean"
    );
}

// ------------------------------------------------------- 6. 账本落库节奏

fn ledger_groups(rows: &[fluxdown_engine::db::BtGuardLedgerRow]) -> Vec<String> {
    let mut v: Vec<String> = rows.iter().map(|r| r.ip_group.clone()).collect();
    v.sort();
    v
}

#[tokio::test]
async fn disconnected_flush_writes_only_that_groups_ledger() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    let gone = ctx_for("1.2.3.4", true);
    let live = ctx_for("5.6.7.8", true);
    deliver_piece(&g, &gone, 0, T0);
    deliver_piece(&g, &live, 0, T0);
    assert!(!g.needs_flush(), "uploads alone do not schedule a flush");
    disconnect(&g, &gone, None, T0);
    assert!(g.needs_flush());
    g.flush(&db).await.expect("flush");
    let rows = db.bt_guard_load_ledger(0).await.expect("ledger");
    assert_eq!(
        ledger_groups(&rows),
        ["1.2.3.4"],
        "live group is not rewritten"
    );
    assert!(!g.needs_flush());
    assert!(g.needs_flush_all(), "the live ledger is still dirty");
    // 会话 / 引擎关闭走 flush_all：其余脏账本一并写入。
    g.flush_all(&db).await.expect("flush_all");
    let rows = db.bt_guard_load_ledger(0).await.expect("ledger");
    assert_eq!(ledger_groups(&rows), ["1.2.3.4", "5.6.7.8"]);
    assert!(!g.needs_flush_all());
}

#[tokio::test]
async fn decisions_write_every_dirty_ledger_but_uploads_alone_do_not() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    let busy = ctx_for("5.6.7.8", true);
    deliver_piece(&g, &busy, 0, T0);
    g.flush(&db).await.expect("idle flush");
    assert!(
        db.bt_guard_load_ledger(0).await.expect("ledger").is_empty(),
        "no decision yet: nothing written"
    );
    // 另一个组触发处置（封禁）：判定点连带写全部脏账本。
    let bad = ctx_for("1.2.3.4", true);
    g.on_event_at(&bad, ev_request(1, true), T0);
    g.on_event_at(&bad, ev_request(2, true), T0);
    g.flush(&db).await.expect("flush");
    let rows = db.bt_guard_load_ledger(0).await.expect("ledger");
    assert_eq!(ledger_groups(&rows), ["1.2.3.4", "5.6.7.8"]);
    assert!(!g.needs_flush_all());
}

#[tokio::test]
async fn removing_a_ban_is_a_flush_point_for_dirty_ledgers() {
    let db = mem_db().await;
    let (g, _c) = guard_with(enforce_config());
    let id = add(&g, "9.9.9.9", FilterAction::Ban, None, "x");
    g.flush(&db).await.expect("flush");
    deliver_piece(&g, &ctx_for("5.6.7.8", true), 0, T0);
    g.remove_entries(&[id]).expect("remove");
    g.flush(&db).await.expect("flush");
    assert_eq!(db.bt_guard_load_ledger(0).await.expect("ledger").len(), 1);
}

// ------------------------------------------------------ 7. 过滤文件层

#[test]
fn entries_of_one_source_share_their_strings() {
    let (g, _c) = guard_with(enforce_config());
    g.apply_filter_file("https://f.example/a", "7.7.7.0/24\n8.8.8.0/24\n9.9.9.9\n");
    let (_, items) = g.list_entries(&EntryFilter {
        layer: Some(Layer::File),
        limit: 10,
        ..EntryFilter::default()
    });
    assert_eq!(items.len(), 3);
    for pair in items.windows(2) {
        assert!(Arc::ptr_eq(&pair[0].source, &pair[1].source), "source");
        assert!(Arc::ptr_eq(&pair[0].rule, &pair[1].rule), "rule");
        assert!(Arc::ptr_eq(&pair[0].reason, &pair[1].reason), "reason");
    }
}

#[test]
fn a_source_is_capped_and_the_overflow_is_reported_on_that_source() {
    let url = "https://f.example/big";
    let mut cfg = enforce_config();
    cfg.filter_urls = url.to_owned();
    let (g, _c) = guard_with(cfg);
    let total = MAX_FILTER_ENTRIES_PER_SOURCE + 3;
    let mut text = String::with_capacity(total * 14);
    for i in 0..total {
        text.push_str(&format!(
            "30.{}.{}.{}\n",
            i / 65536,
            (i / 256) % 256,
            i % 256
        ));
    }
    let summary = g.apply_filter_file(url, &text);
    assert_eq!(summary.entries, MAX_FILTER_ENTRIES_PER_SOURCE);
    assert_eq!(summary.dropped_over_cap, 3);
    assert_eq!(g.counts().file, MAX_FILTER_ENTRIES_PER_SOURCE);
    let source = g.filter_sources().remove(0);
    assert!(source.ok, "the refresh itself succeeded");
    assert_eq!(source.entries, MAX_FILTER_ENTRIES_PER_SOURCE);
    let error = source.error.expect("overflow recorded on the source");
    assert!(error.contains("200000") && error.contains('3'), "{error}");
    // 另一个来源不受影响。
    let other = g.apply_filter_file("https://f.example/small", "7.7.7.7\n");
    assert_eq!((other.entries, other.dropped_over_cap), (1, 0));
}

#[tokio::test]
async fn unchanged_body_skips_the_layer_replacement_and_the_db_write() {
    let db = mem_db().await;
    let url = "https://f.example/a";
    let mut cfg = enforce_config();
    cfg.filter_urls = url.to_owned();
    let (g, _c) = guard_with(cfg);
    let body = "7.7.7.0/24\n8.8.8.0/24\n";
    let first = g.apply_filter_file(url, body);
    assert!(!first.unchanged);
    g.flush(&db).await.expect("flush");
    let before = db.bt_guard_load_entries(T0).await.expect("rows");
    assert_eq!(before.len(), 2);
    let _ = g.take_actions();

    let again = g.apply_filter_file(url, body);
    assert!(again.unchanged);
    assert_eq!(again.entries, first.entries);
    assert!(!g.needs_flush(), "no layer replacement is queued");
    assert!(g.take_actions().is_empty(), "no Changed / DisconnectBanned");
    let sources = g.filter_sources();
    assert!(sources[0].ok && sources[0].error.is_none());
    // 条目 id 保持不变（整层没有被重建）。
    g.flush(&db).await.expect("flush");
    let after = db.bt_guard_load_entries(T0).await.expect("rows");
    assert_eq!(
        before.iter().map(|r| r.id).collect::<Vec<_>>(),
        after.iter().map(|r| r.id).collect::<Vec<_>>()
    );

    // 正文变化 → 照常替换。
    let changed = g.apply_filter_file(url, "7.7.7.0/24\n");
    assert!(!changed.unchanged);
    assert_eq!(g.counts().file, 1);
    assert!(g.needs_flush());
}

#[test]
fn unchanged_body_is_reapplied_when_the_layer_lost_entries() {
    let url = "https://f.example/a";
    let (g, _c) = guard_with(enforce_config());
    let body = "7.7.7.0/24\n8.8.8.0/24\n";
    g.apply_filter_file(url, body);
    let (_, items) = g.list_entries(&EntryFilter {
        layer: Some(Layer::File),
        limit: 10,
        ..EntryFilter::default()
    });
    g.remove_entries(&[items[0].id]).expect("remove one");
    assert_eq!(g.counts().file, 1);
    let again = g.apply_filter_file(url, body);
    assert!(!again.unchanged, "the layer no longer matches the body");
    assert_eq!(g.counts().file, 2);
}

// ----------------------------------------------------------------- 8. 热路径

#[test]
fn disabled_guard_does_not_track_connections_but_still_cleans_up_on_disconnect() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    g.on_event_at(&ctx, ev_request(30, false), T0);
    assert!(g.peer_insight(&ctx.info_hash, ctx.addr).is_some());

    let mut off = enforce_config();
    off.enabled = false;
    g.set_config(off);
    // 关闭后新对端的事件不再建立连接态……
    let other = ctx_for("5.6.7.8", true);
    g.on_event_at(&other, ev_request(30, false), T0);
    assert!(g.peer_insight(&other.info_hash, other.addr).is_none());
    // ……但 Disconnected 仍要清理关闭前建立的连接态，不能泄漏。
    disconnect(&g, &ctx, None, T0);
    assert!(g.peer_insight(&ctx.info_hash, ctx.addr).is_none());
    assert!(g.admit(ip("1.2.3.4")));
    assert!(!g.is_banned(ip("1.2.3.4")));
}

#[test]
fn lan_peers_are_never_tracked() {
    let (g, _c) = guard_with(enforce_config());
    for lan in ["192.168.1.9", "10.1.2.3", "127.0.0.1", "169.254.1.1"] {
        let ctx = ctx_for(lan, true);
        g.on_event_at(&ctx, ev_request(1, true), T0);
        g.on_event_at(&ctx, ev_request(2, true), T0);
        g.on_event_at(&ctx, ev_upload(0, 1 << 20), T0);
        assert!(g.peer_insight(&ctx.info_hash, ctx.addr).is_none(), "{lan}");
    }
    assert_eq!(g.counts().auto, 0);
}

// ------------------------------------------------------------------ 10. stats

#[test]
fn stats_cache_the_day_interval_and_recompute_across_days() {
    let (g, c) = guard_with(enforce_config());
    g.apply_cloud(&cloud_doc(
        1,
        T0 + 10 * DAY,
        vec![cloud_entry("5.5.5.5/32", "ban")],
    ))
    .expect("cloud");
    let banned = ip("5.5.5.5");
    // 同一本地日内多次计数持续累加（区间缓存命中）。
    for step in 0..3 {
        c.set(T0 + step * 20);
        assert!(!g.admit(banned));
    }
    assert_eq!(g.stats().blocked_connections, 3);
    // 隔天：统计归零并按新的一天重算区间。
    c.set(T0 + DAY + 3 * HOUR);
    assert_eq!(g.stats().blocked_connections, 0);
    assert!(!g.admit(banned));
    assert!(!g.admit(banned));
    assert_eq!(g.stats().blocked_connections, 2);
    // 再隔一天，不经过 admit 直接读取也是新的一天。
    c.set(T0 + 3 * DAY);
    assert_eq!(g.stats().blocked_connections, 0);
    assert!(!g.admit(banned));
    assert_eq!(g.stats().blocked_connections, 1);
}

#[test]
fn an_unevaluated_probe_is_not_credited_as_cleared_when_rewind_is_off() {
    let mut cfg = enforce_config();
    cfg.rule_rewind = RuleMode::Off;
    let (g, _c) = guard_with(cfg);
    let ctx = ctx_for(PEER, true);
    for p in 0..4 {
        deliver_piece(&g, &ctx, p, T0);
    }
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 130),
        Verdict::Disconnect
    );
    disconnect(&g, &ctx, None, T0 + 131);
    // 重连后位图仍缺 3/4：rewind 关闭所以不会封禁，但也不能算作「补齐」。
    let b = bits(0..1);
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0 + 140),
        Verdict::Allow
    );
    for p in 4..8 {
        deliver_piece(&g, &ctx, p, T0 + 141);
    }
    assert_eq!(
        g.on_event_at(&ctx, ev_request(51, false), T0 + 400),
        Verdict::Choke,
        "no cooldown was granted, probe quota is spent -> choke"
    );
}

// ------------------------------------------- rewind NAT 保护额度（每账本 24h 3 次）

/// 向 PEER 送满 100 片并以全 1 位图断开，之后另有一条锚定连接（端口 7000）一直存活。
fn anchored_guard() -> (BtGuard, super::support::Clock) {
    let (g, c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    for p in 0..100 {
        deliver_piece(&g, &ctx, p, T0);
    }
    disconnect(&g, &ctx, Some(&bits(0..100)), T0);
    let mut anchor = ctx_for(PEER, true);
    anchor.addr.set_port(7000);
    g.on_event_at(&anchor, ev_request(30, false), T0 + 10);
    (g, c)
}

/// 6881 端口上的一次「重连」：交出空位图，随后（若未被判定）断开。
fn empty_reconnect(g: &BtGuard, now: i64) -> Verdict {
    let ctx = ctx_for(PEER, true);
    let empty = bits(std::iter::empty());
    let verdict = g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &empty }, now);
    disconnect(g, &ctx, None, now + 1);
    verdict
}

#[test]
fn an_anchor_connection_can_suppress_rewind_only_three_times_per_day() {
    let (g, _c) = anchored_guard();
    for round in 0..3 {
        assert_eq!(
            empty_reconnect(&g, T0 + 20 + round * 10),
            Verdict::Allow,
            "skip {round}"
        );
    }
    assert_eq!(auto_entry_count(&g), 0);
    // 额度用完：同样的回退照常判定。
    assert_eq!(empty_reconnect(&g, T0 + 60), Verdict::Disconnect);
    assert_eq!(auto_entry_count(&g), 1);
}

#[test]
fn the_skip_quota_returns_after_24_hours() {
    let (g, _c) = anchored_guard();
    for round in 0..3 {
        assert_eq!(empty_reconnect(&g, T0 + 20 + round * 10), Verdict::Allow);
    }
    // 窗口从第一次跳过（T0+20）起算，24h 内仍受限……
    let almost = T0 + 20 + DAY - 1;
    let ctx = ctx_for(PEER, true);
    let empty = bits(std::iter::empty());
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &empty }, almost),
        Verdict::Disconnect
    );
    disconnect(&g, &ctx, None, almost + 1);
    // ……满 24h 后额度恢复（用新 guard 重做以避开上面产生的封禁）。
    let (g, _c) = anchored_guard();
    for round in 0..3 {
        assert_eq!(empty_reconnect(&g, T0 + 20 + round * 10), Verdict::Allow);
    }
    assert_eq!(
        empty_reconnect(&g, T0 + 20 + DAY),
        Verdict::Allow,
        "quota recovered"
    );
    assert_eq!(auto_entry_count(&g), 0);
}

#[test]
fn both_skip_conditions_draw_on_the_same_quota() {
    let id_b: [u8; 20] = *b"-TR3000-bbbbbbbbbbbb";
    let (g, _c) = anchored_guard();
    // 两次靠锚定连接跳过。
    assert_eq!(empty_reconnect(&g, T0 + 20), Verdict::Allow);
    assert_eq!(empty_reconnect(&g, T0 + 30), Verdict::Allow);
    // 锚定连接离开；换了指纹的客户端交出空位图 → 第三次跳过。
    let mut anchor = ctx_for(PEER, true);
    anchor.addr.set_port(7000);
    disconnect(&g, &anchor, None, T0 + 35);
    let ctx = ctx_for(PEER, true);
    g.on_event_at(
        &ctx,
        GuardEvent::Handshake {
            peer_id: id_b,
            reserved: 0,
            incoming: false,
        },
        T0 + 40,
    );
    let empty = bits(std::iter::empty());
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &empty }, T0 + 41),
        Verdict::Allow
    );
    disconnect(&g, &ctx, None, T0 + 42);
    // 锚定连接回来：额度已被两种条件共同用完 → 判定。
    g.on_event_at(&anchor, ev_request(31, false), T0 + 50);
    assert_eq!(empty_reconnect(&g, T0 + 60), Verdict::Disconnect);
}

#[test]
fn skips_that_would_not_have_judged_do_not_consume_the_quota() {
    let (g, _c) = anchored_guard();
    // 位图完整（没有回退）的重连：跳过条件成立但本来就不会判定，不耗额度。
    for round in 0..10 {
        let ctx = ctx_for(PEER, true);
        let full = bits(0..100);
        assert_eq!(
            g.on_event_at(
                &ctx,
                GuardEvent::Bitfield { bits: &full },
                T0 + 20 + round * 10
            ),
            Verdict::Allow
        );
        disconnect(&g, &ctx, Some(&full), T0 + 25 + round * 10);
    }
    for round in 0..3 {
        assert_eq!(
            empty_reconnect(&g, T0 + 200 + round * 10),
            Verdict::Allow,
            "full quota still available"
        );
    }
}

#[tokio::test]
async fn skipped_rewinds_leave_an_observed_diagnostic_event() {
    let db = mem_db().await;
    let (g, _c) = anchored_guard();
    for round in 0..3 {
        empty_reconnect(&g, T0 + 20 + round * 10);
    }
    g.flush_all(&db).await.expect("flush");
    let (total, rows) = db
        .bt_guard_list_events(&BtGuardEventQuery {
            rule: Some("rewind".to_owned()),
            kind: Some("observed".to_owned()),
            limit: 20,
            ..BtGuardEventQuery::default()
        })
        .await
        .expect("events");
    assert_eq!(total, 3);
    assert!(
        rows.iter()
            .all(|r| r.action == "skip" && r.mode == "observe")
    );
    let detail: serde_json::Value = serde_json::from_str(&rows[0].detail).expect("json");
    assert_eq!(detail["skipped"], "another live connection of the group");
    assert_eq!(detail["skipsLeft"], 0);
    assert_eq!(
        g.stats().observed,
        0,
        "diagnostics are not counted as rule observations"
    );
}

fn auto_entry_count(g: &BtGuard) -> usize {
    g.list_entries(&EntryFilter {
        layer: Some(Layer::Auto),
        limit: 100,
        ..EntryFilter::default()
    })
    .0
}
