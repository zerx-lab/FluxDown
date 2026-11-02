use fluxdown_engine::bt_guard::{
    BtGuard, BtGuardConfig, CloudClientRule, DownloadingMaxAction, EntryFilter, FilterAction,
    GuardAction, GuardEvent, IpGroup, Layer, NewEntry, RuleMode, Verdict,
};

use super::support::{
    DAY, MIB, PIECE_LEN, T0, bits, cloud_doc, ctx_for, deliver_piece, disconnect, enforce_config,
    ev_block, ev_request, ev_upload, guard_with, hash,
};

const PEER: &str = "1.2.3.4";

fn auto_entries(g: &BtGuard) -> usize {
    g.list_entries(&EntryFilter {
        layer: Some(Layer::Auto),
        limit: 100,
        ..EntryFilter::default()
    })
    .0
}

/// 先送满 100 片，再重复送，累计到 `target_mib` MiB（逐片事件）；返回最后一次判决。
fn deliver_total(g: &BtGuard, ip: &str, target_mib: u64, now: i64) -> Verdict {
    let ctx = ctx_for(ip, true);
    let mut verdict = Verdict::Allow;
    for i in 0..target_mib {
        verdict = deliver_piece(g, &ctx, (i % 100) as u32, now);
        if verdict != Verdict::Allow {
            return verdict;
        }
    }
    verdict
}

// ------------------------------------------------------------------ excessive

#[test]
fn excessive_triggers_strictly_above_ratio() {
    let (g, _c) = guard_with(enforce_config());
    // 恰好 200 MiB（= 2 × 100 MiB）不触发。
    assert_eq!(deliver_total(&g, PEER, 200, T0), Verdict::Allow);
    assert_eq!(auto_entries(&g), 0);
    // 多 1 个分片 → 触发。
    let ctx = ctx_for(PEER, true);
    assert_eq!(deliver_piece(&g, &ctx, 0, T0), Verdict::Disconnect);
    assert_eq!(auto_entries(&g), 1);
    assert!(!g.admit(PEER.parse().expect("ip")));
    // dup：前 100 片是首次送达，其后的 101 片都是重复。
    let group: IpGroup = PEER.parse().expect("group");
    assert_eq!(
        g.group_delivered_bytes(&ctx.info_hash, &group),
        Some(201 * MIB)
    );
    assert_eq!(g.group_dup_bytes(&ctx.info_hash, &group), Some(101 * MIB));
}

#[test]
fn excessive_verdict_is_sticky_for_the_connection() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    assert_eq!(deliver_total(&g, PEER, 201, T0), Verdict::Disconnect);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(3, false), T0),
        Verdict::Disconnect
    );
}

#[test]
fn excessive_never_triggers_for_small_torrents() {
    let (g, _c) = guard_with(enforce_config());
    let mut ctx = ctx_for(PEER, true);
    ctx.total_length = 32 * MIB; // < 64 MiB 下限
    ctx.num_pieces = 32;
    for i in 0..400u32 {
        assert_eq!(deliver_piece(&g, &ctx, i % 32, T0), Verdict::Allow);
    }
    assert_eq!(auto_entries(&g), 0);
}

#[test]
fn excessive_respects_configured_ratio_and_min_size() {
    let mut cfg = enforce_config();
    cfg.excess_ratio_pct = 110;
    cfg.min_torrent_mib = 0;
    let (g, _c) = guard_with(cfg);
    let mut ctx = ctx_for(PEER, true);
    ctx.total_length = 10 * MIB;
    ctx.num_pieces = 10;
    // 11 MiB = 110%：不触发；12 MiB：触发。
    for i in 0..11u32 {
        assert_eq!(
            deliver_piece(&g, &ctx, i % 10, T0),
            Verdict::Allow,
            "piece {i}"
        );
    }
    assert_eq!(deliver_piece(&g, &ctx, 0, T0), Verdict::Disconnect);
}

#[test]
fn zero_length_torrent_never_triggers_excessive() {
    let mut cfg = enforce_config();
    cfg.min_torrent_mib = 0;
    let (g, _c) = guard_with(cfg);
    let mut ctx = ctx_for(PEER, true);
    ctx.total_length = 0;
    assert_eq!(
        g.on_event_at(&ctx, ev_upload(0, 1 << 20), T0),
        Verdict::Allow
    );
}

#[test]
fn block_level_accounting_only_marks_complete_pieces_delivered() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    let group: IpGroup = PEER.parse().expect("group");
    let block = 16 * 1024u32;
    // 连接 A 只送半片后断开。
    for i in 0..32 {
        g.on_event_at(&ctx, ev_block(0, i * block, block), T0);
    }
    disconnect(&g, &ctx, None, T0);
    // 连接 B 送完整片：此前的半片不算「已送达」，因此没有 dup。
    for i in 0..64 {
        g.on_event_at(&ctx, ev_block(0, i * block, block), T0);
    }
    assert_eq!(g.group_dup_bytes(&ctx.info_hash, &group), Some(0));
    assert_eq!(
        g.group_delivered_bytes(&ctx.info_hash, &group),
        Some(96 * u64::from(block))
    );
    // 再送一遍整片：全部是 dup。
    for i in 0..64 {
        g.on_event_at(&ctx, ev_block(0, i * block, block), T0);
    }
    assert_eq!(
        g.group_dup_bytes(&ctx.info_hash, &group),
        Some(64 * u64::from(block))
    );
    assert_eq!(u64::from(PIECE_LEN), 64 * u64::from(block));
}

#[test]
fn a_resent_block_never_completes_a_piece_nor_counts_as_dup() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    let group: IpGroup = PEER.parse().expect("group");
    let block = 16 * 1024u32;
    // 同一块（piece 0, begin 0）重发 80 次：累计字节早已超过一片，但只有一个不同块。
    for _ in 0..80 {
        g.on_event_at(&ctx, ev_block(0, 0, block), T0);
    }
    assert_eq!(
        g.group_dup_bytes(&ctx.info_hash, &group),
        Some(0),
        "the piece is not delivered, so resent blocks are not dup bytes"
    );
    // 补齐其余 63 个不同块 → 恰好完成这一片；此后再送任意块才是 dup。
    for i in 1..64 {
        g.on_event_at(&ctx, ev_block(0, i * block, block), T0);
    }
    assert_eq!(g.group_dup_bytes(&ctx.info_hash, &group), Some(0));
    g.on_event_at(&ctx, ev_block(0, 0, block), T0);
    assert_eq!(
        g.group_dup_bytes(&ctx.info_hash, &group),
        Some(u64::from(block))
    );
}

#[test]
fn last_piece_uses_its_real_length() {
    let (g, _c) = guard_with(enforce_config());
    let mut ctx = ctx_for(PEER, true);
    ctx.total_length = 100 * MIB - 1000;
    let group: IpGroup = PEER.parse().expect("group");
    // 末片 = 1 MiB - 1000 字节，一次送完即算送达，再送即 dup。
    let last_len = PIECE_LEN - 1000;
    g.on_event_at(&ctx, ev_upload(99, last_len), T0);
    g.on_event_at(&ctx, ev_upload(99, 1000), T0);
    assert_eq!(g.group_dup_bytes(&ctx.info_hash, &group), Some(1000));
}

// ------------------------------------------------------------------ rerequest

#[test]
fn rerequest_needs_two_distinct_claimed_pieces() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    // 未声明拥有的请求不计。
    for p in 0..10 {
        assert_eq!(
            g.on_event_at(&ctx, ev_request(p, false), T0),
            Verdict::Allow
        );
    }
    // 一片（含重复请求同一片）不触发。
    assert_eq!(
        g.on_event_at(&ctx, ev_request(20, true), T0),
        Verdict::Allow
    );
    assert_eq!(
        g.on_event_at(&ctx, ev_request(20, true), T0),
        Verdict::Allow
    );
    assert_eq!(auto_entries(&g), 0);
    // 第二个不同分片 → 触发。
    assert_eq!(
        g.on_event_at(&ctx, ev_request(21, true), T0),
        Verdict::Disconnect
    );
    assert_eq!(auto_entries(&g), 1);
}

#[test]
fn rerequest_out_of_range_piece_is_ignored() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(500, true), T0),
        Verdict::Allow
    );
    assert_eq!(
        g.on_event_at(&ctx, ev_request(501, true), T0),
        Verdict::Allow
    );
}

// --------------------------------------------------------------------- rewind

/// 向 PEER 送满 100 片并以全 1 位图断开。
fn seeded_ledger(g: &BtGuard) {
    let ctx = ctx_for(PEER, true);
    for p in 0..100 {
        assert_eq!(deliver_piece(g, &ctx, p, T0), Verdict::Allow);
    }
    disconnect(g, &ctx, Some(&bits(0..100)), T0);
}

fn reconnect_with(g: &BtGuard, missing: usize) -> Verdict {
    let ctx = ctx_for(PEER, true);
    let b = bits(missing..100);
    g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0 + 60)
}

#[test]
fn rewind_tolerates_seven_percent_and_flags_above() {
    // 丢 7/100 = 7%：不超过容忍。
    let (g, _c) = guard_with(enforce_config());
    seeded_ledger(&g);
    assert_eq!(reconnect_with(&g, 7), Verdict::Allow);
    // 丢 8/100 = 8% > 7%。
    let (g, _c) = guard_with(enforce_config());
    seeded_ledger(&g);
    assert_eq!(reconnect_with(&g, 8), Verdict::Disconnect);
    assert_eq!(auto_entries(&g), 1);
}

#[test]
fn rewind_tolerance_is_configurable() {
    let mut cfg = enforce_config();
    cfg.rewind_tolerance_pct = 0;
    let (g, _c) = guard_with(cfg);
    seeded_ledger(&g);
    assert_eq!(reconnect_with(&g, 4), Verdict::Disconnect);
    let mut cfg = enforce_config();
    cfg.rewind_tolerance_pct = 100;
    let (g, _c) = guard_with(cfg);
    seeded_ledger(&g);
    assert_eq!(reconnect_with(&g, 100), Verdict::Allow);
}

#[test]
fn rewind_needs_at_least_four_lost_pieces_even_with_zero_tolerance() {
    let mut cfg = enforce_config();
    cfg.rewind_tolerance_pct = 0;
    let (g, _c) = guard_with(cfg);
    seeded_ledger(&g);
    assert_eq!(reconnect_with(&g, 3), Verdict::Allow);
    assert_eq!(auto_entries(&g), 0);
}

#[test]
fn freshly_delivered_pieces_are_not_part_of_the_rewind_baseline() {
    // 刚写进 socket 的分片对端可能还没收到：断开时不满宽限期的送达不进入比较基准。
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    for p in 0..20 {
        deliver_piece(&g, &ctx, p, T0);
    }
    disconnect(&g, &ctx, None, T0 + 30);
    let empty = bits(std::iter::empty());
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &empty }, T0 + 60),
        Verdict::Allow
    );
    assert_eq!(auto_entries(&g), 0);
}

#[test]
fn pieces_delivered_before_the_grace_period_are_part_of_the_rewind_baseline() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    for p in 0..20 {
        deliver_piece(&g, &ctx, p, T0);
    }
    // 断开时已过 120s 宽限：对端理应已收到并声明过这 20 片。
    disconnect(&g, &ctx, None, T0 + 200);
    let empty = bits(std::iter::empty());
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &empty }, T0 + 210),
        Verdict::Disconnect
    );
}

#[test]
fn empty_bitfield_from_a_different_client_is_not_a_rewind() {
    let id_a: [u8; 20] = *b"-qB4630-aaaaaaaaaaaa";
    let id_b: [u8; 20] = *b"-TR3000-bbbbbbbbbbbb";
    let scenario = |second_id: &[u8; 20]| {
        let (g, _c) = guard_with(enforce_config());
        let ctx = ctx_for(PEER, true);
        g.on_event_at(&ctx, handshake(&id_a), T0);
        for p in 0..10 {
            deliver_piece(&g, &ctx, p, T0);
        }
        disconnect(&g, &ctx, Some(&bits(0..10)), T0);
        g.on_event_at(&ctx, handshake(second_id), T0 + 5);
        let empty = bits(std::iter::empty());
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &empty }, T0 + 6)
    };
    // 同 IP 组下换了个客户端、交出空位图：多半不是回退。
    assert_eq!(scenario(&id_b), Verdict::Allow);
    // 对照：同一个客户端交出空位图就是 rewind。
    assert_eq!(scenario(&id_a), Verdict::Disconnect);
}

#[test]
fn rewind_is_skipped_while_another_connection_of_the_group_is_alive() {
    let (g, _c) = guard_with(enforce_config());
    seeded_ledger(&g);
    // 同一 IP 组的另一条连接（NAT 后的第二个客户端）仍然存活。
    let mut other = ctx_for(PEER, true);
    other.addr.set_port(7000);
    g.on_event_at(&other, ev_request(30, false), T0 + 10);
    assert_eq!(reconnect_with(&g, 100), Verdict::Allow);
    assert_eq!(auto_entries(&g), 0);
    // 对照：另一条连接断开后，同样的位图就是 rewind。
    disconnect(&g, &other, None, T0 + 20);
    let ctx = ctx_for(PEER, true);
    disconnect(&g, &ctx, None, T0 + 20);
    assert_eq!(reconnect_with(&g, 100), Verdict::Disconnect);
}

#[test]
fn rewind_counts_last_claimed_pieces_too() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    // 只送 10 片，但对端断开时声明拥有 0..50。
    for p in 0..10 {
        deliver_piece(&g, &ctx, p, T0);
    }
    disconnect(&g, &ctx, Some(&bits(0..50)), T0);
    // 重连只剩 40 片：丢 10/50 = 20%。
    let b = bits(0..40);
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0 + 5),
        Verdict::Disconnect
    );
}

#[test]
fn rewind_requires_at_least_one_delivered_piece() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    // 只有 rerequests 记录（创建账本）但 delivered 为空。
    g.on_event_at(&ctx, ev_request(1, true), T0);
    disconnect(&g, &ctx, Some(&bits(0..100)), T0);
    let b = bits(std::iter::empty());
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0 + 5),
        Verdict::Allow
    );
}

#[test]
fn disconnect_without_bits_falls_back_to_the_connection_claims_including_have() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    for p in 0..10 {
        deliver_piece(&g, &ctx, p, T0);
    }
    let b = bits(0..20);
    g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0);
    g.on_event_at(&ctx, GuardEvent::Have { piece: 30 }, T0);
    // 越界 HAVE 被忽略，不污染位集。
    g.on_event_at(&ctx, GuardEvent::Have { piece: 5000 }, T0);
    disconnect(&g, &ctx, None, T0);
    // 记录的是 bitfield ∪ HAVE = 21 片；重连后一片都没有 → 大幅回退。
    let empty = bits(std::iter::empty());
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &empty }, T0 + 5),
        Verdict::Disconnect
    );
}

#[test]
fn explicit_disconnect_bits_override_the_tracked_claims() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    for p in 0..10 {
        deliver_piece(&g, &ctx, p, T0);
    }
    let claimed = bits(0..50);
    g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &claimed }, T0);
    // 连接结束时的位图只剩已送达的 10 片 → 之后只需保持这 10 片。
    disconnect(&g, &ctx, Some(&bits(0..10)), T0);
    let again = bits(0..10);
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &again }, T0 + 5),
        Verdict::Allow
    );
}

#[test]
fn peer_insight_is_gone_after_disconnect() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    g.on_event_at(&ctx, ev_request(30, false), T0);
    assert!(g.peer_insight(&ctx.info_hash, ctx.addr).is_some());
    disconnect(&g, &ctx, None, T0);
    assert!(g.peer_insight(&ctx.info_hash, ctx.addr).is_none());
}

#[test]
fn guard_is_send_sync_and_survives_concurrent_hammering() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<BtGuard>();
    let (g, _c) = guard_with(enforce_config());
    let g = std::sync::Arc::new(g);
    let mut handles = Vec::new();
    for t in 0..8u8 {
        let g = std::sync::Arc::clone(&g);
        handles.push(std::thread::spawn(move || {
            let ip = format!("30.0.0.{}", t + 1);
            let ctx = ctx_for(&ip, true);
            for round in 0..300u32 {
                let _ = g.admit(ip.parse().expect("ip"));
                g.on_event_at(&ctx, ev_request(round % 100, round % 7 == 0), T0);
                g.on_event_at(&ctx, ev_upload(round % 100, 16384), T0);
                if round % 50 == 0 {
                    g.add_entry(NewEntry {
                        cidr: format!("31.0.{t}.{}", round / 50),
                        action: FilterAction::Throttle,
                        duration_secs: Some(60),
                        reason: String::new(),
                    })
                    .expect("add");
                    let _ = g.counts();
                    let _ = g.take_actions();
                }
            }
            disconnect(&g, &ctx, None, T0);
        }));
    }
    for h in handles {
        h.join().expect("worker thread must not panic");
    }
    // 每线程添加 6 条手动条目（轮次 0,50,…,250），并发下不丢；各线程的 rerequest 封禁也都生效。
    assert_eq!(g.counts().manual, 8 * 6);
    assert_eq!(g.counts().auto, 8);
}

#[test]
fn rewind_ignores_first_connection_without_ledger() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    let b = bits(std::iter::empty());
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0),
        Verdict::Allow
    );
}

// ------------------------------------------------------------------------ lag

fn deliver_four(g: &BtGuard, t: i64) -> fluxdown_engine::bt_guard::GuardCtx {
    let ctx = ctx_for(PEER, true);
    for p in 0..4 {
        assert_eq!(deliver_piece(g, &ctx, p, t), Verdict::Allow);
    }
    ctx
}

#[test]
fn lag_respects_grace_period() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = deliver_four(&g, T0);
    // 宽限内（119s）不评估为滞后。
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 119),
        Verdict::Allow
    );
    // 评估节流 15s 后、已过宽限 → 探测断开。
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 134),
        Verdict::Disconnect
    );
}

#[test]
fn lag_grace_boundary_is_inclusive_at_120s() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = deliver_four(&g, T0);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 120),
        Verdict::Disconnect
    );
}

#[test]
fn lag_needs_more_than_half_missing() {
    // 4 片中 2 片出现 HAVE：恰好 50% 缺失，不触发。
    let (g, _c) = guard_with(enforce_config());
    let ctx = deliver_four(&g, T0);
    g.on_event_at(&ctx, GuardEvent::Have { piece: 0 }, T0 + 10);
    g.on_event_at(&ctx, GuardEvent::Have { piece: 1 }, T0 + 10);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 200),
        Verdict::Allow
    );
    // 只有 1 片出现 HAVE：75% 缺失，触发。
    let (g, _c) = guard_with(enforce_config());
    let ctx = deliver_four(&g, T0);
    g.on_event_at(&ctx, GuardEvent::Have { piece: 0 }, T0 + 10);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 200),
        Verdict::Disconnect
    );
}

#[test]
fn lag_counts_bitfield_claims_as_present() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    let b = bits(0..4);
    g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0);
    for p in 0..4 {
        deliver_piece(&g, &ctx, p, T0);
    }
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 300),
        Verdict::Allow
    );
}

#[test]
fn lag_probe_round_trip_clears_when_peer_is_complete_after_reconnect() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = deliver_four(&g, T0);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 130),
        Verdict::Disconnect
    );
    // 探测只是断开：没有封禁条目、没有 strike。
    assert_eq!(auto_entries(&g), 0);
    let group: IpGroup = PEER.parse().expect("group");
    assert_eq!(g.strikes_of(&group), 0);
    disconnect(&g, &ctx, None, T0 + 131);
    // 对方重连，位图补齐 → 清除嫌疑。
    let b = bits(0..4);
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0 + 140),
        Verdict::Allow
    );
    assert_eq!(auto_entries(&g), 0);
    assert_eq!(g.strikes_of(&group), 0);
}

#[test]
fn lag_probe_round_trip_flags_rewind_when_still_missing() {
    // 容忍度 100：常规 rewind 永不触发，只剩探测往返这条路径。
    let mut cfg = enforce_config();
    cfg.rewind_tolerance_pct = 100;
    let (g, _c) = guard_with(cfg);
    let ctx = deliver_four(&g, T0);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 130),
        Verdict::Disconnect
    );
    disconnect(&g, &ctx, None, T0 + 131);
    // 重连后仍缺 3/4 > 50% → 记为 rewind 命中 → 封禁。
    let b = bits(0..1);
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0 + 140),
        Verdict::Disconnect
    );
    assert_eq!(auto_entries(&g), 1);
    let group: IpGroup = PEER.parse().expect("group");
    assert_eq!(g.strikes_of(&group), 1);
}

#[test]
fn lag_probe_round_trip_tolerates_minority_still_missing() {
    let mut cfg = enforce_config();
    cfg.rewind_tolerance_pct = 100;
    let (g, _c) = guard_with(cfg);
    let ctx = deliver_four(&g, T0);
    g.on_event_at(&ctx, ev_request(50, false), T0 + 130);
    disconnect(&g, &ctx, None, T0 + 131);
    // 4 片中补齐 3 片：仍缺 1/4，不超过一半。
    let b = bits(0..3);
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0 + 140),
        Verdict::Allow
    );
}

#[test]
fn lag_probes_at_most_once_per_group_per_24h() {
    let (g, c) = guard_with(enforce_config());
    let ctx = deliver_four(&g, T0);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 130),
        Verdict::Disconnect
    );
    // 对方没有重连交出位图：探测悬而未决，不算「补齐」。
    disconnect(&g, &ctx, None, T0 + 131);
    // 24h 内再次滞后：不再探测，退为中信号动作（做种任务 → Choke）。
    for p in 4..8 {
        deliver_piece(&g, &ctx, p, T0 + 141);
    }
    assert_eq!(
        g.on_event_at(&ctx, ev_request(51, false), T0 + 400),
        Verdict::Choke
    );
    disconnect(&g, &ctx, None, T0 + 401);
    // 24h 后可再次探测。
    c.set(T0 + 130 + DAY);
    let t = T0 + 130 + DAY;
    let ctx2 = ctx_for(PEER, true);
    for p in 8..12 {
        deliver_piece(&g, &ctx2, p, t);
    }
    assert_eq!(
        g.on_event_at(&ctx2, ev_request(52, false), t + 130),
        Verdict::Disconnect
    );
}

#[test]
fn a_cleared_probe_starts_a_cooldown_where_lag_is_only_observed() {
    let (g, c) = guard_with(enforce_config());
    let ctx = deliver_four(&g, T0);
    assert_eq!(
        g.on_event_at(&ctx, ev_request(50, false), T0 + 130),
        Verdict::Disconnect
    );
    disconnect(&g, &ctx, None, T0 + 131);
    // 对方重连补齐位图 → 嫌疑清除，进入冷却。
    let b = bits(0..4);
    assert_eq!(
        g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0 + 140),
        Verdict::Allow
    );
    for p in 4..8 {
        deliver_piece(&g, &ctx, p, T0 + 141);
    }
    // 冷却期内同组 lag 只观察：不 choke、不探测。
    assert_eq!(
        g.on_event_at(&ctx, ev_request(51, false), T0 + 400),
        Verdict::Allow
    );
    assert_eq!(g.stats().observed, 1);
    assert_eq!(
        g.stats().enforced,
        1,
        "only the original probe was enforced"
    );
    disconnect(&g, &ctx, None, T0 + 401);
    // 冷却结束（清除后满 24h）且探测配额已恢复：可再次探测。
    c.set(T0 + 140 + DAY);
    let t = T0 + 140 + DAY;
    let ctx2 = ctx_for(PEER, true);
    for p in 8..12 {
        deliver_piece(&g, &ctx2, p, t);
    }
    assert_eq!(
        g.on_event_at(&ctx2, ev_request(52, false), t + 130),
        Verdict::Disconnect
    );
}

#[test]
fn lag_downloading_task_gets_throttle_instead_of_probe_when_probe_unavailable() {
    let (g, _c) = guard_with(enforce_config());
    // 先用做种任务消耗掉探测配额。
    let seed_ctx = deliver_four(&g, T0);
    g.on_event_at(&seed_ctx, ev_request(50, false), T0 + 130);
    disconnect(&g, &seed_ctx, None, T0 + 131);
    // 同组再以下载中任务连接并滞后 → Throttle。
    let dl = ctx_for(PEER, false);
    for p in 10..14 {
        deliver_piece(&g, &dl, p, T0 + 200);
    }
    assert_eq!(
        g.on_event_at(&dl, ev_request(60, false), T0 + 400),
        Verdict::Throttle
    );
}

// ---------------------------------------------------------------- fingerprint

fn xl_rule(seeding_only: bool, action: &str) -> CloudClientRule {
    CloudClientRule {
        id: "xl0019".to_owned(),
        field: "peerIdPrefix".to_owned(),
        op: "prefix".to_owned(),
        value: "-XL0019-".to_owned(),
        action: action.to_owned(),
        seeding_only,
        reason: "xunlei".to_owned(),
    }
}

fn apply_rules(g: &BtGuard, rules: Vec<CloudClientRule>) {
    let mut doc = cloud_doc(1, T0 + DAY, Vec::new());
    doc.client_rules = rules;
    g.apply_cloud(&doc).expect("apply");
}

fn handshake(id: &[u8; 20]) -> GuardEvent<'static> {
    GuardEvent::Handshake {
        peer_id: *id,
        reserved: 0,
        incoming: false,
    }
}

const XL_ID: [u8; 20] = *b"-XL0019-abcdefghijkl";

#[test]
fn fingerprint_by_peer_id_prefix_chokes_seeding_tasks_without_banning() {
    let (g, _c) = guard_with(enforce_config());
    apply_rules(&g, vec![xl_rule(true, "choke")]);
    let ctx = ctx_for(PEER, true);
    assert_eq!(g.on_event_at(&ctx, handshake(&XL_ID), T0), Verdict::Choke);
    // 弱信号：不写封禁条目、不 strike。
    assert_eq!(auto_entries(&g), 0);
    let group: IpGroup = PEER.parse().expect("group");
    assert_eq!(g.strikes_of(&group), 0);
    assert!(g.admit(PEER.parse().expect("ip")));
    let insight = g.peer_insight(&ctx.info_hash, ctx.addr).expect("insight");
    assert_eq!(insight.suspicion, "weak");
    assert_eq!(insight.rules, vec!["fingerprint"]);
    assert_eq!(insight.action, "choke");
    assert!(!insight.observe_only);
}

#[test]
fn fingerprint_seeding_only_rule_skips_downloading_tasks() {
    let (g, _c) = guard_with(enforce_config());
    apply_rules(&g, vec![xl_rule(true, "choke")]);
    let ctx = ctx_for(PEER, false);
    assert_eq!(g.on_event_at(&ctx, handshake(&XL_ID), T0), Verdict::Allow);
}

#[test]
fn fingerprint_downloading_is_throttled_not_choked() {
    let (g, _c) = guard_with(enforce_config());
    apply_rules(&g, vec![xl_rule(false, "choke")]);
    let ctx = ctx_for(PEER, false);
    assert_eq!(
        g.on_event_at(&ctx, handshake(&XL_ID), T0),
        Verdict::Throttle
    );
}

#[test]
fn fingerprint_matches_client_name_contains_and_prefix() {
    let (g, _c) = guard_with(enforce_config());
    let mut contains = xl_rule(false, "throttle");
    contains.id = "name-contains".to_owned();
    contains.field = "clientName".to_owned();
    contains.op = "contains".to_owned();
    contains.value = "Xunlei".to_owned();
    apply_rules(&g, vec![contains]);
    let ctx = ctx_for(PEER, true);
    assert_eq!(
        g.on_event_at(
            &ctx,
            GuardEvent::ExtendedHandshake {
                client: Some("Xunlei 0.0.1.9")
            },
            T0
        ),
        Verdict::Throttle
    );
    // prefix 与 contains 不同：中间出现不算。
    let (g, _c) = guard_with(enforce_config());
    let mut prefix = xl_rule(false, "throttle");
    prefix.field = "clientName".to_owned();
    prefix.value = "Xunlei".to_owned();
    apply_rules(&g, vec![prefix]);
    let ctx = ctx_for(PEER, true);
    assert_eq!(
        g.on_event_at(
            &ctx,
            GuardEvent::ExtendedHandshake {
                client: Some("My Xunlei")
            },
            T0
        ),
        Verdict::Allow
    );
    assert_eq!(
        g.on_event_at(
            &ctx,
            GuardEvent::ExtendedHandshake {
                client: Some("Xunlei 1")
            },
            T0
        ),
        Verdict::Throttle
    );
    // 没有 `v` 字段：不匹配、不崩。
    let ctx2 = ctx_for("5.6.7.8", true);
    assert_eq!(
        g.on_event_at(&ctx2, GuardEvent::ExtendedHandshake { client: None }, T0),
        Verdict::Allow
    );
}

#[test]
fn fingerprint_inactive_without_cloud_or_when_stale_or_cloud_disabled() {
    // 从未应用云名单。
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    assert_eq!(g.on_event_at(&ctx, handshake(&XL_ID), T0), Verdict::Allow);
    // 文档过期（stale）后停用。
    let (g, c) = guard_with(enforce_config());
    apply_rules(&g, vec![xl_rule(false, "choke")]);
    c.set(T0 + DAY + 1);
    assert_eq!(
        g.on_event_at(&ctx, handshake(&XL_ID), T0 + DAY + 1),
        Verdict::Allow
    );
    // cloud_enabled = false。
    let mut cfg = enforce_config();
    cfg.cloud_enabled = false;
    let (g, _c) = guard_with(cfg);
    apply_rules(&g, vec![xl_rule(false, "choke")]);
    assert_eq!(g.on_event_at(&ctx, handshake(&XL_ID), T0), Verdict::Allow);
}

#[test]
fn fingerprint_ignores_non_printable_and_non_matching_ids() {
    let (g, _c) = guard_with(enforce_config());
    apply_rules(&g, vec![xl_rule(false, "choke")]);
    let ctx = ctx_for(PEER, true);
    assert_eq!(
        g.on_event_at(&ctx, handshake(b"-qB4650-abcdefghijkl"), T0),
        Verdict::Allow
    );
}

// -------------------------------------------------------------- observe / off

#[test]
fn observe_mode_never_changes_the_verdict_and_creates_no_entries() {
    let (g, _c) = guard_with(BtGuardConfig::default());
    // excessive
    assert_eq!(deliver_total(&g, "1.1.1.9", 250, T0), Verdict::Allow);
    // rerequest
    let ctx = ctx_for("2.2.2.2", true);
    g.on_event_at(&ctx, ev_request(1, true), T0);
    assert_eq!(g.on_event_at(&ctx, ev_request(2, true), T0), Verdict::Allow);
    // rewind
    let seeded = ctx_for("3.3.3.3", true);
    for p in 0..100 {
        deliver_piece(&g, &seeded, p, T0);
    }
    disconnect(&g, &seeded, Some(&bits(0..100)), T0);
    let b = bits(50..100);
    assert_eq!(
        g.on_event_at(&seeded, GuardEvent::Bitfield { bits: &b }, T0 + 5),
        Verdict::Allow
    );
    assert_eq!(auto_entries(&g), 0);
    assert!(g.admit("1.1.1.9".parse().expect("ip")));
    // 三条规则各记一次「本应执行」。
    assert_eq!(g.stats().observed, 3);
    assert_eq!(g.stats().enforced, 0);
    assert_eq!(g.stats().refused_bytes, 0);
}

#[test]
fn observe_hit_is_flagged_observe_only_in_insight() {
    let (g, _c) = guard_with(BtGuardConfig::default());
    let ctx = ctx_for(PEER, true);
    g.on_event_at(&ctx, ev_request(1, true), T0);
    g.on_event_at(&ctx, ev_request(2, true), T0);
    let insight = g.peer_insight(&ctx.info_hash, ctx.addr).expect("insight");
    assert_eq!(insight.suspicion, "strong");
    assert_eq!(insight.rules, vec!["rerequest"]);
    assert!(insight.observe_only);
    assert_eq!(insight.action, "allow");
}

#[test]
fn observe_records_are_deduplicated_per_group_and_rule() {
    let (g, c) = guard_with(BtGuardConfig::default());
    for conn in 0..5u16 {
        // 同一组多次重连、每次都命中 rerequest：窗口内只记一次。
        let mut ctx = ctx_for(PEER, true);
        ctx.addr.set_port(7000 + conn);
        g.on_event_at(&ctx, ev_request(1, true), T0);
        g.on_event_at(&ctx, ev_request(2, true), T0);
    }
    assert_eq!(g.stats().observed, 1);
    // 窗口（1h）过后再记一次。
    c.set(T0 + 3601);
    let mut ctx = ctx_for(PEER, true);
    ctx.addr.set_port(7100);
    g.on_event_at(&ctx, ev_request(1, true), T0 + 3601);
    g.on_event_at(&ctx, ev_request(2, true), T0 + 3601);
    assert_eq!(g.stats().observed, 2);
}

#[test]
fn rule_mode_off_disables_detection() {
    let mut cfg = enforce_config();
    cfg.rule_rerequest = RuleMode::Off;
    cfg.rule_excessive = RuleMode::Off;
    let (g, _c) = guard_with(cfg);
    let ctx = ctx_for(PEER, true);
    g.on_event_at(&ctx, ev_request(1, true), T0);
    assert_eq!(g.on_event_at(&ctx, ev_request(2, true), T0), Verdict::Allow);
    assert_eq!(deliver_total(&g, "9.9.9.9", 300, T0), Verdict::Allow);
    assert_eq!(g.stats().observed, 0);
    assert_eq!(g.stats().enforced, 0);
}

#[test]
fn disabled_guard_allows_everything_even_with_ban_entries() {
    let mut cfg = enforce_config();
    cfg.enabled = false;
    let (g, _c) = guard_with(cfg);
    g.add_entry(NewEntry {
        cidr: "1.2.3.4".to_owned(),
        action: FilterAction::Ban,
        duration_secs: None,
        reason: String::new(),
    })
    .expect("add");
    assert!(g.admit(PEER.parse().expect("ip")));
    assert!(!g.is_banned(PEER.parse().expect("ip")));
    assert_eq!(deliver_total(&g, PEER, 300, T0), Verdict::Allow);
}

// ----------------------------------------------------------- private / LAN / allow

#[test]
fn private_torrents_are_skipped_by_default() {
    let (g, _c) = guard_with(enforce_config());
    let mut ctx = ctx_for(PEER, true);
    ctx.private = true;
    for i in 0..300u32 {
        assert_eq!(deliver_piece(&g, &ctx, i % 100, T0), Verdict::Allow);
    }
    assert_eq!(auto_entries(&g), 0);
    // 私有种子也不建账本。
    let group: IpGroup = PEER.parse().expect("group");
    assert_eq!(g.group_delivered_bytes(&ctx.info_hash, &group), None);
}

#[test]
fn private_detection_can_be_enabled() {
    let mut cfg = enforce_config();
    cfg.skip_private = false;
    let (g, _c) = guard_with(cfg);
    let mut ctx = ctx_for(PEER, true);
    ctx.private = true;
    let mut verdict = Verdict::Allow;
    for i in 0..300u32 {
        verdict = deliver_piece(&g, &ctx, i % 100, T0);
        if verdict != Verdict::Allow {
            break;
        }
    }
    assert_eq!(verdict, Verdict::Disconnect);
}

#[test]
fn lan_peers_are_exempt_unless_skip_lan_is_off() {
    let (g, _c) = guard_with(enforce_config());
    for lan in [
        "192.168.1.5",
        "10.0.0.9",
        "127.0.0.1",
        "fe80::1",
        "100.64.1.1",
    ] {
        assert_eq!(deliver_total(&g, lan, 300, T0), Verdict::Allow, "{lan}");
        assert!(g.admit(lan.parse().expect("ip")));
    }
    assert_eq!(auto_entries(&g), 0);
    let mut cfg = enforce_config();
    cfg.skip_lan = false;
    let (g, _c) = guard_with(cfg);
    assert_eq!(
        deliver_total(&g, "192.168.1.5", 300, T0),
        Verdict::Disconnect
    );
}

#[test]
fn lan_peers_are_exempt_from_manual_bans_too() {
    let (g, _c) = guard_with(enforce_config());
    g.add_entry(NewEntry {
        cidr: "192.168.0.0/16".to_owned(),
        action: FilterAction::Ban,
        duration_secs: None,
        reason: String::new(),
    })
    .expect("add");
    assert!(g.admit("192.168.1.1".parse().expect("ip")));
    assert!(!g.is_banned("192.168.1.1".parse().expect("ip")));
}

#[test]
fn allow_layer_exempts_from_detection() {
    let (g, _c) = guard_with(enforce_config());
    let entry = g
        .add_entry(NewEntry {
            cidr: PEER.to_owned(),
            action: FilterAction::Allow,
            duration_secs: None,
            reason: "friend".to_owned(),
        })
        .expect("add");
    assert_eq!(entry.layer, Layer::Allow);
    assert_eq!(deliver_total(&g, PEER, 300, T0), Verdict::Allow);
    assert_eq!(auto_entries(&g), 0);
    // 移除白名单后恢复检测。
    g.remove_entries(&[entry.id]).expect("remove");
    let (g2, _c) = guard_with(enforce_config());
    assert_eq!(deliver_total(&g2, PEER, 300, T0), Verdict::Disconnect);
}

// -------------------------------------------------------------- ip grouping

#[test]
fn ledger_is_shared_across_an_ipv6_group_and_ban_covers_the_group() {
    let (g, _c) = guard_with(enforce_config());
    let a = ctx_for("2001:db8:1:2::1", true);
    let b = ctx_for("2001:db8:1:2::2", true);
    assert_eq!(
        g.on_event_at(&a, ev_upload(0, 150 * 1024 * 1024), T0),
        Verdict::Allow
    );
    // 同 /64 的另一个地址继续送：账本合计 210 MiB > 200 MiB。
    assert_eq!(
        g.on_event_at(&b, ev_upload(1, 60 * 1024 * 1024), T0),
        Verdict::Disconnect
    );
    assert!(!g.admit("2001:db8:1:2::99".parse().expect("ip")));
    assert!(
        g.admit("2001:db8:1:3::1".parse().expect("ip")),
        "other /64 unaffected"
    );
    assert!(
        g.admit("1.2.3.4".parse().expect("ip")),
        "v4 is a separate group"
    );
    let (_, entries) = g.list_entries(&EntryFilter {
        layer: Some(Layer::Auto),
        limit: 10,
        ..EntryFilter::default()
    });
    assert_eq!(entries[0].cidr(), "2001:db8:1:2::/64");
}

#[test]
fn v4_mapped_peers_share_the_v4_group() {
    let (g, _c) = guard_with(enforce_config());
    let mapped = ctx_for("::ffff:1.2.3.4", true);
    let plain = ctx_for("1.2.3.4", true);
    assert_eq!(
        g.on_event_at(&mapped, ev_upload(0, 150 * 1024 * 1024), T0),
        Verdict::Allow
    );
    assert_eq!(
        g.on_event_at(&plain, ev_upload(1, 60 * 1024 * 1024), T0),
        Verdict::Disconnect
    );
    let group: IpGroup = "1.2.3.4".parse().expect("group");
    assert_eq!(
        g.group_delivered_bytes(&plain.info_hash, &group),
        Some(210 * MIB)
    );
    assert!(!g.admit("::ffff:1.2.3.4".parse().expect("ip")));
}

#[test]
fn ledgers_are_per_torrent() {
    let (g, _c) = guard_with(enforce_config());
    let a = ctx_for(PEER, true);
    let mut b = ctx_for(PEER, true);
    b.info_hash = hash(2);
    assert_eq!(
        g.on_event_at(&a, ev_upload(0, 150 * 1024 * 1024), T0),
        Verdict::Allow
    );
    assert_eq!(
        g.on_event_at(&b, ev_upload(0, 150 * 1024 * 1024), T0),
        Verdict::Allow
    );
}

// ----------------------------------------------------------- downloading cap

fn rerequest_hit(g: &BtGuard, seeding: bool) -> Verdict {
    let ctx = ctx_for(PEER, seeding);
    g.on_event_at(&ctx, ev_request(1, true), T0);
    g.on_event_at(&ctx, ev_request(2, true), T0)
}

#[test]
fn downloading_tasks_are_capped_by_downloading_max_action() {
    let group: IpGroup = PEER.parse().expect("group");
    let cases = [
        (DownloadingMaxAction::Throttle, Verdict::Throttle, 0),
        (DownloadingMaxAction::Choke, Verdict::Choke, 0),
        (DownloadingMaxAction::Observe, Verdict::Allow, 0),
        (DownloadingMaxAction::Ban, Verdict::Disconnect, 1),
    ];
    for (cap, verdict, entries) in cases {
        let mut cfg = enforce_config();
        cfg.downloading_max_action = cap;
        let (g, _c) = guard_with(cfg);
        assert_eq!(rerequest_hit(&g, false), verdict, "{cap:?}");
        assert_eq!(auto_entries(&g), entries, "{cap:?}");
        // 被封顶的动作不产生 strike；真封禁才有。
        assert_eq!(
            g.strikes_of(&group),
            u32::try_from(entries).expect("small"),
            "{cap:?}"
        );
    }
}

#[test]
fn downloading_cap_to_observe_counts_as_observation() {
    let mut cfg = enforce_config();
    cfg.downloading_max_action = DownloadingMaxAction::Observe;
    let (g, _c) = guard_with(cfg);
    assert_eq!(rerequest_hit(&g, false), Verdict::Allow);
    assert_eq!(g.stats().observed, 1);
    assert_eq!(g.stats().enforced, 0);
}

#[test]
fn seeding_tasks_ignore_the_downloading_cap() {
    let mut cfg = enforce_config();
    cfg.downloading_max_action = DownloadingMaxAction::Observe;
    let (g, _c) = guard_with(cfg);
    assert_eq!(rerequest_hit(&g, true), Verdict::Disconnect);
    assert_eq!(auto_entries(&g), 1);
}

// ------------------------------------------------------------------ misc

#[test]
fn passive_events_leave_nothing_to_persist() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    g.on_event_at(&ctx, handshake(&XL_ID), T0);
    g.on_event_at(
        &ctx,
        GuardEvent::ExtendedHandshake {
            client: Some("qBittorrent"),
        },
        T0,
    );
    let b = bits(0..10);
    g.on_event_at(&ctx, GuardEvent::Bitfield { bits: &b }, T0);
    g.on_event_at(&ctx, GuardEvent::Have { piece: 11 }, T0);
    g.on_event_at(&ctx, ev_request(30, false), T0);
    g.on_event_at(
        &ctx,
        GuardEvent::Cancel {
            piece: 30,
            begin: 0,
            length: 16384,
        },
        T0,
    );
    disconnect(&g, &ctx, Some(&b), T0);
    assert!(
        !g.needs_flush(),
        "idle observation must not schedule writes"
    );
    assert!(g.take_actions().is_empty());
}

#[test]
fn uploads_mark_the_ledger_dirty_but_only_flush_all_writes_it() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    deliver_piece(&g, &ctx, 0, T0);
    assert!(
        !g.needs_flush(),
        "upload accumulation alone must not trigger a flush"
    );
    assert!(g.needs_flush_all(), "but the dirty ledger is not forgotten");
}

#[test]
fn ban_queues_disconnect_and_changed_actions() {
    let (g, _c) = guard_with(enforce_config());
    assert_eq!(rerequest_hit(&g, true), Verdict::Disconnect);
    let actions = g.take_actions();
    assert!(actions.contains(&GuardAction::DisconnectBanned));
    assert!(actions.contains(&GuardAction::Changed));
    assert!(g.take_actions().is_empty(), "drained");
}

// ------------------------------------------------------------------ waker

fn woken(n: &tokio::sync::Notify) -> bool {
    use futures_util::FutureExt;
    n.notified().now_or_never().is_some()
}

#[test]
fn waker_fires_on_new_work_but_not_on_per_block_ledger_accounting() {
    let (g, _c) = guard_with(enforce_config());
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    g.set_waker(std::sync::Arc::clone(&notify));
    let ctx = ctx_for(PEER, true);
    // 持续上传：只累加账本，不唤醒（空闲静默 / 落库节奏）。
    for p in 0..20 {
        deliver_piece(&g, &ctx, p, T0);
    }
    assert!(!g.needs_flush());
    assert!(g.needs_flush_all());
    assert!(
        !woken(&notify),
        "ledger accumulation must not wake the host"
    );
    // Disconnected 是落库驱动点。
    disconnect(&g, &ctx, None, T0);
    assert!(woken(&notify));
    assert!(!woken(&notify), "permit is coalesced and consumed");
    // 处置（封禁）→ 新 action + 事件。
    let other = ctx_for("5.6.7.8", true);
    g.on_event_at(&other, ev_request(1, true), T0);
    assert!(!woken(&notify), "a single claimed request is not a hit");
    g.on_event_at(&other, ev_request(2, true), T0);
    assert!(woken(&notify));
    // 管理操作：取走 action 后再次新增会再次唤醒。
    g.take_actions();
    assert!(!woken(&notify));
    manual_ban_for_wake(&g);
    assert!(woken(&notify));
    // 被动事件不唤醒。
    let quiet = ctx_for("9.9.9.9", true);
    g.on_event_at(&quiet, ev_request(30, false), T0);
    g.on_event_at(&quiet, GuardEvent::Have { piece: 3 }, T0);
    assert!(!woken(&notify));
}

fn manual_ban_for_wake(g: &BtGuard) {
    g.add_entry(NewEntry {
        cidr: "6.6.6.6".to_owned(),
        action: FilterAction::Ban,
        duration_secs: None,
        reason: String::new(),
    })
    .expect("add");
}

#[test]
fn no_waker_means_no_wake_and_waker_can_be_replaced() {
    let (g, _c) = guard_with(enforce_config());
    manual_ban_for_wake(&g); // 未设置 waker：不崩
    let first = std::sync::Arc::new(tokio::sync::Notify::new());
    let second = std::sync::Arc::new(tokio::sync::Notify::new());
    g.set_waker(std::sync::Arc::clone(&first));
    g.set_waker(std::sync::Arc::clone(&second));
    g.take_actions();
    g.add_entry(NewEntry {
        cidr: "6.6.6.7".to_owned(),
        action: FilterAction::Allow,
        duration_secs: None,
        reason: String::new(),
    })
    .expect("add");
    assert!(woken(&second));
    assert!(!woken(&first), "replaced waker is no longer notified");
}

#[test]
fn clear_connections_drops_connection_state_only() {
    let (g, _c) = guard_with(enforce_config());
    let ctx = ctx_for(PEER, true);
    deliver_piece(&g, &ctx, 0, T0);
    manual_ban_for_wake(&g);
    assert!(g.peer_insight(&ctx.info_hash, ctx.addr).is_some());
    g.clear_connections();
    assert!(g.peer_insight(&ctx.info_hash, ctx.addr).is_none());
    let group: IpGroup = PEER.parse().expect("group");
    assert!(
        g.group_delivered_bytes(&ctx.info_hash, &group).is_some(),
        "ledger kept"
    );
    assert_eq!(g.counts().manual, 1, "entries kept");
}
