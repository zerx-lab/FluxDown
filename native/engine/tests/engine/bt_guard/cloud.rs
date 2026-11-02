use std::net::IpAddr;

use fluxdown_engine::bt_guard::{
    BUILTIN_CANARIES, BtGuard, ClientRule, CloudClientRule, CloudDocument, CloudReject,
    EntryFilter, GuardAction, Layer,
};
use serde_json::json;

use super::support::{DAY, T0, cloud_doc, cloud_entry, enforce_config, guard_with};

fn ip(s: &str) -> IpAddr {
    s.parse().expect("ip")
}

fn guard() -> (BtGuard, super::support::Clock) {
    guard_with(enforce_config())
}

fn cloud_entries(g: &BtGuard) -> Vec<fluxdown_engine::bt_guard::Entry> {
    g.list_entries(&EntryFilter {
        layer: Some(Layer::Cloud),
        limit: 100_000,
        ..EntryFilter::default()
    })
    .1
}

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
fn contract_sample_payload_parses_and_applies() {
    let value = json!({
        "format": "fluxdown-bt-guard-list/1",
        "sequence": 123,
        "generatedAt": T0,
        "expiresAt": T0 + DAY,
        "entries": [
            {"cidr": "20.1.2.3/32", "category": "community", "action": "ban", "reason": "r", "expiresAt": T0 + 1000},
            {"cidr": "20.9.0.0/16", "category": "consensus", "action": "throttle", "reason": "r2"}
        ],
        "clientRules": [
            {"id": "xl0019", "field": "peerIdPrefix", "op": "prefix", "value": "-XL0019-",
             "action": "throttle", "seedingOnly": true, "reason": "x"}
        ],
        "canaries": ["1.1.1.1", "8.8.8.8", "223.5.5.5", "2606:4700:4700::1111"],
        "attribution": ["BTN-Collected-Rules by PBH-BTN, CC BY 4.0, https://github.com/PBH-BTN/BTN-Collected-Rules"],
        "unknownFutureField": {"ignored": true}
    });
    let doc = CloudDocument::from_value(value).expect("parse");
    assert_eq!(doc.sequence, 123);
    assert!(doc.client_rules[0].seeding_only);
    let (g, _c) = guard();
    let summary = g.apply_cloud(&doc).expect("apply");
    assert_eq!(
        (
            summary.entries,
            summary.client_rules,
            summary.added,
            summary.removed
        ),
        (2, 1, 2, 0)
    );
    let status = g.cloud_status();
    assert_eq!(status.sequence, 123);
    assert_eq!(status.updated_at, T0);
    assert_eq!(status.expires_at, T0 + DAY);
    assert_eq!((status.entries, status.client_rules), (2, 1));
    assert!(!status.stale);
    assert_eq!(status.rejected_reason, None);
    assert_eq!(status.last_error, None);
    assert_eq!(status.attribution.len(), 1);
    let entries = cloud_entries(&g);
    assert_eq!(entries.len(), 2);
    let by_cidr = |c: &str| entries.iter().find(|e| e.cidr() == c).expect("entry");
    assert_eq!(by_cidr("20.1.2.3/32").expires_at, Some(T0 + 1000));
    assert_eq!(
        by_cidr("20.9.0.0/16").expires_at,
        Some(T0 + DAY),
        "missing expiresAt falls back to the document expiry"
    );
    assert_eq!(&*by_cidr("20.1.2.3/32").reason, "r");
    assert!(
        by_cidr("20.9.0.0/16")
            .evidence
            .as_deref()
            .is_some_and(|e| e.contains("consensus"))
    );
    assert!(!g.admit(ip("20.1.2.3")));
    assert!(
        g.admit(ip("20.9.1.1")),
        "throttle entries do not refuse connections"
    );
}

#[test]
fn malformed_payload_is_rejected_by_parser_or_format_check() {
    assert!(CloudDocument::from_value(json!({"sequence": "nope"})).is_err());
    let (g, _c) = guard();
    // 缺字段 → format 为空 → 拒绝。
    let doc =
        CloudDocument::from_value(json!({"sequence": 1, "expiresAt": T0 + 100})).expect("parse");
    assert!(matches!(g.apply_cloud(&doc), Err(CloudReject::Format(_))));
    let mut doc = cloud_doc(1, T0 + 100, vec![]);
    doc.format = "fluxdown-bt-guard-list/2".to_owned();
    let err = g.apply_cloud(&doc).expect_err("format");
    assert_eq!(
        err,
        CloudReject::Format("fluxdown-bt-guard-list/2".to_owned())
    );
    assert!(g.cloud_status().rejected_reason.is_some());
}

#[test]
fn expired_document_is_rejected() {
    let (g, _c) = guard();
    // 恰好等于当前时刻也算已过期。
    assert_eq!(
        g.apply_cloud(&cloud_doc(1, T0, vec![])),
        Err(CloudReject::Expired { expires_at: T0 })
    );
    assert!(g.apply_cloud(&cloud_doc(1, T0 + 1, vec![])).is_ok());
}

#[test]
fn sequence_rollback_is_rejected_equal_and_newer_are_accepted() {
    let (g, _c) = guard();
    g.apply_cloud(&cloud_doc(
        10,
        T0 + DAY,
        vec![cloud_entry("20.0.0.1/32", "ban")],
    ))
    .expect("seq 10");
    assert_eq!(g.cloud_sequence(), 10);
    let err = g
        .apply_cloud(&cloud_doc(
            9,
            T0 + DAY,
            vec![cloud_entry("20.0.0.2/32", "ban")],
        ))
        .expect_err("rollback");
    assert_eq!(
        err,
        CloudReject::Rollback {
            current: 10,
            got: 9
        }
    );
    // 被拒绝时保留上一份，并记录原因。
    assert_eq!(g.cloud_sequence(), 10);
    assert_eq!(cloud_entries(&g).len(), 1);
    assert_eq!(cloud_entries(&g)[0].cidr(), "20.0.0.1/32");
    assert!(
        g.cloud_status()
            .rejected_reason
            .expect("reason")
            .contains("rollback")
    );
    // 相同 sequence 幂等接受（重启后重放缓存信封）。
    g.apply_cloud(&cloud_doc(
        10,
        T0 + DAY,
        vec![cloud_entry("20.0.0.1/32", "ban")],
    ))
    .expect("same sequence");
    assert_eq!(
        g.cloud_status().rejected_reason,
        None,
        "a successful apply clears the rejection"
    );
    g.apply_cloud(&cloud_doc(
        11,
        T0 + DAY,
        vec![cloud_entry("20.0.0.3/32", "ban")],
    ))
    .expect("newer");
    assert_eq!(cloud_entries(&g)[0].cidr(), "20.0.0.3/32");
}

#[test]
fn rejection_does_not_touch_the_previous_layer_or_rules() {
    let (g, _c) = guard();
    let mut first = cloud_doc(5, T0 + DAY, vec![cloud_entry("20.0.0.1/32", "ban")]);
    first.client_rules = vec![CloudClientRule {
        id: "a".to_owned(),
        field: "clientName".to_owned(),
        op: "contains".to_owned(),
        value: "X".to_owned(),
        action: "choke".to_owned(),
        seeding_only: false,
        reason: String::new(),
    }];
    g.apply_cloud(&first).expect("apply");
    assert!(g.apply_cloud(&cloud_doc(4, T0 + DAY, vec![])).is_err());
    assert!(!g.admit(ip("20.0.0.1")));
    assert_eq!(g.cloud_status().client_rules, 1);
    assert!(g.take_actions().contains(&GuardAction::Changed));
}

#[test]
fn canary_coverage_rejects_the_whole_document() {
    for covering in [
        "1.1.1.0/24",
        "1.1.1.1/32",
        "1.1.1.1",
        "8.8.8.0/24",
        "223.5.5.5/32",
        "2606:4700:4700::/48",
        "2606:4700:4700::1111/128",
        // 宽条目本来会被丢弃，但覆盖金丝雀同样使整份文档作废。
        "8.0.0.0/8",
        "0.0.0.0/0",
    ] {
        let (g, _c) = guard();
        let doc = cloud_doc(
            1,
            T0 + DAY,
            vec![
                cloud_entry("20.0.0.1/32", "ban"),
                cloud_entry(covering, "ban"),
            ],
        );
        let err = g.apply_cloud(&doc).expect_err(covering);
        assert!(
            matches!(err, CloudReject::CanaryCovered { .. }),
            "{covering}: {err}"
        );
        assert!(
            cloud_entries(&g).is_empty(),
            "nothing is applied: {covering}"
        );
        assert!(g.admit(ip("20.0.0.1")));
    }
    assert_eq!(BUILTIN_CANARIES.len(), 4);
}

#[test]
fn near_miss_canary_entries_are_accepted_and_document_canaries_are_not_trusted() {
    let (g, _c) = guard();
    let mut doc = cloud_doc(
        1,
        T0 + DAY,
        vec![
            cloud_entry("1.1.1.2/32", "ban"),
            cloud_entry("8.8.4.4/32", "ban"),
            cloud_entry("2606:4700:4700::1112/128", "ban"),
        ],
    );
    // 文档自带的 canaries 不改变内置判定：这里声明一个无关地址。
    doc.canaries = vec!["9.9.9.9".to_owned()];
    assert_eq!(g.apply_cloud(&doc).expect("apply").entries, 3);
    // 反过来：文档把 canaries 清空也不能让覆盖内置金丝雀的条目通过。
    let (g, _c) = guard();
    let mut bad = cloud_doc(1, T0 + DAY, vec![cloud_entry("8.8.8.8/32", "ban")]);
    bad.canaries.clear();
    assert!(g.apply_cloud(&bad).is_err());
}

#[test]
fn diff_guard_is_inactive_below_one_thousand_previous_entries() {
    let (g, _c) = guard();
    g.apply_cloud(&cloud_doc(1, T0 + DAY, numbered(0..999)))
        .expect("first");
    let summary = g
        .apply_cloud(&cloud_doc(2, T0 + DAY, numbered(5000..5999)))
        .expect("full replacement of 999 entries is allowed");
    assert_eq!((summary.added, summary.removed), (999, 999));
}

#[test]
fn diff_guard_boundary_is_exactly_fifty_percent() {
    let build = |keep: std::ops::Range<u32>, extra: std::ops::Range<u32>| {
        let mut entries = numbered(keep);
        entries.extend(numbered(extra));
        entries
    };
    // 基线 1000 条。新文档：保留 750 条（移除 250）+ 新增 250 条 → 500 = 50%，接受。
    let (g, _c) = guard();
    g.apply_cloud(&cloud_doc(1, T0 + DAY, numbered(0..1000)))
        .expect("baseline");
    let ok = g
        .apply_cloud(&cloud_doc(2, T0 + DAY, build(0..750, 5000..5250)))
        .expect("exactly 50% is allowed");
    assert_eq!((ok.added, ok.removed), (250, 250));
    // 保留 749（移除 251）+ 新增 250 → 501 > 50%，拒绝。
    let (g, _c) = guard();
    g.apply_cloud(&cloud_doc(1, T0 + DAY, numbered(0..1000)))
        .expect("baseline");
    let err = g
        .apply_cloud(&cloud_doc(2, T0 + DAY, build(0..749, 5000..5250)))
        .expect_err("501/1000 > 50%");
    assert_eq!(
        err,
        CloudReject::DiffTooLarge {
            added: 250,
            removed: 251,
            previous: 1000
        }
    );
    // 被拒后保持基线，且后续温和的更新仍可接受。
    assert_eq!(cloud_entries(&g).len(), 1000);
    assert!(
        g.cloud_status()
            .rejected_reason
            .expect("reason")
            .contains("diff")
    );
    assert!(
        g.apply_cloud(&cloud_doc(3, T0 + DAY, build(0..900, 5000..5100)))
            .is_ok()
    );
}

#[test]
fn diff_guard_catches_a_wipe_out() {
    let (g, _c) = guard();
    g.apply_cloud(&cloud_doc(1, T0 + DAY, numbered(0..1000)))
        .expect("baseline");
    assert!(matches!(
        g.apply_cloud(&cloud_doc(2, T0 + DAY, vec![])),
        Err(CloudReject::DiffTooLarge { .. })
    ));
    assert_eq!(cloud_entries(&g).len(), 1000);
}

#[test]
fn identical_resubmission_has_zero_diff() {
    let (g, _c) = guard();
    g.apply_cloud(&cloud_doc(1, T0 + DAY, numbered(0..1200)))
        .expect("baseline");
    let again = g
        .apply_cloud(&cloud_doc(1, T0 + DAY, numbered(0..1200)))
        .expect("same content");
    assert_eq!((again.added, again.removed), (0, 0));
}

#[test]
fn bad_entries_are_dropped_individually() {
    let (g, _c) = guard();
    let mut expired = cloud_entry("20.0.0.9/32", "ban");
    expired.expires_at = Some(T0 - 1);
    let doc = cloud_doc(
        1,
        T0 + DAY,
        vec![
            cloud_entry("20.0.0.1/32", "ban"),
            cloud_entry("20.0.0.1/32", "ban"),    // 重复
            cloud_entry("20.0.0.2/32", "allow"),  // 云层不允许 allow
            cloud_entry("20.0.0.3/32", "nuke"),   // 未知动作
            cloud_entry("not-a-cidr", "ban"),     // 非法 CIDR
            cloud_entry("20.0.0.0/8", "ban"),     // 宽于 /16
            cloud_entry("2a00::/31", "ban"),      // 宽于 v6 /32
            cloud_entry("20.1.0.0/16", "choke"),  // 刚好 /16：保留
            cloud_entry("2a00::/32", "throttle"), // 刚好 v6 /32：保留
            expired,                              // 已过期
        ],
    );
    let s = g.apply_cloud(&doc).expect("apply");
    assert_eq!(s.entries, 3);
    assert_eq!(s.dropped_wide, 2);
    assert_eq!(s.dropped_invalid, 3, "allow + unknown action + bad cidr");
    assert_eq!(s.dropped_expired, 1);
    let cidrs: Vec<String> = cloud_entries(&g).iter().map(|e| e.cidr()).collect();
    assert!(cidrs.contains(&"20.0.0.1/32".to_owned()));
    assert!(cidrs.contains(&"20.1.0.0/16".to_owned()));
    assert!(cidrs.contains(&"2a00::/32".to_owned()));
    assert!(g.admit(ip("20.200.0.1")), "wide /8 must not be active");
}

#[test]
fn client_rules_are_validated() {
    let (g, _c) = guard();
    let rule = |id: &str, field: &str, op: &str, value: &str, action: &str| CloudClientRule {
        id: id.to_owned(),
        field: field.to_owned(),
        op: op.to_owned(),
        value: value.to_owned(),
        action: action.to_owned(),
        seeding_only: false,
        reason: String::new(),
    };
    let mut doc = cloud_doc(1, T0 + DAY, vec![]);
    doc.client_rules = vec![
        rule("ok", "peerIdPrefix", "prefix", "-XL", "choke"),
        rule("bad-field", "userAgent", "prefix", "x", "choke"),
        rule("bad-op", "clientName", "regex", "x", "choke"),
        rule("bad-action", "clientName", "prefix", "x", "ban"),
        rule("empty-value", "clientName", "prefix", "", "choke"),
        rule("", "clientName", "prefix", "x", "choke"),
    ];
    let s = g.apply_cloud(&doc).expect("apply");
    assert_eq!(s.client_rules, 1);
    assert_eq!(s.dropped_invalid_rules, 5);
}

#[test]
fn client_rule_matching_semantics() {
    use fluxdown_engine::bt_guard::FilterAction;
    let make = |field, op, value: &str| ClientRule {
        id: "r".to_owned(),
        field,
        op,
        value: value.to_owned(),
        action: FilterAction::Choke,
        seeding_only: false,
        reason: String::new(),
    };
    // 字段 / 操作符枚举经云文档编译得到；这里借 apply 后的行为在 rules 测试里覆盖，
    // 此处只直接验证 peer_id 字节级匹配与大小写敏感性。
    let id: [u8; 20] = *b"-XL0019-abcdefghijkl";
    let (f, o) = (
        fluxdown_engine::bt_guard::ClientField::PeerIdPrefix,
        fluxdown_engine::bt_guard::ClientOp::Prefix,
    );
    assert!(make(f, o, "-XL0019-").matches(Some(&id), None));
    assert!(
        !make(f, o, "-xl0019-").matches(Some(&id), None),
        "case-sensitive"
    );
    assert!(
        !make(f, o, "XL0019").matches(Some(&id), None),
        "prefix, not contains"
    );
    assert!(
        !make(f, o, "-XL0019-").matches(None, Some("-XL0019-")),
        "field decides the source"
    );
    let contains = fluxdown_engine::bt_guard::ClientOp::Contains;
    assert!(make(f, contains, "XL0019").matches(Some(&id), None));
    assert!(make(f, contains, "abcdefghijkl").matches(Some(&id), None));
    assert!(!make(f, contains, "zzz").matches(Some(&id), None));
    let name = fluxdown_engine::bt_guard::ClientField::ClientName;
    assert!(make(name, contains, "unlei").matches(None, Some("Xunlei 1")));
    assert!(
        !make(name, contains, "Unlei").matches(None, Some("Xunlei 1")),
        "case-sensitive"
    );
    assert!(!make(name, contains, "unlei").matches(None, None));
}

#[test]
fn apply_queues_actions_and_clears_last_error() {
    let (g, _c) = guard();
    g.note_cloud_error("fetch failed: timeout");
    assert_eq!(
        g.cloud_status().last_error.as_deref(),
        Some("fetch failed: timeout")
    );
    g.take_actions();
    g.apply_cloud(&cloud_doc(
        1,
        T0 + DAY,
        vec![cloud_entry("20.0.0.1/32", "ban")],
    ))
    .expect("apply");
    let actions = g.take_actions();
    assert!(actions.contains(&GuardAction::Changed));
    assert!(actions.contains(&GuardAction::DisconnectBanned));
    assert_eq!(g.cloud_status().last_error, None);
    // 只有 throttle / choke 条目时不需要断开连接。
    g.apply_cloud(&cloud_doc(
        2,
        T0 + DAY,
        vec![cloud_entry("20.0.0.1/32", "throttle")],
    ))
    .expect("apply");
    let actions = g.take_actions();
    assert!(!actions.contains(&GuardAction::DisconnectBanned));
}

#[test]
fn whole_layer_is_replaced_not_merged() {
    let (g, _c) = guard();
    g.apply_cloud(&cloud_doc(
        1,
        T0 + DAY,
        vec![cloud_entry("20.0.0.1/32", "ban")],
    ))
    .expect("first");
    g.apply_cloud(&cloud_doc(
        2,
        T0 + DAY,
        vec![cloud_entry("20.0.0.2/32", "ban")],
    ))
    .expect("second");
    assert!(g.admit(ip("20.0.0.1")), "removed from the new document");
    assert!(!g.admit(ip("20.0.0.2")));
    assert_eq!(g.counts().cloud, 1);
}
