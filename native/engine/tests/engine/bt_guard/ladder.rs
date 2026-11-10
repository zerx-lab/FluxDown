use std::net::IpAddr;

use fluxdown_engine::bt_guard::{
    BtGuard, EntryFilter, FilterAction, IpGroup, Layer, NewEntry, Verdict, ban_duration_secs,
};

use super::support::{DAY, HOUR, T0, ctx_for, enforce_config, ev_request, guard_with};

const PEER: &str = "1.2.3.4";

fn peer_ip() -> IpAddr {
    PEER.parse().expect("ip")
}

fn group() -> IpGroup {
    PEER.parse().expect("group")
}

/// 新连接（唯一端口）上触发 rerequest 强信号，返回第二个请求的判决。
fn strong_hit(g: &BtGuard, port: u16, now: i64) -> Verdict {
    let mut ctx = ctx_for(PEER, true);
    ctx.addr.set_port(port);
    g.on_event_at(&ctx, ev_request(1, true), now);
    g.on_event_at(&ctx, ev_request(2, true), now)
}

fn active_auto(g: &BtGuard) -> Vec<fluxdown_engine::bt_guard::Entry> {
    g.list_entries(&EntryFilter {
        layer: Some(Layer::Auto),
        limit: 100,
        ..EntryFilter::default()
    })
    .1
}

#[test]
fn ban_duration_follows_four_to_the_n_with_cap() {
    let h = |strikes| ban_duration_secs(24, 30, strikes) / HOUR;
    assert_eq!(h(1), 24);
    assert_eq!(h(2), 96);
    assert_eq!(h(3), 384);
    assert_eq!(h(4), 720, "1536h capped at 30 days");
    assert_eq!(h(5), 720);
    assert_eq!(
        h(1000),
        720,
        "huge strike counts saturate instead of overflowing"
    );
    assert_eq!(h(0), 24, "defensive: zero behaves like first strike");
    assert_eq!(ban_duration_secs(1, 365, 1), HOUR);
    assert_eq!(ban_duration_secs(720, 365, 3), 365 * 24 * HOUR);
    assert_eq!(
        ban_duration_secs(24, 1, 5),
        24 * HOUR,
        "cap can be below base"
    );
}

#[test]
fn strikes_escalate_across_bans_and_cap() {
    // 封顶 10 天：24h → 96h → 240h(封顶) → 240h …，且封顶后 strike 仍累加。
    let mut cfg = enforce_config();
    cfg.ban_max_days = 10;
    let (g, c) = guard_with(cfg);
    let mut now = T0;
    let mut expected_hours = [24i64, 96, 240, 240, 240].into_iter();
    for round in 1..=5u16 {
        c.set(now);
        assert_eq!(
            strong_hit(&g, 7000 + round, now),
            Verdict::Disconnect,
            "round {round}"
        );
        let entries = active_auto(&g);
        assert_eq!(entries.len(), 1, "round {round}");
        let hours = (entries[0].expires_at.expect("expiry") - now) / HOUR;
        assert_eq!(
            hours,
            expected_hours.next().expect("expected"),
            "round {round}"
        );
        assert_eq!(g.strikes_of(&group()), u32::from(round));
        // 封禁到期后再犯（仍在 30 天衰减期内）。
        now = entries[0].expires_at.expect("expiry") + 1;
    }
}

#[test]
fn default_cap_ban_expiry_then_immediate_reoffence_still_escalates_at_the_cap() {
    // 默认 ban_max_days=30：封顶（720h）封禁到期后立即再犯仍为封顶时长，不会被衰减归零。
    let (g, c) = guard_with(enforce_config());
    let mut now = T0;
    for (round, hours) in [(1u16, 24i64), (2, 96), (3, 384), (4, 720), (5, 720)] {
        c.set(now);
        assert_eq!(strong_hit(&g, 7200 + round, now), Verdict::Disconnect);
        let expiry = active_auto(&g)[0].expires_at.expect("expiry");
        assert_eq!((expiry - now) / HOUR, hours, "round {round}");
        assert_eq!(g.strikes_of(&group()), u32::from(round));
        now = expiry + 1;
    }
}

#[test]
fn strikes_decay_thirty_days_after_the_last_ban_expired() {
    let (g, c) = guard_with(enforce_config());
    assert_eq!(strong_hit(&g, 7001, T0), Verdict::Disconnect);
    let ban_end = T0 + 24 * HOUR;
    c.set(ban_end - 1);
    assert_eq!(g.strikes_of(&group()), 1);
    // 距首次 strike 30 天、但封禁结束才 29 天：仍计数。
    c.set(T0 + 30 * DAY);
    assert_eq!(
        g.strikes_of(&group()),
        1,
        "decay counts from the ban's expiry"
    );
    c.set(ban_end + 30 * DAY - 1);
    assert_eq!(g.strikes_of(&group()), 1);
    c.set(ban_end + 30 * DAY);
    assert_eq!(
        g.strikes_of(&group()),
        0,
        "30 days after the ban ended resets strikes"
    );
    // 衰减后再犯：回到首档（24h）。
    let again = ban_end + 30 * DAY + 5;
    c.set(again);
    assert_eq!(strong_hit(&g, 7002, again), Verdict::Disconnect);
    let entries = active_auto(&g);
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0].expires_at.expect("expiry") - again) / HOUR, 24);
    assert_eq!(g.strikes_of(&group()), 1);
}

#[test]
fn reoffence_just_before_decay_still_escalates() {
    let (g, c) = guard_with(enforce_config());
    assert_eq!(strong_hit(&g, 7001, T0), Verdict::Disconnect);
    let ban_end = T0 + 24 * HOUR;
    let later = ban_end + 30 * DAY - 1;
    c.set(later);
    assert_eq!(strong_hit(&g, 7002, later), Verdict::Disconnect);
    assert_eq!(g.strikes_of(&group()), 2);
    assert_eq!(
        (active_auto(&g)[0].expires_at.expect("expiry") - later) / HOUR,
        96
    );
}

#[test]
fn a_new_strike_within_the_window_does_not_decay() {
    let (g, _c) = guard_with(enforce_config());
    assert_eq!(strong_hit(&g, 7001, T0), Verdict::Disconnect);
    // 29 天（封禁早已到期）后再犯：strike 累加到 2 → 96h。
    let later = T0 + 29 * DAY;
    assert_eq!(strong_hit(&g, 7002, later), Verdict::Disconnect);
    let entries = active_auto(&g);
    assert_eq!((entries[0].expires_at.expect("expiry") - later) / HOUR, 96);
}

#[test]
fn ban_expiry_is_lazy_and_exact() {
    let (g, c) = guard_with(enforce_config());
    assert_eq!(strong_hit(&g, 7001, T0), Verdict::Disconnect);
    let expiry = T0 + 24 * HOUR;
    c.set(expiry - 1);
    assert!(!g.admit(peer_ip()));
    assert!(g.is_banned(peer_ip()));
    c.set(expiry);
    assert!(g.admit(peer_ip()), "expires_at is exclusive");
    assert!(!g.is_banned(peer_ip()));
    // 过期条目不再出现在列表 / 计数中。
    assert!(active_auto(&g).is_empty());
    assert_eq!(g.counts().auto, 0);
}

#[test]
fn no_extra_strike_while_the_group_is_already_banned() {
    let (g, _c) = guard_with(enforce_config());
    // 连接 B 先建立并发出第一个请求。
    let mut ctx_b = ctx_for(PEER, true);
    ctx_b.addr.set_port(7002);
    g.on_event_at(&ctx_b, ev_request(1, true), T0);
    // 连接 A 触发封禁。
    assert_eq!(strong_hit(&g, 7001, T0), Verdict::Disconnect);
    assert_eq!(g.strikes_of(&group()), 1);
    // 连接 B（封禁前已存在）随后也命中：被断开，但不追加 strike / 条目。
    assert_eq!(
        g.on_event_at(&ctx_b, ev_request(2, true), T0 + 1),
        Verdict::Disconnect
    );
    assert_eq!(g.strikes_of(&group()), 1);
    assert_eq!(active_auto(&g).len(), 1);
    // 封禁期内新连接（即使绕过 admit）同样不追加。
    assert_eq!(strong_hit(&g, 7003, T0 + 2), Verdict::Disconnect);
    assert_eq!(g.strikes_of(&group()), 1);
}

#[test]
fn ban_parameters_come_from_config() {
    let mut cfg = enforce_config();
    cfg.ban_base_hours = 2;
    cfg.ban_max_days = 1;
    let (g, _c) = guard_with(cfg);
    let mut now = T0;
    let mut hours = Vec::new();
    for round in 0..4u16 {
        assert_eq!(strong_hit(&g, 7100 + round, now), Verdict::Disconnect);
        let expiry = active_auto(&g)[0].expires_at.expect("expiry");
        hours.push((expiry - now) / HOUR);
        now = expiry + 1;
    }
    assert_eq!(hours, [2, 8, 24, 24]);
}

#[test]
fn out_of_range_config_is_clamped_not_trusted() {
    let mut cfg = enforce_config();
    cfg.ban_base_hours = 0;
    cfg.ban_max_days = 0;
    cfg.excess_ratio_pct = 0;
    let (g, _c) = guard_with(cfg);
    let c = g.config();
    assert_eq!(c.ban_base_hours, 1);
    assert_eq!(c.ban_max_days, 1);
    assert_eq!(c.excess_ratio_pct, 110);
}

#[test]
fn manual_bans_expire_lazily_without_timers() {
    let (g, c) = guard_with(enforce_config());
    g.add_entry(NewEntry {
        cidr: "9.9.9.0/24".to_owned(),
        action: FilterAction::Ban,
        duration_secs: Some(100),
        reason: "temp".to_owned(),
    })
    .expect("add");
    let ip: IpAddr = "9.9.9.9".parse().expect("ip");
    c.set(T0 + 99);
    assert!(!g.admit(ip));
    c.set(T0 + 100);
    assert!(g.admit(ip));
    assert_eq!(g.counts().manual, 0);
}

#[test]
fn flush_prunes_expired_entries_from_memory() {
    let (g, c) = guard_with(enforce_config());
    g.add_entry(NewEntry {
        cidr: "9.9.9.9".to_owned(),
        action: FilterAction::Ban,
        duration_secs: Some(10),
        reason: String::new(),
    })
    .expect("add");
    c.set(T0 + 11);
    assert_eq!(g.counts().manual, 0);
    // 内存中的过期条目在 flush 的准备阶段被剔除（无需 DB 也不 panic）。
    let (total, _) = g.list_entries(&EntryFilter {
        limit: 10,
        ..EntryFilter::default()
    });
    assert_eq!(total, 0);
}
