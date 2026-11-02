use std::net::IpAddr;

use fluxdown_engine::bt_guard::{IpRange, RangeIndex};

fn ip(s: &str) -> IpAddr {
    s.parse().expect("ip")
}

fn matches(index: &RangeIndex, addr: &str) -> Vec<u32> {
    let mut out = Vec::new();
    index.for_each_match(ip(addr), |p| out.push(p));
    out.sort_unstable();
    out
}

#[test]
fn cidr_parsing_masks_host_bits_and_accepts_single_ips() {
    let r = IpRange::parse_cidr("1.2.3.77/24").expect("cidr");
    assert_eq!(r.cidr_string().as_deref(), Some("1.2.3.0/24"));
    let single = IpRange::parse_cidr("1.2.3.4").expect("single");
    assert_eq!(single.cidr_string().as_deref(), Some("1.2.3.4/32"));
    let v6 = IpRange::parse_cidr("2001:db8::1:2/32").expect("v6");
    assert_eq!(v6.cidr_string().as_deref(), Some("2001:db8::/32"));
    let v6_single = IpRange::parse_cidr("2001:db8::5").expect("v6 single");
    assert_eq!(v6_single.cidr_string().as_deref(), Some("2001:db8::5/128"));
}

#[test]
fn mapped_and_nat64_cidrs_fold_to_v4() {
    let mapped = IpRange::parse_cidr("::ffff:1.2.3.0/120").expect("mapped");
    assert_eq!(mapped.cidr_string().as_deref(), Some("1.2.3.0/24"));
    let nat64 = IpRange::parse_cidr("64:ff9b::102:300/120").expect("nat64");
    assert_eq!(nat64.cidr_string().as_deref(), Some("1.2.3.0/24"));
    let single = IpRange::parse_cidr("::ffff:9.9.9.9").expect("mapped single");
    assert_eq!(single.cidr_string().as_deref(), Some("9.9.9.9/32"));
}

#[test]
fn invalid_cidrs_are_rejected() {
    for bad in [
        "",
        "abc",
        "1.2.3.4/33",
        "1.2.3.4/-1",
        "1.2.3.4/x",
        "2001:db8::/129",
        "1.2.3/24",
    ] {
        assert!(
            IpRange::parse_cidr(bad).is_err(),
            "{bad:?} must be rejected"
        );
    }
}

#[test]
fn wide_net_boundaries() {
    let wide = |c: &str| IpRange::parse_cidr(c).expect("cidr").is_wide();
    assert!(!wide("1.2.0.0/16"));
    assert!(!wide("1.2.3.4/32"));
    assert!(wide("1.2.0.0/15"));
    assert!(wide("0.0.0.0/0"));
    assert!(!wide("2001:db8::/32"));
    assert!(wide("2001:db8::/31"));
    assert!(wide("::/0"));
    // 非 CIDR 范围按跨度判定。
    let r = IpRange::from_addrs(ip("1.0.0.0"), ip("1.1.0.0")).expect("range");
    assert!(r.is_wide(), "65537 addresses is wider than /16");
    let r = IpRange::from_addrs(ip("1.0.0.0"), ip("1.0.255.255")).expect("range");
    assert!(!r.is_wide());
}

#[test]
fn from_addrs_validates_order_and_family() {
    assert!(IpRange::from_addrs(ip("1.0.0.9"), ip("1.0.0.1")).is_err());
    assert!(IpRange::from_addrs(ip("1.0.0.1"), ip("2001:db8::1")).is_err());
    let ok = IpRange::from_addrs(ip("::ffff:1.0.0.1"), ip("1.0.0.9")).expect("mapped + v4");
    assert!(ok.contains(ip("1.0.0.5")));
}

#[test]
fn contains_respects_family() {
    let r = IpRange::parse_cidr("1.2.3.0/24").expect("cidr");
    assert!(r.contains(ip("1.2.3.255")));
    assert!(r.contains(ip("::ffff:1.2.3.9")));
    assert!(!r.contains(ip("1.2.4.0")));
    assert!(!r.contains(ip("2001:db8::1")));
}

#[test]
fn cidr_decomposition_is_minimal_and_exact() {
    let r = IpRange::from_addrs(ip("1.0.0.1"), ip("1.0.0.6")).expect("range");
    let blocks: Vec<String> = r
        .to_cidr_ranges()
        .iter()
        .map(|b| b.cidr_string().expect("aligned"))
        .collect();
    assert_eq!(
        blocks,
        ["1.0.0.1/32", "1.0.0.2/31", "1.0.0.4/31", "1.0.0.6/32"]
    );
    let aligned = IpRange::parse_cidr("10.0.0.0/8").expect("cidr");
    assert_eq!(aligned.to_cidr_ranges(), vec![aligned]);
    let all_v4 = IpRange::from_addrs(ip("0.0.0.0"), ip("255.255.255.255")).expect("all");
    assert_eq!(all_v4.to_cidr_ranges().len(), 1);
    assert_eq!(all_v4.cidr_string().as_deref(), Some("0.0.0.0/0"));
    let all_v6 = IpRange::parse_cidr("::/0").expect("all v6");
    assert_eq!(all_v6.to_cidr_ranges(), vec![all_v6]);
    // 非单块区间没有 cidr 文本。
    assert!(r.cidr_string().is_none());
    // 高端边界（避免上溢）。
    let top = IpRange::from_addrs(ip("255.255.255.250"), ip("255.255.255.255")).expect("top");
    let blocks: Vec<String> = top
        .to_cidr_ranges()
        .iter()
        .map(|b| b.cidr_string().expect("aligned"))
        .collect();
    assert_eq!(blocks, ["255.255.255.250/31", "255.255.255.252/30"]);
}

#[test]
fn decomposition_covers_exactly_the_range() {
    let r = IpRange::from_addrs(ip("2001:db8::5"), ip("2001:db8::1234")).expect("range");
    let blocks = r.to_cidr_ranges();
    for probe in [
        "2001:db8::4",
        "2001:db8::5",
        "2001:db8::1234",
        "2001:db8::1235",
        "2001:db8::800",
    ] {
        let in_range = r.contains(ip(probe));
        let in_blocks = blocks.iter().any(|b| b.contains(ip(probe)));
        assert_eq!(in_range, in_blocks, "{probe}");
    }
}

#[test]
fn index_handles_nesting_overlap_and_boundaries() {
    let items = [
        ("10.0.0.0/8", 0u32),
        ("10.1.0.0/16", 1),
        ("10.1.2.0/24", 2),
        ("10.1.2.3/32", 3),
        ("11.0.0.0/8", 4),
    ];
    let index = RangeIndex::build(
        items
            .iter()
            .map(|(c, p)| (IpRange::parse_cidr(c).expect("cidr"), *p)),
    );
    assert_eq!(matches(&index, "10.1.2.3"), vec![0, 1, 2, 3]);
    assert_eq!(matches(&index, "10.1.2.4"), vec![0, 1, 2]);
    assert_eq!(matches(&index, "10.1.3.0"), vec![0, 1]);
    assert_eq!(matches(&index, "10.255.255.255"), vec![0]);
    assert_eq!(matches(&index, "11.0.0.0"), vec![4]);
    assert_eq!(matches(&index, "9.255.255.255"), Vec::<u32>::new());
    assert_eq!(matches(&index, "12.0.0.0"), Vec::<u32>::new());
    // v6 与 v4 分开。
    assert_eq!(matches(&index, "2001:db8::1"), Vec::<u32>::new());
}

#[test]
fn index_v6_and_empty() {
    let empty = RangeIndex::build(std::iter::empty());
    assert!(empty.is_empty());
    assert!(matches(&empty, "1.2.3.4").is_empty());
    assert!(matches(&empty, "::1").is_empty());
    let index = RangeIndex::build([
        (IpRange::parse_cidr("2001:db8::/32").expect("cidr"), 7),
        (IpRange::parse_cidr("2001:db8:1::/48").expect("cidr"), 8),
    ]);
    assert_eq!(matches(&index, "2001:db8:1::5"), vec![7, 8]);
    assert_eq!(matches(&index, "2001:db8:2::5"), vec![7]);
    assert_eq!(matches(&index, "2001:db9::"), Vec::<u32>::new());
}

#[test]
fn index_agrees_with_brute_force_on_random_ranges() {
    // 确定性 LCG，覆盖任意重叠（含非 CIDR 区间）。
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };
    let mut ranges = Vec::new();
    for payload in 0..300u32 {
        let a = next() % 5000;
        let len = next() % 400;
        ranges.push((
            IpRange::V4 {
                start: a,
                end: a + len,
            },
            payload,
        ));
    }
    let index = RangeIndex::build(ranges.iter().copied());
    for probe in (0..5500u32).step_by(7) {
        let addr = IpAddr::V4(std::net::Ipv4Addr::from(probe));
        let mut got = Vec::new();
        index.for_each_match(addr, |p| got.push(p));
        got.sort_unstable();
        let mut want: Vec<u32> = ranges
            .iter()
            .filter(|(r, _)| r.contains(addr))
            .map(|(_, p)| *p)
            .collect();
        want.sort_unstable();
        assert_eq!(got, want, "probe {probe}");
    }
}
