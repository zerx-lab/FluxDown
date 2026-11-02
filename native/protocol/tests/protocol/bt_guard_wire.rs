//! BT 防吸血（`daemon.bt.guard.*`）的 wire 契约：方法名与慢方法归属、事件信封、DTO 往返与旧数据兼容。

use std::collections::HashSet;

use fluxdown_protocol::method::{
    ALL_METHODS, DAEMON_BT_GUARD_CLOUD_APPLY, DAEMON_BT_GUARD_ENTRIES_ADD,
    DAEMON_BT_GUARD_ENTRIES_LIST, DAEMON_BT_GUARD_ENTRIES_REMOVE, DAEMON_BT_GUARD_EVENTS_LIST,
    DAEMON_BT_GUARD_EVIDENCE_DRAIN, DAEMON_BT_GUARD_EXPORT, DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
    DAEMON_BT_GUARD_STATE, is_slow_daemon_method,
};
use fluxdown_protocol::{
    BT_GUARD_LIST_DEFAULT_LIMIT, BtGuardCloudApplyParams, BtGuardCloudStatusDto,
    BtGuardEntriesAddParams, BtGuardEntriesListParams, BtGuardEntryDto, BtGuardEventsListParams,
    BtGuardEvidenceItemDto, BtGuardStateDto, BtPeerDto, DaemonEvent, DaemonSnapshot, EventFrame,
    ServiceEvent, apply_daemon_event,
};
use serde_json::{Value, json};

#[test]
fn every_guard_method_is_canonical_unique_and_literal() {
    let expected = [
        (DAEMON_BT_GUARD_STATE, "daemon.bt.guard.state"),
        (DAEMON_BT_GUARD_ENTRIES_LIST, "daemon.bt.guard.entries.list"),
        (DAEMON_BT_GUARD_ENTRIES_ADD, "daemon.bt.guard.entries.add"),
        (
            DAEMON_BT_GUARD_ENTRIES_REMOVE,
            "daemon.bt.guard.entries.remove",
        ),
        (DAEMON_BT_GUARD_EVENTS_LIST, "daemon.bt.guard.events.list"),
        (DAEMON_BT_GUARD_EXPORT, "daemon.bt.guard.export"),
        (
            DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
            "daemon.bt.guard.filterSources.refresh",
        ),
        (DAEMON_BT_GUARD_CLOUD_APPLY, "daemon.bt.guard.cloud.apply"),
        (
            DAEMON_BT_GUARD_EVIDENCE_DRAIN,
            "daemon.bt.guard.evidence.drain",
        ),
    ];
    for (constant, literal) in expected {
        assert_eq!(constant, literal);
        assert_eq!(
            ALL_METHODS.iter().filter(|name| **name == literal).count(),
            1,
            "{literal} must appear exactly once in ALL_METHODS"
        );
    }
    let guard_methods = ALL_METHODS
        .iter()
        .filter(|name| name.starts_with("daemon.bt.guard."))
        .count();
    assert_eq!(
        guard_methods,
        expected.len(),
        "no undeclared daemon.bt.guard.*"
    );
    let unique = ALL_METHODS.iter().collect::<HashSet<_>>();
    assert_eq!(unique.len(), ALL_METHODS.len());
}

#[test]
fn only_network_or_bulk_guard_methods_are_slow() {
    for slow in [
        DAEMON_BT_GUARD_EXPORT,
        DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
        DAEMON_BT_GUARD_CLOUD_APPLY,
    ] {
        assert!(is_slow_daemon_method(slow), "{slow}");
    }
    // 条目增删保持同连接内的请求顺序（添加后立即列表必须看到）。
    for inline in [
        DAEMON_BT_GUARD_STATE,
        DAEMON_BT_GUARD_ENTRIES_LIST,
        DAEMON_BT_GUARD_ENTRIES_ADD,
        DAEMON_BT_GUARD_ENTRIES_REMOVE,
        DAEMON_BT_GUARD_EVENTS_LIST,
        DAEMON_BT_GUARD_EVIDENCE_DRAIN,
    ] {
        assert!(!is_slow_daemon_method(inline), "{inline}");
    }
}

#[test]
fn guard_events_use_the_daemon_event_envelope_and_never_touch_the_snapshot() {
    let cases = [
        (
            DaemonEvent::BtGuardChanged,
            json!({"type": "btGuardChanged"}),
        ),
        (
            DaemonEvent::BtGuardCloudRefreshWanted { have_sequence: 123 },
            json!({"type": "btGuardCloudRefreshWanted", "data": {"haveSequence": 123}}),
        ),
        (
            DaemonEvent::BtGuardEvidenceReady { count: 7 },
            json!({"type": "btGuardEvidenceReady", "data": {"count": 7}}),
        ),
    ];
    for (event, wire) in cases {
        assert_eq!(serde_json::to_value(&event).expect("serialize"), wire);
        let back: DaemonEvent = serde_json::from_value(wire).expect("deserialize");
        let mut snapshot = DaemonSnapshot::default();
        let before = serde_json::to_value(&snapshot).expect("snapshot");
        apply_daemon_event(&mut snapshot, &back);
        assert_eq!(serde_json::to_value(&snapshot).expect("snapshot"), before);
    }
}

#[test]
fn guard_events_are_never_coalesced_away() {
    let frame = |sequence: u64, event: DaemonEvent| EventFrame {
        epoch: "e".to_owned(),
        sequence,
        previous: None,
        event: ServiceEvent::Daemon(event),
    };
    let frames = vec![
        frame(
            1,
            DaemonEvent::BtGuardCloudRefreshWanted { have_sequence: 1 },
        ),
        frame(2, DaemonEvent::BtGuardEvidenceReady { count: 1 }),
        frame(3, DaemonEvent::BtGuardChanged),
        frame(4, DaemonEvent::BtGuardChanged),
    ];
    let mut queue = fluxdown_protocol::coalesce::FrameQueue::new();
    for frame in frames.clone() {
        queue.push(frame);
    }
    let drained = std::iter::from_fn(|| queue.pop()).count();
    assert_eq!(drained, frames.len());
}

#[test]
fn peer_dto_decodes_old_payloads_and_omits_defaults() {
    let old: BtPeerDto = serde_json::from_value(json!({
        "addr": "1.2.3.4:6881", "transport": "tcp",
        "downloaded": 1, "uploaded": 2, "downBps": 3, "upBps": 4
    }))
    .expect("old peer payload");
    assert_eq!(old.action, "allow");
    assert_eq!(
        old,
        BtPeerDto {
            addr: "1.2.3.4:6881".to_owned(),
            transport: "tcp".to_owned(),
            downloaded: 1,
            uploaded: 2,
            down_bps: 3,
            up_bps: 4,
            ..BtPeerDto::default()
        }
    );
    let wire = serde_json::to_value(&old).expect("serialize");
    for absent in [
        "ipGroup",
        "groupDeliveredBytes",
        "suspicion",
        "rules",
        "observeOnly",
    ] {
        assert!(wire.get(absent).is_none(), "{absent} must be omitted");
    }
    assert_eq!(wire["action"], "allow");

    let flagged = BtPeerDto {
        addr: "2001:db8::1:6881".to_owned(),
        ip_group: "2001:db8::/64".to_owned(),
        group_delivered_bytes: 9,
        suspicion: Some("strong".to_owned()),
        rules: vec!["excessive".to_owned(), "rewind".to_owned()],
        action: "banned".to_owned(),
        observe_only: true,
        ..BtPeerDto::default()
    };
    let wire = serde_json::to_value(&flagged).expect("serialize");
    assert_eq!(wire["ipGroup"], "2001:db8::/64");
    assert_eq!(wire["groupDeliveredBytes"], 9);
    assert_eq!(wire["suspicion"], "strong");
    assert_eq!(wire["rules"], json!(["excessive", "rewind"]));
    assert_eq!(wire["action"], "banned");
    assert_eq!(wire["observeOnly"], true);
    assert_eq!(
        serde_json::from_value::<BtPeerDto>(wire).expect("round trip"),
        flagged
    );
}

#[test]
fn entries_list_params_default_to_the_first_page() {
    let params: BtGuardEntriesListParams = serde_json::from_value(json!({})).expect("empty");
    assert_eq!(params, BtGuardEntriesListParams::default());
    assert_eq!(params.offset, 0);
    assert_eq!(params.limit, BT_GUARD_LIST_DEFAULT_LIMIT);
    let events: BtGuardEventsListParams = serde_json::from_value(json!({})).expect("empty");
    assert_eq!(events, BtGuardEventsListParams::default());
    let filtered: BtGuardEntriesListParams = serde_json::from_value(
        json!({"layer": "auto", "rule": "rewind", "query": "1.2", "offset": 5, "limit": 10}),
    )
    .expect("filtered");
    assert_eq!(filtered.layer.as_deref(), Some("auto"));
    assert_eq!(filtered.limit, 10);
    let wire = serde_json::to_value(BtGuardEntriesListParams::default()).expect("serialize");
    assert_eq!(
        wire,
        json!({"offset": 0, "limit": BT_GUARD_LIST_DEFAULT_LIMIT})
    );
    // 新增的两个过滤字段：camelCase，缺省不上线。
    let by_ip: BtGuardEntriesListParams =
        serde_json::from_value(json!({"containsIp": "203.0.113.9"})).expect("containsIp");
    assert_eq!(by_ip.contains_ip.as_deref(), Some("203.0.113.9"));
    assert_eq!(
        serde_json::to_value(&by_ip).expect("serialize"),
        json!({"containsIp": "203.0.113.9", "offset": 0, "limit": BT_GUARD_LIST_DEFAULT_LIMIT})
    );
    let by_group: BtGuardEventsListParams =
        serde_json::from_value(json!({"ipGroup": "2001:db8::/64", "kind": "banned"}))
            .expect("ipGroup");
    assert_eq!(by_group.ip_group.as_deref(), Some("2001:db8::/64"));
    assert_eq!(
        serde_json::to_value(BtGuardEventsListParams::default()).expect("serialize"),
        json!({"offset": 0, "limit": BT_GUARD_LIST_DEFAULT_LIMIT})
    );
}

#[test]
fn entry_and_add_params_wire_is_camel_case() {
    let add: BtGuardEntriesAddParams = serde_json::from_value(
        json!({"cidr": "1.2.3.0/24", "action": "ban", "durationSecs": 3600, "reason": "x"}),
    )
    .expect("add");
    assert_eq!(add.duration_secs, Some(3600));
    let permanent: BtGuardEntriesAddParams =
        serde_json::from_value(json!({"cidr": "1.2.3.4", "action": "allow"})).expect("permanent");
    assert_eq!(permanent.duration_secs, None);
    assert_eq!(permanent.reason, "");

    let entry = BtGuardEntryDto {
        id: 7,
        layer: "auto".to_owned(),
        cidr: "1.2.3.4".to_owned(),
        action: "ban".to_owned(),
        rule: "excessive".to_owned(),
        evidence: Some(json!({"ratioMilli": 3700})),
        created_at: 100,
        expires_at: Some(200),
        remaining_secs: Some(100),
        ..BtGuardEntryDto::default()
    };
    let wire = serde_json::to_value(&entry).expect("serialize");
    assert_eq!(wire["createdAt"], 100);
    assert_eq!(wire["expiresAt"], 200);
    assert_eq!(wire["remainingSecs"], 100);
    assert_eq!(wire["evidence"], json!({"ratioMilli": 3700}));
    assert!(wire.get("reason").is_none());
    let permanent = serde_json::to_value(BtGuardEntryDto::default()).expect("serialize");
    assert!(permanent.get("expiresAt").is_none());
    assert!(permanent.get("remainingSecs").is_none());
    assert_eq!(
        serde_json::from_value::<BtGuardEntryDto>(wire).expect("round trip"),
        entry
    );
}

#[test]
fn state_and_cloud_apply_round_trip() {
    let wire = json!({
        "enabled": true, "sessionActive": false,
        "stats": {"todayObserved": 1, "todayEnforced": 2, "todayBlockedConnections": 3, "todayRefusedBytes": 4},
        "cloud": {"sequence": 9, "updatedAt": 10, "expiresAt": 11, "entries": 12, "clientRules": 13,
                  "stale": true, "attribution": ["BTN-Collected-Rules by PBH-BTN, CC BY 4.0"]},
        "filterSources": [{"url": "https://e/x", "ok": false, "entries": 0, "updatedAt": 0, "error": "boom"}],
        "counts": {"allow": 1, "manual": 2, "auto": 3, "cloud": 4, "file": 5}
    });
    let state: BtGuardStateDto = serde_json::from_value(wire.clone()).expect("state");
    assert_eq!(state.stats.today_refused_bytes, 4);
    assert_eq!(state.cloud.attribution.len(), 1);
    assert_eq!(state.cloud.last_error, None);
    assert_eq!(state.filter_sources[0].error.as_deref(), Some("boom"));
    assert_eq!(state.counts.file, 5);
    assert_eq!(serde_json::to_value(&state).expect("serialize"), wire);
    assert_eq!(
        BtGuardCloudStatusDto::default().attribution,
        Vec::<String>::new()
    );

    let apply: BtGuardCloudApplyParams = serde_json::from_value(json!({
        "envelope": "{\"payload\":\"e30=\"}", "sequence": 5, "verifiedPayload": {"format": "x"}
    }))
    .expect("apply");
    assert_eq!(apply.sequence, 5);
    assert_eq!(apply.verified_payload["format"], "x");
    // 缺 envelope 必须是反序列化错误，而不是被默认值吞掉。
    assert!(
        serde_json::from_value::<BtGuardCloudApplyParams>(
            json!({"sequence": 5, "verifiedPayload": {}})
        )
        .is_err()
    );
}

#[test]
fn evidence_item_matches_the_cloud_upload_schema() {
    let item = BtGuardEvidenceItemDto {
        ip_group: "1.2.3.4".to_owned(),
        rule: "excessive".to_owned(),
        strength: "strong".to_owned(),
        info_hash: "a".repeat(40),
        torrent_size: 123,
        delivered_bytes: 456,
        ratio_milli: 3700,
        peer_id_prefix: "-XL0019-".to_owned(),
        client: "Xunlei 0.0.1.9".to_owned(),
        first_seen: 1_761_900_000,
        last_seen: 1_761_903_600,
        ..BtGuardEvidenceItemDto::default()
    };
    let wire: Value = serde_json::to_value(&item).expect("serialize");
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
        assert!(wire.get(key).is_some(), "{key}");
    }
    assert_eq!(wire.as_object().map(|o| o.len()), Some(14));
}
