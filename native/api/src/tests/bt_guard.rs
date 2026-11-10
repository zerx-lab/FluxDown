//! BT 防吸血 REST（`/api/v1/bt/guard/*`）：鉴权、参数校验、宿主转发与降级。

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use async_trait::async_trait;
use fluxdown_protocol::daemon::{
    BtGuardCloudStatusDto, BtGuardCountsDto, BtGuardEntriesListParams, BtGuardEntryDto,
    BtGuardStateDto, BtGuardStatsDto, CreateTaskRequest, DownloadRequest, QueueDto, TaskDto,
};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::{MockHost, TestServer, request, send_raw};
use crate::routes;
use crate::server::{self, ApiServerConfig};
use crate::service::{ApiError, ApiHost, LiveSpeed, UNKNOWN_ENDPOINT_MESSAGE};

const TOKEN_HEADERS: &[(&str, &str)] = &[("X-FluxDown-Token", "T")];
const JSON_TOKEN_HEADERS: &[(&str, &str)] = &[
    ("X-FluxDown-Token", "T"),
    ("Content-Type", "application/json"),
];

async fn server_with(host: MockHost) -> TestServer {
    TestServer::start(host, |c| {
        c.token.set("T");
        c.management_enabled = true;
    })
    .await
}

pub(super) fn entry(id: i64, layer: &str, cidr: &str, action: &str) -> BtGuardEntryDto {
    BtGuardEntryDto {
        id,
        layer: layer.to_string(),
        cidr: cidr.to_string(),
        action: action.to_string(),
        rule: "excessive".to_string(),
        reason: "uploaded 3x the torrent size".to_string(),
        evidence: Some(json!({"ratioMilli": 3700})),
        source: "auto".to_string(),
        created_at: 1_761_900_000,
        expires_at: Some(1_762_000_000),
        remaining_secs: Some(3600),
    }
}

#[tokio::test]
async fn routes_enforce_management_auth_and_the_management_switch() {
    let server = server_with(MockHost::new()).await;
    let entry_path = routes::bt_guard_entry_path(7);
    let routes_and_methods = [
        ("GET", routes::API_BT_GUARD_STATE),
        ("GET", routes::API_BT_GUARD_ENTRIES),
        ("POST", routes::API_BT_GUARD_ENTRIES),
        ("DELETE", entry_path.as_str()),
        ("GET", routes::API_BT_GUARD_EVENTS),
    ];
    for (method, path) in routes_and_methods {
        let anonymous = server.send(&request(method, path, &[], "{}")).await;
        assert_eq!(anonymous.status, 401, "{method} {path} without a token");
        let wrong = server
            .send(&request(
                method,
                path,
                &[("X-FluxDown-Token", "nope")],
                "{}",
            ))
            .await;
        assert_eq!(wrong.status, 401, "{method} {path} with a wrong token");
    }
    let bearer = server
        .send(&request(
            "GET",
            routes::API_BT_GUARD_STATE,
            &[("Authorization", "Bearer T")],
            "",
        ))
        .await;
    assert_eq!(bearer.status, 200);
    // 没有任何一次未授权请求触达宿主。
    assert!(server.host.bt_entries_lists().is_empty());
    assert!(server.host.bt_added().is_empty());
    assert!(server.host.bt_removed().is_empty());
    assert!(server.host.bt_events_lists().is_empty());

    // 管理 API 分组未启用 → 路由未注册。
    let disabled = TestServer::start(MockHost::new(), |c| {
        c.token.set("T");
        c.management_enabled = false;
    })
    .await;
    let missing = disabled
        .send(&request(
            "GET",
            routes::API_BT_GUARD_STATE,
            TOKEN_HEADERS,
            "",
        ))
        .await;
    assert_eq!(missing.status, 404);
    assert_eq!(missing.json()["message"], UNKNOWN_ENDPOINT_MESSAGE);
}

#[tokio::test]
async fn state_returns_camel_case_dto_from_host() {
    let state = BtGuardStateDto {
        enabled: true,
        session_active: true,
        stats: BtGuardStatsDto {
            today_observed: 4,
            today_enforced: 2,
            today_blocked_connections: 9,
            today_refused_bytes: 16_384,
        },
        cloud: BtGuardCloudStatusDto {
            sequence: 12,
            entries: 1500,
            attribution: vec!["BTN-Collected-Rules by PBH-BTN, CC BY 4.0".to_string()],
            ..BtGuardCloudStatusDto::default()
        },
        counts: BtGuardCountsDto {
            manual: 3,
            cloud: 1500,
            ..BtGuardCountsDto::default()
        },
        ..BtGuardStateDto::default()
    };
    let server = server_with(MockHost::new().with_bt_state(state)).await;

    let resp = server
        .send(&request(
            "GET",
            routes::API_BT_GUARD_STATE,
            TOKEN_HEADERS,
            "",
        ))
        .await;

    assert_eq!(resp.status, 200);
    let body = resp.json();
    assert_eq!(body["enabled"], true);
    assert_eq!(body["sessionActive"], true);
    assert_eq!(body["stats"]["todayObserved"], 4);
    assert_eq!(body["stats"]["todayRefusedBytes"], 16_384);
    assert_eq!(body["cloud"]["sequence"], 12);
    assert_eq!(body["cloud"]["clientRules"], 0);
    assert_eq!(
        body["cloud"]["attribution"][0],
        "BTN-Collected-Rules by PBH-BTN, CC BY 4.0"
    );
    assert_eq!(body["counts"]["cloud"], 1500);
}

#[tokio::test]
async fn entries_list_forwards_filters_and_defaults_the_page() {
    let server = server_with(MockHost::new()).await;

    let filtered = server
        .send(&request(
            "GET",
            &format!(
                "{}?layer=manual&rule=excessive&query=1.2&containsIp=203.0.113.9&offset=10&limit=50",
                routes::API_BT_GUARD_ENTRIES
            ),
            TOKEN_HEADERS,
            "",
        ))
        .await;
    assert_eq!(filtered.status, 200);
    let body = filtered.json();
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["cidr"], "1.2.3.4/32");
    assert_eq!(body["items"][0]["remainingSecs"], 3600);
    assert_eq!(body["items"][0]["evidence"]["ratioMilli"], 3700);

    let defaults = server
        .send(&request(
            "GET",
            routes::API_BT_GUARD_ENTRIES,
            TOKEN_HEADERS,
            "",
        ))
        .await;
    assert_eq!(defaults.status, 200);
    // 空串过滤等同于未提供。
    let blank = server
        .send(&request(
            "GET",
            &format!(
                "{}?layer=&rule=&query=&limit=500",
                routes::API_BT_GUARD_ENTRIES
            ),
            TOKEN_HEADERS,
            "",
        ))
        .await;
    assert_eq!(blank.status, 200);

    let calls = server.host.bt_entries_lists();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].layer.as_deref(), Some("manual"));
    assert_eq!(calls[0].rule.as_deref(), Some("excessive"));
    assert_eq!(calls[0].query.as_deref(), Some("1.2"));
    assert_eq!(calls[0].contains_ip.as_deref(), Some("203.0.113.9"));
    assert_eq!((calls[0].offset, calls[0].limit), (10, 50));
    assert_eq!(calls[1], BtGuardEntriesListParams::default());
    assert_eq!(
        calls[2],
        BtGuardEntriesListParams {
            limit: 500,
            ..BtGuardEntriesListParams::default()
        }
    );
}

#[tokio::test]
async fn entries_list_rejects_bad_filters_before_reaching_the_host() {
    let server = server_with(MockHost::new()).await;
    for query in [
        "layer=everything",
        "rule=telepathy",
        "limit=0",
        "limit=501",
        "layer=cloud&rule=nope",
        "containsIp=nope",
        "containsIp=203.0.113.0%2F24",
    ] {
        let resp = server
            .send(&request(
                "GET",
                &format!("{}?{query}", routes::API_BT_GUARD_ENTRIES),
                TOKEN_HEADERS,
                "",
            ))
            .await;
        assert_eq!(resp.status, 400, "{query}");
        assert_eq!(resp.json()["success"], false, "{query}");
    }
    assert!(server.host.bt_entries_lists().is_empty());

    // 每个合法的层 / 规则取值都能通过。
    for layer in fluxdown_protocol::BT_GUARD_LAYERS {
        let resp = server
            .send(&request(
                "GET",
                &format!("{}?layer={layer}", routes::API_BT_GUARD_ENTRIES),
                TOKEN_HEADERS,
                "",
            ))
            .await;
        assert_eq!(resp.status, 200, "{layer}");
    }
    for rule in crate::server::BT_GUARD_FILTER_RULES {
        let resp = server
            .send(&request(
                "GET",
                &format!("{}?rule={rule}", routes::API_BT_GUARD_ENTRIES),
                TOKEN_HEADERS,
                "",
            ))
            .await;
        assert_eq!(resp.status, 200, "{rule}");
    }
    assert_eq!(
        server.host.bt_entries_lists().len(),
        fluxdown_protocol::BT_GUARD_LAYERS.len() + crate::server::BT_GUARD_FILTER_RULES.len()
    );
}

#[tokio::test]
async fn entries_add_validates_then_forwards_to_the_host() {
    let server = server_with(MockHost::new()).await;

    let ok = server
        .send(&request(
            "POST",
            routes::API_BT_GUARD_ENTRIES,
            JSON_TOKEN_HEADERS,
            r#"{"cidr":"203.0.113.0/24","action":"ban","durationSecs":3600,"reason":"leecher"}"#,
        ))
        .await;
    assert_eq!(ok.status, 200);
    let body = ok.json();
    assert_eq!(body["cidr"], "203.0.113.0/24");
    assert_eq!(body["layer"], "manual");
    assert_eq!(body["action"], "ban");

    let allow = server
        .send(&request(
            "POST",
            routes::API_BT_GUARD_ENTRIES,
            JSON_TOKEN_HEADERS,
            r#"{"cidr":"198.51.100.7","action":"allow"}"#,
        ))
        .await;
    assert_eq!(allow.status, 200);
    assert_eq!(allow.json()["layer"], "allow");

    for bad in [
        r#"{"cidr":"","action":"ban"}"#,
        r#"{"cidr":"   ","action":"ban"}"#,
        r#"{"cidr":"1.2.3.4","action":"nuke"}"#,
        r#"{"cidr":"1.2.3.4","action":"ban","durationSecs":0}"#,
        r#"{"cidr":"1.2.3.4","action":"ban","durationSecs":-5}"#,
        r#"{"action":"ban"}"#,
        "not json",
    ] {
        let resp = server
            .send(&request(
                "POST",
                routes::API_BT_GUARD_ENTRIES,
                JSON_TOKEN_HEADERS,
                bad,
            ))
            .await;
        assert_eq!(resp.status, 400, "{bad}");
    }

    let added = server.host.bt_added();
    assert_eq!(added.len(), 2, "invalid payloads must not reach the host");
    assert_eq!(added[0].cidr, "203.0.113.0/24");
    assert_eq!(added[0].action, "ban");
    assert_eq!(added[0].duration_secs, Some(3600));
    assert_eq!(added[0].reason, "leecher");
    assert_eq!(added[1].action, "allow");
    assert_eq!(added[1].duration_secs, None);
}

#[tokio::test]
async fn entries_add_maps_host_rejection_of_a_bad_cidr_to_400() {
    let server = server_with(
        MockHost::new().with_bt_add_error(ApiError::BadRequest("invalid cidr".to_string())),
    )
    .await;

    let resp = server
        .send(&request(
            "POST",
            routes::API_BT_GUARD_ENTRIES,
            JSON_TOKEN_HEADERS,
            r#"{"cidr":"999.1.1.1","action":"ban"}"#,
        ))
        .await;

    assert_eq!(resp.status, 400);
    assert_eq!(resp.json()["message"], "invalid cidr");
}

#[tokio::test]
async fn entry_delete_maps_missing_cloud_layer_and_bad_ids() {
    let server = server_with(MockHost::new()).await;

    let removed = server
        .send(&request(
            "DELETE",
            &routes::bt_guard_entry_path(7),
            TOKEN_HEADERS,
            "",
        ))
        .await;
    assert_eq!(removed.status, 200);
    assert_eq!(server.host.bt_removed(), [7]);

    for bad in ["abc", "0", "-3", "1.5"] {
        let resp = server
            .send(&request(
                "DELETE",
                &format!("{}/{bad}", routes::API_BT_GUARD_ENTRIES),
                TOKEN_HEADERS,
                "",
            ))
            .await;
        assert_eq!(resp.status, 400, "{bad}");
    }
    assert_eq!(
        server.host.bt_removed(),
        [7],
        "bad ids never reach the host"
    );

    server.host.set_bt_remove_error(ApiError::NotFound);
    let missing = server
        .send(&request(
            "DELETE",
            &routes::bt_guard_entry_path(99),
            TOKEN_HEADERS,
            "",
        ))
        .await;
    assert_eq!(missing.status, 404);

    server.host.set_bt_remove_error(ApiError::BadRequest(
        "cloud entries cannot be removed; add an allow entry instead".to_string(),
    ));
    let cloud = server
        .send(&request(
            "DELETE",
            &routes::bt_guard_entry_path(5),
            TOKEN_HEADERS,
            "",
        ))
        .await;
    assert_eq!(cloud.status, 400);
    assert!(
        cloud.json()["message"]
            .as_str()
            .is_some_and(|message| message.contains("allow entry"))
    );
}

#[tokio::test]
async fn events_forward_filters_and_validate_the_page() {
    let server = server_with(MockHost::new()).await;

    let ok = server
        .send(&request(
            "GET",
            &format!(
                "{}?offset=5&limit=20&rule=lag&kind=probe&ipGroup=2001%3Adb8%3A%3A%2F64",
                routes::API_BT_GUARD_EVENTS
            ),
            TOKEN_HEADERS,
            "",
        ))
        .await;
    assert_eq!(ok.status, 200);
    let body = ok.json();
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["ipGroup"], "1.2.3.4");
    assert_eq!(body["items"][0]["kind"], "enforce");

    let blank = server
        .send(&request(
            "GET",
            &format!("{}?ipGroup=", routes::API_BT_GUARD_EVENTS),
            TOKEN_HEADERS,
            "",
        ))
        .await;
    assert_eq!(blank.status, 200);
    for query in ["limit=0", "limit=501", "rule=bogus"] {
        let resp = server
            .send(&request(
                "GET",
                &format!("{}?{query}", routes::API_BT_GUARD_EVENTS),
                TOKEN_HEADERS,
                "",
            ))
            .await;
        assert_eq!(resp.status, 400, "{query}");
    }

    let calls = server.host.bt_events_lists();
    assert_eq!(calls.len(), 2);
    assert_eq!((calls[0].offset, calls[0].limit), (5, 20));
    assert_eq!(calls[0].rule.as_deref(), Some("lag"));
    assert_eq!(calls[0].kind.as_deref(), Some("probe"));
    assert_eq!(calls[0].ip_group.as_deref(), Some("2001:db8::/64"));
    assert_eq!(calls[1].ip_group, None, "blank ipGroup is no filter");
}

/// 只实现必需方法的宿主：防吸血方法走 trait 默认降级。
struct BareHost;

#[async_trait]
impl ApiHost for BareHost {
    async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError> {
        Ok(Vec::new())
    }
    async fn get_task(&self, _task_id: &str) -> Result<Option<TaskDto>, ApiError> {
        Ok(None)
    }
    async fn create_task(&self, _req: CreateTaskRequest) -> Result<String, ApiError> {
        Err(ApiError::Unavailable)
    }
    async fn delete_task(&self, _task_id: &str, _delete_files: bool) -> Result<(), ApiError> {
        Ok(())
    }
    async fn pause_task(&self, _task_id: &str) -> Result<(), ApiError> {
        Ok(())
    }
    async fn continue_task(&self, _task_id: &str) -> Result<(), ApiError> {
        Ok(())
    }
    async fn pause_all(&self) -> Result<(), ApiError> {
        Ok(())
    }
    async fn continue_all(&self) -> Result<(), ApiError> {
        Ok(())
    }
    async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
        Ok(Vec::new())
    }
    async fn submit_external(&self, _req: DownloadRequest) -> Result<(), ApiError> {
        Ok(())
    }
    async fn get_config(&self) -> Result<HashMap<String, String>, ApiError> {
        Ok(HashMap::new())
    }
    async fn apply_config(&self, _changes: HashMap<String, String>) -> Result<(), ApiError> {
        Ok(())
    }
    async fn live_speeds(&self) -> Result<HashMap<String, LiveSpeed>, ApiError> {
        Ok(HashMap::new())
    }
}

#[tokio::test]
async fn unsupported_host_degrades_to_500_not_a_panic() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = ApiServerConfig::from_config_map(&HashMap::new(), "9.9.9-test");
    config.port = addr.port();
    config.token.set("T");
    config.management_enabled = true;
    let cancel = CancellationToken::new();
    tokio::spawn(server::serve_on(
        listener,
        Arc::new(BareHost),
        config,
        cancel.clone(),
    ));

    for (method, path, body) in [
        ("GET", routes::API_BT_GUARD_STATE.to_string(), ""),
        ("GET", routes::API_BT_GUARD_ENTRIES.to_string(), ""),
        (
            "POST",
            routes::API_BT_GUARD_ENTRIES.to_string(),
            r#"{"cidr":"1.2.3.4","action":"ban"}"#,
        ),
        ("DELETE", routes::bt_guard_entry_path(1), ""),
        ("GET", routes::API_BT_GUARD_EVENTS.to_string(), ""),
    ] {
        let resp = send_raw(addr, &request(method, &path, JSON_TOKEN_HEADERS, body)).await;
        assert_eq!(resp.status, 500, "{method} {path}");
        assert_eq!(
            resp.json()["message"],
            "bt guard not supported by this host"
        );
    }
    cancel.cancel();
}
