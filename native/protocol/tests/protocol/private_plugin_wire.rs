//! `daemon.plugin.revokePrivate` 的 wire 契约：`scope` 可选，旧 wire 双向兼容。

use fluxdown_protocol::{PluginRevokePrivateParams, PrivateRevocationDto};
use serde_json::json;

#[test]
fn legacy_params_without_scope_parse_as_none_and_do_not_serialize_one() {
    let legacy = json!({ "revoked": [{ "pluginId": "a@private", "version": "1.0.0" }] });
    let params: PluginRevokePrivateParams =
        serde_json::from_value(legacy.clone()).expect("legacy wire parses");
    assert_eq!(params.revoked.len(), 1);
    assert_eq!(params.scope, None);
    assert_eq!(
        serde_json::to_value(&params).expect("serialize"),
        legacy,
        "a missing scope must stay absent on the wire"
    );
}

#[test]
fn scope_round_trips_and_an_empty_scope_stays_distinct_from_none() {
    let params = PluginRevokePrivateParams {
        revoked: vec![PrivateRevocationDto {
            plugin_id: "a@private".to_owned(),
            version: "1.0.0".to_owned(),
        }],
        scope: Some(vec!["a@private".to_owned()]),
    };
    let wire = serde_json::to_value(&params).expect("serialize");
    assert_eq!(wire["scope"], json!(["a@private"]));
    let back: PluginRevokePrivateParams = serde_json::from_value(wire).expect("round trip");
    assert_eq!(back.scope, Some(vec!["a@private".to_owned()]));

    let empty: PluginRevokePrivateParams =
        serde_json::from_value(json!({ "revoked": [], "scope": [] })).expect("empty scope");
    assert_eq!(empty.scope, Some(Vec::new()));
    assert_eq!(
        serde_json::to_value(&empty).expect("serialize")["scope"],
        json!([])
    );
}
