use fluxdown_engine::bt_guard::{IpRange, parse_filter_file};

fn cidrs(text: &str) -> Vec<String> {
    parse_filter_file(text)
        .ranges
        .iter()
        .flat_map(IpRange::to_cidr_ranges)
        .map(|r| r.cidr_string().expect("aligned"))
        .collect()
}

#[test]
fn plain_cidr_and_single_ip_lines() {
    let text = "1.2.3.4\n5.6.7.0/24\n2001:db8::/32\n2001:db8:1::7\n";
    assert_eq!(
        cidrs(text),
        [
            "1.2.3.4/32",
            "5.6.7.0/24",
            "2001:db8::/32",
            "2001:db8:1::7/128"
        ]
    );
}

#[test]
fn comments_blank_lines_and_inline_comments_are_ignored() {
    let text = "# header\n\n; semi\n// slash\n   \n1.2.3.4 # trailing note\n  5.6.7.8/32  \n";
    let parsed = parse_filter_file(text);
    assert_eq!(parsed.ranges.len(), 2);
    assert_eq!(parsed.invalid_lines, 0);
}

#[test]
fn start_end_ranges_with_optional_spaces() {
    let text = "1.0.0.1-1.0.0.6\n2.0.0.0 - 2.0.0.255\n";
    assert_eq!(
        cidrs(text),
        [
            "1.0.0.1/32",
            "1.0.0.2/31",
            "1.0.0.4/31",
            "1.0.0.6/32",
            "2.0.0.0/24"
        ]
    );
}

#[test]
fn peerguardian_name_start_end() {
    let text = "Some Org:1.0.0.0-1.0.0.255\nOrg: With:Colons-and-dash:3.0.0.0-3.0.0.255\n";
    let parsed = parse_filter_file(text);
    assert_eq!(parsed.invalid_lines, 0);
    assert_eq!(cidrs(text), ["1.0.0.0/24", "3.0.0.0/24"]);
}

#[test]
fn emule_dat_lines_respect_access_level() {
    let text = "001.002.003.000 - 001.002.003.255 , 000 , Evil Corp\n\
                004.000.000.000 - 004.000.000.255 , 100 , Another\n\
                005.000.000.000 - 005.000.000.255 , 127 , Level at pass threshold\n\
                006.000.000.000 - 006.000.000.255 , 200 , Not filtered\n";
    let parsed = parse_filter_file(text);
    assert_eq!(parsed.ranges.len(), 2);
    assert_eq!(parsed.skipped_by_level, 2);
    assert_eq!(parsed.invalid_lines, 0);
    assert_eq!(cidrs(text), ["1.2.3.0/24", "4.0.0.0/24"]);
}

#[test]
fn emule_description_may_contain_hash_and_commas() {
    let text = "7.0.0.0 - 7.0.0.255 , 10 , Group #1, subnet, more\n";
    assert_eq!(cidrs(text), ["7.0.0.0/24"]);
}

#[test]
fn emule_with_bad_level_or_range_is_invalid() {
    let parsed = parse_filter_file(
        "1.0.0.0 - 1.0.0.255 , abc , x\n1.0.0.0 , 10 , missing end\n9.9.9.9 - 1.1.1.1 , 10 , reversed\n",
    );
    assert_eq!(parsed.ranges.len(), 0);
    assert_eq!(parsed.invalid_lines, 3);
}

#[test]
fn wide_entries_are_dropped_and_counted() {
    let text = "1.0.0.0/8\n2.0.0.0/15\n3.0.0.0/16\n2001:db8::/31\n2001:db8::/32\n\
                4.0.0.0-4.2.0.0\n5.0.0.0 - 5.0.255.255 , 0 , narrow enough\n";
    let parsed = parse_filter_file(text);
    assert_eq!(
        parsed.dropped_wide, 4,
        "/8, /15, v6 /31 and the >65536 span"
    );
    assert_eq!(parsed.ranges.len(), 3);
}

#[test]
fn garbage_is_counted_invalid_not_fatal() {
    let parsed = parse_filter_file("not an ip\n1.2.3.4/99\n1.2.3.4\n300.1.1.1\n");
    assert_eq!(parsed.ranges.len(), 1);
    assert_eq!(parsed.invalid_lines, 3);
}

#[test]
fn bom_and_crlf_are_tolerated() {
    let text = "\u{feff}1.2.3.4\r\n5.6.7.8\r\n";
    assert_eq!(cidrs(text), ["1.2.3.4/32", "5.6.7.8/32"]);
}

#[test]
fn mixed_formats_in_one_file() {
    let text =
        "# mixed\n1.1.1.0/24\nOrg:2.2.2.0-2.2.2.255\n3.3.3.0 - 3.3.3.255 , 5 , emule\n4.4.4.4\n";
    let parsed = parse_filter_file(text);
    assert_eq!(parsed.ranges.len(), 4);
    assert_eq!(parsed.invalid_lines, 0);
}
