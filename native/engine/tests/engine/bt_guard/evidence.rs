use fluxdown_engine::bt_guard::{BtGuard, BtGuardConfig, GuardAction, GuardEvent, Verdict};

use super::support::{
    MIB, T0, TOTAL, bits, ctx_for, deliver_piece, disconnect, ev_request, ev_upload, guard_with,
    hash,
};

fn reporting() -> BtGuardConfig {
    BtGuardConfig {
        report_enabled: true,
        ..BtGuardConfig::default()
    }
}

fn excessive(g: &BtGuard, ip: &str) {
    let ctx = ctx_for(ip, true);
    for i in 0..201u32 {
        deliver_piece(g, &ctx, i % 100, T0);
    }
}

#[test]
fn nothing_is_collected_unless_reporting_is_enabled() {
    let (g, _c) = guard_with(BtGuardConfig::default());
    excessive(&g, "1.2.3.4");
    assert_eq!(g.pending_evidence(), 0);
    assert!(g.drain_evidence().is_empty());
}

#[test]
fn excessive_evidence_has_the_contract_fields() {
    let (g, _c) = guard_with(reporting());
    let ctx = ctx_for("1.2.3.4", true);
    g.on_event_at(
        &ctx,
        GuardEvent::Handshake {
            peer_id: *b"-XL0019-abcdefghijkl",
            reserved: 0,
            incoming: false,
        },
        T0,
    );
    g.on_event_at(
        &ctx,
        GuardEvent::ExtendedHandshake {
            client: Some("Xunlei 0.0.1.9"),
        },
        T0,
    );
    for i in 0..201u32 {
        // 最后一片晚 100s 送达；其余同一时刻，宽限内不会触发 lag。
        let at = if i == 200 { T0 + 100 } else { T0 };
        deliver_piece(&g, &ctx, i % 100, at);
    }
    let items = g.drain_evidence();
    assert_eq!(items.len(), 1);
    let e = &items[0];
    assert_eq!(e.ip_group, "1.2.3.4");
    assert_eq!(e.rule, "excessive");
    assert_eq!(e.strength, "strong");
    assert_eq!(e.info_hash, "01".repeat(20));
    assert_eq!(e.torrent_size, TOTAL);
    assert_eq!(e.delivered_bytes, 201 * MIB);
    assert_eq!(e.dup_bytes, 101 * MIB);
    assert_eq!(e.ratio_milli, 2010);
    assert_eq!(e.peer_id_prefix, "-XL0019-");
    assert_eq!(e.client, "Xunlei 0.0.1.9");
    assert_eq!(e.first_seen, T0);
    assert_eq!(e.last_seen, T0 + 100);
    // 序列化字段名符合契约 §6。
    let json = serde_json::to_value(e).expect("json");
    for key in [
        "ipGroup",
        "rule",
        "strength",
        "infoHash",
        "torrentSize",
        "deliveredBytes",
        "dupBytes",
        "ratioMilli",
        "rerequests",
        "rewindPermille",
        "peerIdPrefix",
        "client",
        "firstSeen",
        "lastSeen",
    ] {
        assert!(json.get(key).is_some(), "missing {key}");
    }
    // 不含任何文件名 / 路径 / 种子名字段。
    assert_eq!(json.as_object().expect("object").len(), 14);
    assert!(g.drain_evidence().is_empty(), "drain empties the queue");
}

#[test]
fn evidence_is_deduplicated_per_group_rule_and_torrent() {
    let (g, _c) = guard_with(reporting());
    excessive(&g, "1.2.3.4");
    assert_eq!(g.pending_evidence(), 1);
    // 同一组重连再犯：不重复入队。
    let mut ctx = ctx_for("1.2.3.4", true);
    ctx.addr.set_port(9000);
    g.on_event_at(&ctx, ev_upload(0, 250 * 1024 * 1024), T0 + 5);
    assert_eq!(g.pending_evidence(), 1);
    // 另一个种子是新证据。
    let mut other = ctx_for("1.2.3.4", true);
    other.info_hash = hash(2);
    g.on_event_at(&other, ev_upload(0, 250 * 1024 * 1024), T0 + 6);
    assert_eq!(g.pending_evidence(), 2);
}

#[test]
fn private_torrents_never_report_even_when_detection_is_enabled() {
    let cfg = BtGuardConfig {
        skip_private: false,
        ..reporting()
    };
    let (g, _c) = guard_with(cfg);
    let mut ctx = ctx_for("1.2.3.4", true);
    ctx.private = true;
    assert_eq!(
        g.on_event_at(&ctx, ev_upload(0, 250 * 1024 * 1024), T0),
        Verdict::Allow
    );
    assert_eq!(g.pending_evidence(), 0);
    // 对照：同样的事件在非私有种子上入队。
    let public = ctx_for("5.6.7.8", true);
    g.on_event_at(&public, ev_upload(0, 250 * 1024 * 1024), T0);
    assert_eq!(g.pending_evidence(), 1);
}

#[test]
fn disabling_reporting_clears_the_queue_and_stops_collection() {
    let (g, _c) = guard_with(reporting());
    excessive(&g, "1.2.3.4");
    assert_eq!(g.pending_evidence(), 1);
    let mut cfg = (*g.config()).clone();
    cfg.report_enabled = false;
    g.set_config(cfg);
    assert_eq!(g.pending_evidence(), 0);
    excessive(&g, "5.6.7.8");
    assert_eq!(g.pending_evidence(), 0);
}

#[test]
fn queue_is_capped_at_1000_dropping_the_oldest() {
    let (g, _c) = guard_with(reporting());
    for i in 0..1100u32 {
        let ip = format!("20.{}.{}.{}", i / 65536, (i / 256) % 256, i % 256);
        let ctx = ctx_for(&ip, true);
        g.on_event_at(&ctx, ev_upload(0, 250 * 1024 * 1024), T0);
    }
    assert_eq!(g.pending_evidence(), 1000);
    let items = g.drain_evidence();
    assert_eq!(items.len(), 1000);
    assert_eq!(items[0].ip_group, "20.0.0.100", "first 100 were dropped");
    assert_eq!(items[999].ip_group, "20.0.4.75");
}

#[test]
fn evidence_ready_action_is_queued_once() {
    let (g, _c) = guard_with(reporting());
    excessive(&g, "1.2.3.4");
    excessive(&g, "5.6.7.8");
    let ready = g
        .take_actions()
        .into_iter()
        .filter(|a| *a == GuardAction::EvidenceReady)
        .count();
    assert_eq!(ready, 1);
}

#[test]
fn rule_strengths_and_rule_specific_fields() {
    let (g, _c) = guard_with(reporting());
    // rerequest（strong）：rerequests 计数。
    let ctx = ctx_for("2.2.2.2", true);
    g.on_event_at(&ctx, ev_request(1, true), T0);
    g.on_event_at(&ctx, ev_request(2, true), T0);
    // rewind（strong）：丢失 50/100 → 500‰。
    let seeded = ctx_for("3.3.3.3", true);
    for p in 0..100 {
        deliver_piece(&g, &seeded, p, T0);
    }
    disconnect(&g, &seeded, Some(&bits(0..100)), T0);
    let b = bits(50..100);
    g.on_event_at(&seeded, GuardEvent::Bitfield { bits: &b }, T0 + 5);
    // lag（medium）。
    let lagging = ctx_for("4.4.4.4", true);
    for p in 0..4 {
        deliver_piece(&g, &lagging, p, T0);
    }
    g.on_event_at(&lagging, ev_request(60, false), T0 + 200);
    let items = g.drain_evidence();
    let find = |rule: &str| items.iter().find(|e| e.rule == rule).expect(rule);
    assert_eq!(find("rerequest").strength, "strong");
    assert_eq!(find("rerequest").rerequests, 2);
    assert_eq!(find("rewind").strength, "strong");
    assert_eq!(find("rewind").rewind_permille, 500);
    assert_eq!(find("lag").strength, "medium");
    assert_eq!(find("lag").ip_group, "4.4.4.4");
}

#[test]
fn non_printable_peer_id_prefix_is_not_reported() {
    let (g, _c) = guard_with(reporting());
    let ctx = ctx_for("1.2.3.4", true);
    let mut id = [0u8; 20];
    id[..8].copy_from_slice(&[0x01, 0xff, b'-', b'X', b'L', 0x00, b'9', b'-']);
    g.on_event_at(
        &ctx,
        GuardEvent::Handshake {
            peer_id: id,
            reserved: 0,
            incoming: true,
        },
        T0,
    );
    g.on_event_at(&ctx, ev_upload(0, 250 * 1024 * 1024), T0);
    let items = g.drain_evidence();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].peer_id_prefix, "");
    assert_eq!(items[0].client, "");
}

#[test]
fn fingerprint_and_cloud_hits_follow_the_reporting_rules() {
    // fingerprint（weak）上报；cloud 命中不上报。
    let (g, _c) = guard_with(reporting());
    let mut doc = super::support::cloud_doc(
        1,
        T0 + 86_400,
        vec![super::support::cloud_entry("20.0.0.1/32", "ban")],
    );
    doc.client_rules = vec![fluxdown_engine::bt_guard::CloudClientRule {
        id: "xl".to_owned(),
        field: "peerIdPrefix".to_owned(),
        op: "prefix".to_owned(),
        value: "-XL".to_owned(),
        action: "choke".to_owned(),
        seeding_only: false,
        reason: String::new(),
    }];
    g.apply_cloud(&doc).expect("apply");
    let ctx = ctx_for("5.5.5.5", true);
    g.on_event_at(
        &ctx,
        GuardEvent::Handshake {
            peer_id: *b"-XL0019-abcdefghijkl",
            reserved: 0,
            incoming: false,
        },
        T0,
    );
    let cloud_ctx = ctx_for("20.0.0.1", true);
    g.on_event_at(&cloud_ctx, ev_request(1, false), T0);
    let items = g.drain_evidence();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].rule, "fingerprint");
    assert_eq!(items[0].strength, "weak");
}
