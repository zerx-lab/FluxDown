use std::net::IpAddr;

use fluxdown_engine::bt_guard::{IpGroup, is_lan, normalize_ip};

fn ip(s: &str) -> IpAddr {
    s.parse().expect("ip")
}

#[test]
fn v4_mapped_is_normalized_to_v4() {
    assert_eq!(normalize_ip(ip("::ffff:1.2.3.4")), ip("1.2.3.4"));
    assert_eq!(normalize_ip(ip("1.2.3.4")), ip("1.2.3.4"));
}

#[test]
fn nat64_embedded_v4_is_normalized() {
    assert_eq!(normalize_ip(ip("64:ff9b::102:304")), ip("1.2.3.4"));
    // 仅 /96 前缀内才归一：其它 64:ff9b:: 地址保持 v6。
    assert_eq!(
        normalize_ip(ip("64:ff9b:1::102:304")),
        ip("64:ff9b:1::102:304")
    );
    assert_eq!(normalize_ip(ip("2001:db8::1")), ip("2001:db8::1"));
}

#[test]
fn mapped_and_native_v4_share_one_group() {
    let a = IpGroup::of(ip("::ffff:1.2.3.4"), 64);
    let b = IpGroup::of(ip("1.2.3.4"), 64);
    let c = IpGroup::of(ip("64:ff9b::102:304"), 64);
    assert_eq!(a, b);
    assert_eq!(b, c);
    assert_eq!(b.to_string(), "1.2.3.4");
    assert_eq!(b.cidr_text(), "1.2.3.4/32");
}

#[test]
fn v6_aggregates_by_prefix_and_dual_stack_stays_separate() {
    let a = IpGroup::of(ip("2001:db8:1:2:aaaa::1"), 64);
    let b = IpGroup::of(ip("2001:db8:1:2:bbbb::2"), 64);
    let other = IpGroup::of(ip("2001:db8:1:3::1"), 64);
    assert_eq!(a, b);
    assert_ne!(a, other);
    assert_eq!(a.to_string(), "2001:db8:1:2::/64");
    // 双栈：同一主机的 v4 与 v6 是两个组。
    assert_ne!(IpGroup::of(ip("1.2.3.4"), 64), a);
}

#[test]
fn v6_prefix_is_configurable_and_clamped() {
    let a = IpGroup::of(ip("2001:db8:1:2:aaaa::1"), 56);
    let b = IpGroup::of(ip("2001:db8:1:3:bbbb::1"), 56);
    assert_eq!(a, b, "same /56");
    // 低于 48 钳制到 48，高于 128 钳制到 128。
    assert_eq!(
        IpGroup::of(ip("2001:db8:1:2::1"), 8).to_string(),
        "2001:db8:1::/48"
    );
    assert_eq!(
        IpGroup::of(ip("2001:db8::1"), 200).to_string(),
        "2001:db8::1/128"
    );
}

#[test]
fn group_text_round_trips() {
    for text in ["1.2.3.4", "2001:db8:1:2::/64", "2001:db8::1/128"] {
        let g: IpGroup = text.parse().expect("parse");
        assert_eq!(g.to_string(), text);
    }
    assert!("garbage".parse::<IpGroup>().is_err());
    assert!("1.2.3.4/24".parse::<IpGroup>().is_err());
    // 非规范主机位被掩掉。
    let g: IpGroup = "2001:db8:1:2:ffff::1/64".parse().expect("parse");
    assert_eq!(g.to_string(), "2001:db8:1:2::/64");
}

#[test]
fn group_range_covers_exactly_the_group() {
    let g = IpGroup::of(ip("2001:db8:1:2:aaaa::1"), 64);
    let range = g.range();
    assert!(range.contains(ip("2001:db8:1:2:ffff:ffff:ffff:ffff")));
    assert!(range.contains(ip("2001:db8:1:2::")));
    assert!(!range.contains(ip("2001:db8:1:3::")));
    assert!(!range.contains(ip("2001:db8:1:1:ffff:ffff:ffff:ffff")));
    assert_eq!(range.cidr_string().as_deref(), Some("2001:db8:1:2::/64"));
}

#[test]
fn lan_exemption_table() {
    for lan in [
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.1",
        "127.0.0.1",
        "169.254.10.10",
        "100.64.0.1",
        "100.127.255.255",
        "0.1.2.3",
        "::1",
        "fe80::1",
        "febf::1",
        "fc00::1",
        "fd12:3456::1",
        "::ffff:192.168.1.1",
        "64:ff9b::a00:1",
        "::",
    ] {
        assert!(is_lan(ip(lan)), "{lan} should be LAN");
    }
    for public in [
        "8.8.8.8",
        "172.15.255.255",
        "172.32.0.1",
        "100.63.255.255",
        "100.128.0.1",
        "1.2.3.4",
        "2001:db8::1",
        "2606:4700:4700::1111",
        "fec0::1",
        "::ffff:8.8.8.8",
    ] {
        assert!(!is_lan(ip(public)), "{public} should not be LAN");
    }
}
