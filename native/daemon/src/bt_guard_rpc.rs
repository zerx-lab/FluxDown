//! BT 防吸血 RPC（`daemon.bt.guard.*`）。作为 `service` 的子模块编译，复用其私有辅助函数。
//!
//! 全部方法只操作引擎级 [`BtGuardHost`]（内存中的 guard + 落库），不经 actor：guard 自身线程安全，
//! 条目 / 订阅 / 云名单变化引发的事件、断开与重新拨号由宿主的泵经 guard 的 waker 触发。
//!
//! `cloud.apply` 与 `evidence.drain` 只允许本机 agent（`fluxdown-agent`）调用：云名单的验签发生在
//! agent，daemon 没有公钥，只做 sequence 防回滚与差异防护。

use fluxdown_engine::bt_guard::{
    BtGuardHost, CloudDocument, CloudStatus, Entry, EntryFilter, EvidenceItem, FilterAction,
    FilterSourceStatus, GuardError, Layer, NewEntry,
};
use fluxdown_engine::db::{BtGuardEventQuery, BtGuardEventRow};
use fluxdown_protocol::{
    ApplicationErrorCode, BT_GUARD_LIST_MAX_LIMIT, BT_GUARD_REMOVE_MAX_IDS, BtGuardCloudApplyDto,
    BtGuardCloudApplyParams, BtGuardCloudStatusDto, BtGuardCountsDto, BtGuardEntriesAddParams,
    BtGuardEntriesListDto, BtGuardEntriesListParams, BtGuardEntriesRemoveDto,
    BtGuardEntriesRemoveParams, BtGuardEntryDto, BtGuardEventDto, BtGuardEventsListDto,
    BtGuardEventsListParams, BtGuardEvidenceDrainDto, BtGuardEvidenceItemDto, BtGuardExportDto,
    BtGuardExportParams, BtGuardFilterSourceDto, BtGuardFilterSourcesRefreshDto, BtGuardStateDto,
    BtGuardStatsDto, RpcErrorData, RpcErrorObject, method,
};
use serde_json::Value;

use super::{
    DaemonService, internal_error, invalid_argument, parse_optional_params, parse_params,
    require_local_agent, to_value,
};

/// 云名单信封原文上限（契约 §5）。
const CLOUD_ENVELOPE_MAX_BYTES: usize = 16 * 1024 * 1024;
/// 手动条目说明 / CIDR 文本长度上限。
const REASON_MAX_CHARS: usize = 512;
const CIDR_MAX_CHARS: usize = 128;

/// 引擎错误 → RPC 错误：非法参数归调用方，存储不可用是服务端内部错误。
fn guard_error(field: &str, error: GuardError) -> RpcErrorObject {
    match error {
        GuardError::InvalidArgument(message) => invalid_argument(field, &message),
        GuardError::StoreUnavailable(message) => internal_error(message),
    }
}

fn unavailable() -> RpcErrorObject {
    RpcErrorObject::application(
        "bt guard is not available",
        RpcErrorData::new(ApplicationErrorCode::Unavailable, false),
    )
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.is_empty())
}

fn parse_layer(value: Option<&str>) -> Result<Option<Layer>, RpcErrorObject> {
    match value {
        None | Some("") => Ok(None),
        Some(text) => Layer::parse(text)
            .map(Some)
            .ok_or_else(|| invalid_argument("layer", "unknown layer")),
    }
}

fn check_limit(limit: u32) -> Result<usize, RpcErrorObject> {
    if limit == 0 || limit > BT_GUARD_LIST_MAX_LIMIT {
        return Err(invalid_argument(
            "limit",
            &format!("limit must be between 1 and {BT_GUARD_LIST_MAX_LIMIT}"),
        ));
    }
    Ok(limit as usize)
}

fn cloud_status_dto(status: CloudStatus) -> BtGuardCloudStatusDto {
    BtGuardCloudStatusDto {
        sequence: u64::try_from(status.sequence).unwrap_or(0),
        updated_at: status.updated_at,
        expires_at: status.expires_at,
        entries: count(status.entries),
        client_rules: count(status.client_rules),
        stale: status.stale,
        last_error: status.last_error,
        rejected_reason: status.rejected_reason,
        attribution: status.attribution,
    }
}

fn filter_source_dto(status: FilterSourceStatus) -> BtGuardFilterSourceDto {
    BtGuardFilterSourceDto {
        url: status.url,
        ok: status.ok,
        entries: count(status.entries),
        updated_at: status.updated_at,
        error: status.error,
    }
}

fn entry_dto(entry: &Entry, now: i64) -> BtGuardEntryDto {
    BtGuardEntryDto {
        id: entry.id,
        layer: entry.layer.as_str().to_owned(),
        cidr: entry.cidr(),
        action: entry.action.as_str().to_owned(),
        rule: entry.rule.to_string(),
        reason: entry.reason.to_string(),
        evidence: entry
            .evidence
            .as_deref()
            .and_then(|text| serde_json::from_str::<Value>(text).ok()),
        source: entry.source.to_string(),
        created_at: entry.created_at,
        expires_at: entry.expires_at,
        remaining_secs: entry.remaining_secs(now),
    }
}

fn event_dto(row: BtGuardEventRow) -> BtGuardEventDto {
    BtGuardEventDto {
        id: row.id,
        at: row.at,
        kind: row.kind,
        ip_group: row.ip_group,
        info_hash: row.info_hash,
        rule: row.rule,
        mode: row.mode,
        action: row.action,
        detail: row.detail,
    }
}

fn evidence_dto(item: EvidenceItem) -> BtGuardEvidenceItemDto {
    BtGuardEvidenceItemDto {
        ip_group: item.ip_group,
        rule: item.rule,
        strength: item.strength,
        info_hash: item.info_hash,
        torrent_size: item.torrent_size,
        delivered_bytes: item.delivered_bytes,
        dup_bytes: item.dup_bytes,
        ratio_milli: item.ratio_milli,
        rerequests: item.rerequests,
        rewind_permille: item.rewind_permille,
        peer_id_prefix: item.peer_id_prefix,
        client: item.client,
        first_seen: item.first_seen,
        last_seen: item.last_seen,
    }
}

/// 管理类调用返回前把变化写库。失败不改变调用结果（内存状态已生效，泵会重试），但必须留下日志。
async fn persist(host: &BtGuardHost) {
    if let Err(error) = host.flush_now().await {
        fluxdown_engine::logger::report_warning("bt-guard", "persist management change", &error);
    }
}

impl DaemonService {
    pub(super) async fn bt_guard_call(
        &self,
        is_local_agent: bool,
        method_name: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcErrorObject> {
        let host = self.bt_guard.as_deref().ok_or_else(unavailable)?;
        match method_name {
            method::DAEMON_BT_GUARD_STATE => to_value(state_dto(host)),
            method::DAEMON_BT_GUARD_ENTRIES_LIST => entries_list(host, params),
            method::DAEMON_BT_GUARD_ENTRIES_ADD => entries_add(host, params).await,
            method::DAEMON_BT_GUARD_ENTRIES_REMOVE => entries_remove(host, params).await,
            method::DAEMON_BT_GUARD_EVENTS_LIST => self.events_list(host, params).await,
            method::DAEMON_BT_GUARD_EXPORT => export(host, params),
            method::DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH => {
                let sources = host.refresh_filters_now().await;
                persist(host).await;
                to_value(BtGuardFilterSourcesRefreshDto {
                    sources: sources.into_iter().map(filter_source_dto).collect(),
                })
            }
            method::DAEMON_BT_GUARD_CLOUD_APPLY => {
                require_local_agent(is_local_agent)?;
                cloud_apply(host, params).await
            }
            method::DAEMON_BT_GUARD_EVIDENCE_DRAIN => {
                require_local_agent(is_local_agent)?;
                to_value(BtGuardEvidenceDrainDto {
                    items: host
                        .guard()
                        .drain_evidence()
                        .into_iter()
                        .map(evidence_dto)
                        .collect(),
                })
            }
            other => Err(RpcErrorObject::application(
                format!("unknown bt guard method {other}"),
                RpcErrorData::new(ApplicationErrorCode::Unsupported, false),
            )),
        }
    }

    async fn events_list(
        &self,
        host: &BtGuardHost,
        params: Option<Value>,
    ) -> Result<Value, RpcErrorObject> {
        let params = parse_optional_params::<BtGuardEventsListParams>(params)?;
        let limit = check_limit(params.limit)?;
        let ip_group = match non_empty(params.ip_group) {
            Some(text) => Some(
                host.guard()
                    .normalize_group(text.trim())
                    .map_err(|error| guard_error("ipGroup", error))?,
            ),
            None => None,
        };
        // 事件先进内存、随落库批次进库：列表前先落库，调用方立即看到刚发生的记录。
        persist(host).await;
        let (total, rows) = self
            .db
            .bt_guard_list_events(&BtGuardEventQuery {
                rule: non_empty(params.rule),
                kind: non_empty(params.kind),
                ip_group,
                offset: params.offset,
                limit: u32::try_from(limit).unwrap_or(BT_GUARD_LIST_MAX_LIMIT),
            })
            .await
            .map_err(|error| internal_error(format!("{error:#}")))?;
        to_value(BtGuardEventsListDto {
            total: u64::try_from(total).unwrap_or(0),
            items: rows.into_iter().map(event_dto).collect(),
        })
    }
}

async fn cloud_apply(host: &BtGuardHost, params: Option<Value>) -> Result<Value, RpcErrorObject> {
    let params = parse_params::<BtGuardCloudApplyParams>(params)?;
    if params.envelope.is_empty() {
        return Err(invalid_argument("envelope", "envelope is required"));
    }
    if params.envelope.len() > CLOUD_ENVELOPE_MAX_BYTES {
        return Err(invalid_argument(
            "envelope",
            &format!("envelope exceeds {CLOUD_ENVELOPE_MAX_BYTES} bytes"),
        ));
    }
    let document = CloudDocument::from_value(params.verified_payload)
        .map_err(|message| invalid_argument("verifiedPayload", &message))?;
    if u64::try_from(document.sequence).ok() != Some(params.sequence) {
        return Err(invalid_argument(
            "sequence",
            "sequence does not match the verified payload",
        ));
    }
    let guard = host.guard();
    let outcome = guard.apply_cloud(&document);
    let applied = outcome.is_ok();
    persist(host).await;
    to_value(BtGuardCloudApplyDto {
        applied,
        rejected_reason: outcome.err().map(|reject| reject.to_string()),
        status: cloud_status_dto(guard.cloud_status()),
    })
}

fn state_dto(host: &BtGuardHost) -> BtGuardStateDto {
    let guard = host.guard();
    let stats = guard.stats();
    let counts = guard.counts();
    BtGuardStateDto {
        enabled: host.config().enabled,
        session_active: host.session_active(),
        stats: BtGuardStatsDto {
            today_observed: stats.observed,
            today_enforced: stats.enforced,
            today_blocked_connections: stats.blocked_connections,
            today_refused_bytes: stats.refused_bytes,
        },
        cloud: cloud_status_dto(guard.cloud_status()),
        filter_sources: guard
            .filter_sources()
            .into_iter()
            .map(filter_source_dto)
            .collect(),
        counts: BtGuardCountsDto {
            allow: count(counts.allow),
            manual: count(counts.manual),
            auto: count(counts.auto),
            cloud: count(counts.cloud),
            file: count(counts.file),
        },
    }
}

fn entries_list(host: &BtGuardHost, params: Option<Value>) -> Result<Value, RpcErrorObject> {
    let params = parse_optional_params::<BtGuardEntriesListParams>(params)?;
    let limit = check_limit(params.limit)?;
    let layer = parse_layer(params.layer.as_deref())?;
    let contains_ip = match non_empty(params.contains_ip) {
        Some(text) => Some(text.trim().parse::<std::net::IpAddr>().map_err(|_| {
            invalid_argument("containsIp", "containsIp must be a valid IP address")
        })?),
        None => None,
    };
    let guard = host.guard();
    let (total, items) = guard.list_entries(&EntryFilter {
        layer,
        rule: non_empty(params.rule),
        query: non_empty(params.query),
        contains_ip,
        offset: params.offset as usize,
        limit,
    });
    let now = guard.now();
    to_value(BtGuardEntriesListDto {
        total: total as u64,
        items: items.iter().map(|entry| entry_dto(entry, now)).collect(),
    })
}

async fn entries_add(host: &BtGuardHost, params: Option<Value>) -> Result<Value, RpcErrorObject> {
    let params = parse_params::<BtGuardEntriesAddParams>(params)?;
    let cidr = params.cidr.trim();
    if cidr.is_empty() || cidr.chars().count() > CIDR_MAX_CHARS {
        return Err(invalid_argument("cidr", "cidr is required"));
    }
    let Some(action) = FilterAction::parse(&params.action) else {
        return Err(invalid_argument("action", "unknown action"));
    };
    if params.duration_secs.is_some_and(|secs| secs <= 0) {
        return Err(invalid_argument(
            "durationSecs",
            "durationSecs must be positive",
        ));
    }
    if params.reason.chars().count() > REASON_MAX_CHARS {
        return Err(invalid_argument(
            "reason",
            &format!("reason is limited to {REASON_MAX_CHARS} characters"),
        ));
    }
    let guard = host.guard();
    let entry = guard
        .add_entry(NewEntry {
            cidr: cidr.to_owned(),
            action,
            duration_secs: params.duration_secs,
            reason: params.reason,
        })
        .map_err(|error| guard_error("cidr", error))?;
    persist(host).await;
    to_value(entry_dto(&entry, guard.now()))
}

async fn entries_remove(
    host: &BtGuardHost,
    params: Option<Value>,
) -> Result<Value, RpcErrorObject> {
    let params = parse_params::<BtGuardEntriesRemoveParams>(params)?;
    if params.ids.is_empty() {
        return Err(invalid_argument("ids", "ids must not be empty"));
    }
    if params.ids.len() > BT_GUARD_REMOVE_MAX_IDS {
        return Err(invalid_argument(
            "ids",
            &format!("at most {BT_GUARD_REMOVE_MAX_IDS} ids per call"),
        ));
    }
    let removed = host
        .guard()
        .remove_entries(&params.ids)
        .map_err(|error| guard_error("ids", error))?;
    persist(host).await;
    to_value(BtGuardEntriesRemoveDto {
        removed: count(removed),
    })
}

fn export(host: &BtGuardHost, params: Option<Value>) -> Result<Value, RpcErrorObject> {
    let params = parse_optional_params::<BtGuardExportParams>(params)?;
    let layer = parse_layer(params.layer.as_deref())?;
    let text = host.guard().export(layer);
    let lines = count(text.lines().count());
    to_value(BtGuardExportDto { text, lines })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use fluxdown_engine::bt_guard::{
        BtGuard, BtGuardConfig, BtGuardHost, GuardCtx, GuardEvent, RuleMode,
    };
    use fluxdown_protocol::{ApplicationErrorCode, DaemonEvent, ServiceEvent, method};
    use serde_json::{Value, json};

    use super::DaemonService;
    use crate::event_hub::{DaemonEngineEventSink, DaemonEventHub};

    struct Fixture {
        service: DaemonService,
        host: Arc<BtGuardHost>,
        events: DaemonEventHub,
        db: fluxdown_engine::db::Db,
        dir: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.dir) {
                eprintln!("bt guard rpc test dir removal failed: {error}");
            }
        }
    }

    async fn fixture(config: BtGuardConfig, wired: bool) -> Fixture {
        let dir = std::env::temp_dir().join(format!("fluxdown_guard_rpc_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("test dir");
        let db = fluxdown_engine::db::Db::connect("sqlite::memory:")
            .await
            .expect("memory db");
        let events = DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 64);
        let selections = crate::selection::DaemonSelection::new(events.clone());
        let blobs = Arc::new(
            crate::blob_store::BlobStore::open(dir.join("blobs"))
                .await
                .expect("blob store"),
        );
        let host = Arc::new(BtGuardHost::new(
            Arc::new(BtGuard::new(config)),
            db.clone(),
            Arc::new(DaemonEngineEventSink(events.clone())),
            fluxdown_engine::proxy_config::ProxyConfig::default(),
        ));
        let service = DaemonService::new(
            crate::service_hello("guard-rpc-test", Vec::new()),
            events.clone(),
            selections,
            blobs,
            crate::actor::DaemonActorHandle::disconnected(),
            db.clone(),
            dir.clone(),
            #[cfg(feature = "plugins")]
            None,
        );
        let service = if wired {
            service.with_bt_guard(Arc::clone(&host))
        } else {
            service
        };
        Fixture {
            service,
            host,
            events,
            db,
            dir,
        }
    }

    async fn call(
        fx: &Fixture,
        agent: bool,
        name: &str,
        params: Option<Value>,
    ) -> Result<Value, fluxdown_protocol::RpcErrorObject> {
        fx.service
            .call_inner("guard-rpc-test", agent, name, params)
            .await
    }

    async fn ok(fx: &Fixture, name: &str, params: Value) -> Value {
        call(fx, false, name, Some(params))
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error:?}"))
    }

    fn code(error: &fluxdown_protocol::RpcErrorObject) -> ApplicationErrorCode {
        error
            .data
            .as_ref()
            .map(|data| data.code)
            .expect("error code")
    }

    async fn fails(fx: &Fixture, agent: bool, name: &str, params: Value) -> ApplicationErrorCode {
        let error = call(fx, agent, name, Some(params))
            .await
            .expect_err("call must fail");
        code(&error)
    }

    fn now() -> i64 {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_secs(),
        )
        .expect("secs")
    }

    fn cloud_payload(sequence: u64, cidrs: &[&str]) -> Value {
        let now = now();
        json!({
            "format": "fluxdown-bt-guard-list/1",
            "sequence": sequence,
            "generatedAt": now,
            "expiresAt": now + 86_400,
            "entries": cidrs.iter().map(|cidr| json!({
                "cidr": cidr, "category": "community", "action": "ban",
                "reason": "listed", "expiresAt": now + 3600,
            })).collect::<Vec<_>>(),
            "clientRules": [],
            "canaries": [],
            "attribution": ["BTN-Collected-Rules by PBH-BTN, CC BY 4.0"],
        })
    }

    fn cloud_params(sequence: u64, payload: Value) -> Value {
        json!({ "envelope": "{\"payload\":\"x\"}", "sequence": sequence, "verifiedPayload": payload })
    }

    #[tokio::test]
    async fn state_accepts_missing_null_and_empty_params() {
        let fx = fixture(BtGuardConfig::default(), true).await;
        for params in [None, Some(Value::Null), Some(json!({}))] {
            let state = call(&fx, false, method::DAEMON_BT_GUARD_STATE, params)
                .await
                .expect("state");
            assert_eq!(state["enabled"], json!(true));
            assert_eq!(state["sessionActive"], json!(false));
            assert_eq!(state["counts"]["manual"], json!(0));
            assert_eq!(state["cloud"]["sequence"], json!(0));
            assert_eq!(state["stats"]["todayObserved"], json!(0));
            assert_eq!(state["filterSources"], json!([]));
        }
    }

    #[tokio::test]
    async fn unwired_service_answers_unavailable_not_unsupported() {
        let fx = fixture(BtGuardConfig::default(), false).await;
        for name in [
            method::DAEMON_BT_GUARD_STATE,
            method::DAEMON_BT_GUARD_ENTRIES_LIST,
            method::DAEMON_BT_GUARD_EVIDENCE_DRAIN,
        ] {
            assert_eq!(
                fails(&fx, true, name, json!({})).await,
                ApplicationErrorCode::Unavailable,
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn entries_add_list_export_remove_round_trip_and_persist() {
        let fx = fixture(BtGuardConfig::default(), true).await;
        let ban = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_ADD,
            json!({"cidr": "203.0.113.7/24", "action": "ban", "durationSecs": 3600, "reason": "leech"}),
        )
        .await;
        assert_eq!(ban["layer"], json!("manual"));
        assert_eq!(ban["cidr"], json!("203.0.113.0/24"), "host bits are masked");
        assert_eq!(ban["action"], json!("ban"));
        assert_eq!(ban["source"], json!("user"));
        assert_eq!(ban["reason"], json!("leech"));
        let remaining = ban["remainingSecs"].as_i64().expect("remaining");
        assert!((3590..=3600).contains(&remaining), "{remaining}");
        let allow = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_ADD,
            json!({"cidr": "198.51.100.9", "action": "allow"}),
        )
        .await;
        assert_eq!(allow["layer"], json!("allow"));
        assert!(allow.get("expiresAt").is_none(), "permanent entry");

        let all = ok(&fx, method::DAEMON_BT_GUARD_ENTRIES_LIST, json!({})).await;
        assert_eq!(all["total"], json!(2));
        assert_eq!(all["items"][0]["id"], allow["id"], "newest first");
        let manual = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_LIST,
            json!({"layer": "manual", "query": "203.0.113", "limit": 1}),
        )
        .await;
        assert_eq!(manual["total"], json!(1));
        assert_eq!(manual["items"][0]["id"], ban["id"]);
        let none = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_LIST,
            json!({"query": "no-such-thing"}),
        )
        .await;
        assert_eq!(none["total"], json!(0));

        let state = ok(&fx, method::DAEMON_BT_GUARD_STATE, json!({})).await;
        assert_eq!(state["counts"]["manual"], json!(1));
        assert_eq!(state["counts"]["allow"], json!(1));

        let export = ok(&fx, method::DAEMON_BT_GUARD_EXPORT, json!({})).await;
        assert_eq!(export["lines"], json!(2));
        let text = export["text"].as_str().expect("text");
        assert!(
            text.contains("203.0.113.0/24 # manual manual leech "),
            "{text}"
        );
        let only_allow = ok(
            &fx,
            method::DAEMON_BT_GUARD_EXPORT,
            json!({"layer": "allow"}),
        )
        .await;
        assert_eq!(only_allow["lines"], json!(1));

        // 管理调用返回前已落库。
        assert_eq!(fx.db.bt_guard_load_entries(0).await.expect("rows").len(), 2);

        let removed = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_REMOVE,
            json!({"ids": [ban["id"], 99_999]}),
        )
        .await;
        assert_eq!(removed["removed"], json!(1), "unknown ids are ignored");
        assert_eq!(fx.db.bt_guard_load_entries(0).await.expect("rows").len(), 1);
    }

    #[tokio::test]
    async fn entry_arguments_are_validated() {
        let fx = fixture(BtGuardConfig::default(), true).await;
        let invalid = ApplicationErrorCode::InvalidArgument;
        for params in [
            json!({"cidr": "", "action": "ban"}),
            json!({"cidr": "not-an-ip", "action": "ban"}),
            json!({"cidr": "1.2.3.4/33", "action": "ban"}),
            json!({"cidr": "1.2.3.4", "action": "explode"}),
            json!({"cidr": "1.2.3.4", "action": "ban", "durationSecs": 0}),
            json!({"cidr": "1.2.3.4", "action": "ban", "durationSecs": -5}),
            json!({"cidr": "1.2.3.4", "action": "ban", "reason": "x".repeat(513)}),
        ] {
            assert_eq!(
                fails(
                    &fx,
                    false,
                    method::DAEMON_BT_GUARD_ENTRIES_ADD,
                    params.clone()
                )
                .await,
                invalid,
                "{params}"
            );
        }
        for params in [
            json!({"limit": 0}),
            json!({"limit": 501}),
            json!({"layer": "nowhere"}),
        ] {
            assert_eq!(
                fails(
                    &fx,
                    false,
                    method::DAEMON_BT_GUARD_ENTRIES_LIST,
                    params.clone()
                )
                .await,
                invalid,
                "{params}"
            );
        }
        assert_eq!(
            fails(
                &fx,
                false,
                method::DAEMON_BT_GUARD_ENTRIES_REMOVE,
                json!({"ids": []})
            )
            .await,
            invalid
        );
        let too_many: Vec<i64> = (0..1001).collect();
        assert_eq!(
            fails(
                &fx,
                false,
                method::DAEMON_BT_GUARD_ENTRIES_REMOVE,
                json!({"ids": too_many})
            )
            .await,
            invalid
        );
        assert_eq!(
            fails(
                &fx,
                false,
                method::DAEMON_BT_GUARD_EXPORT,
                json!({"layer": "x"})
            )
            .await,
            invalid
        );
        assert_eq!(
            fails(
                &fx,
                false,
                method::DAEMON_BT_GUARD_EVENTS_LIST,
                json!({"limit": 0})
            )
            .await,
            invalid
        );
    }

    #[tokio::test]
    async fn cloud_apply_is_agent_only_validated_and_never_writes_config() {
        let fx = fixture(BtGuardConfig::default(), true).await;
        let params = cloud_params(5, cloud_payload(5, &["203.0.113.0/24", "198.51.100.0/24"]));

        assert_eq!(
            fails(
                &fx,
                false,
                method::DAEMON_BT_GUARD_CLOUD_APPLY,
                params.clone()
            )
            .await,
            ApplicationErrorCode::Unauthorized
        );
        assert_eq!(
            fx.host.guard().cloud_sequence(),
            0,
            "a rejected caller changes nothing"
        );

        let mut config_events = fx.events.subscribe();
        let applied = call(
            &fx,
            true,
            method::DAEMON_BT_GUARD_CLOUD_APPLY,
            Some(params.clone()),
        )
        .await
        .expect("apply");
        assert_eq!(applied["applied"], json!(true));
        assert_eq!(applied["status"]["sequence"], json!(5));
        assert_eq!(applied["status"]["entries"], json!(2));
        assert_eq!(
            applied["status"]["attribution"][0],
            json!("BTN-Collected-Rules by PBH-BTN, CC BY 4.0")
        );
        let listed = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_LIST,
            json!({"layer": "cloud"}),
        )
        .await;
        assert_eq!(listed["total"], json!(2));
        assert_eq!(fx.db.bt_guard_load_entries(0).await.expect("rows").len(), 2);
        assert_eq!(
            fx.db
                .get_config("bt_guard_cloud_cache")
                .await
                .expect("config"),
            None,
            "the daemon keeps no copy of the envelope"
        );
        while let Ok(frame) = config_events.try_recv() {
            assert!(
                !matches!(
                    frame.event,
                    ServiceEvent::Daemon(DaemonEvent::ConfigChanged(_))
                ),
                "cloud.apply does not touch the config snapshot"
            );
        }

        // 序号回滚：不是 RPC 错误，保留上一份并给出原因。
        let rolled_back = call(
            &fx,
            true,
            method::DAEMON_BT_GUARD_CLOUD_APPLY,
            Some(cloud_params(4, cloud_payload(4, &["192.0.2.0/24"]))),
        )
        .await
        .expect("rollback is a result, not an error");
        assert_eq!(rolled_back["applied"], json!(false));
        assert!(
            rolled_back["rejectedReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("rollback")),
            "{rolled_back}"
        );
        assert_eq!(rolled_back["status"]["sequence"], json!(5));
        assert_eq!(rolled_back["status"]["entries"], json!(2));

        // 参数错误才是 RPC 错误。
        let invalid = ApplicationErrorCode::InvalidArgument;
        assert_eq!(
            fails(
                &fx,
                true,
                method::DAEMON_BT_GUARD_CLOUD_APPLY,
                cloud_params(9, cloud_payload(8, &[]))
            )
            .await,
            invalid,
            "sequence must match the payload"
        );
        assert_eq!(
            fails(
                &fx,
                true,
                method::DAEMON_BT_GUARD_CLOUD_APPLY,
                json!({"envelope": "", "sequence": 6, "verifiedPayload": cloud_payload(6, &[])})
            )
            .await,
            invalid
        );
        assert_eq!(
            fails(
                &fx,
                true,
                method::DAEMON_BT_GUARD_CLOUD_APPLY,
                json!({"envelope": "x".repeat(16 * 1024 * 1024 + 1), "sequence": 6,
                       "verifiedPayload": cloud_payload(6, &[])})
            )
            .await,
            invalid
        );
        assert_eq!(
            fails(
                &fx,
                true,
                method::DAEMON_BT_GUARD_CLOUD_APPLY,
                json!({"envelope": "x", "sequence": 6, "verifiedPayload": "not an object"})
            )
            .await,
            invalid
        );
        assert_eq!(
            fails(&fx, true, method::DAEMON_BT_GUARD_CLOUD_APPLY, json!({})).await,
            invalid
        );

        // 云层条目不能直接删除。
        let id = listed["items"][0]["id"].clone();
        assert_eq!(
            fails(
                &fx,
                false,
                method::DAEMON_BT_GUARD_ENTRIES_REMOVE,
                json!({"ids": [id]})
            )
            .await,
            invalid
        );
        let still = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_LIST,
            json!({"layer": "cloud"}),
        )
        .await;
        assert_eq!(still["total"], json!(2));
    }

    #[tokio::test]
    async fn evidence_drain_is_agent_only_and_pops_the_queue() {
        let fx = fixture(
            BtGuardConfig {
                report_enabled: true,
                skip_lan: false,
                min_torrent_mib: 0,
                rule_excessive: RuleMode::Observe,
                ..BtGuardConfig::default()
            },
            true,
        )
        .await;
        assert_eq!(
            fails(
                &fx,
                false,
                method::DAEMON_BT_GUARD_EVIDENCE_DRAIN,
                json!({})
            )
            .await,
            ApplicationErrorCode::Unauthorized
        );
        // 无参数（缺省 / null）与 `{}` 都接受。
        for params in [None, Some(Value::Null), Some(json!({}))] {
            let drained = call(&fx, true, method::DAEMON_BT_GUARD_EVIDENCE_DRAIN, params)
                .await
                .expect("drain");
            assert_eq!(drained["items"], json!([]));
        }

        let ctx = GuardCtx {
            info_hash: [3; 20],
            addr: "203.0.113.50:6881".parse().expect("addr"),
            private: false,
            total_length: 1024 * 1024,
            piece_length: 1024 * 1024,
            num_pieces: 1,
            we_are_seeding: true,
        };
        for _ in 0..3 {
            fx.host.guard().on_event(
                &ctx,
                GuardEvent::Uploaded {
                    piece: 0,
                    begin: 0,
                    length: 1024 * 1024,
                },
            );
        }
        let drained = call(&fx, true, method::DAEMON_BT_GUARD_EVIDENCE_DRAIN, None)
            .await
            .expect("drain");
        let items = drained["items"].as_array().expect("items");
        assert_eq!(items.len(), 1, "{drained}");
        assert_eq!(items[0]["rule"], json!("excessive"));
        assert_eq!(items[0]["strength"], json!("strong"));
        assert_eq!(items[0]["ipGroup"], json!("203.0.113.50"));
        assert_eq!(items[0]["infoHash"], json!("03".repeat(20)));
        assert_eq!(items[0]["torrentSize"], json!(1024 * 1024));
        let again = call(&fx, true, method::DAEMON_BT_GUARD_EVIDENCE_DRAIN, None)
            .await
            .expect("drain");
        assert_eq!(again["items"], json!([]), "drained items do not come back");
    }

    #[tokio::test]
    async fn events_list_filters_and_paginates_the_persisted_log() {
        let fx = fixture(BtGuardConfig::default(), true).await;
        for cidr in ["192.0.2.1", "192.0.2.2", "192.0.2.3"] {
            ok(
                &fx,
                method::DAEMON_BT_GUARD_ENTRIES_ADD,
                json!({"cidr": cidr, "action": "choke"}),
            )
            .await;
        }
        let all = ok(&fx, method::DAEMON_BT_GUARD_EVENTS_LIST, json!({})).await;
        assert_eq!(all["total"], json!(3));
        assert_eq!(all["items"][0]["kind"], json!("entry_added"));
        assert_eq!(all["items"][0]["action"], json!("choke"));
        let first = all["items"][0]["id"].as_i64().expect("id");
        let second = all["items"][1]["id"].as_i64().expect("id");
        assert!(first > second, "newest first");
        let page = ok(
            &fx,
            method::DAEMON_BT_GUARD_EVENTS_LIST,
            json!({"offset": 2, "limit": 5}),
        )
        .await;
        assert_eq!(page["total"], json!(3));
        assert_eq!(page["items"].as_array().expect("items").len(), 1);
        let none = ok(
            &fx,
            method::DAEMON_BT_GUARD_EVENTS_LIST,
            json!({"kind": "banned"}),
        )
        .await;
        assert_eq!(none["total"], json!(0));
        let kind = ok(
            &fx,
            method::DAEMON_BT_GUARD_EVENTS_LIST,
            json!({"kind": "entry_added"}),
        )
        .await;
        assert_eq!(kind["total"], json!(3));
    }

    #[tokio::test]
    async fn events_list_filters_by_ip_group_and_rejects_garbage() {
        let fx = fixture(
            BtGuardConfig {
                skip_lan: false,
                min_torrent_mib: 0,
                rule_excessive: RuleMode::Observe,
                ..BtGuardConfig::default()
            },
            true,
        )
        .await;
        for (host, hash) in [("203.0.113.50:6881", 3u8), ("198.51.100.7:6881", 4u8)] {
            let ctx = GuardCtx {
                info_hash: [hash; 20],
                addr: host.parse().expect("addr"),
                private: false,
                total_length: 1024 * 1024,
                piece_length: 1024 * 1024,
                num_pieces: 1,
                we_are_seeding: true,
            };
            for _ in 0..3 {
                fx.host.guard().on_event(
                    &ctx,
                    GuardEvent::Uploaded {
                        piece: 0,
                        begin: 0,
                        length: 1024 * 1024,
                    },
                );
            }
        }
        let all = ok(&fx, method::DAEMON_BT_GUARD_EVENTS_LIST, json!({})).await;
        assert_eq!(all["total"], json!(2), "{all}");
        let one = ok(
            &fx,
            method::DAEMON_BT_GUARD_EVENTS_LIST,
            json!({"ipGroup": "203.0.113.50"}),
        )
        .await;
        assert_eq!(one["total"], json!(1), "{one}");
        assert_eq!(one["items"][0]["ipGroup"], json!("203.0.113.50"));
        let mapped = ok(
            &fx,
            method::DAEMON_BT_GUARD_EVENTS_LIST,
            json!({"ipGroup": "::ffff:198.51.100.7"}),
        )
        .await;
        assert_eq!(
            mapped["total"],
            json!(1),
            "v4-mapped is normalised: {mapped}"
        );
        assert_eq!(mapped["items"][0]["ipGroup"], json!("198.51.100.7"));
        let none = ok(
            &fx,
            method::DAEMON_BT_GUARD_EVENTS_LIST,
            json!({"ipGroup": "192.0.2.99"}),
        )
        .await;
        assert_eq!(none["total"], json!(0));
        assert_eq!(
            fails(
                &fx,
                false,
                method::DAEMON_BT_GUARD_EVENTS_LIST,
                json!({"ipGroup": "not-an-ip"})
            )
            .await,
            ApplicationErrorCode::InvalidArgument
        );
    }

    #[tokio::test]
    async fn entries_list_contains_ip_matches_wide_ranges_and_validates() {
        let fx = fixture(BtGuardConfig::default(), true).await;
        for (cidr, action) in [
            ("203.0.113.0/24", "ban"),
            ("203.0.113.9", "choke"),
            ("198.51.100.0/24", "ban"),
        ] {
            ok(
                &fx,
                method::DAEMON_BT_GUARD_ENTRIES_ADD,
                json!({"cidr": cidr, "action": action}),
            )
            .await;
        }
        let hit = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_LIST,
            json!({"containsIp": "203.0.113.9"}),
        )
        .await;
        assert_eq!(hit["total"], json!(2), "{hit}");
        let narrow = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_LIST,
            json!({"containsIp": "203.0.113.10"}),
        )
        .await;
        assert_eq!(narrow["total"], json!(1), "{narrow}");
        assert_eq!(narrow["items"][0]["cidr"], json!("203.0.113.0/24"));
        let stacked = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_LIST,
            json!({"containsIp": "203.0.113.9", "query": "choke-none"}),
        )
        .await;
        assert_eq!(stacked["total"], json!(0), "stacks with other filters");
        for bad in ["nope", "203.0.113.0/24", "1.2.3"] {
            assert_eq!(
                fails(
                    &fx,
                    false,
                    method::DAEMON_BT_GUARD_ENTRIES_LIST,
                    json!({"containsIp": bad})
                )
                .await,
                ApplicationErrorCode::InvalidArgument,
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn entries_add_is_idempotent_for_the_same_cidr_and_action() {
        let fx = fixture(BtGuardConfig::default(), true).await;
        let first = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_ADD,
            json!({"cidr": "203.0.113.7", "action": "ban", "durationSecs": 60, "reason": "a"}),
        )
        .await;
        let again = ok(
            &fx,
            method::DAEMON_BT_GUARD_ENTRIES_ADD,
            json!({"cidr": "203.0.113.7", "action": "ban", "durationSecs": 7200, "reason": "b"}),
        )
        .await;
        assert_eq!(again["id"], first["id"]);
        assert_eq!(again["reason"], json!("b"));
        let list = ok(&fx, method::DAEMON_BT_GUARD_ENTRIES_LIST, json!({})).await;
        assert_eq!(list["total"], json!(1), "{list}");
    }

    #[tokio::test]
    async fn filter_sources_refresh_reports_every_configured_source() {
        let empty = fixture(BtGuardConfig::default(), true).await;
        let none = ok(
            &empty,
            method::DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
            json!({}),
        )
        .await;
        assert_eq!(none["sources"], json!([]));
        // 无参数同样接受。
        assert!(
            call(
                &empty,
                false,
                method::DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
                None
            )
            .await
            .is_ok()
        );

        // 没有监听者的端口：连接被拒，状态里带错误，调用本身成功。
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("probe")
            .local_addr()
            .expect("addr")
            .port();
        let url = format!("http://127.0.0.1:{closed}/list.txt");
        let fx = fixture(
            BtGuardConfig {
                filter_urls: url.clone(),
                ..BtGuardConfig::default()
            },
            true,
        )
        .await;
        let refreshed = ok(
            &fx,
            method::DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
            json!({}),
        )
        .await;
        assert_eq!(refreshed["sources"][0]["url"], json!(url));
        assert_eq!(refreshed["sources"][0]["ok"], json!(false));
        assert!(refreshed["sources"][0]["error"].is_string(), "{refreshed}");
        let state = ok(&fx, method::DAEMON_BT_GUARD_STATE, json!({})).await;
        assert_eq!(state["filterSources"][0]["ok"], json!(false));
    }

    #[tokio::test]
    async fn management_changes_reach_subscribers_as_a_coalesced_changed_event() {
        let fx = fixture(BtGuardConfig::default(), true).await;
        let mut events = fx.events.subscribe();
        for cidr in ["192.0.2.10", "192.0.2.11", "192.0.2.12"] {
            ok(
                &fx,
                method::DAEMON_BT_GUARD_ENTRIES_ADD,
                json!({"cidr": cidr, "action": "throttle"}),
            )
            .await;
        }
        let mut changed = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while changed == 0 && std::time::Instant::now() < deadline {
            while let Ok(frame) = events.try_recv() {
                if matches!(
                    frame.event,
                    ServiceEvent::Daemon(DaemonEvent::BtGuardChanged)
                ) {
                    changed += 1;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(changed >= 1, "BtGuardChanged reaches the hub");
        tokio::time::sleep(Duration::from_millis(900)).await;
        while let Ok(frame) = events.try_recv() {
            if matches!(
                frame.event,
                ServiceEvent::Daemon(DaemonEvent::BtGuardChanged)
            ) {
                changed += 1;
            }
        }
        assert!(
            changed <= 2,
            "a burst of changes is coalesced, got {changed}"
        );
    }
}
