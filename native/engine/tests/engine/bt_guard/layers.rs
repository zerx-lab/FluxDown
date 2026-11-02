use std::net::IpAddr;
use std::sync::Arc;

use fluxdown_engine::bt_guard::{
    BtGuard, BtGuardConfig, DecideInput, Decision, DownloadingMaxAction, Entry, EntryFilter,
    FilterAction, GuardAction, GuardError, IpRange, Layer, NewEntry, RuleMode, Tables, Verdict,
    decide,
};

use super::support::{
    DAY, T0, cloud_doc, cloud_entry, ctx_for, enforce_config, ev_request, ev_upload, guard_with,
};

fn ip(s: &str) -> IpAddr {
    s.parse().expect("ip")
}

fn add(g: &BtGuard, cidr: &str, action: FilterAction, secs: Option<i64>) -> Entry {
    g.add_entry(NewEntry {
        cidr: cidr.to_owned(),
        action,
        duration_secs: secs,
        reason: format!("reason for {cidr}"),
    })
    .expect("add")
}

fn apply_cloud(g: &BtGuard, entries: Vec<(&str, &str)>) {
    let doc = cloud_doc(
        1,
        T0 + DAY,
        entries
            .into_iter()
            .map(|(c, a)| cloud_entry(c, a))
            .collect(),
    );
    g.apply_cloud(&doc).expect("apply cloud");
}

fn entry(id: i64, layer: Layer, cidr: &str, action: FilterAction, expires: Option<i64>) -> Entry {
    Entry {
        id,
        layer,
        range: IpRange::parse_cidr(cidr).expect("cidr"),
        action,
        rule: Arc::from("t"),
        reason: Arc::from(""),
        evidence: None,
        source: Arc::from(""),
        created_at: T0,
        expires_at: expires,
    }
}

fn input(mode: RuleMode) -> DecideInput {
    DecideInput {
        now: T0,
        cloud_mode: mode,
        cloud_active: true,
        cloud_valid_until: i64::MAX,
    }
}

// --------------------------------------------------------------- decide()

#[test]
fn decide_priority_allow_over_manual_over_the_rest() {
    let tables = Tables::default()
        .with_layer(
            Layer::Allow,
            vec![entry(
                1,
                Layer::Allow,
                "1.2.3.4/32",
                FilterAction::Allow,
                None,
            )],
        )
        .with_layer(
            Layer::Manual,
            vec![entry(
                2,
                Layer::Manual,
                "1.2.3.0/24",
                FilterAction::Throttle,
                None,
            )],
        )
        .with_layer(
            Layer::Auto,
            vec![entry(3, Layer::Auto, "1.2.0.0/16", FilterAction::Ban, None)],
        )
        .with_layer(
            Layer::Cloud,
            vec![entry(
                4,
                Layer::Cloud,
                "1.0.0.0/16",
                FilterAction::Ban,
                None,
            )],
        )
        .with_layer(
            Layer::File,
            vec![entry(
                5,
                Layer::File,
                "1.2.0.0/16",
                FilterAction::Choke,
                None,
            )],
        );
    let d = |addr: &str| decide(&tables, ip(addr), input(RuleMode::Enforce)).decision;
    assert_eq!(d("1.2.3.4"), Decision::Exempt);
    // manual（throttle）压过 auto / file 的 ban。
    assert_eq!(
        d("1.2.3.5"),
        Decision::Act {
            action: FilterAction::Throttle,
            layer: Layer::Manual,
            entry_id: 2
        }
    );
    // 没有 manual：auto ban 为三者中最严。
    assert_eq!(
        d("1.2.9.9"),
        Decision::Act {
            action: FilterAction::Ban,
            layer: Layer::Auto,
            entry_id: 3
        }
    );
    assert_eq!(d("8.8.4.4"), Decision::None);
}

#[test]
fn decide_picks_strongest_among_auto_cloud_file() {
    let mk = |auto, cloud, file| {
        let mut t = Tables::default();
        if let Some(a) = auto {
            t = t.with_layer(
                Layer::Auto,
                vec![entry(1, Layer::Auto, "1.2.3.4/32", a, None)],
            );
        }
        if let Some(a) = cloud {
            t = t.with_layer(
                Layer::Cloud,
                vec![entry(2, Layer::Cloud, "1.2.3.4/32", a, None)],
            );
        }
        if let Some(a) = file {
            t = t.with_layer(
                Layer::File,
                vec![entry(3, Layer::File, "1.2.3.4/32", a, None)],
            );
        }
        decide(&t, ip("1.2.3.4"), input(RuleMode::Enforce)).decision
    };
    let act = |action, layer, entry_id| Decision::Act {
        action,
        layer,
        entry_id,
    };
    assert_eq!(
        mk(
            Some(FilterAction::Throttle),
            Some(FilterAction::Choke),
            Some(FilterAction::Throttle)
        ),
        act(FilterAction::Choke, Layer::Cloud, 2)
    );
    assert_eq!(
        mk(
            Some(FilterAction::Throttle),
            Some(FilterAction::Choke),
            Some(FilterAction::Ban)
        ),
        act(FilterAction::Ban, Layer::File, 3)
    );
    assert_eq!(
        mk(None, None, Some(FilterAction::Choke)),
        act(FilterAction::Choke, Layer::File, 3)
    );
}

#[test]
fn decide_cloud_mode_observe_and_off() {
    let tables = Tables::default().with_layer(
        Layer::Cloud,
        vec![entry(
            4,
            Layer::Cloud,
            "1.2.3.4/32",
            FilterAction::Ban,
            None,
        )],
    );
    assert_eq!(
        decide(&tables, ip("1.2.3.4"), input(RuleMode::Observe)).decision,
        Decision::ObserveCloud {
            action: FilterAction::Ban,
            entry_id: 4
        }
    );
    assert_eq!(
        decide(&tables, ip("1.2.3.4"), input(RuleMode::Off)).decision,
        Decision::None
    );
    // 其它层的执行性命中优先于云名单的 observe。
    let tables = tables.with_layer(
        Layer::File,
        vec![entry(
            5,
            Layer::File,
            "1.2.3.4/32",
            FilterAction::Throttle,
            None,
        )],
    );
    assert!(matches!(
        decide(&tables, ip("1.2.3.4"), input(RuleMode::Observe)).decision,
        Decision::Act {
            layer: Layer::File,
            ..
        }
    ));
    // 云文档失效（stale）时整层不参与。
    let stale = DecideInput {
        cloud_active: false,
        ..input(RuleMode::Enforce)
    };
    let only_cloud = Tables::default().with_layer(
        Layer::Cloud,
        vec![entry(
            4,
            Layer::Cloud,
            "1.2.3.4/32",
            FilterAction::Ban,
            None,
        )],
    );
    assert_eq!(
        decide(&only_cloud, ip("1.2.3.4"), stale).decision,
        Decision::None
    );
}

#[test]
fn decide_skips_expired_entries_and_reports_validity_horizon() {
    let tables = Tables::default().with_layer(
        Layer::Manual,
        vec![
            entry(1, Layer::Manual, "1.2.3.4/32", FilterAction::Ban, Some(T0)),
            entry(
                2,
                Layer::Manual,
                "1.2.3.4/32",
                FilterAction::Choke,
                Some(T0 + 50),
            ),
            entry(
                3,
                Layer::Manual,
                "1.2.3.0/24",
                FilterAction::Throttle,
                Some(T0 + 80),
            ),
        ],
    );
    let out = decide(&tables, ip("1.2.3.4"), input(RuleMode::Enforce));
    // id 1 恰在 T0 到期（不含），已失效；id 2 choke 生效。
    assert_eq!(
        out.decision,
        Decision::Act {
            action: FilterAction::Choke,
            layer: Layer::Manual,
            entry_id: 2
        }
    );
    assert_eq!(
        out.valid_until,
        T0 + 50,
        "cache must be recomputed once the nearest match expires"
    );
    let later = DecideInput {
        now: T0 + 50,
        ..input(RuleMode::Enforce)
    };
    assert_eq!(
        decide(&tables, ip("1.2.3.4"), later).decision,
        Decision::Act {
            action: FilterAction::Throttle,
            layer: Layer::Manual,
            entry_id: 3
        }
    );
    assert_eq!(
        decide(&Tables::default(), ip("1.2.3.4"), input(RuleMode::Enforce)).valid_until,
        i64::MAX
    );
}

#[test]
fn decide_allow_validity_is_the_latest_expiry() {
    let tables = Tables::default().with_layer(
        Layer::Allow,
        vec![
            entry(
                1,
                Layer::Allow,
                "1.2.3.4/32",
                FilterAction::Allow,
                Some(T0 + 10),
            ),
            entry(
                2,
                Layer::Allow,
                "1.2.3.0/24",
                FilterAction::Allow,
                Some(T0 + 99),
            ),
        ],
    );
    let out = decide(&tables, ip("1.2.3.4"), input(RuleMode::Enforce));
    assert_eq!(out.decision, Decision::Exempt);
    assert_eq!(out.valid_until, T0 + 99);
}

// ------------------------------------------------------------ via BtGuard

#[test]
fn allow_overrides_manual_and_cloud_bans() {
    let (g, _c) = guard_with(enforce_config());
    add(&g, "1.2.3.0/24", FilterAction::Ban, None);
    add(&g, "1.2.3.4", FilterAction::Allow, None);
    assert!(g.admit(ip("1.2.3.4")));
    assert!(!g.admit(ip("1.2.3.5")));
    apply_cloud(&g, vec![("5.5.5.0/24", "ban")]);
    assert!(!g.admit(ip("5.5.5.6")));
    add(&g, "5.5.5.5", FilterAction::Allow, None);
    assert!(g.admit(ip("5.5.5.5")));
    assert!(!g.admit(ip("5.5.5.6")));
}

#[test]
fn manual_overrides_cloud_for_the_same_address() {
    let (g, _c) = guard_with(enforce_config());
    apply_cloud(&g, vec![("5.5.5.0/24", "ban")]);
    add(&g, "5.5.5.5", FilterAction::Throttle, None);
    assert!(g.admit(ip("5.5.5.5")), "manual throttle beats cloud ban");
    let ctx = ctx_for("5.5.5.5", true);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(1, false), T0),
        Verdict::Throttle
    );
    assert!(!g.admit(ip("5.5.5.6")));
}

#[test]
fn cloud_modes_via_guard() {
    // observe：不拒绝、不改判决，只记一次观察。
    let (g, _c) = guard_with(BtGuardConfig::default());
    apply_cloud(&g, vec![("5.5.5.0/24", "ban")]);
    assert!(g.admit(ip("5.5.5.5")));
    assert!(!g.is_banned(ip("5.5.5.5")));
    let ctx = ctx_for("5.5.5.5", true);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(1, false), T0),
        Verdict::Allow
    );
    assert_eq!(
        g.on_event_at(&ctx, ev_request(2, false), T0),
        Verdict::Allow
    );
    assert_eq!(g.stats().observed, 1);
    assert_eq!(g.stats().enforced, 0);
    let insight = g.peer_insight(&ctx.info_hash, ctx.addr).expect("insight");
    assert!(insight.observe_only);
    assert_eq!(insight.rules, vec!["cloud"]);
    // off：完全忽略。
    let cfg = BtGuardConfig {
        cloud_mode: RuleMode::Off,
        ..BtGuardConfig::default()
    };
    let (g, _c) = guard_with(cfg);
    apply_cloud(&g, vec![("5.5.5.0/24", "ban")]);
    assert!(g.admit(ip("5.5.5.5")));
    assert_eq!(
        g.on_event_at(&ctx, ev_request(1, false), T0),
        Verdict::Allow
    );
    assert_eq!(g.stats().observed, 0);
    // enforce：拒绝连接并计数。
    let (g, _c) = guard_with(enforce_config());
    apply_cloud(&g, vec![("5.5.5.0/24", "ban")]);
    assert!(!g.admit(ip("5.5.5.5")));
    assert!(!g.admit(ip("5.5.5.6")));
    assert_eq!(g.stats().blocked_connections, 2);
}

#[test]
fn cloud_throttle_and_choke_entries_follow_their_action_and_downloading_cap() {
    let (g, _c) = guard_with(enforce_config());
    apply_cloud(
        &g,
        vec![("5.5.5.1/32", "choke"), ("5.5.5.2/32", "throttle")],
    );
    // 做种任务：按条目动作。
    assert_eq!(
        g.on_event_at(&ctx_for("5.5.5.1", true), ev_request(1, false), T0),
        Verdict::Choke
    );
    assert_eq!(
        g.on_event_at(&ctx_for("5.5.5.2", true), ev_request(1, false), T0),
        Verdict::Throttle
    );
    // 下载中任务：封顶为 throttle（默认）。
    let mut downloading = ctx_for("5.5.5.1", false);
    downloading.addr.set_port(7001);
    assert_eq!(
        g.on_event_at(&downloading, ev_request(1, false), T0),
        Verdict::Throttle
    );
    // 手动条目不受下载上限约束。
    add(&g, "6.6.6.6", FilterAction::Choke, None);
    assert_eq!(
        g.on_event_at(&ctx_for("6.6.6.6", false), ev_request(1, false), T0),
        Verdict::Choke
    );
    // 上限 observe：云名单 choke 对下载任务不执行。
    let mut cfg = enforce_config();
    cfg.downloading_max_action = DownloadingMaxAction::Observe;
    let (g, _c) = guard_with(cfg);
    apply_cloud(&g, vec![("5.5.5.1/32", "choke")]);
    assert_eq!(
        g.on_event_at(&ctx_for("5.5.5.1", false), ev_request(1, false), T0),
        Verdict::Allow
    );
    let mut seeding_ctx = ctx_for("5.5.5.1", true);
    seeding_ctx.addr.set_port(7777);
    assert_eq!(
        g.on_event_at(&seeding_ctx, ev_request(2, false), T0),
        Verdict::Choke
    );
}

#[test]
fn cloud_bans_are_global_even_for_downloading_tasks() {
    let mut cfg = enforce_config();
    cfg.downloading_max_action = DownloadingMaxAction::Observe;
    let (g, _c) = guard_with(cfg);
    apply_cloud(&g, vec![("5.5.5.5/32", "ban")]);
    assert!(
        !g.admit(ip("5.5.5.5")),
        "ban refuses at admit regardless of task state"
    );
}

#[test]
fn cloud_layer_goes_stale_after_document_expiry() {
    let (g, c) = guard_with(enforce_config());
    let doc = cloud_doc(1, T0 + 1000, vec![cloud_entry("5.5.5.5/32", "ban")]);
    g.apply_cloud(&doc).expect("apply");
    c.set(T0 + 999);
    assert!(!g.admit(ip("5.5.5.5")));
    assert!(!g.cloud_status().stale);
    c.set(T0 + 1000);
    assert!(g.admit(ip("5.5.5.5")), "stale cloud layer is disabled");
    assert!(g.cloud_status().stale);
    // 新文档到来后恢复。
    let fresh = cloud_doc(2, T0 + 5000, vec![cloud_entry("5.5.5.5/32", "ban")]);
    g.apply_cloud(&fresh).expect("apply fresh");
    assert!(!g.admit(ip("5.5.5.5")));
    assert!(!g.cloud_status().stale);
}

#[test]
fn per_entry_cloud_expiry_is_honoured() {
    let (g, c) = guard_with(enforce_config());
    let mut short = cloud_entry("5.5.5.5/32", "ban");
    short.expires_at = Some(T0 + 100);
    g.apply_cloud(&cloud_doc(
        1,
        T0 + DAY,
        vec![short, cloud_entry("5.5.5.6/32", "ban")],
    ))
    .expect("apply");
    c.set(T0 + 100);
    assert!(g.admit(ip("5.5.5.5")));
    assert!(!g.admit(ip("5.5.5.6")));
}

#[test]
fn connection_decision_cache_is_invalidated_by_entry_changes_and_expiry() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for("1.2.3.4", true);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(1, false), T0),
        Verdict::Allow
    );
    // 连接期间新增封禁 → 下一个事件即 Disconnect。
    add(&g, "1.2.3.4", FilterAction::Ban, None);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(2, false), T0 + 1),
        Verdict::Disconnect
    );

    // 白名单到期后检测恢复：缓存按最近到期时刻失效。
    let (g, c2) = guard_with(enforce_config());
    add(&g, "9.9.9.9", FilterAction::Allow, Some(100));
    let ctx = ctx_for("9.9.9.9", true);
    assert_eq!(
        g.on_event_at(&ctx, ev_upload(0, 250 * 1024 * 1024), T0 + 50),
        Verdict::Allow
    );
    c2.set(T0 + 100);
    assert_eq!(
        g.on_event_at(&ctx, ev_upload(1, 250 * 1024 * 1024), T0 + 100),
        Verdict::Disconnect
    );
    assert!(g.admit(ip("1.1.1.9")));
}

#[test]
fn allow_added_mid_connection_exempts_immediately() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for("9.9.9.9", true);
    g.on_event_at(&ctx, ev_request(1, true), T0);
    add(&g, "9.9.9.9", FilterAction::Allow, None);
    assert_eq!(g.on_event_at(&ctx, ev_request(2, true), T0), Verdict::Allow);
}

#[test]
fn throttle_and_choke_entries_count_refused_request_bytes() {
    let (g, _c) = guard_with(enforce_config());
    add(&g, "7.7.7.7", FilterAction::Choke, None);
    add(&g, "8.8.4.4", FilterAction::Throttle, None);
    for addr in ["7.7.7.7", "8.8.4.4"] {
        let ctx = ctx_for(addr, true);
        for p in 0..3 {
            g.on_event_at(&ctx, ev_request(p, false), T0);
        }
    }
    // 6 个 16 KiB 请求；upload 不计入。
    assert_eq!(g.stats().refused_bytes, 6 * 16384);
    g.on_event_at(&ctx_for("7.7.7.7", true), ev_upload(0, 1000), T0);
    assert_eq!(g.stats().refused_bytes, 6 * 16384);
    // 普通 peer 的请求不计。
    g.on_event_at(&ctx_for("1.1.1.9", true), ev_request(0, false), T0);
    assert_eq!(g.stats().refused_bytes, 6 * 16384);
    assert_eq!(g.stats().enforced, 2, "one layer action per connection");
}

#[test]
fn stats_reset_on_local_day_rollover() {
    let (g, c) = guard_with(enforce_config());
    apply_cloud(&g, vec![("5.5.5.5/32", "ban")]);
    assert!(!g.admit(ip("5.5.5.5")));
    assert_eq!(g.stats().blocked_connections, 1);
    c.set(T0 + 2 * DAY);
    assert_eq!(
        g.stats().blocked_connections,
        0,
        "new local day starts from zero"
    );
    // 文档已过期（stale），换一份新的再计数。
    g.apply_cloud(&cloud_doc(
        2,
        T0 + 5 * DAY,
        vec![cloud_entry("5.5.5.5/32", "ban")],
    ))
    .expect("apply");
    assert!(!g.admit(ip("5.5.5.5")));
    assert_eq!(g.stats().blocked_connections, 1);
}

// ------------------------------------------------------------- 条目管理

#[test]
fn add_entry_validates_and_normalizes() {
    let (g, _c) = guard_with(enforce_config());
    let e = add(&g, "1.2.3.77/24", FilterAction::Ban, Some(3600));
    assert_eq!(e.cidr(), "1.2.3.0/24");
    assert_eq!(e.layer, Layer::Manual);
    assert_eq!(e.expires_at, Some(T0 + 3600));
    let allow = add(&g, "2001:db8::/32", FilterAction::Allow, None);
    assert_eq!(allow.layer, Layer::Allow);
    assert_eq!(allow.expires_at, None);
    // 手动层允许宽网段（宽网段守卫只针对 cloud / file）。
    add(&g, "8.0.0.0/8", FilterAction::Ban, None);
    assert!(!g.admit(ip("8.1.1.1")));
    let err = |new: NewEntry| g.add_entry(new).expect_err("must fail");
    let base = || NewEntry {
        cidr: "1.2.3.4".to_owned(),
        action: FilterAction::Ban,
        duration_secs: None,
        reason: String::new(),
    };
    assert!(matches!(
        err(NewEntry {
            cidr: "nope".to_owned(),
            ..base()
        }),
        GuardError::InvalidArgument(_)
    ));
    assert!(matches!(
        err(NewEntry {
            duration_secs: Some(0),
            ..base()
        }),
        GuardError::InvalidArgument(_)
    ));
    assert!(matches!(
        err(NewEntry {
            duration_secs: Some(-5),
            ..base()
        }),
        GuardError::InvalidArgument(_)
    ));
}

#[test]
fn add_ban_queues_disconnect_action_but_allow_does_not() {
    let (g, _c) = guard_with(enforce_config());
    add(&g, "1.2.3.4", FilterAction::Allow, None);
    assert_eq!(g.take_actions(), vec![GuardAction::Changed]);
    add(&g, "1.2.3.5", FilterAction::Ban, None);
    let actions = g.take_actions();
    assert!(actions.contains(&GuardAction::DisconnectBanned));
    assert!(actions.contains(&GuardAction::Changed));
}

#[test]
fn remove_entries_semantics() {
    let (g, _c) = guard_with(enforce_config());
    let a = add(&g, "1.1.1.9", FilterAction::Ban, None);
    let b = add(&g, "2.2.2.2", FilterAction::Allow, None);
    apply_cloud(&g, vec![("5.5.5.5/32", "ban")]);
    let cloud_id = g
        .list_entries(&EntryFilter {
            layer: Some(Layer::Cloud),
            limit: 10,
            ..EntryFilter::default()
        })
        .1[0]
        .id;
    // 含 cloud 条目的请求整体被拒绝，不删任何东西。
    let err = g
        .remove_entries(&[a.id, cloud_id])
        .expect_err("cloud removal refused");
    assert!(matches!(err, GuardError::InvalidArgument(_)));
    assert_eq!(g.counts().manual, 1);
    assert_eq!(g.counts().cloud, 1);
    // 不存在的 id 忽略；其余层可删。
    assert_eq!(g.remove_entries(&[a.id, b.id, 999_999]).expect("remove"), 2);
    assert_eq!(g.counts().manual, 0);
    assert_eq!(g.counts().allow, 0);
    assert!(g.admit(ip("1.1.1.9")));
    assert_eq!(g.remove_entries(&[]).expect("empty"), 0);
}

#[test]
fn list_entries_filters_paginates_and_orders_newest_first() {
    let (g, _c) = guard_with(enforce_config());
    for i in 1..=5u8 {
        g.add_entry(NewEntry {
            cidr: format!("1.1.1.{i}"),
            action: FilterAction::Ban,
            duration_secs: None,
            reason: if i % 2 == 0 {
                "Spammer".to_owned()
            } else {
                "other".to_owned()
            },
        })
        .expect("add");
    }
    add(&g, "9.9.9.9", FilterAction::Allow, None);
    let list = |f: EntryFilter| g.list_entries(&f);
    let (total, items) = list(EntryFilter {
        limit: 100,
        ..EntryFilter::default()
    });
    assert_eq!(total, 6);
    assert!(items.windows(2).all(|w| w[0].id > w[1].id), "descending id");
    let (total, page) = list(EntryFilter {
        layer: Some(Layer::Manual),
        offset: 1,
        limit: 2,
        ..EntryFilter::default()
    });
    assert_eq!(total, 5);
    assert_eq!(page.len(), 2);
    let (total, hits) = list(EntryFilter {
        query: Some("spam".to_owned()),
        limit: 100,
        ..EntryFilter::default()
    });
    assert_eq!((total, hits.len()), (2, 2), "case-insensitive reason match");
    let (total, _) = list(EntryFilter {
        query: Some("1.1.1.3".to_owned()),
        limit: 100,
        ..EntryFilter::default()
    });
    assert_eq!(total, 1);
    let (total, _) = list(EntryFilter {
        rule: Some("manual".to_owned()),
        limit: 100,
        ..EntryFilter::default()
    });
    assert_eq!(total, 6);
    let (total, _) = list(EntryFilter {
        rule: Some("excessive".to_owned()),
        limit: 100,
        ..EntryFilter::default()
    });
    assert_eq!(total, 0);
    let (_, none) = list(EntryFilter {
        offset: 100,
        limit: 10,
        ..EntryFilter::default()
    });
    assert!(none.is_empty());
}

#[test]
fn export_format_and_layer_filter() {
    let (g, _c) = guard_with(enforce_config());
    g.add_entry(NewEntry {
        cidr: "1.2.3.4".to_owned(),
        action: FilterAction::Ban,
        duration_secs: Some(60),
        reason: "line\nbreak".to_owned(),
    })
    .expect("add");
    add(&g, "2.2.2.0/24", FilterAction::Allow, None);
    let all = g.export(None);
    let lines: Vec<&str> = all.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0],
        format!("1.2.3.4/32 # manual manual line break {}", T0 + 60)
    );
    assert_eq!(
        lines[1],
        "2.2.2.0/24 # allow manual reason for 2.2.2.0/24 never"
    );
    assert_eq!(g.export(Some(Layer::Allow)).lines().count(), 1);
    assert_eq!(g.export(Some(Layer::Cloud)), "");
}

#[test]
fn counts_report_each_layer() {
    let (g, _c) = guard_with(enforce_config());
    add(&g, "1.1.1.1", FilterAction::Ban, None);
    add(&g, "1.1.1.2", FilterAction::Allow, None);
    apply_cloud(&g, vec![("5.5.5.5/32", "ban"), ("5.5.5.6/32", "choke")]);
    g.apply_filter_file("https://f.example/a", "7.7.7.0/24\n");
    let c = g.counts();
    assert_eq!(
        (c.allow, c.manual, c.auto, c.cloud, c.file),
        (1, 1, 0, 2, 1)
    );
}

// --------------------------------------------------------------- 过滤文件

#[test]
fn filter_file_bans_and_replaces_per_source() {
    let (g, _c) = guard_with(enforce_config());
    let a = g.apply_filter_file("https://f.example/a", "7.7.7.0/24\n8.8.8.0-8.8.8.9\n");
    assert_eq!(
        a.entries,
        1 + 2,
        "7.7.7.0/24 plus range 8.8.8.0-9 → one /29 and one /31"
    );
    let b = g.apply_filter_file("https://f.example/b", "9.9.9.9\n");
    assert_eq!(b.entries, 1);
    assert!(!g.admit(ip("7.7.7.77")));
    assert!(!g.admit(ip("8.8.8.9")));
    assert!(g.admit(ip("8.8.8.10")));
    assert!(!g.admit(ip("9.9.9.9")));
    // 只替换来源 a。
    g.apply_filter_file("https://f.example/a", "7.7.8.0/24\n");
    assert!(g.admit(ip("7.7.7.77")));
    assert!(!g.admit(ip("7.7.8.1")));
    assert!(!g.admit(ip("9.9.9.9")), "source b untouched");
    // 文件层条目永久、不可被「到期」。
    assert_eq!(g.counts().file, 2);
}

#[test]
fn filter_file_drops_wide_ranges_and_reports_counts() {
    let (g, _c) = guard_with(enforce_config());
    let s = g.apply_filter_file(
        "https://f.example/a",
        "1.0.0.0/8\n2.0.0.0/15\n3.3.0.0/16\nbad line\n2001:db8::/31\n",
    );
    assert_eq!(s.dropped_wide, 3);
    assert_eq!(s.invalid_lines, 1);
    assert_eq!(s.entries, 1);
    assert!(g.admit(ip("1.1.1.1")), "wide entries must not take effect");
    assert!(!g.admit(ip("3.3.9.9")));
}

#[test]
fn filter_file_error_keeps_old_entries_and_reports_status() {
    let mut cfg = enforce_config();
    cfg.filter_urls =
        "https://f.example/a\nhttps://f.example/b\n  \nhttps://f.example/a".to_owned();
    let (g, _c) = guard_with(cfg);
    g.apply_filter_file("https://f.example/a", "7.7.7.0/24\n");
    g.note_filter_error("https://f.example/a", "timeout");
    assert!(
        !g.admit(ip("7.7.7.1")),
        "stale-but-kept after a failed refresh"
    );
    let sources = g.filter_sources();
    assert_eq!(sources.len(), 2, "deduplicated config list");
    assert_eq!(sources[0].url, "https://f.example/a");
    assert!(!sources[0].ok);
    assert_eq!(sources[0].entries, 1);
    assert_eq!(sources[0].error.as_deref(), Some("timeout"));
    assert_eq!(sources[1].url, "https://f.example/b");
    assert!(!sources[1].ok && sources[1].error.is_none() && sources[1].entries == 0);
    // 成功刷新清除错误。
    g.apply_filter_file("https://f.example/a", "7.7.7.0/24\n");
    let sources = g.filter_sources();
    assert!(sources[0].ok);
    assert_eq!(sources[0].error, None);
}

#[test]
fn removing_a_subscription_url_drops_its_entries() {
    let mut cfg = enforce_config();
    cfg.filter_urls = "https://f.example/a\nhttps://f.example/b".to_owned();
    let (g, _c) = guard_with(cfg.clone());
    g.apply_filter_file("https://f.example/a", "7.7.7.0/24\n");
    g.apply_filter_file("https://f.example/b", "9.9.9.9\n");
    cfg.filter_urls = "https://f.example/b".to_owned();
    g.set_config(cfg);
    assert!(g.admit(ip("7.7.7.1")));
    assert!(!g.admit(ip("9.9.9.9")));
    assert_eq!(g.counts().file, 1);
}

#[test]
fn hot_config_update_takes_effect_without_rebuilding() {
    let (g, _c) = guard_with(enforce_config());
    apply_cloud(&g, vec![("5.5.5.5/32", "ban")]);
    assert!(!g.admit(ip("5.5.5.5")));
    let mut cfg = (*g.config()).clone();
    cfg.cloud_mode = RuleMode::Observe;
    g.set_config(cfg.clone());
    assert!(g.admit(ip("5.5.5.5")));
    cfg.cloud_mode = RuleMode::Enforce;
    cfg.cloud_enabled = false;
    g.set_config(cfg);
    assert!(g.admit(ip("5.5.5.5")), "cloud disabled");
    let actions = g.take_actions();
    assert!(actions.contains(&GuardAction::DisconnectBanned));
}
