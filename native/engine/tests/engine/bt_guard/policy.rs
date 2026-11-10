//! `GuardPolicy`（librqbit `PeerPolicy` 适配器）：rqbit 回调与 guard 之间的一对一转换。
//!
//! 位图（`Bitfield` / `Disconnected`）需要 rqbit 自己构造的 `BitSlice`，由 `e2e.rs` 的真实连接覆盖。

use std::marker::PhantomData;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use fluxdown_engine::bt_guard::{
    BtGuard, BtGuardConfig, FilterAction, GuardPolicy, NewEntry, RuleMode,
};
use librqbit::dht::Id20;
use librqbit::{PeerCtx, PeerEvent, PeerPolicy, PeerVerdict};

const MIB: u64 = 1024 * 1024;

fn enforce_all() -> BtGuardConfig {
    BtGuardConfig {
        skip_lan: false,
        min_torrent_mib: 0,
        rule_excessive: RuleMode::Enforce,
        rule_rerequest: RuleMode::Enforce,
        rule_rewind: RuleMode::Enforce,
        rule_lag: RuleMode::Enforce,
        rule_fingerprint: RuleMode::Off,
        cloud_mode: RuleMode::Off,
        ..BtGuardConfig::default()
    }
}

fn policy(config: BtGuardConfig) -> (GuardPolicy, Arc<BtGuard>) {
    let guard = Arc::new(BtGuard::new(config));
    (GuardPolicy::new(Arc::clone(&guard)), guard)
}

fn socket(ip: &str) -> SocketAddr {
    SocketAddr::new(ip.parse::<IpAddr>().expect("ip"), 6881)
}

/// 1 片、1 MiB 的种子：一片送达即一份完整副本。
fn ctx(addr: SocketAddr, private: bool, seeding: bool) -> PeerCtx<'static> {
    PeerCtx {
        info_hash: Id20::new([7; 20]),
        addr,
        private,
        total_length: MIB,
        piece_length: u32::try_from(MIB).expect("piece"),
        num_pieces: 1,
        we_are_seeding: seeding,
        _p: PhantomData,
    }
}

fn upload_full_piece() -> PeerEvent<'static> {
    PeerEvent::Uploaded {
        piece: 0,
        begin: 0,
        length: u32::try_from(MIB).expect("len"),
    }
}

/// 反复送达整份种子：返回第一次非 `Allow` 的判决。
fn pour(policy: &GuardPolicy, ctx: &PeerCtx<'_>) -> PeerVerdict {
    for _ in 0..4 {
        let verdict = policy.on_event(ctx, upload_full_piece());
        if verdict != PeerVerdict::Allow {
            return verdict;
        }
    }
    PeerVerdict::Allow
}

#[test]
fn we_are_seeding_decides_between_ban_and_the_downloading_cap() {
    let (policy, _) = policy(enforce_all());
    let ip = "203.0.113.5";
    // 做种：强信号直接断开。
    assert_eq!(
        pour(&policy, &ctx(socket(ip), false, true)),
        PeerVerdict::Disconnect
    );
    // 下载中：默认封顶 throttle，不得断开。
    let (policy, _) = self::policy(enforce_all());
    assert_eq!(
        pour(&policy, &ctx(socket(ip), false, false)),
        PeerVerdict::Throttle
    );
}

#[test]
fn private_flag_reaches_the_guard() {
    let skipped = policy(enforce_all()).0;
    assert_eq!(
        pour(&skipped, &ctx(socket("203.0.113.6"), true, true)),
        PeerVerdict::Allow,
        "private torrents are skipped by default"
    );
    let detected = policy(BtGuardConfig {
        skip_private: false,
        ..enforce_all()
    })
    .0;
    assert_eq!(
        pour(&detected, &ctx(socket("203.0.113.6"), true, true)),
        PeerVerdict::Disconnect
    );
}

#[test]
fn torrent_size_and_info_hash_are_reported_in_evidence() {
    let (policy, guard) = policy(BtGuardConfig {
        report_enabled: true,
        ..enforce_all()
    });
    pour(&policy, &ctx(socket("203.0.113.7"), false, true));
    let evidence = guard.drain_evidence();
    assert_eq!(evidence.len(), 1);
    let item = &evidence[0];
    assert_eq!(item.rule, "excessive");
    assert_eq!(item.info_hash, hex::encode([7u8; 20]));
    assert_eq!(item.torrent_size, MIB);
    assert_eq!(item.ip_group, "203.0.113.7");
    assert!(item.delivered_bytes > 2 * MIB);
}

#[test]
fn handshake_and_extended_handshake_fields_reach_the_evidence() {
    let (policy, guard) = policy(BtGuardConfig {
        report_enabled: true,
        ..enforce_all()
    });
    let ctx = ctx(socket("203.0.113.8"), false, true);
    let mut peer_id = [b'x'; 20];
    peer_id[..8].copy_from_slice(b"-XL0019-");
    assert_eq!(
        policy.on_event(
            &ctx,
            PeerEvent::Handshake {
                peer_id: Id20::new(peer_id),
                reserved: 1 << 20,
                incoming: true,
            }
        ),
        PeerVerdict::Allow
    );
    assert_eq!(
        policy.on_event(
            &ctx,
            PeerEvent::ExtendedHandshake {
                client: Some("Xunlei 0.0.1.9")
            }
        ),
        PeerVerdict::Allow
    );
    assert_eq!(pour(&policy, &ctx), PeerVerdict::Disconnect);
    let evidence = guard.drain_evidence();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].peer_id_prefix, "-XL0019-");
    assert_eq!(evidence[0].client, "Xunlei 0.0.1.9");
}

#[test]
fn request_fields_and_claims_flag_are_forwarded() {
    let (policy, _) = policy(enforce_all());
    let mut ctx = ctx(socket("203.0.113.9"), false, true);
    ctx.num_pieces = 8;
    ctx.piece_length = 128 * 1024;
    let request = |piece, claims| PeerEvent::Request {
        piece,
        begin: 0,
        length: 16 * 1024,
        peer_claims_piece: claims,
    };
    // 同一片重复请求只算一片；未声明的片不计。
    assert_eq!(policy.on_event(&ctx, request(0, true)), PeerVerdict::Allow);
    assert_eq!(policy.on_event(&ctx, request(0, true)), PeerVerdict::Allow);
    assert_eq!(policy.on_event(&ctx, request(1, false)), PeerVerdict::Allow);
    assert_eq!(policy.on_event(&ctx, request(3, false)), PeerVerdict::Allow);
    // 第二个不同的已声明分片 → rerequest。
    assert_eq!(
        policy.on_event(&ctx, request(2, true)),
        PeerVerdict::Disconnect
    );
}

#[test]
fn v4_mapped_peer_is_tracked_as_its_ipv4_group() {
    let (policy, guard) = policy(enforce_all());
    let mapped = SocketAddr::new(
        IpAddr::V6("::ffff:203.0.113.10".parse::<Ipv6Addr>().expect("v6")),
        6881,
    );
    let ctx = ctx(mapped, false, true);
    assert_eq!(
        policy.on_event(
            &ctx,
            PeerEvent::Handshake {
                peer_id: Id20::new([b'a'; 20]),
                reserved: 0,
                incoming: false,
            }
        ),
        PeerVerdict::Allow
    );
    let plain = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)), 6881);
    let insight = guard
        .peer_insight(&[7; 20], plain)
        .expect("the mapped connection is addressed by its IPv4 form");
    assert_eq!(insight.ip_group, "203.0.113.10");
    // 映射形式查询同样命中。
    assert!(guard.peer_insight(&[7; 20], mapped).is_some());

    // 封禁 IPv4 后，v4-mapped 地址的 admit 同样被拒。
    guard
        .add_entry(NewEntry {
            cidr: "203.0.113.10/32".to_owned(),
            action: FilterAction::Ban,
            duration_secs: None,
            reason: String::new(),
        })
        .expect("ban");
    assert!(!policy.admit(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10))));
    assert!(!policy.admit(mapped.ip()));
    assert!(policy.admit(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 11))));
}

#[test]
fn table_verdicts_map_to_peer_verdicts() {
    let (policy, guard) = policy(enforce_all());
    for (cidr, action) in [
        ("203.0.113.20/32", FilterAction::Throttle),
        ("203.0.113.21/32", FilterAction::Choke),
        ("203.0.113.22/32", FilterAction::Ban),
    ] {
        guard
            .add_entry(NewEntry {
                cidr: cidr.to_owned(),
                action,
                duration_secs: None,
                reason: String::new(),
            })
            .expect("entry");
    }
    let handshake = |ip: &str| {
        policy.on_event(
            &ctx(socket(ip), false, true),
            PeerEvent::Handshake {
                peer_id: Id20::new([b'b'; 20]),
                reserved: 0,
                incoming: true,
            },
        )
    };
    assert_eq!(handshake("203.0.113.20"), PeerVerdict::Throttle);
    assert_eq!(handshake("203.0.113.21"), PeerVerdict::Choke);
    assert_eq!(handshake("203.0.113.22"), PeerVerdict::Disconnect);
    assert_eq!(handshake("203.0.113.23"), PeerVerdict::Allow);
}

#[test]
fn admit_counts_blocked_connections_and_allow_list_wins() {
    let (policy, guard) = policy(enforce_all());
    let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 30));
    let add = |action| {
        guard
            .add_entry(NewEntry {
                cidr: "203.0.113.30/32".to_owned(),
                action,
                duration_secs: None,
                reason: String::new(),
            })
            .expect("entry")
    };
    add(FilterAction::Ban);
    assert!(!policy.admit(ip));
    assert!(!policy.admit(ip));
    assert_eq!(guard.stats().blocked_connections, 2);
    add(FilterAction::Allow);
    assert!(policy.admit(ip), "allow layer outranks the manual ban");
    assert_eq!(guard.stats().blocked_connections, 2);
}

#[test]
fn disabled_guard_is_a_no_op_policy() {
    let (policy, guard) = policy(BtGuardConfig {
        enabled: false,
        ..enforce_all()
    });
    guard
        .add_entry(NewEntry {
            cidr: "203.0.113.40/32".to_owned(),
            action: FilterAction::Ban,
            duration_secs: None,
            reason: String::new(),
        })
        .expect("entry");
    assert!(policy.admit(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 40))));
    assert_eq!(
        pour(&policy, &ctx(socket("203.0.113.40"), false, true)),
        PeerVerdict::Allow
    );
    assert!(
        guard
            .peer_insight(&[7; 20], socket("203.0.113.40"))
            .is_none()
    );
}

#[test]
fn disconnected_forgets_the_connection_and_never_escalates() {
    let (policy, guard) = policy(enforce_all());
    let addr = socket("203.0.113.50");
    let ctx = ctx(addr, false, true);
    assert_eq!(
        policy.on_event(
            &ctx,
            PeerEvent::Handshake {
                peer_id: Id20::new([b'c'; 20]),
                reserved: 0,
                incoming: true,
            }
        ),
        PeerVerdict::Allow
    );
    assert!(guard.peer_insight(&[7; 20], addr).is_some());
    assert_eq!(
        policy.on_event(&ctx, PeerEvent::Disconnected { bits: None }),
        PeerVerdict::Allow
    );
    assert!(guard.peer_insight(&[7; 20], addr).is_none());
}

#[test]
fn live_connection_exposes_its_verdict_through_peer_insight() {
    let (policy, guard) = policy(BtGuardConfig {
        rule_excessive: RuleMode::Observe,
        ..enforce_all()
    });
    let addr = socket("203.0.113.60");
    let ctx = ctx(addr, false, true);
    pour(&policy, &ctx);
    let insight = guard.peer_insight(&[7; 20], addr).expect("insight");
    assert_eq!(insight.suspicion, "strong");
    assert_eq!(insight.rules, vec!["excessive"]);
    assert!(insight.observe_only, "observe mode never enforces");
    assert_eq!(insight.action, "allow");
    assert!(insight.group_delivered_bytes >= 2 * MIB);
}

#[test]
fn readmit_follows_the_ban_state_without_counting_blocked_connections() {
    let (policy, guard) = policy(enforce_all());
    let banned = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 70));
    let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5));
    guard
        .add_entry(NewEntry {
            cidr: "203.0.113.70/32".to_owned(),
            action: FilterAction::Ban,
            duration_secs: None,
            reason: String::new(),
        })
        .expect("ban");
    for _ in 0..5 {
        assert!(!policy.readmit(banned), "still banned");
    }
    assert_eq!(
        guard.stats().blocked_connections,
        0,
        "retries are not connection attempts"
    );
    assert!(!policy.admit(banned));
    assert_eq!(
        guard.stats().blocked_connections,
        1,
        "a real attempt counts"
    );
    assert!(policy.readmit(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 71))));

    // 白名单优先；LAN 在 skip_lan 开启时恒放行；防吸血关闭时一律放行。
    guard
        .add_entry(NewEntry {
            cidr: "203.0.113.70/32".to_owned(),
            action: FilterAction::Allow,
            duration_secs: None,
            reason: String::new(),
        })
        .expect("allow");
    assert!(policy.readmit(banned));
    let (lan_policy, lan_guard) = self::policy(BtGuardConfig {
        skip_lan: true,
        ..enforce_all()
    });
    lan_guard
        .add_entry(NewEntry {
            cidr: "192.168.1.0/24".to_owned(),
            action: FilterAction::Ban,
            duration_secs: None,
            reason: String::new(),
        })
        .expect("lan ban");
    assert!(lan_policy.readmit(lan));
    let (off_policy, off_guard) = self::policy(BtGuardConfig {
        enabled: false,
        ..enforce_all()
    });
    off_guard
        .add_entry(NewEntry {
            cidr: "203.0.113.70/32".to_owned(),
            action: FilterAction::Ban,
            duration_secs: None,
            reason: String::new(),
        })
        .expect("ban while disabled");
    assert!(off_policy.readmit(banned));
}
