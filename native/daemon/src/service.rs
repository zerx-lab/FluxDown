//! daemon RPC 方法分发与 actor 命令桥接。

#[cfg(feature = "plugins")]
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(feature = "components")]
use std::sync::atomic::{AtomicBool, Ordering};

use fluxdown_engine::download_manager::{CreateGroupSpec, GroupItemSpec};
use fluxdown_engine::log_info;
use fluxdown_protocol::method;
use fluxdown_protocol::{
    ApplicationErrorCode, CREATE_TASKS_BATCH_ID_MAX_LEN, CREATE_TASKS_MAX_ITEMS,
    CdnConfigApplyParams, CdnReportAckParams, CreateGroupRequest, CreateQueueRequest,
    DaemonConfigPatch, DaemonCreateTaskParams, DaemonCreateTasksParams, DaemonCreateTasksResult,
    DaemonDeleteTasksParams, DaemonTaskIdsParams, MigrationAckParams, RpcErrorData, RpcErrorObject,
    RpcRequest, RpcResponse, SelectionResolutionDto, ServiceHello, SiteAuthCredentialDto,
    SiteAuthDeleteParams, SiteAuthMatchParams, SnapshotBody, TaskActivityQuery,
    TaskSpeedHistoryQuery,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::actor::{ActorCallError, ActorError, ActorOperation, ActorResult, DaemonActorHandle};
use crate::blob_store::{BlobKind, BlobStore};
use crate::config::demo_allows;
use crate::event_hub::DaemonEventHub;
use crate::selection::DaemonSelection;

/// `daemon.task.seedLocal` 读取现成 `.torrent` 的大小上限。
#[cfg(feature = "bt")]
const SEED_LOCAL_TORRENT_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// daemon 运行时共享服务。
pub struct DaemonService {
    hello: ServiceHello,
    events: DaemonEventHub,
    selections: DaemonSelection,
    db: fluxdown_engine::db::Db,
    /// daemon 数据目录：托管组件、插件，以及 Doctor 的数据目录写入探测。
    data_dir: PathBuf,
    #[cfg(feature = "plugins")]
    plugin_manager: Option<Arc<fluxdown_engine::plugin::PluginManager>>,
    /// 宿主自身外网请求的统一出口（组件下载、agent 自更新等的代理与插件改写）。
    egress: fluxdown_engine::egress::Egress,
    blobs: Arc<BlobStore>,
    #[cfg(feature = "components")]
    ffmpeg_installing: AtomicBool,
    #[cfg(feature = "components")]
    ytdlp_installing: AtomicBool,
    actor: DaemonActorHandle,
    demo_url: Option<String>,
    /// BT 防吸血宿主（`daemon.bt.guard.*`）；未接线时这些方法答 `unavailable`。
    #[cfg(feature = "bt")]
    bt_guard: Option<Arc<fluxdown_engine::bt_guard::BtGuardHost>>,
}

impl DaemonService {
    #[allow(
        clippy::too_many_arguments,
        reason = "the daemon composition root passes each stateful service dependency explicitly"
    )]
    #[must_use]
    pub fn new(
        hello: ServiceHello,
        events: DaemonEventHub,
        selections: DaemonSelection,
        blobs: Arc<BlobStore>,
        actor: DaemonActorHandle,
        db: fluxdown_engine::db::Db,
        data_dir: PathBuf,
        #[cfg(feature = "plugins")] plugin_manager: Option<
            Arc<fluxdown_engine::plugin::PluginManager>,
        >,
    ) -> Self {
        let egress = fluxdown_engine::egress::Egress::new(db.clone());
        #[cfg(feature = "plugins")]
        let egress = match &plugin_manager {
            Some(manager) => egress.with_plugins(Arc::clone(manager)),
            None => egress,
        };
        Self {
            hello,
            events,
            selections,
            blobs,
            db,
            data_dir,
            #[cfg(feature = "plugins")]
            plugin_manager,
            egress,
            #[cfg(feature = "components")]
            ffmpeg_installing: AtomicBool::new(false),
            #[cfg(feature = "components")]
            ytdlp_installing: AtomicBool::new(false),
            actor,
            demo_url: None,
            #[cfg(feature = "bt")]
            bt_guard: None,
        }
    }

    /// 启用演示模式：任务 / 任务组创建只放行 `demo_url`（见 [`demo_allows`]）。
    #[must_use]
    pub fn with_demo_url(mut self, demo_url: Option<String>) -> Self {
        self.demo_url = demo_url;
        self
    }

    /// 接入 BT 防吸血宿主（`daemon.bt.guard.*` 的数据面）。
    #[cfg(feature = "bt")]
    #[must_use]
    pub fn with_bt_guard(mut self, bt_guard: Arc<fluxdown_engine::bt_guard::BtGuardHost>) -> Self {
        self.bt_guard = Some(bt_guard);
        self
    }

    #[must_use]
    pub fn hello(&self) -> &ServiceHello {
        &self.hello
    }

    #[must_use]
    pub fn events(&self) -> &DaemonEventHub {
        &self.events
    }

    #[must_use]
    pub fn selections(&self) -> &DaemonSelection {
        &self.selections
    }

    #[must_use]
    pub fn blobs(&self) -> &Arc<BlobStore> {
        &self.blobs
    }

    /// 刷新仅在完整引擎初始化后可计算的动态投影。
    pub async fn initialize_dynamic_projection(&self) -> Result<(), RpcErrorObject> {
        #[cfg(feature = "plugins")]
        self.publish_plugins().await?;
        #[cfg(feature = "components")]
        self.publish_component_statuses().await;
        let stats = self.runtime_stats().await;
        self.events
            .publish(fluxdown_protocol::DaemonEvent::RuntimeStatsChanged(stats));
        Ok(())
    }

    /// 从线性化投影读取任务，用于二进制文件端点。
    #[must_use]
    pub fn task(&self, task_id: &str) -> Option<fluxdown_protocol::TaskDto> {
        self.events
            .inspect(|snapshot| snapshot.task(task_id).cloned())
    }

    /// 分发 hello 后的连接级 RPC 调用。
    pub async fn call(
        &self,
        connection_id: &str,
        is_local_agent: bool,
        request: RpcRequest,
    ) -> RpcResponse {
        let id = request.id.clone();
        match self
            .call_inner(
                connection_id,
                is_local_agent,
                &request.method,
                request.params,
            )
            .await
        {
            Ok(result) => RpcResponse::success(id, result),
            Err(error) => RpcResponse::failure(id, error),
        }
    }

    pub(crate) async fn call_inner(
        &self,
        connection_id: &str,
        is_local_agent: bool,
        method_name: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcErrorObject> {
        match method_name {
            method::SYSTEM_PING => Ok(json!({ "ok": true })),
            // 线上快照只带头；任务表与队列位置表经 `system.taskPage` 分页（见协议 `table_sync`）。
            method::SYSTEM_SNAPSHOT => to_value(self.events.wire_snapshot()),
            method::SYSTEM_TASK_PAGE => {
                let params = parse_params::<fluxdown_protocol::TablePageParams>(params)?;
                let page = self.events.task_page(&params).map_err(|_| {
                    invalid_argument("cursor", "cursor is not valid for this section")
                })?;
                to_value(page)
            }
            method::DAEMON_TASK_LIST => {
                let params = parse_optional_params::<fluxdown_protocol::TaskListParams>(params)?;
                to_value(
                    self.events
                        .inspect(|snapshot| list_tasks(snapshot, &params))?,
                )
            }
            method::DAEMON_TASK_GET => {
                let params = parse_params::<IdParams>(params)?;
                let task = self
                    .events
                    .inspect(|snapshot| snapshot.task(&params.id).cloned())
                    .ok_or_else(not_found)?;
                to_value(task)
            }
            method::DAEMON_TASK_ACTIVITY => {
                let query = parse_params::<TaskActivityQuery>(params)?;
                let page = self
                    .db
                    .query_task_activity(&fluxdown_engine::task_activity::TaskActivityQuery {
                        task_id: query.task_id,
                        before_id: query.before_id,
                        after_id: query.after_id,
                        limit: query.limit,
                    })
                    .await
                    .map_err(|error| match error {
                        fluxdown_engine::db::DbError::InvalidActivityQuery(message) => {
                            invalid_argument("params", message)
                        }
                        other => internal_error(other.to_string()),
                    })?;
                to_value(fluxdown_protocol::TaskActivityPage {
                    entries: page
                        .entries
                        .into_iter()
                        .map(fluxdown_engine_protocol::task_activity_to_dto)
                        .collect(),
                    has_more: page.has_more,
                    oldest_id: page.oldest_id,
                    newest_id: page.newest_id,
                    truncated: page.truncated,
                })
            }
            method::DAEMON_TASK_SPEED_HISTORY => {
                let query = parse_params::<TaskSpeedHistoryQuery>(params)?;
                if query.task_id.is_empty() {
                    return Err(invalid_argument("taskId", "taskId is required"));
                }
                let history = self
                    .db
                    .task_speed_history(&query.task_id)
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                to_value(history.map(|history| {
                    fluxdown_engine_protocol::task_speed_history_to_dto(query.task_id, history)
                }))
            }
            method::DAEMON_TASK_CREATE => self.create_task(params).await,
            method::DAEMON_TASK_SEED_LOCAL => self.seed_local(params).await,
            method::DAEMON_TASK_BT_STATUS => {
                let params = parse_params::<fluxdown_protocol::DaemonBtStatusParams>(params)?;
                match self
                    .actor
                    .execute(ActorOperation::BtStatus {
                        task_id: params.task_id,
                    })
                    .await
                {
                    Ok(ActorResult::BtStatus(status)) => to_value(*status),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_TASK_TORRENT_SHARE => {
                let params = parse_params::<fluxdown_protocol::DaemonTorrentShareParams>(params)?;
                match self
                    .actor
                    .execute(ActorOperation::TorrentShare {
                        task_id: params.task_id,
                        include_torrent: params.include_torrent,
                    })
                    .await
                {
                    Ok(ActorResult::TorrentShare(share)) => to_value(*share),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_TASK_CREATE_MANY => self.create_tasks(params).await,
            method::DAEMON_TASK_PAUSE => {
                let params = parse_params::<IdParams>(params)?;
                self.execute_unit(ActorOperation::PauseTask { task_id: params.id })
                    .await
            }
            method::DAEMON_TASK_RESUME => {
                let params = parse_params::<IdParams>(params)?;
                self.execute_unit(ActorOperation::ResumeTask { task_id: params.id })
                    .await
            }
            method::DAEMON_TASK_RENAME => {
                let params = parse_params::<RenameParams>(params)?;
                self.execute_unit(ActorOperation::RenameTask {
                    task_id: params.task_id,
                    file_name: params.file_name,
                })
                .await
            }
            method::DAEMON_TASK_CHANGE_URL => {
                let params = parse_params::<fluxdown_protocol::ChangeTaskUrlParams>(params)?;
                self.demo_guard(&params.url)?;
                self.execute_unit(ActorOperation::ChangeTaskUrl {
                    task_id: params.task_id,
                    url: params.url,
                })
                .await
            }
            method::DAEMON_TASK_SET_SEGMENTS => {
                let params = parse_params::<fluxdown_protocol::SetTaskSegmentsParams>(params)?;
                if params.segments > fluxdown_protocol::MAX_TASK_SEGMENTS {
                    return Err(invalid_argument(
                        "segments",
                        &format!(
                            "segments must be at most {}",
                            fluxdown_protocol::MAX_TASK_SEGMENTS
                        ),
                    ));
                }
                self.execute_unit(ActorOperation::SetTaskSegments {
                    task_id: params.task_id,
                    segments: params.segments.max(0),
                })
                .await
            }
            method::DAEMON_TASK_DELETE => {
                let params = parse_params::<DeleteParams>(params)?;
                self.execute_unit(ActorOperation::DeleteTask {
                    task_id: params.id,
                    delete_files: params.delete_files,
                })
                .await
            }
            method::DAEMON_TASK_PAUSE_ALL => self.execute_unit(ActorOperation::PauseAll).await,
            method::DAEMON_TASK_RESUME_ALL => self.execute_unit(ActorOperation::ResumeAll).await,
            method::DAEMON_TASK_RESCAN => self.execute_unit(ActorOperation::RescanFiles).await,
            method::DAEMON_TASK_SET_SEED_LIMITS => {
                let params = parse_params::<SeedLimitsParams>(params)?;
                self.execute_unit(ActorOperation::SetTaskSeedLimits {
                    task_id: params.task_id,
                    ratio_limit_milli: params.ratio_limit_milli,
                    post_ratio_limit_milli: params.post_ratio_limit_milli,
                    seed_time_limit_minutes: params.seed_time_limit_minutes,
                    inactive_time_limit_minutes: params.inactive_time_limit_minutes,
                    upload_limit_bps: params.upload_limit_bps,
                })
                .await
            }
            method::DAEMON_TASK_VERIFY => {
                let params = parse_params::<IdParams>(params)?;
                match self
                    .actor
                    .execute(ActorOperation::VerifyTask { task_id: params.id })
                    .await
                {
                    Ok(ActorResult::TaskVerify(result)) => to_value(result),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_TASK_PAUSE_MANY => {
                let params = parse_params::<DaemonTaskIdsParams>(params)?;
                self.execute_unit(ActorOperation::PauseTasks {
                    task_ids: params.task_ids,
                })
                .await
            }
            method::DAEMON_TASK_RESUME_MANY => {
                let params = parse_params::<DaemonTaskIdsParams>(params)?;
                self.execute_unit(ActorOperation::ResumeTasks {
                    task_ids: params.task_ids,
                })
                .await
            }
            method::DAEMON_TASK_DELETE_MANY => {
                let params = parse_params::<DaemonDeleteTasksParams>(params)?;
                self.execute_unit(ActorOperation::DeleteTasks {
                    task_ids: params.task_ids,
                    delete_files: params.delete_files,
                })
                .await
            }
            method::DAEMON_QUEUE_LIST => {
                to_value(self.events.inspect(|snapshot| snapshot.queues.clone()))
            }
            method::DAEMON_QUEUE_CREATE => {
                let request = parse_params::<CreateQueueRequest>(params)?;
                self.execute_unit(ActorOperation::CreateQueue {
                    name: request.name,
                    speed_limit_kbps: request.speed_limit_kbps,
                    upload_limit_kbps: request.upload_limit_kbps,
                    max_concurrent: request.max_concurrent,
                    default_save_dir: request.default_save_dir,
                    default_segments: request.default_segments,
                    default_user_agent: request.default_user_agent,
                })
                .await
            }
            method::DAEMON_QUEUE_UPDATE => {
                let params = parse_params::<UpdateQueueParams>(params)?;
                self.execute_unit(ActorOperation::UpdateQueue {
                    queue_id: params.queue_id,
                    name: params.request.name,
                    speed_limit_kbps: params.request.speed_limit_kbps,
                    upload_limit_kbps: params.request.upload_limit_kbps,
                    max_concurrent: params.request.max_concurrent,
                    default_save_dir: params.request.default_save_dir,
                    default_segments: params.request.default_segments,
                    default_user_agent: params.request.default_user_agent,
                })
                .await
            }
            method::DAEMON_QUEUE_DELETE => {
                self.queue_id_operation(params, QueueAction::Delete).await
            }
            method::DAEMON_QUEUE_START => self.queue_id_operation(params, QueueAction::Start).await,
            method::DAEMON_QUEUE_STOP => self.queue_id_operation(params, QueueAction::Stop).await,
            method::DAEMON_QUEUE_SCHEDULE => {
                let params = parse_params::<QueueScheduleParams>(params)?;
                validate_queue_schedule(&params)?;
                self.execute_unit(ActorOperation::SetQueueSchedule {
                    queue_id: params.queue_id,
                    enabled: params.enabled,
                    start_time: params.start_time,
                    stop_time: params.stop_time,
                    days: params.days,
                })
                .await
            }
            method::DAEMON_QUEUE_REORDER => {
                let params = parse_params::<QueueOrderParams>(params)?;
                self.execute_unit(ActorOperation::ReorderQueue {
                    queue_id: params.queue_id,
                    task_ids: params.task_ids,
                })
                .await
            }
            method::DAEMON_QUEUE_MOVE_TASK => {
                let params = parse_params::<MoveTaskParams>(params)?;
                self.execute_unit(ActorOperation::MoveToQueue {
                    task_id: params.task_id,
                    queue_id: params.queue_id,
                })
                .await
            }
            method::DAEMON_QUEUE_BOOST => {
                let params = parse_params::<IdParams>(params)?;
                self.execute_unit(ActorOperation::Boost { task_id: params.id })
                    .await
            }
            method::DAEMON_GROUP_LIST => {
                to_value(self.events.inspect(|snapshot| snapshot.groups.clone()))
            }
            method::DAEMON_GROUP_RESOLVE_PREVIEW => {
                let request = parse_params::<fluxdown_protocol::ResolvePreviewRequest>(params)?;
                #[cfg(feature = "plugins")]
                {
                    let source_url = request.url.clone();
                    match self
                        .actor
                        .execute(ActorOperation::ResolvePreview {
                            url: request.url,
                            cookies: request.cookies,
                            referrer: request.referrer,
                            user_agent: request.user_agent,
                            extra_headers: request.extra_headers,
                        })
                        .await
                    {
                        Ok(ActorResult::ResolvePreview(outcome)) => {
                            to_value(fluxdown_protocol::ResolvePreviewResponse {
                                name: outcome.name,
                                source_url,
                                error: outcome.error,
                                plugin_id: outcome.plugin_id,
                                items: outcome
                                    .items
                                    .into_iter()
                                    .map(manifest_item_to_preview_dto)
                                    .collect(),
                            })
                        }
                        Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                        Err(error) => Err(actor_error(error)),
                    }
                }
                #[cfg(not(feature = "plugins"))]
                {
                    let _ = request;
                    Err(unsupported_error("resolve preview requires plugins"))
                }
            }
            method::DAEMON_GROUP_CREATE => {
                let request = parse_params::<CreateGroupRequest>(params)?;
                self.demo_guard(&request.source_url)?;
                if !request.items.is_empty() {
                    self.demo_forbid("group items")?;
                }
                let spec = self.group_spec(request);
                match self
                    .actor
                    .execute(ActorOperation::CreateGroup {
                        spec: Box::new(spec),
                    })
                    .await
                {
                    Ok(ActorResult::Created(group_id)) => to_value(json!({ "groupId": group_id })),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_GROUP_PAUSE => self.group_id_operation(params, GroupAction::Pause).await,
            method::DAEMON_GROUP_RESUME => {
                self.group_id_operation(params, GroupAction::Resume).await
            }
            method::DAEMON_GROUP_DELETE => {
                let params = parse_params::<DeleteParams>(params)?;
                self.execute_unit(ActorOperation::DeleteGroup {
                    group_id: params.id,
                    delete_files: params.delete_files,
                })
                .await
            }
            method::DAEMON_CONFIG_GET => {
                to_value(self.events.inspect(|snapshot| snapshot.config.clone()))
            }
            method::DAEMON_CONFIG_PATCH => {
                let patch = parse_params::<DaemonConfigPatch>(params)?;
                let values = crate::config::validate_config_patch(&patch.values)
                    .map_err(|error| invalid_argument("values", &error.to_string()))?;
                match self
                    .actor
                    .execute(ActorOperation::PatchConfig {
                        expected_revision: patch.expected_revision,
                        values,
                    })
                    .await
                {
                    Ok(ActorResult::Config(snapshot)) => to_value(snapshot),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_CONFIG_PROXY_TEST => {
                let params = parse_params::<ProxyTestParams>(params)?;
                match self
                    .actor
                    .execute(ActorOperation::TestProxy {
                        proxy_type: params.proxy_type,
                        host: params.host,
                        port: params.port,
                        username: params.username,
                        password: params.password,
                    })
                    .await
                {
                    Ok(ActorResult::ProxyLatency(latency_ms)) => {
                        to_value(json!({ "latencyMs": latency_ms }))
                    }
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_CONFIG_PROXY_ROUTE => {
                let params = parse_params::<fluxdown_protocol::ProxyRouteRequest>(params)?;
                let mut map = self
                    .db
                    .get_all_config()
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                if let Some(rules) = params.rules {
                    map.insert(
                        fluxdown_engine::proxy_rules::PROXY_RULES_KEY.to_owned(),
                        rules,
                    );
                }
                let url = params.url;
                let response = tokio::task::spawn_blocking(move || {
                    let raw = map
                        .get(fluxdown_engine::proxy_rules::PROXY_RULES_KEY)
                        .map(String::as_str)
                        .unwrap_or_default();
                    let issues = fluxdown_engine::proxy_rules::validate(raw);
                    let url = url.trim();
                    let explanation = (!url.is_empty()).then(|| {
                        fluxdown_engine::proxy_rules::explain(
                            &fluxdown_engine::proxy_config::ProxyConfig::from_config_map(&map),
                            url,
                        )
                    });
                    fluxdown_engine_protocol::proxy_route_to_dto(issues, explanation)
                })
                .await
                .map_err(|error| internal_error(format!("{error:#}")))?;
                to_value(response)
            }
            method::DAEMON_CONFIG_CONN_POLICY => {
                let raw = self
                    .db
                    .get_config("domain_conn_caps")
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?
                    .unwrap_or_default();
                let count = fluxdown_engine::segment_coordinator::count_domain_conn_policies(&raw);
                to_value(fluxdown_protocol::ConnPolicySummaryDto {
                    domain_count: u64::try_from(count).unwrap_or(u64::MAX),
                })
            }
            method::DAEMON_CONFIG_SYSTEM_PROXY => {
                let detected =
                    tokio::task::spawn_blocking(fluxdown_engine::proxy_config::detect_system_proxy)
                        .await
                        .map_err(|error| internal_error(format!("{error:#}")))?;
                to_value(match detected {
                    Ok(Some(cfg)) => fluxdown_protocol::SystemProxyDto {
                        detected: true,
                        proxy_type: cfg.proxy_type.as_str().to_owned(),
                        host: cfg.host,
                        port: cfg.port,
                        no_list: cfg.no_proxy_list,
                    },
                    Ok(None) => fluxdown_protocol::SystemProxyDto::default(),
                    Err(error) => {
                        log_info!("[daemon] system proxy detection error: {error:#}");
                        fluxdown_protocol::SystemProxyDto::default()
                    }
                })
            }
            method::DAEMON_CONFIG_CLEAR_CONN_POLICY => {
                match self.actor.execute(ActorOperation::ClearConnPolicy).await {
                    Ok(ActorResult::ConnPolicy(summary)) => to_value(summary),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_SITE_AUTH_LIST => {
                let json = self
                    .db
                    .get_config(fluxdown_engine::site_auth::SITE_AUTH_CONFIG_KEY)
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?
                    .unwrap_or_default();
                to_value(crate::actor::site_auth_entries(
                    &fluxdown_engine::site_auth::parse_store(&json),
                ))
            }
            method::DAEMON_SITE_AUTH_DELETE => {
                let params = parse_params::<SiteAuthDeleteParams>(params)?;
                let site = crate::actor::normalize_site(&params.site)
                    .ok_or_else(|| invalid_argument("site", "invalid site"))?;
                self.site_auth_operation(ActorOperation::SiteAuthDelete { site })
                    .await
            }
            method::DAEMON_SITE_AUTH_GET => {
                let params = parse_params::<fluxdown_protocol::SiteAuthGetParams>(params)?;
                let site = crate::actor::normalize_site(&params.site)
                    .ok_or_else(|| invalid_argument("site", "invalid site"))?;
                let json = self
                    .db
                    .get_config(fluxdown_engine::site_auth::SITE_AUTH_CONFIG_KEY)
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?
                    .unwrap_or_default();
                to_value(
                    fluxdown_engine::site_auth::parse_store(&json)
                        .remove(&site)
                        .map(|credential| SiteAuthCredentialDto {
                            site,
                            user: credential.user,
                            pass: credential.pass,
                        }),
                )
            }
            method::DAEMON_SITE_AUTH_SAVE => {
                let request = parse_params::<fluxdown_protocol::SiteAuthSaveRequest>(params)?;
                let site = crate::actor::normalize_site(&request.site)
                    .ok_or_else(|| invalid_argument("site", "invalid site"))?;
                let user = request.user.trim().to_owned();
                if user.is_empty() {
                    return Err(invalid_argument("user", "user is required"));
                }
                match self
                    .actor
                    .execute(ActorOperation::SiteAuthSave {
                        site,
                        user,
                        pass: request.pass,
                    })
                    .await
                {
                    Ok(ActorResult::SiteAuthEntry(entry)) => to_value(entry),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_SITE_AUTH_CLEAR => {
                self.site_auth_operation(ActorOperation::SiteAuthClear)
                    .await
            }
            method::DAEMON_SITE_AUTH_MATCH => {
                let params = parse_params::<SiteAuthMatchParams>(params)?;
                let json = self
                    .db
                    .get_config(fluxdown_engine::site_auth::SITE_AUTH_CONFIG_KEY)
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?
                    .unwrap_or_default();
                to_value(match_site_auth(&json, &params.url))
            }
            method::DAEMON_RSS_LIST_SOURCES => {
                match self.actor.execute(ActorOperation::RssListSources).await {
                    Ok(ActorResult::RssSources(sources)) => to_value(
                        sources
                            .into_iter()
                            .map(fluxdown_engine_protocol::rss_source_info_to_dto)
                            .collect::<Vec<_>>(),
                    ),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_RSS_GET_ITEMS => {
                let params = parse_params::<RssSourceIdParams>(params)?;
                let items = self
                    .db
                    .load_rss_items(
                        &params.source_id,
                        fluxdown_engine::rss::MAX_ITEMS_PER_SOURCE,
                    )
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?
                    .into_iter()
                    .map(fluxdown_engine_protocol::rss_item_info_to_dto)
                    .collect::<Vec<_>>();
                to_value(items)
            }
            method::DAEMON_RSS_CREATE_SOURCE => {
                self.demo_forbid("RSS sources")?;
                let source = parse_params::<fluxdown_protocol::RssSourceDto>(params)?;
                match self
                    .actor
                    .execute(ActorOperation::RssCreate {
                        source: Box::new(fluxdown_engine_protocol::rss_source_dto_to_engine(
                            source,
                        )),
                    })
                    .await
                {
                    Ok(ActorResult::Created(source_id)) => {
                        to_value(json!({ "sourceId": source_id }))
                    }
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_RSS_UPDATE_SOURCE => {
                self.demo_forbid("RSS sources")?;
                let source = parse_params::<fluxdown_protocol::RssSourceDto>(params)?;
                let source_id = source.source_id.clone();
                if source_id.trim().is_empty() {
                    return Err(invalid_argument("sourceId", "sourceId is required"));
                }
                match self
                    .actor
                    .execute(ActorOperation::RssUpdate {
                        source: Box::new(fluxdown_engine_protocol::rss_source_dto_to_engine(
                            source,
                        )),
                    })
                    .await
                {
                    Ok(ActorResult::Boolean(true)) => Ok(json!({ "ok": true })),
                    Ok(ActorResult::Boolean(false)) => Err(not_found()),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_RSS_DELETE_SOURCE => {
                let params = parse_params::<RssSourceIdParams>(params)?;
                self.rss_boolean_operation(ActorOperation::RssDelete {
                    source_id: params.source_id,
                })
                .await
            }
            method::DAEMON_RSS_REFRESH_SOURCE => {
                let params = parse_params::<RssSourceIdParams>(params)?;
                self.rss_boolean_operation(ActorOperation::RssRefresh {
                    source_id: params.source_id,
                })
                .await
            }
            method::DAEMON_RSS_ITEM_ACTION => {
                let params = parse_params::<RssItemActionParams>(params)?;
                self.execute_unit(ActorOperation::RssItemAction {
                    source_id: params.source_id,
                    guid: params.guid,
                    action: params.action,
                })
                .await
            }
            method::DAEMON_RSS_VALIDATE => {
                let request = parse_params::<fluxdown_protocol::RssValidateRequest>(params)?;
                match self
                    .actor
                    .execute(ActorOperation::RssValidate {
                        provider_id: request.provider_id,
                        provider_config: request.provider_config,
                        url: request.url,
                        cookies: request.cookies,
                        user_agent: request.user_agent,
                        proxy_url: request.proxy_url,
                    })
                    .await
                {
                    Ok(ActorResult::RssValidation(outcome)) => {
                        to_value(fluxdown_protocol::RssValidateResponse {
                            url: outcome.url,
                            feed_title: outcome.feed_title,
                            items: outcome
                                .items
                                .into_iter()
                                .map(fluxdown_engine_protocol::rss_item_info_to_dto)
                                .collect(),
                            error: outcome.error,
                        })
                    }
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_LINK_PROBE => {
                let request = parse_params::<fluxdown_protocol::LinkProbeParams>(params)?;
                match self
                    .actor
                    .execute(ActorOperation::LinkProbe { url: request.url })
                    .await
                {
                    Ok(ActorResult::LinkProbe(result)) => to_value(result),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_RUNTIME_STATS => to_value(self.runtime_stats().await),
            method::DAEMON_FS_LIST => {
                let params = parse_optional_params::<FsListParams>(params)?;
                self.list_directories(params.path).await.and_then(to_value)
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_LIST => to_value(self.list_plugins().await?),
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_SNIFF_RULES => {
                let manager = self.plugin_manager()?;
                let plugins = manager.list().await;
                let result = fluxdown_engine::plugin::sniff_pack::query(&plugins, &self.db)
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                to_value(fluxdown_protocol::PluginSniffRulesDto {
                    sequence: result.sequence,
                    epoch: result.epoch,
                    pack: result.pack,
                })
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_PAGE_SCRIPTS => {
                let manager = self.plugin_manager()?;
                let plugins = manager.list().await;
                let result = fluxdown_engine::plugin::page_scripts::query(&plugins, &self.db)
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                to_value(fluxdown_protocol::PluginPageScriptsDto {
                    sequence: result.sequence,
                    epoch: result.epoch,
                    scripts: result
                        .scripts
                        .into_iter()
                        .map(|s| fluxdown_protocol::PluginPageScriptDto {
                            identity: s.identity,
                            plugin_name: s.plugin_name,
                            plugin_version: s.plugin_version,
                            id: s.id,
                            title: s.title,
                            matches: s.matches,
                            exclude_matches: s.exclude_matches,
                            run_at: s.run_at,
                            world: s.world,
                            all_frames: s.all_frames,
                            sha256: s.sha256,
                            bytes: s.bytes,
                        })
                        .collect(),
                })
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_PAGE_SCRIPT_SOURCE => {
                let params =
                    parse_params::<fluxdown_protocol::PluginPageScriptSourceParams>(params)?;
                let manager = self.plugin_manager()?;
                let plugins = manager.list().await;
                let source = fluxdown_engine::plugin::page_scripts::source(
                    &plugins,
                    &params.identity,
                    &params.id,
                )
                .await
                .map_err(|error| invalid_argument("id", &error.to_string()))?;
                to_value(fluxdown_protocol::PluginPageScriptSourceDto {
                    identity: params.identity,
                    id: params.id,
                    sha256: source.sha256,
                    code: source.code,
                })
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_AUTH => {
                let request = parse_params::<fluxdown_protocol::PluginAuthRequest>(params)?;
                let result = self
                    .plugin_manager()?
                    .authenticate(
                        &request.identity,
                        fluxdown_engine::plugin::AuthRequest {
                            action: request.action,
                            site: request.site,
                            auth_ref: request.auth_ref,
                            session_id: request.session_id,
                            input: request.input,
                        },
                    )
                    .await
                    .map_err(|error| invalid_argument("auth", &error.to_string()))?;
                to_value(fluxdown_protocol::PluginAuthResponse {
                    status: result.status,
                    session_id: result.session_id,
                    challenge: result.challenge,
                    challenge_type: result.challenge_type,
                    message: result.message,
                    auth_ref: result.auth_ref,
                })
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_SET_ENABLED => {
                let params = parse_params::<PluginEnabledParams>(params)?;
                let manager = self.plugin_manager()?;
                manager
                    .set_enabled(&params.identity, params.enabled)
                    .await
                    .map_err(|error| invalid_argument("identity", &error.to_string()))?;
                Ok(json!({ "ok": true }))
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_CONSENT => {
                let params = parse_params::<fluxdown_protocol::PluginConsentParams>(params)?;
                self.plugin_manager()?
                    .grant_consent(&params.identity, &params.permissions, &params.network_hosts)
                    .await
                    .map_err(|error| match error {
                        fluxdown_engine::plugin::PluginError::ManifestInvalid(message) => {
                            invalid_argument("permissions", &message)
                        }
                        other => invalid_argument("identity", &other.to_string()),
                    })?;
                Ok(json!({ "ok": true }))
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_AUDIT_LIST => {
                let params = parse_params::<fluxdown_protocol::PluginAuditListParams>(params)?;
                let (rows, next_before_id) = self
                    .plugin_manager()?
                    .audit_list(
                        params.identity.as_deref(),
                        params.before_id,
                        params.limit.unwrap_or(200),
                    )
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                to_value(fluxdown_protocol::PluginAuditListDto {
                    entries: rows
                        .into_iter()
                        .map(|row| fluxdown_protocol::PluginAuditEntryDto {
                            id: row.id,
                            at_ms: row.at_ms,
                            identity: row.plugin_id,
                            origin: row.origin,
                            action: row.action,
                            target: row.target,
                            outcome: row.outcome,
                            detail: row.detail,
                        })
                        .collect(),
                    next_before_id,
                })
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_AUDIT_CLEAR => {
                let params = parse_params::<fluxdown_protocol::PluginAuditClearParams>(params)?;
                self.plugin_manager()?
                    .audit_clear(params.identity.as_deref())
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                Ok(json!({ "ok": true }))
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_AUDIT_RECORD => {
                let params = parse_params::<fluxdown_protocol::PluginAuditRecordParams>(params)?;
                let entries = params
                    .entries
                    .into_iter()
                    .map(
                        |entry| fluxdown_engine::plugin::manager::DesktopAuditEntry {
                            identity: entry.identity,
                            action: entry.action,
                            target: entry.target,
                            outcome: entry.outcome,
                            detail: entry.detail,
                        },
                    )
                    .collect();
                self.plugin_manager()?
                    .audit_record(entries)
                    .await
                    .map_err(|error| invalid_argument("entries", &error.to_string()))?;
                Ok(json!({ "ok": true }))
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_UPDATE_SETTINGS => {
                let params = parse_params::<PluginSettingsParams>(params)?;
                let entries = params.entries.into_iter().collect::<Vec<_>>();
                self.plugin_manager()?
                    .update_settings(&params.identity, &entries)
                    .await
                    .map_err(|error| match error {
                        fluxdown_engine::plugin::PluginError::InvalidSetting { key, message } => {
                            invalid_argument(&key, &message)
                        }
                        other => invalid_argument("entries", &other.to_string()),
                    })?;
                Ok(json!({ "ok": true }))
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_INSTALL => {
                let params = parse_params::<PluginInstallParams>(params)?;
                let bytes = self
                    .blobs
                    .read(&params.blob_id, BlobKind::Plugin)
                    .await
                    .map_err(|error| invalid_argument("blobId", &error.to_string()))?;
                let identity = self
                    .plugin_manager()?
                    .install_from_zip(bytes)
                    .await
                    .map_err(|error| plugin_package_error("blobId", &error))?;
                self.blobs
                    .consume(&params.blob_id, BlobKind::Plugin)
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                let missing_components = self.plugin_missing_components(&identity).await;
                to_value(fluxdown_protocol::InstalledPlugin {
                    identity,
                    missing_components,
                })
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_INSTALL_DEV => {
                let request = parse_params::<fluxdown_protocol::InstallPluginDevRequest>(params)?;
                let identity = self
                    .plugin_manager()?
                    .install_dev(std::path::Path::new(&request.dir_path))
                    .await
                    .map_err(|error| plugin_package_error("dirPath", &error))?;
                let missing_components = self.plugin_missing_components(&identity).await;
                to_value(fluxdown_protocol::InstalledPlugin {
                    identity,
                    missing_components,
                })
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_RELOAD_DEV => {
                let params = parse_params::<PluginIdentityParams>(params)?;
                let result = self.plugin_manager()?.reload_dev(&params.identity).await;
                result.map_err(|error| plugin_package_error("identity", &error))?;
                let missing_components = self.plugin_missing_components(&params.identity).await;
                to_value(fluxdown_protocol::InstalledPlugin {
                    identity: params.identity,
                    missing_components,
                })
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_UNINSTALL => {
                let params = parse_params::<PluginIdentityParams>(params)?;
                self.plugin_manager()?
                    .uninstall(&params.identity)
                    .await
                    .map_err(|error| invalid_argument("identity", &error.to_string()))?;
                Ok(json!({ "ok": true }))
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_MARKET_LIST => {
                let index = self
                    .market_client()
                    .await?
                    .fetch_index()
                    .await
                    .map_err(|error| market_error(&error))?;
                // 被标记 vulnerable / malicious 的版本：已装的同版本插件强制禁用。
                let revoked: std::collections::HashSet<(String, String)> = index
                    .entries
                    .iter()
                    .filter(|entry| matches!(entry.yanked.as_str(), "vulnerable" | "malicious"))
                    .map(|entry| (entry.plugin_id.clone(), entry.version.clone()))
                    .collect();
                // 撤回结果由引擎经 `PluginsChanged` 广播，这里无需再发。
                self.plugin_manager()?
                    .apply_market_revocations(&revoked)
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                to_value(
                    index
                        .entries
                        .into_iter()
                        .map(fluxdown_engine_protocol::market_entry_to_dto)
                        .collect::<Vec<_>>(),
                )
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_MARKET_INSTALL => {
                let request = parse_params::<fluxdown_protocol::MarketInstallRequest>(params)?;
                let identity = self
                    .market_client()
                    .await?
                    .install_latest(&request.plugin_id, request.version.as_deref())
                    .await
                    .map_err(|error| market_error(&error))?;
                let missing_components = self.plugin_missing_components(&identity).await;
                to_value(fluxdown_protocol::InstalledPlugin {
                    identity,
                    missing_components,
                })
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_IGNORE_RETRY => {
                let params = parse_params::<TaskIdParams>(params)?;
                if self.task(&params.task_id).is_none() {
                    return Err(not_found());
                }
                self.plugin_manager()?
                    .clear_task_resolver(&params.task_id)
                    .await
                    .map_err(|error| match error {
                        fluxdown_engine::plugin::PluginError::InvalidSetting { key, message } => {
                            invalid_argument(&key, &message)
                        }
                        other => internal_error(format!("{other:#}")),
                    })?;
                self.execute_unit(ActorOperation::ResumeTask {
                    task_id: params.task_id,
                })
                .await
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_SITE_DEFAULTS => {
                let defaults = self
                    .plugin_manager()?
                    .site_defaults()
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                to_value(
                    defaults
                        .into_iter()
                        .map(|entry| fluxdown_protocol::PluginSiteDefaultDto {
                            site: entry.site,
                            identity: entry.identity,
                            candidates: entry.candidates,
                            automatic: entry.automatic,
                        })
                        .collect::<Vec<_>>(),
                )
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_SET_SITE_DEFAULT => {
                let params = parse_params::<fluxdown_protocol::PluginSetSiteDefaultParams>(params)?;
                self.plugin_manager()?
                    .set_site_default(&params.site, params.identity.as_deref())
                    .await
                    .map_err(|error| match error {
                        fluxdown_engine::plugin::PluginError::InvalidSetting { key, message } => {
                            invalid_argument(&key, &message)
                        }
                        other => internal_error(format!("{other:#}")),
                    })?;
                Ok(json!({ "ok": true }))
            }
            #[cfg(feature = "plugins")]
            method::DAEMON_PLUGIN_VIEW_OPEN
            | method::DAEMON_PLUGIN_VIEW_EVENT
            | method::DAEMON_PLUGIN_VIEW_CLOSE
            | method::DAEMON_PLUGIN_VIEW_SNAPSHOT
            | method::DAEMON_PLUGIN_VIEW_KEEPALIVE
            | method::DAEMON_PLUGIN_COMMAND
            | method::DAEMON_PLUGIN_INVOKE
            | method::DAEMON_PLUGIN_PAGE_INVOKE => self.plugin_host_call(method_name, params).await,
            #[cfg(not(feature = "plugins"))]
            method::DAEMON_PLUGIN_VIEW_OPEN
            | method::DAEMON_PLUGIN_VIEW_EVENT
            | method::DAEMON_PLUGIN_VIEW_CLOSE
            | method::DAEMON_PLUGIN_VIEW_SNAPSHOT
            | method::DAEMON_PLUGIN_VIEW_KEEPALIVE
            | method::DAEMON_PLUGIN_COMMAND
            | method::DAEMON_PLUGIN_INVOKE
            | method::DAEMON_PLUGIN_PAGE_INVOKE => {
                Err(unsupported_error("daemon was built without plugin support"))
            }
            #[cfg(not(feature = "plugins"))]
            method::DAEMON_PLUGIN_LIST
            | method::DAEMON_PLUGIN_SNIFF_RULES
            | method::DAEMON_PLUGIN_PAGE_SCRIPTS
            | method::DAEMON_PLUGIN_PAGE_SCRIPT_SOURCE
            | method::DAEMON_PLUGIN_SET_ENABLED
            | method::DAEMON_PLUGIN_UPDATE_SETTINGS
            | method::DAEMON_PLUGIN_INSTALL
            | method::DAEMON_PLUGIN_INSTALL_DEV
            | method::DAEMON_PLUGIN_RELOAD_DEV
            | method::DAEMON_PLUGIN_UNINSTALL
            | method::DAEMON_PLUGIN_MARKET_LIST
            | method::DAEMON_PLUGIN_MARKET_INSTALL
            | method::DAEMON_PLUGIN_IGNORE_RETRY
            | method::DAEMON_PLUGIN_SITE_DEFAULTS
            | method::DAEMON_PLUGIN_SET_SITE_DEFAULT
            | method::DAEMON_PLUGIN_CONSENT
            | method::DAEMON_PLUGIN_AUDIT_LIST
            | method::DAEMON_PLUGIN_AUDIT_CLEAR
            | method::DAEMON_PLUGIN_AUDIT_RECORD => {
                Err(unsupported_error("daemon was built without plugin support"))
            }
            #[cfg(feature = "components")]
            method::DAEMON_COMPONENT_GET => {
                let params = parse_params::<fluxdown_protocol::ComponentParams>(params)?;
                to_value(self.component_status(params.component).await)
            }
            #[cfg(feature = "components")]
            method::DAEMON_COMPONENT_LIST_VERSIONS => {
                let params = parse_params::<fluxdown_protocol::ComponentParams>(params)?;
                to_value(self.component_versions(params.component).await?)
            }
            #[cfg(feature = "components")]
            method::DAEMON_COMPONENT_INSTALL => {
                let params = parse_params::<fluxdown_protocol::ComponentInstallParams>(params)?;
                self.install_component(params.component, params.version)
                    .await
            }
            #[cfg(feature = "components")]
            method::DAEMON_COMPONENT_UNINSTALL => {
                let params = parse_params::<fluxdown_protocol::ComponentParams>(params)?;
                self.uninstall_component(params.component).await
            }
            #[cfg(not(feature = "components"))]
            method::DAEMON_COMPONENT_GET
            | method::DAEMON_COMPONENT_LIST_VERSIONS
            | method::DAEMON_COMPONENT_INSTALL
            | method::DAEMON_COMPONENT_UNINSTALL => Err(unsupported_error(
                "daemon was built without component support",
            )),
            method::DAEMON_WEBHOOK_GET => {
                match self.actor.execute(ActorOperation::WebhookDeliveries).await {
                    Ok(ActorResult::WebhookDeliveries(deliveries)) => {
                        to_value(fluxdown_protocol::WebhookDeliveriesResponse {
                            deliveries: deliveries
                                .into_iter()
                                .map(fluxdown_engine_protocol::webhook_delivery_to_dto)
                                .collect(),
                            presets: fluxdown_engine::webhook::preset_catalog()
                                .into_iter()
                                .map(fluxdown_engine_protocol::webhook_preset_to_dto)
                                .collect(),
                            variables: fluxdown_engine::webhook::TEMPLATE_VARIABLES
                                .iter()
                                .map(|variable| (*variable).to_owned())
                                .collect(),
                        })
                    }
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_WEBHOOK_CLEAR_DELIVERIES => {
                self.execute_unit(ActorOperation::WebhookClear).await
            }
            method::DAEMON_WEBHOOK_SIMULATE => {
                match self.actor.execute(ActorOperation::WebhookSimulate).await {
                    Ok(ActorResult::WebhookSimulation(dispatched)) => {
                        to_value(fluxdown_protocol::WebhookSimulateResponse {
                            dispatched: i32::try_from(dispatched).unwrap_or(i32::MAX),
                        })
                    }
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_WEBHOOK_TEST => {
                let request = parse_params::<fluxdown_protocol::WebhookTestRequest>(params)?;
                match self
                    .actor
                    .execute(ActorOperation::WebhookTest {
                        endpoint_json: request.endpoint.to_string(),
                    })
                    .await
                {
                    Ok(ActorResult::WebhookTest(delivery)) => {
                        to_value(fluxdown_protocol::WebhookTestResponse {
                            success: delivery.success,
                            status_code: delivery.status_code,
                            latency_ms: delivery.latency_ms,
                            error: delivery.error.clone(),
                        })
                    }
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_CDN_REPORTS_PEEK => {
                match self.actor.execute(ActorOperation::CdnReportsPeek).await {
                    Ok(ActorResult::CdnLease(lease)) => to_value(lease),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_CDN_REPORTS_ACK => {
                let params = parse_params::<CdnReportAckParams>(params)?;
                match self
                    .actor
                    .execute(ActorOperation::CdnReportsAck {
                        batch_id: params.batch_id,
                    })
                    .await
                {
                    Ok(ActorResult::Boolean(true)) => Ok(json!({ "ok": true })),
                    Ok(ActorResult::Boolean(false)) => Err(RpcErrorObject::application(
                        "CDN report lease does not match",
                        RpcErrorData::new(ApplicationErrorCode::Conflict, false),
                    )),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_CDN_CONFIG_APPLY => {
                let params = parse_params::<CdnConfigApplyParams>(params)?;
                self.execute_unit(ActorOperation::CdnConfigApply {
                    values: params.values,
                })
                .await
            }
            method::DAEMON_EGRESS_PLAN => {
                let params = parse_params::<fluxdown_protocol::EgressPlanParams>(params)?;
                if params.url.trim().is_empty() {
                    return Err(invalid_argument("url", "url is required"));
                }
                let plan = self
                    .egress
                    .plan(&params.url, params.verified_payload)
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                to_value(fluxdown_engine_protocol::egress_plan_to_dto(plan))
            }
            #[cfg(feature = "bt")]
            method::DAEMON_BT_GUARD_STATE
            | method::DAEMON_BT_GUARD_ENTRIES_LIST
            | method::DAEMON_BT_GUARD_ENTRIES_ADD
            | method::DAEMON_BT_GUARD_ENTRIES_REMOVE
            | method::DAEMON_BT_GUARD_EVENTS_LIST
            | method::DAEMON_BT_GUARD_EXPORT
            | method::DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH
            | method::DAEMON_BT_GUARD_CLOUD_APPLY
            | method::DAEMON_BT_GUARD_EVIDENCE_DRAIN => {
                self.bt_guard_call(is_local_agent, method_name, params)
                    .await
            }
            #[cfg(not(feature = "bt"))]
            method::DAEMON_BT_GUARD_STATE
            | method::DAEMON_BT_GUARD_ENTRIES_LIST
            | method::DAEMON_BT_GUARD_ENTRIES_ADD
            | method::DAEMON_BT_GUARD_ENTRIES_REMOVE
            | method::DAEMON_BT_GUARD_EVENTS_LIST
            | method::DAEMON_BT_GUARD_EXPORT
            | method::DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH
            | method::DAEMON_BT_GUARD_CLOUD_APPLY
            | method::DAEMON_BT_GUARD_EVIDENCE_DRAIN => Err(unsupported_error(
                "daemon was built without BitTorrent support",
            )),
            method::DAEMON_BT_TRACKER_SUBSCRIPTION_REFRESH => {
                match self
                    .actor
                    .execute(ActorOperation::RefreshTrackerSubscription)
                    .await
                {
                    Ok(ActorResult::TrackerRefresh(response)) => to_value(response),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_ED2K_SERVER_SUBSCRIPTION_REFRESH => {
                match self
                    .actor
                    .execute(ActorOperation::RefreshEd2kServerSubscription)
                    .await
                {
                    Ok(ActorResult::Ed2kRefresh(response)) => to_value(response),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_DIAGNOSTICS_DESCRIBE => {
                let (tasks, queues, groups, config_revision, components) =
                    self.events.inspect(|snapshot| {
                        (
                            snapshot.tasks.len(),
                            snapshot.queues.len(),
                            snapshot.groups.len(),
                            snapshot.config.revision,
                            snapshot.components.clone(),
                        )
                    });
                Ok(json!({
                    "service": self.hello,
                    "tasks": tasks,
                    "queues": queues,
                    "groups": groups,
                    "configRevision": config_revision,
                    "logDir": fluxdown_engine::logger::log_dir().display().to_string(),
                    "components": components,
                }))
            }
            method::DAEMON_DIAGNOSTICS_PROBE => {
                require_local_agent(is_local_agent)?;
                let params =
                    parse_optional_params::<fluxdown_protocol::DiagnosticsProbeParams>(params)?;
                to_value(self.doctor_probe(params).await)
            }
            method::DAEMON_DIAGNOSTICS_FIX_COMPONENT => {
                require_local_agent(is_local_agent)?;
                let params = parse_params::<fluxdown_protocol::ComponentRepairParams>(params)?;
                let components =
                    crate::doctor_probe::component_paths(&self.db, &self.data_dir).await;
                match crate::doctor_probe::fix_component(components, &self.data_dir, &params.name)
                    .await
                {
                    Ok(probe) => to_value(probe),
                    Err(
                        error @ (crate::doctor_probe::FixComponentError::NotFound(_)
                        | crate::doctor_probe::FixComponentError::NotManaged(_)),
                    ) => Err(invalid_argument("name", &error.to_string())),
                    Err(error) => Err(internal_error(error.to_string())),
                }
            }
            method::DAEMON_DIAGNOSTICS_PREPARE_LOG_EXPORT => {
                let mut snapshot = self.events.snapshot();
                if let SnapshotBody::Daemon(body) = &mut snapshot.body {
                    crate::log_redact::redact_snapshot(body);
                }
                let bytes = serde_json::to_vec(&snapshot)
                    .map_err(|error| internal_error(error.to_string()))?;
                let export_id = self
                    .blobs
                    .put(BlobKind::Logs, &bytes)
                    .await
                    .map_err(|error| internal_error(format!("{error:#}")))?;
                Ok(json!({ "exportId": export_id }))
            }
            method::DAEMON_MIGRATION_LINK_EXPORT => {
                require_local_agent(is_local_agent)?;
                match self
                    .actor
                    .execute(ActorOperation::MigrationLinkExport)
                    .await
                {
                    Ok(ActorResult::LinkMigration(export)) => to_value(export),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_MIGRATION_LINK_ACK => {
                require_local_agent(is_local_agent)?;
                let params = parse_params::<MigrationAckParams>(params)?;
                self.execute_unit(ActorOperation::MigrationLinkAck {
                    revision: params.revision,
                })
                .await
            }
            method::DAEMON_MIGRATION_GATEWAY_EXPORT => {
                require_local_agent(is_local_agent)?;
                match self
                    .actor
                    .execute(ActorOperation::MigrationGatewayExport)
                    .await
                {
                    Ok(ActorResult::GatewayMigration(export)) => to_value(export),
                    Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
                    Err(error) => Err(actor_error(error)),
                }
            }
            method::DAEMON_MIGRATION_GATEWAY_ACK => {
                require_local_agent(is_local_agent)?;
                let params = parse_params::<MigrationAckParams>(params)?;
                self.execute_unit(ActorOperation::MigrationGatewayAck {
                    revision: params.revision,
                })
                .await
            }
            method::DAEMON_SELECTION_SUBSCRIBE => {
                self.selections.subscribe(connection_id.to_owned());
                Ok(json!({ "ok": true }))
            }
            method::DAEMON_SELECTION_UNSUBSCRIBE => {
                self.selections.unsubscribe(connection_id);
                Ok(json!({ "ok": true }))
            }
            method::DAEMON_SELECTION_RESOLVE => {
                let resolution = parse_params::<SelectionResolutionDto>(params)?;
                self.selections.resolve(resolution).map_err(|error| {
                    let code = match error {
                        crate::selection::SelectionError::NotFound => {
                            ApplicationErrorCode::NotFound
                        }
                        crate::selection::SelectionError::Conflict => {
                            ApplicationErrorCode::Conflict
                        }
                        crate::selection::SelectionError::InvalidOutcome
                        | crate::selection::SelectionError::HlsCancel => {
                            ApplicationErrorCode::InvalidArgument
                        }
                    };
                    RpcErrorObject::application(error.to_string(), RpcErrorData::new(code, false))
                })?;
                Ok(json!({ "ok": true }))
            }
            _ => Err(RpcErrorObject::application(
                "method is not supported by this daemon build",
                RpcErrorData::new(ApplicationErrorCode::Unsupported, false),
            )),
        }
    }

    async fn create_task(&self, params: Option<Value>) -> Result<Value, RpcErrorObject> {
        let params = parse_params::<DaemonCreateTaskParams>(params)?;
        self.demo_guard(&params.request.url)?;
        if params.torrent_blob_id.is_some()
            || params.request.torrent_b64.is_some()
            || params.request.audio_url.is_some()
        {
            self.demo_forbid("torrent / audio attachments")?;
        }
        if params.torrent_blob_id.is_some() && params.request.torrent_b64.is_some() {
            return Err(invalid_argument(
                "torrentBlobId",
                "torrentBlobId and torrentB64 are mutually exclusive",
            ));
        }
        let bytes = match params.torrent_blob_id.as_deref() {
            Some(blob_id) => {
                self.blobs
                    .read(blob_id, BlobKind::Torrent)
                    .await
                    .map_err(|error| {
                        RpcErrorObject::application(
                            error.to_string(),
                            RpcErrorData::new(ApplicationErrorCode::NotFound, false),
                        )
                    })?
            }
            None => Vec::new(),
        };
        let result = self
            .actor
            .execute(ActorOperation::CreateTask {
                request: Box::new(params.request),
                torrent_file_bytes: bytes,
                hint_file_size: params.hint_file_size.filter(|size| *size > 0).unwrap_or(0),
                name_hint: params.name_hint.unwrap_or_default(),
                unattended: params.unattended,
                category_route: params.category_route,
            })
            .await;
        match result {
            Ok(ActorResult::Created(task_id)) => {
                if let Some(blob_id) = params.torrent_blob_id {
                    self.blobs
                        .consume(&blob_id, BlobKind::Torrent)
                        .await
                        .map_err(|error| internal_error(format!("{error:#}")))?;
                }
                to_value(json!({ "taskId": task_id }))
            }
            Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
            Err(error) => Err(actor_error(error)),
        }
    }

    /// `daemon.task.seedLocal`：现成 `.torrent`（有大小上限）从 daemon 主机读取，否则在 actor 之外
    /// 为内容路径生成种子（哈希耗时与数据量成正比，故走慢方法通道），再交 actor 登记并开始做种。
    async fn seed_local(&self, params: Option<Value>) -> Result<Value, RpcErrorObject> {
        let params = parse_params::<fluxdown_protocol::DaemonSeedLocalParams>(params)?;
        if params.path.trim().is_empty() {
            return Err(invalid_argument("path", "path is required"));
        }
        #[cfg(feature = "bt")]
        let torrent_bytes = match params
            .torrent_path
            .as_deref()
            .filter(|path| !path.is_empty())
        {
            Some(torrent_path) => {
                let metadata = tokio::fs::metadata(torrent_path)
                    .await
                    .map_err(|error| invalid_argument("torrentPath", &error.to_string()))?;
                if !metadata.is_file() || metadata.len() > SEED_LOCAL_TORRENT_MAX_BYTES {
                    return Err(invalid_argument(
                        "torrentPath",
                        "torrentPath must be a .torrent file no larger than 32 MiB",
                    ));
                }
                tokio::fs::read(torrent_path)
                    .await
                    .map_err(|error| invalid_argument("torrentPath", &error.to_string()))?
            }
            None => fluxdown_engine::bt_local_seed::build_torrent(
                std::path::Path::new(&params.path),
                params
                    .trackers
                    .iter()
                    .map(|tracker| tracker.trim().to_owned())
                    .filter(|tracker| !tracker.is_empty())
                    .collect(),
            )
            .await
            .map_err(|error| match error {
                fluxdown_engine::bt_local_seed::LocalSeedError::InvalidArgument(message) => {
                    invalid_argument("path", &message)
                }
                fluxdown_engine::bt_local_seed::LocalSeedError::Operation(message) => {
                    internal_error(message)
                }
                error @ fluxdown_engine::bt_local_seed::LocalSeedError::Duplicate { .. } => {
                    internal_error(error.to_string())
                }
            })?,
        };
        #[cfg(not(feature = "bt"))]
        let torrent_bytes = Vec::new();
        match self
            .actor
            .execute(ActorOperation::SeedLocal {
                path: params.path,
                torrent_bytes,
                unlimited: params.unlimited,
            })
            .await
        {
            Ok(ActorResult::SeedLocal(result)) => to_value(result),
            Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
            Err(error) => Err(actor_error(error)),
        }
    }

    /// `daemon.task.createMany`：校验整块参数（任何一项违规都是整体 `InvalidArgument`），
    /// 逐条过演示模式守卫，然后交给 actor 在一个事务内落库。逐项拒绝（thunder 解码失败、
    /// BT 被禁用、checksum 畸形…）以 `results[i].error` 返回，不影响同块其它项。
    async fn create_tasks(&self, params: Option<Value>) -> Result<Value, RpcErrorObject> {
        let params = parse_params::<DaemonCreateTasksParams>(params)?;
        validate_create_tasks(&params)?;
        for item in &params.items {
            self.demo_guard(&item.url)?;
        }
        let DaemonCreateTasksParams {
            batch_id,
            common,
            items,
            category_route,
        } = params;
        match self
            .actor
            .execute(ActorOperation::CreateTasks {
                batch_id,
                common: Box::new(common),
                items,
                category_route,
            })
            .await
        {
            Ok(ActorResult::CreatedMany(results)) => to_value(DaemonCreateTasksResult { results }),
            Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
            Err(error) => Err(actor_error(error)),
        }
    }

    async fn execute_unit(&self, operation: ActorOperation) -> Result<Value, RpcErrorObject> {
        match self.actor.execute(operation).await {
            Ok(ActorResult::Unit) => Ok(json!({ "ok": true })),
            Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
            Err(error) => Err(actor_error(error)),
        }
    }

    /// 站点凭据表写操作：返回更新后的脱敏列表。
    async fn site_auth_operation(
        &self,
        operation: ActorOperation,
    ) -> Result<Value, RpcErrorObject> {
        match self.actor.execute(operation).await {
            Ok(ActorResult::SiteAuth(entries)) => to_value(entries),
            Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
            Err(error) => Err(actor_error(error)),
        }
    }

    async fn queue_id_operation(
        &self,
        params: Option<Value>,
        action: QueueAction,
    ) -> Result<Value, RpcErrorObject> {
        let params = parse_params::<IdParams>(params)?;
        if matches!(action, QueueAction::Stop) {
            validate_queue_stop(&params.id)?;
        }
        let operation = match action {
            QueueAction::Delete => ActorOperation::DeleteQueue {
                queue_id: params.id,
            },
            QueueAction::Start => ActorOperation::StartQueue {
                queue_id: params.id,
            },
            QueueAction::Stop => ActorOperation::StopQueue {
                queue_id: params.id,
            },
        };
        self.execute_unit(operation).await
    }

    async fn group_id_operation(
        &self,
        params: Option<Value>,
        action: GroupAction,
    ) -> Result<Value, RpcErrorObject> {
        let params = parse_params::<IdParams>(params)?;
        let operation = match action {
            GroupAction::Pause => ActorOperation::PauseGroup {
                group_id: params.id,
            },
            GroupAction::Resume => ActorOperation::ResumeGroup {
                group_id: params.id,
            },
        };
        self.execute_unit(operation).await
    }

    /// Doctor 动态探测：daemon 自己的目录在前、调用方追加的目录在后，按路径去重后并发真实写入；
    /// 同时运行已解析到路径的外部组件。
    async fn doctor_probe(
        &self,
        params: fluxdown_protocol::DiagnosticsProbeParams,
    ) -> fluxdown_protocol::DiagnosticsProbeResult {
        let mut targets = self
            .events
            .inspect(|snapshot| crate::doctor_probe::daemon_targets(snapshot, &self.data_dir));
        targets.extend(params.extra_dirs);
        let components = crate::doctor_probe::component_paths(&self.db, &self.data_dir).await;
        let (storage, components) = tokio::join!(
            crate::doctor_probe::probe_storage(crate::doctor_probe::dedupe_targets(targets)),
            crate::doctor_probe::probe_components(components, &self.data_dir),
        );
        fluxdown_protocol::DiagnosticsProbeResult {
            storage,
            components,
        }
    }

    /// 演示模式守卫：仅放行指定 URL（所有任务 / 任务组创建入口共用）。
    fn demo_guard(&self, url: &str) -> Result<(), RpcErrorObject> {
        if demo_allows(self.demo_url.as_deref(), url) {
            Ok(())
        } else {
            Err(invalid_argument(
                "url",
                "demo mode: only the designated demo file can be downloaded",
            ))
        }
    }

    /// 演示模式下整体禁用的入口（RSS 自动下载、外部附件等会绕过 URL 守卫）。
    fn demo_forbid(&self, what: &str) -> Result<(), RpcErrorObject> {
        if self.demo_url.is_some() {
            Err(unsupported_error(&format!("demo mode: {what} is disabled")))
        } else {
            Ok(())
        }
    }

    fn group_spec(&self, request: CreateGroupRequest) -> CreateGroupSpec {
        let base_save_dir = if request.save_dir.trim().is_empty() {
            self.events.inspect(|snapshot| {
                snapshot
                    .config
                    .values
                    .get("default_save_dir")
                    .filter(|value| !value.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(fluxdown_engine::user_dirs::download_dir_or_cwd)
            })
        } else {
            request.save_dir
        };
        CreateGroupSpec {
            source_url: request.source_url,
            group_name: request.group_name,
            base_save_dir,
            queue_id: request.queue_id,
            segments: request.segments,
            cookies: request.cookies,
            referrer: request.referrer,
            user_agent: request.user_agent,
            proxy_url: request.proxy_url,
            extra_headers: request.extra_headers,
            ignore_tls_errors: request.ignore_tls_errors,
            start_paused: request.start_paused,
            items: request
                .items
                .into_iter()
                .map(|item| GroupItemSpec {
                    resolver_item: item.resolver_item,
                    file_name: item.file_name,
                    rel_path: item.rel_path,
                    size: item.size,
                })
                .collect(),
            resolver_plugin_id: request.resolver_plugin_id,
        }
    }
}

impl DaemonService {
    async fn rss_boolean_operation(
        &self,
        operation: ActorOperation,
    ) -> Result<Value, RpcErrorObject> {
        match self.actor.execute(operation).await {
            Ok(ActorResult::Boolean(true)) => Ok(json!({ "ok": true })),
            Ok(ActorResult::Boolean(false)) => Err(not_found()),
            Ok(_) => Err(internal_error("unexpected actor result".to_owned())),
            Err(error) => Err(actor_error(error)),
        }
    }
}

#[cfg(feature = "plugins")]
impl DaemonService {
    fn plugin_manager(
        &self,
    ) -> Result<&Arc<fluxdown_engine::plugin::PluginManager>, RpcErrorObject> {
        self.plugin_manager
            .as_ref()
            .ok_or_else(|| unsupported_error("plugin manager is unavailable"))
    }

    async fn list_plugins(&self) -> Result<Vec<fluxdown_protocol::PluginDto>, RpcErrorObject> {
        Ok(self
            .plugin_manager()?
            .list()
            .await
            .into_iter()
            .map(fluxdown_engine_protocol::plugin_info_to_dto)
            .collect())
    }

    async fn publish_plugins(&self) -> Result<(), RpcErrorObject> {
        let plugins = self.list_plugins().await?;
        self.events
            .publish(fluxdown_protocol::DaemonEvent::PluginsChanged(plugins));
        Ok(())
    }

    async fn market_client(&self) -> Result<fluxdown_engine::plugin::MarketClient, RpcErrorObject> {
        let config = self
            .db
            .get_all_config()
            .await
            .map_err(|error| internal_error(format!("{error:#}")))?;
        let sources = fluxdown_engine::plugin::MarketClient::source_config(&config);
        let proxy = fluxdown_engine::proxy_config::ProxyConfig::from_config_map(&config);
        fluxdown_engine::plugin::MarketClient::new(
            self.plugin_manager()?.clone(),
            self.db.clone(),
            sources,
            &proxy,
        )
        .map_err(|error| market_error(&error))
    }

    async fn plugin_missing_components(&self, identity: &str) -> Vec<String> {
        let Some(manager) = self.plugin_manager.as_ref() else {
            return Vec::new();
        };
        let permissions = manager.permissions_of(identity).await;
        fluxdown_engine::plugin::dependencies::missing_components(
            &self.db,
            &self.data_dir,
            &permissions,
        )
        .await
    }
}

#[cfg(feature = "components")]
impl DaemonService {
    async fn component_status(
        &self,
        component: fluxdown_protocol::ComponentKind,
    ) -> fluxdown_protocol::ComponentStatusDto {
        match component {
            fluxdown_protocol::ComponentKind::Ffmpeg => {
                fluxdown_protocol::ComponentStatusDto::Ffmpeg(
                    fluxdown_engine_protocol::ffmpeg_status_to_dto(
                        fluxdown_engine::components::ffmpeg_status(&self.db, &self.data_dir).await,
                    ),
                )
            }
            fluxdown_protocol::ComponentKind::Ytdlp => {
                fluxdown_protocol::ComponentStatusDto::Ytdlp(
                    fluxdown_engine_protocol::ytdlp_status_to_dto(
                        fluxdown_engine::components::ytdlp_status(&self.db, &self.data_dir).await,
                    ),
                )
            }
        }
    }

    async fn component_versions(
        &self,
        component: fluxdown_protocol::ComponentKind,
    ) -> Result<fluxdown_protocol::ComponentVersions, RpcErrorObject> {
        match component {
            fluxdown_protocol::ComponentKind::Ffmpeg => {
                fluxdown_engine::components::list_versions(&self.db, &self.egress)
                    .await
                    .map(fluxdown_engine_protocol::ffmpeg_versions_to_dto)
                    .map_err(|error| internal_error(error.to_string()))
            }
            fluxdown_protocol::ComponentKind::Ytdlp => {
                fluxdown_engine::components::list_ytdlp_versions(&self.db, &self.egress)
                    .await
                    .map(fluxdown_engine_protocol::ytdlp_versions_to_dto)
                    .map_err(|error| internal_error(error.to_string()))
            }
        }
    }

    async fn install_component(
        &self,
        component: fluxdown_protocol::ComponentKind,
        version: Option<String>,
    ) -> Result<Value, RpcErrorObject> {
        let flag = self.component_install_flag(component);
        if flag.swap(true, Ordering::SeqCst) {
            return Err(conflict_error("component install already in progress"));
        }
        let _guard = ComponentInstallGuard(flag);
        let component_name = component_name(component);
        let progress_events = self.events.clone();
        let progress = move |downloaded: u64, total: u64| {
            progress_events.publish(fluxdown_protocol::DaemonEvent::Engine(
                fluxdown_protocol::WsServerMsg::ComponentProgress {
                    component: component_name.to_owned(),
                    downloaded_bytes: i64::try_from(downloaded).unwrap_or(i64::MAX),
                    total_bytes: i64::try_from(total).unwrap_or(i64::MAX),
                },
            ));
        };
        let outcome = match component {
            fluxdown_protocol::ComponentKind::Ffmpeg => {
                fluxdown_engine::components::install_ffmpeg(
                    &self.db,
                    &self.data_dir,
                    &self.egress,
                    version.as_deref(),
                    &progress,
                )
                .await
                .map(|_| ())
            }
            fluxdown_protocol::ComponentKind::Ytdlp => fluxdown_engine::components::install_ytdlp(
                &self.db,
                &self.data_dir,
                &self.egress,
                version.as_deref(),
                &progress,
            )
            .await
            .map(|_| ()),
        };
        match outcome {
            Ok(()) => {
                self.events.publish(fluxdown_protocol::DaemonEvent::Engine(
                    fluxdown_protocol::WsServerMsg::ComponentResult {
                        component: component_name.to_owned(),
                        ok: true,
                        message: "installed".to_owned(),
                    },
                ));
                self.publish_component_statuses().await;
                Ok(json!({ "ok": true }))
            }
            Err(error) => {
                self.events.publish(fluxdown_protocol::DaemonEvent::Engine(
                    fluxdown_protocol::WsServerMsg::ComponentResult {
                        component: component_name.to_owned(),
                        ok: false,
                        message: error.to_string(),
                    },
                ));
                self.publish_component_statuses().await;
                Err(internal_error(error.to_string()))
            }
        }
    }

    async fn uninstall_component(
        &self,
        component: fluxdown_protocol::ComponentKind,
    ) -> Result<Value, RpcErrorObject> {
        if self
            .component_install_flag(component)
            .load(Ordering::SeqCst)
        {
            return Err(conflict_error("component install is in progress"));
        }
        match component {
            fluxdown_protocol::ComponentKind::Ffmpeg => {
                fluxdown_engine::components::uninstall_ffmpeg(&self.db, &self.data_dir).await
            }
            fluxdown_protocol::ComponentKind::Ytdlp => {
                fluxdown_engine::components::uninstall_ytdlp(&self.db, &self.data_dir).await
            }
        }
        .map_err(|error| internal_error(error.to_string()))?;
        self.publish_component_statuses().await;
        Ok(json!({ "ok": true }))
    }

    fn component_install_flag(&self, component: fluxdown_protocol::ComponentKind) -> &AtomicBool {
        match component {
            fluxdown_protocol::ComponentKind::Ffmpeg => &self.ffmpeg_installing,
            fluxdown_protocol::ComponentKind::Ytdlp => &self.ytdlp_installing,
        }
    }

    async fn publish_component_statuses(&self) {
        let statuses = vec![
            self.component_status(fluxdown_protocol::ComponentKind::Ffmpeg)
                .await,
            self.component_status(fluxdown_protocol::ComponentKind::Ytdlp)
                .await,
        ];
        self.events
            .publish(fluxdown_protocol::DaemonEvent::ComponentsChanged(statuses));
    }
}

#[cfg(feature = "components")]
struct ComponentInstallGuard<'a>(&'a AtomicBool);

#[cfg(feature = "components")]
impl Drop for ComponentInstallGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(feature = "components")]
fn component_name(component: fluxdown_protocol::ComponentKind) -> &'static str {
    match component {
        fluxdown_protocol::ComponentKind::Ffmpeg => "ffmpeg",
        fluxdown_protocol::ComponentKind::Ytdlp => "ytdlp",
    }
}

impl DaemonService {
    async fn runtime_stats(&self) -> fluxdown_protocol::DaemonRuntimeStatsDto {
        let (save_dir, active_tasks, pending_tasks, stats) = self.events.inspect(|snapshot| {
            let save_dir = snapshot
                .config
                .values
                .get("default_save_dir")
                .cloned()
                .filter(|path| !path.trim().is_empty())
                .unwrap_or_else(fluxdown_engine::user_dirs::download_dir_or_cwd);
            let active = snapshot
                .tasks
                .iter()
                .filter(|task| matches!(task.status, 1 | 5))
                .count();
            let pending = snapshot
                .tasks
                .iter()
                .filter(|task| task.status == 0)
                .count();
            (save_dir, active, pending, snapshot.runtime_stats.clone())
        });
        fluxdown_protocol::DaemonRuntimeStatsDto {
            active_tasks: u32::try_from(active_tasks).unwrap_or(u32::MAX),
            pending_tasks: u32::try_from(pending_tasks).unwrap_or(u32::MAX),
            total_download_bps: stats.total_download_bps,
            retry_pending_tasks: stats.retry_pending_tasks,
            total_upload_bps: stats.total_upload_bps,
            disk_free_bytes: fluxdown_engine::disk_space::available_space_checked(
                std::path::PathBuf::from(&save_dir),
            )
            .await,
            save_dir,
        }
    }

    async fn list_directories(
        &self,
        requested_path: String,
    ) -> Result<fluxdown_protocol::FsListResponse, RpcErrorObject> {
        let base = if requested_path.trim().is_empty() {
            let (configured, fallback) = self.events.inspect(|snapshot| {
                (
                    snapshot
                        .config
                        .values
                        .get("default_save_dir")
                        .cloned()
                        .unwrap_or_default(),
                    snapshot.runtime_stats.save_dir.clone(),
                )
            });
            if configured.trim().is_empty() {
                fallback
            } else {
                configured
            }
        } else {
            requested_path
        };
        let base_path = std::path::Path::new(&base);
        let parent = base_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.to_string_lossy().into_owned());
        let mut dirs = Vec::new();
        // 受限用户（NAS 套件）浏览到未授权目录时 EACCES：不整体失败，返回
        // `denied=true` 让 UI 提示授权，parent 仍可用以继续导航。
        let mut denied = false;
        match tokio::fs::read_dir(base_path).await {
            Ok(mut entries) => {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let Ok(file_type) = entry.file_type().await else {
                        continue;
                    };
                    if !file_type.is_dir() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    dirs.push(fluxdown_protocol::FsEntry {
                        name,
                        path: entry.path().to_string_lossy().into_owned(),
                    });
                }
            }
            Err(e) => denied = e.kind() == std::io::ErrorKind::PermissionDenied,
        }
        dirs.sort_by_key(|entry| entry.name.to_lowercase());
        Ok(fluxdown_protocol::FsListResponse {
            path: base,
            parent,
            dirs,
            denied,
        })
    }
}

#[cfg(feature = "plugins")]
fn manifest_item_to_preview_dto(
    item: fluxdown_engine::model::ManifestItemInfo,
) -> fluxdown_protocol::PreviewItemDto {
    fluxdown_protocol::PreviewItemDto {
        id: item.id,
        name: item.name,
        path: item.path,
        size: item.size,
        variants: item
            .variants
            .into_iter()
            .map(|variant| fluxdown_protocol::PreviewVariantDto {
                id: variant.id,
                label: variant.label,
                size: variant.size,
            })
            .collect(),
    }
}

#[derive(Default, Deserialize)]
struct FsListParams {
    #[serde(default)]
    path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RssSourceIdParams {
    source_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RssItemActionParams {
    source_id: String,
    #[serde(default)]
    guid: String,
    action: String,
}

#[cfg(feature = "plugins")]
#[derive(Deserialize)]
struct PluginIdentityParams {
    identity: String,
}

#[cfg(feature = "plugins")]
#[derive(Deserialize)]
struct PluginEnabledParams {
    identity: String,
    enabled: bool,
}

#[cfg(feature = "plugins")]
#[derive(Deserialize)]
struct PluginSettingsParams {
    identity: String,
    entries: HashMap<String, String>,
}

#[cfg(feature = "plugins")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginInstallParams {
    blob_id: String,
}

#[cfg(feature = "plugins")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskIdParams {
    task_id: String,
}

#[derive(Deserialize)]
struct IdParams {
    #[serde(alias = "taskId", alias = "queueId", alias = "groupId")]
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameParams {
    task_id: String,
    file_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteParams {
    #[serde(alias = "taskId", alias = "groupId")]
    id: String,
    #[serde(default)]
    delete_files: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeedLimitsParams {
    task_id: String,
    ratio_limit_milli: i64,
    post_ratio_limit_milli: i64,
    seed_time_limit_minutes: i64,
    inactive_time_limit_minutes: i64,
    upload_limit_bps: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateQueueParams {
    queue_id: String,
    #[serde(flatten)]
    request: CreateQueueRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueScheduleParams {
    queue_id: String,
    enabled: bool,
    #[serde(default)]
    start_time: String,
    #[serde(default)]
    stop_time: String,
    #[serde(default)]
    days: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueOrderParams {
    queue_id: String,
    task_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveTaskParams {
    task_id: String,
    #[serde(default)]
    queue_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxyTestParams {
    proxy_type: String,
    host: String,
    port: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

enum QueueAction {
    Delete,
    Start,
    Stop,
}
enum GroupAction {
    Pause,
    Resume,
}

/// 按链接站点键（与引擎 `apply_site_auth` 同一 `site_key`）查已保存凭据；非 http(s)
/// 链接、无匹配或凭据表损坏都返回 `None`。
fn match_site_auth(store_json: &str, url: &str) -> Option<SiteAuthCredentialDto> {
    let site = fluxdown_engine::site_auth::site_key(url.trim())?;
    let credential = fluxdown_engine::site_auth::parse_store(store_json).remove(&site)?;
    Some(SiteAuthCredentialDto {
        site,
        user: credential.user,
        pass: credential.pass,
    })
}

fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T, RpcErrorObject> {
    let Some(params) = params else {
        return Err(invalid_argument("params", "params are required"));
    };
    serde_json::from_value(params).map_err(|error| invalid_argument("params", &error.to_string()))
}

fn parse_optional_params<T: DeserializeOwned + Default>(
    params: Option<Value>,
) -> Result<T, RpcErrorObject> {
    params.map_or_else(
        || Ok(T::default()),
        |value| {
            serde_json::from_value(value)
                .map_err(|error| invalid_argument("params", &error.to_string()))
        },
    )
}

/// `daemon.task.list`：无分页参数时取全部（超过单页上限即拒绝，避免一次返回整张大表）；
/// 带 `limit` / `cursor` 时按 `taskId` 升序分页。
fn list_tasks(
    snapshot: &fluxdown_protocol::DaemonSnapshot,
    params: &fluxdown_protocol::TaskListParams,
) -> Result<Vec<fluxdown_protocol::TaskDto>, RpcErrorObject> {
    let max = fluxdown_protocol::TASK_PAGE_MAX_LIMIT;
    if params.cursor.is_none() && params.limit.is_none() {
        if snapshot.tasks.len() > max {
            return Err(invalid_argument(
                "limit",
                &format!(
                    "{} tasks exceed the {max}-task list cap; page with {{cursor, limit}}",
                    snapshot.tasks.len()
                ),
            ));
        }
        return Ok(snapshot.tasks.clone());
    }
    let limit = params
        .limit
        .map_or(max, |limit| usize::try_from(limit).unwrap_or(max))
        .clamp(1, max);
    Ok(snapshot.task_page(params.cursor.as_deref(), limit).0)
}

fn to_value(value: impl serde::Serialize) -> Result<Value, RpcErrorObject> {
    serde_json::to_value(value).map_err(|error| internal_error(error.to_string()))
}

/// `daemon.task.createMany` 参数的结构校验（不含演示模式守卫）。
fn validate_create_tasks(params: &DaemonCreateTasksParams) -> Result<(), RpcErrorObject> {
    if params.batch_id.is_empty() || params.batch_id.len() > CREATE_TASKS_BATCH_ID_MAX_LEN {
        return Err(invalid_argument(
            "batchId",
            "batchId must be 1..=128 bytes long",
        ));
    }
    if params.items.is_empty() || params.items.len() > CREATE_TASKS_MAX_ITEMS {
        return Err(invalid_argument(
            "items",
            "items must contain 1..=512 entries",
        ));
    }
    let common = &params.common;
    for (field, value) in [
        ("common.url", &common.url),
        ("common.fileName", &common.file_name),
        ("common.checksum", &common.checksum),
    ] {
        if !value.is_empty() {
            return Err(invalid_argument(
                field,
                "this field is decided per item and must be empty in common",
            ));
        }
    }
    if common
        .torrent_b64
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        return Err(invalid_argument(
            "common.torrentB64",
            "torrent files are not supported by daemon.task.createMany",
        ));
    }
    if common
        .audio_url
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        return Err(invalid_argument(
            "common.audioUrl",
            "audio tracks are not supported by daemon.task.createMany",
        ));
    }
    if common.method.is_some() || common.body.is_some() {
        return Err(invalid_argument(
            "common.method",
            "custom method / body are not supported by daemon.task.createMany",
        ));
    }
    if let Some(index) = params
        .items
        .iter()
        .position(|item| item.url.trim().is_empty())
    {
        return Err(invalid_argument(
            "items",
            &format!("items[{index}].url must not be empty"),
        ));
    }
    Ok(())
}

fn actor_error(error: ActorCallError) -> RpcErrorObject {
    match error {
        ActorCallError::Unavailable => RpcErrorObject::application(
            error.to_string(),
            RpcErrorData::new(ApplicationErrorCode::Unavailable, true),
        ),
        ActorCallError::Operation(ActorError::InvalidArgument(message)) => {
            invalid_argument("params", &message)
        }
        ActorCallError::Operation(ActorError::Operation(message)) => internal_error(message),
        ActorCallError::Operation(ActorError::RevisionConflict { current }) => {
            RpcErrorObject::application(
                "config revision conflict",
                RpcErrorData {
                    code: ApplicationErrorCode::Conflict,
                    retryable: false,
                    field: None,
                    revision: Some(current),
                    reason: None,
                },
            )
        }
        ActorCallError::Operation(ActorError::NotFound) => not_found(),
        ActorCallError::Operation(ActorError::Conflict(message)) => RpcErrorObject::application(
            message,
            RpcErrorData::new(ApplicationErrorCode::Conflict, false),
        ),
        ActorCallError::Operation(ActorError::TaskStateConflict(message)) => {
            RpcErrorObject::application(
                message,
                RpcErrorData::new(ApplicationErrorCode::Conflict, false)
                    .with_reason(fluxdown_protocol::ErrorReason::TaskStateConflict),
            )
        }
    }
}

fn require_local_agent(is_local_agent: bool) -> Result<(), RpcErrorObject> {
    if is_local_agent {
        Ok(())
    } else {
        Err(RpcErrorObject::application(
            "this method is restricted to the local agent",
            RpcErrorData::new(ApplicationErrorCode::Unauthorized, false),
        ))
    }
}

fn invalid_argument(field: &str, message: &str) -> RpcErrorObject {
    RpcErrorObject::application(
        message,
        RpcErrorData {
            code: ApplicationErrorCode::InvalidArgument,
            retryable: false,
            field: Some(field.to_owned()),
            revision: None,
            reason: None,
        },
    )
}

const ALWAYS_RUNNING_QUEUE: &str = "the main queue always runs and cannot be stopped or scheduled";

/// 主队列恒运行（`is_always_running_queue`）：停止请求显式拒绝，不让客户端以为已生效。
fn validate_queue_stop(queue_id: &str) -> Result<(), RpcErrorObject> {
    if fluxdown_engine::model::is_always_running_queue(queue_id) {
        return Err(invalid_argument("queueId", ALWAYS_RUNNING_QUEUE));
    }
    Ok(())
}

/// 主队列不可定时；只放行「关闭且无时刻」这一等价于现状的空操作，
/// 兼容仍对每个队列都提交一次定时的旧客户端保存流程。
fn validate_queue_schedule(params: &QueueScheduleParams) -> Result<(), RpcErrorObject> {
    let sets_schedule =
        params.enabled || !params.start_time.is_empty() || !params.stop_time.is_empty();
    if sets_schedule && fluxdown_engine::model::is_always_running_queue(&params.queue_id) {
        return Err(invalid_argument("queueId", ALWAYS_RUNNING_QUEUE));
    }
    Ok(())
}

/// 插件包 / 插件目录校验失败：记录完整原因，客户端按细分原因展示。
#[cfg(feature = "plugins")]
fn plugin_package_error(
    field: &str,
    error: &fluxdown_engine::plugin::PluginError,
) -> RpcErrorObject {
    let message = error.to_string();
    fluxdown_engine::log_warn!("[plugin] 安装失败: {message}");
    let mut data = RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
        .with_reason(fluxdown_protocol::ErrorReason::PluginPackageInvalid);
    data.field = Some(field.to_owned());
    RpcErrorObject::application(message, data)
}

/// 市场索引 / 插件下载失败：按失败性质给出错误码与细分原因，并记录完整原因。
#[cfg(feature = "plugins")]
fn market_error(error: &fluxdown_engine::plugin::MarketError) -> RpcErrorObject {
    use fluxdown_engine::plugin::MarketError;
    use fluxdown_protocol::ErrorReason;
    let (code, retryable, reason) = match error {
        MarketError::Network(_) => (
            ApplicationErrorCode::Unavailable,
            true,
            ErrorReason::MarketUnreachable,
        ),
        MarketError::IndexParse(_) | MarketError::IndexTooLarge => (
            ApplicationErrorCode::Unavailable,
            false,
            ErrorReason::MarketIndexInvalid,
        ),
        MarketError::SequenceRollback { .. } => (
            ApplicationErrorCode::Conflict,
            false,
            ErrorReason::MarketIndexRollback,
        ),
        MarketError::NotFound(_) => (
            ApplicationErrorCode::NotFound,
            false,
            ErrorReason::PluginNotInMarket,
        ),
        MarketError::Yanked(_) => (
            ApplicationErrorCode::Conflict,
            false,
            ErrorReason::PluginYanked,
        ),
        MarketError::VersionChanged { .. } => (
            ApplicationErrorCode::Conflict,
            false,
            ErrorReason::MarketVersionChanged,
        ),
        MarketError::AllMirrorsFailed | MarketError::HashMismatch { .. } => (
            ApplicationErrorCode::Unavailable,
            true,
            ErrorReason::PluginDownloadFailed,
        ),
        MarketError::TooLarge => (
            ApplicationErrorCode::InvalidArgument,
            false,
            ErrorReason::PluginPackageTooLarge,
        ),
        MarketError::Plugin(_) => (
            ApplicationErrorCode::InvalidArgument,
            false,
            ErrorReason::PluginPackageInvalid,
        ),
    };
    let message = error.to_string();
    fluxdown_engine::log_warn!("[plugin-market] {message}");
    RpcErrorObject::application(
        message,
        RpcErrorData::new(code, retryable).with_reason(reason),
    )
}

fn not_found() -> RpcErrorObject {
    RpcErrorObject::application(
        "not found",
        RpcErrorData::new(ApplicationErrorCode::NotFound, false),
    )
}

fn internal_error(message: String) -> RpcErrorObject {
    RpcErrorObject::application(
        message,
        RpcErrorData::new(ApplicationErrorCode::Internal, false),
    )
}

#[cfg(feature = "components")]
fn conflict_error(message: &str) -> RpcErrorObject {
    RpcErrorObject::application(
        message,
        RpcErrorData::new(ApplicationErrorCode::Conflict, false),
    )
}

fn unsupported_error(message: &str) -> RpcErrorObject {
    RpcErrorObject::application(
        message,
        RpcErrorData::new(ApplicationErrorCode::Unsupported, false),
    )
}

/// 常驻插件宿主的 RPC 处理（视图会话 / 命令 / invoke）。以子模块形式放置，以便直接复用
/// 本文件的私有辅助函数；本文件的 `call_inner` 只负责分发。
#[cfg(feature = "plugins")]
#[path = "plugin_host_rpc.rs"]
mod plugin_host_rpc;

/// BT 防吸血 RPC（`daemon.bt.guard.*`）。同样作为子模块编译。
#[cfg(feature = "bt")]
#[path = "bt_guard_rpc.rs"]
mod bt_guard_rpc;

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{ApplicationErrorCode, DaemonCreateTasksParams, RpcErrorData};
    use serde_json::{Value, json};

    use super::{
        DaemonService, QueueScheduleParams, match_site_auth, validate_create_tasks,
        validate_queue_schedule, validate_queue_stop,
    };

    fn schedule(queue_id: &str, enabled: bool, start: &str, stop: &str) -> QueueScheduleParams {
        QueueScheduleParams {
            queue_id: queue_id.to_owned(),
            enabled,
            start_time: start.to_owned(),
            stop_time: stop.to_owned(),
            days: 127,
        }
    }

    /// 主队列恒运行：停止与任何实际定时都按 `queueId` 参数错误拒绝；只放行等价于
    /// 现状的「关闭且无时刻」（旧客户端保存每个队列都会提交一次定时）。其他队列不受限。
    #[test]
    fn main_queue_rejects_stop_and_schedule_requests() {
        let rejected_field = |result: Result<(), super::RpcErrorObject>| {
            let data = result
                .expect_err("must be rejected")
                .data
                .expect("application error data");
            assert_eq!(data.code, ApplicationErrorCode::InvalidArgument);
            data.field
        };
        assert_eq!(
            rejected_field(validate_queue_stop("main")).as_deref(),
            Some("queueId")
        );
        for params in [
            schedule("main", true, "08:00", ""),
            schedule("main", false, "", "23:00"),
            schedule("main", true, "", ""),
        ] {
            assert_eq!(
                rejected_field(validate_queue_schedule(&params)).as_deref(),
                Some("queueId")
            );
        }
        assert!(validate_queue_schedule(&schedule("main", false, "", "")).is_ok());
        assert!(validate_queue_stop("later").is_ok());
        assert!(validate_queue_schedule(&schedule("later", true, "08:00", "23:00")).is_ok());
    }

    fn create_many(edit: impl FnOnce(&mut Value)) -> DaemonCreateTasksParams {
        let mut value = json!({
            "batchId": "batch-1",
            "common": { "url": "", "startPaused": true },
            "items": [{ "url": "http://example.test/a.bin" }],
        });
        edit(&mut value);
        serde_json::from_value(value).expect("createMany params deserialize")
    }

    /// 违规参数必须是 `InvalidArgument`，`field` 指向违规字段。
    fn rejected_field(params: &DaemonCreateTasksParams) -> Option<String> {
        let error = validate_create_tasks(params).expect_err("params must be rejected");
        let data = error.data.expect("application error data");
        assert_eq!(data.code, ApplicationErrorCode::InvalidArgument);
        data.field
    }

    #[test]
    fn create_many_accepts_boundary_batch_ids_and_item_counts() {
        assert!(validate_create_tasks(&create_many(|_| {})).is_ok());
        let longest = "b".repeat(fluxdown_protocol::CREATE_TASKS_BATCH_ID_MAX_LEN);
        assert!(validate_create_tasks(&create_many(|v| v["batchId"] = json!(longest))).is_ok());
        let full = vec![json!({ "url": "http://example.test/a.bin" }); 512];
        assert!(validate_create_tasks(&create_many(|v| v["items"] = json!(full))).is_ok());
    }

    #[test]
    fn create_many_rejects_invalid_batch_id_and_item_counts() {
        assert_eq!(
            rejected_field(&create_many(|v| v["batchId"] = json!(""))).as_deref(),
            Some("batchId")
        );
        let too_long = "b".repeat(fluxdown_protocol::CREATE_TASKS_BATCH_ID_MAX_LEN + 1);
        assert_eq!(
            rejected_field(&create_many(|v| v["batchId"] = json!(too_long))).as_deref(),
            Some("batchId")
        );
        assert_eq!(
            rejected_field(&create_many(|v| v["items"] = json!([]))).as_deref(),
            Some("items")
        );
        let overfull = vec![json!({ "url": "http://example.test/a.bin" }); 513];
        assert_eq!(
            rejected_field(&create_many(|v| v["items"] = json!(overfull))).as_deref(),
            Some("items")
        );
        assert_eq!(
            rejected_field(&create_many(|v| v["items"] = json!([{ "url": "  " }]))).as_deref(),
            Some("items")
        );
    }

    #[test]
    fn create_many_rejects_per_item_and_unsupported_common_fields() {
        for (pointer, value, field) in [
            ("/common/url", json!("http://example.test/x"), "common.url"),
            ("/common/fileName", json!("x.bin"), "common.fileName"),
            ("/common/checksum", json!("md5=aa"), "common.checksum"),
            ("/common/torrentB64", json!("AAAA"), "common.torrentB64"),
            (
                "/common/audioUrl",
                json!("http://example.test/a"),
                "common.audioUrl",
            ),
            ("/common/method", json!("POST"), "common.method"),
            (
                "/common/body",
                json!({ "kind": "formData", "fields": {} }),
                "common.method",
            ),
        ] {
            let params = create_many(|v| {
                if let Some(common) = v.get_mut("common").and_then(Value::as_object_mut) {
                    common.insert(pointer.trim_start_matches("/common/").to_owned(), value);
                }
            });
            assert_eq!(rejected_field(&params).as_deref(), Some(field), "{pointer}");
        }
    }

    #[test]
    fn site_auth_match_uses_engine_site_key_including_non_default_port() {
        let store = r#"{"https://example.com":{"user":"u","pass":"p"},"https://example.com:8443":{"user":"alt","pass":"q"}}"#;
        let matched = match_site_auth(store, "https://EXAMPLE.com/files/a.bin?x=1")
            .expect("default port matches bare host");
        assert_eq!(
            (
                matched.site.as_str(),
                matched.user.as_str(),
                matched.pass.as_str()
            ),
            ("https://example.com", "u", "p")
        );
        let alt = match_site_auth(store, "https://example.com:8443/a.bin")
            .expect("explicit non-default port matches host:port");
        assert_eq!(alt.user, "alt");
        assert!(match_site_auth(store, "https://other.example.com/a.bin").is_none());
        assert!(
            match_site_auth(store, "http://example.com/a.bin").is_none(),
            "https 凭据不得套用到同 host 的 http 请求"
        );
        assert!(match_site_auth(store, "magnet:?xt=urn:btih:abc").is_none());
        assert!(match_site_auth("not json", "https://example.com/a.bin").is_none());
    }

    #[tokio::test]
    async fn proxy_route_explains_draft_rules() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_proxy_route_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create proxy route dir");
        let db = fluxdown_engine::db::Db::open(&dir)
            .await
            .expect("open proxy route db");
        db.init_default_config("/tmp")
            .await
            .expect("seed proxy route config");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        let selections = crate::selection::DaemonSelection::new(events.clone());
        let blobs = std::sync::Arc::new(
            crate::blob_store::BlobStore::open(dir.join("blobs"))
                .await
                .expect("open proxy route blobs"),
        );
        let service = DaemonService::new(
            crate::service_hello("proxy-route-test", Vec::new()),
            events,
            selections,
            blobs,
            crate::actor::DaemonActorHandle::disconnected(),
            db,
            dir.clone(),
            #[cfg(feature = "plugins")]
            None,
        );
        let call = |params: Value| {
            let service = &service;
            async move {
                service
                    .call_inner(
                        "proxy-route-test",
                        true,
                        fluxdown_protocol::method::DAEMON_CONFIG_PROXY_ROUTE,
                        Some(params),
                    )
                    .await
                    .expect("proxyRoute call")
            }
        };

        let hit = call(json!({
            "url": "https://x.github.com/a",
            "rules": r#"[{"id":"a","match":"domain","pattern":"github.com","action":"direct"}]"#,
        }))
        .await;
        assert_eq!(hit["matched"]["index"], json!(0), "{hit}");
        assert_eq!(hit["reason"], json!("direct"), "{hit}");
        assert_eq!(hit["issues"], json!([]), "{hit}");

        let miss = call(json!({"url": "https://other.test", "rules": "[]"})).await;
        assert!(miss["matched"].is_null(), "{miss}");
        assert_eq!(miss["reason"], json!("modeNone"), "{miss}");

        let bad = call(json!({
            "rules": r#"[{"id":"a","match":"regex","pattern":"(","action":"direct"}]"#,
        }))
        .await;
        assert_eq!(bad["issues"][0]["field"], json!("pattern"), "{bad}");
        assert_eq!(bad["issues"][0]["code"], json!("invalidRegex"), "{bad}");
        assert_eq!(bad["reason"], json!(""), "{bad}");

        drop(service);
        tokio::fs::remove_dir_all(&dir)
            .await
            .expect("remove proxy route dir");
    }

    #[tokio::test]
    async fn every_canonical_daemon_method_reaches_a_real_dispatch_branch() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_dispatch_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create daemon dispatch dir");
        let db = fluxdown_engine::db::Db::open(&dir)
            .await
            .expect("open daemon dispatch db");
        db.init_default_config("/tmp")
            .await
            .expect("seed daemon dispatch config");
        // `daemon.diagnostics.probe` 会真实写入保存目录并运行组件：保存目录指向测试目录，组件
        // 指向测试目录里不可执行的空文件，不碰用户的「下载」目录，也不运行 PATH 里的程序。
        let ffprobe = if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        };
        for name in ["ffmpeg", ffprobe, "yt-dlp"] {
            tokio::fs::write(dir.join(name), b"")
                .await
                .expect("create placeholder component");
        }
        for (key, name) in [
            (fluxdown_engine::components::CONFIG_FFMPEG_PATH, "ffmpeg"),
            (fluxdown_engine::components::CONFIG_YTDLP_PATH, "yt-dlp"),
        ] {
            db.set_config(key, &dir.join(name).to_string_lossy())
                .await
                .expect("point component at placeholder");
        }
        let mut snapshot = fluxdown_protocol::DaemonSnapshot::default();
        snapshot.config.values.insert(
            "default_save_dir".to_owned(),
            dir.to_string_lossy().into_owned(),
        );
        let events = crate::event_hub::DaemonEventHub::new(snapshot, 32);
        let selections = crate::selection::DaemonSelection::new(events.clone());
        let blobs = std::sync::Arc::new(
            crate::blob_store::BlobStore::open(dir.join("blobs"))
                .await
                .expect("open daemon dispatch blobs"),
        );
        let service = DaemonService::new(
            crate::service_hello("dispatch-test", Vec::new()),
            events,
            selections,
            blobs,
            crate::actor::DaemonActorHandle::disconnected(),
            db,
            dir.clone(),
            #[cfg(feature = "plugins")]
            None,
        );

        for method_name in fluxdown_protocol::method::ALL_METHODS
            .iter()
            .copied()
            .filter(|name| name.starts_with("daemon."))
        {
            let result = service
                .call_inner(
                    "dispatch-test",
                    true,
                    method_name,
                    Some(serde_json::json!({})),
                )
                .await;
            if let Err(error) = result
                && error.data.as_ref().map(|data| data.code)
                    == Some(ApplicationErrorCode::Unsupported)
            {
                assert!(
                    optional_feature_method(method_name),
                    "{method_name} fell through daemon dispatch: {:?}",
                    error.data.unwrap_or_else(|| {
                        RpcErrorData::new(ApplicationErrorCode::Internal, false)
                    })
                );
            }
        }
        drop(service);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("daemon dispatch test directory removal failed: {error}");
        }
    }

    fn optional_feature_method(method_name: &str) -> bool {
        method_name == fluxdown_protocol::method::DAEMON_GROUP_RESOLVE_PREVIEW
            || method_name.starts_with("daemon.plugin.")
            || method_name.starts_with("daemon.component.")
            || (cfg!(not(feature = "bt")) && method_name.starts_with("daemon.bt.guard."))
    }
}

#[cfg(all(test, feature = "plugins"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod plugin_consent_audit_tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use fluxdown_engine::events::{EngineEvent, EventSink};
    use fluxdown_engine::plugin::PluginManager;
    use fluxdown_engine::plugin::quickjs::QuickJsScriptRuntime;
    use fluxdown_engine::plugin::runtime::{
        BridgeHttpRequest, BridgeHttpResponse, PluginBridge, PluginError, PluginLogLevel,
    };
    use fluxdown_protocol::ApplicationErrorCode;
    use serde_json::{Value, json};

    use super::DaemonService;

    const ID: &str = "acme@demo";

    struct NullBridge;

    #[async_trait::async_trait]
    impl PluginBridge for NullBridge {
        async fn http_request(
            &self,
            _plugin_id: &str,
            _req: BridgeHttpRequest,
        ) -> Result<BridgeHttpResponse, PluginError> {
            Err(PluginError::Runtime("no network in test".to_owned()))
        }
        async fn storage_get(&self, _plugin_id: &str, _key: &str) -> Option<String> {
            None
        }
        async fn storage_set(
            &self,
            _plugin_id: &str,
            _key: &str,
            _value: String,
        ) -> Result<(), PluginError> {
            Ok(())
        }
        fn log(&self, _plugin_id: &str, _level: PluginLogLevel, _message: &str) {}
        fn request_retry(&self, _plugin_id: &str, _task_id: &str, _delay_ms: u64) {}
    }

    struct NullSink;

    impl EventSink for NullSink {
        fn emit(&self, _event: EngineEvent) {}
    }

    struct Fixture {
        service: DaemonService,
        db: fluxdown_engine::db::Db,
        manager: Arc<PluginManager>,
        root: PathBuf,
        base: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.base) {
                eprintln!("plugin test dir removal failed: {error}");
            }
        }
    }

    fn write_plugin(dir: &Path, permissions: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            json!({
                "identity": ID, "name": "Demo", "version": "1.0.0",
                "permissions": permissions,
            })
            .to_string(),
        )
        .unwrap();
    }

    async fn fixture() -> Fixture {
        let base =
            std::env::temp_dir().join(format!("fluxdown-plugin-svc-{}", uuid::Uuid::new_v4()));
        let root = base.join("plugins");
        std::fs::create_dir_all(&root).unwrap();
        let db = fluxdown_engine::db::Db::connect("sqlite::memory:")
            .await
            .unwrap();
        let manager = Arc::new(PluginManager::new(
            Arc::new(QuickJsScriptRuntime::new(1).unwrap()),
            Arc::new(NullBridge),
            db.clone(),
            root.clone(),
            "1.0.0".to_owned(),
            Arc::new(NullSink),
        ));
        let events = crate::event_hub::DaemonEventHub::new(Default::default(), 32);
        let selections = crate::selection::DaemonSelection::new(events.clone());
        let blobs = Arc::new(
            crate::blob_store::BlobStore::open(base.join("blobs"))
                .await
                .unwrap(),
        );
        let service = DaemonService::new(
            crate::service_hello("plugin-test", Vec::new()),
            events,
            selections,
            blobs,
            crate::actor::DaemonActorHandle::disconnected(),
            db.clone(),
            base.clone(),
            Some(manager.clone()),
        );
        Fixture {
            service,
            db,
            manager,
            root,
            base,
        }
    }

    async fn call(
        f: &Fixture,
        method: &str,
        params: Value,
    ) -> Result<Value, fluxdown_protocol::RpcErrorObject> {
        f.service
            .call_inner("plugin-test", true, method, Some(params))
            .await
    }

    async fn plugin(f: &Fixture) -> Value {
        let list = call(f, "daemon.plugin.list", json!({})).await.unwrap();
        list.as_array()
            .unwrap()
            .iter()
            .find(|p| p["identity"] == ID)
            .cloned()
            .expect("plugin listed")
    }

    async fn audit_actions(f: &Fixture) -> Vec<String> {
        let out = call(f, "daemon.plugin.audit.list", json!({}))
            .await
            .unwrap();
        out["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap().to_owned())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_consent_gates_enable_and_requires_exact_set() {
        let f = fixture().await;
        // 首次加载写入迁移标记；此后安装的插件不再自动获得授权。
        f.manager.load_all().await;
        let src = f.base.join("src");
        write_plugin(&src, &["tasks"]);
        call(&f, "daemon.plugin.installDev", json!({ "dirPath": src }))
            .await
            .unwrap();

        let p = plugin(&f).await;
        assert_eq!(p["consentPending"], json!(["tasks"]));
        assert_eq!(p["disabledReason"], "ConsentRequired");
        assert_eq!(p["enabled"], false);

        let err = call(
            &f,
            "daemon.plugin.setEnabled",
            json!({ "identity": ID, "enabled": true }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.data.as_ref().unwrap().code,
            ApplicationErrorCode::InvalidArgument
        );
        assert!(err.message.contains("consent required"), "{}", err.message);

        let err = call(
            &f,
            "daemon.plugin.consent",
            json!({ "identity": ID, "permissions": ["ytdlp"], "networkHosts": [] }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.data.as_ref().unwrap().code,
            ApplicationErrorCode::InvalidArgument
        );
        assert_eq!(
            err.data.as_ref().unwrap().field.as_deref(),
            Some("permissions")
        );
        assert!(
            err.message
                .contains("consent does not match requested capabilities")
        );
        assert_eq!(plugin(&f).await["disabledReason"], "ConsentRequired");

        call(
            &f,
            "daemon.plugin.consent",
            json!({ "identity": ID, "permissions": ["tasks"], "networkHosts": [] }),
        )
        .await
        .unwrap();
        let p = plugin(&f).await;
        assert_eq!(p["consentPending"], json!([]));
        assert_eq!(p["enabled"], true);
        assert_eq!(p["disabledReason"], "None");

        let raw =
            f.db.get_config(&format!("plugin.{ID}.consent"))
                .await
                .unwrap()
                .expect("consent persisted");
        let record: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(record["source"], "user");
        assert_eq!(record["permissions"], json!(["tasks"]));

        // list 为 id 降序：最新的 consent.granted 在前，install 在后。
        let actions = audit_actions(&f).await;
        let granted = actions.iter().position(|a| a == "consent.granted").unwrap();
        let installed = actions.iter().position(|a| a == "install").unwrap();
        assert!(granted < installed, "{actions:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_consent_migration_grants_preinstalled_plugins_once() {
        let f = fixture().await;
        write_plugin(&f.root.join(ID), &["tasks", "ytdlp"]);
        f.manager.load_all().await;

        let p = plugin(&f).await;
        assert_eq!(p["consentPending"], json!([]));
        assert_eq!(p["enabled"], true);
        let raw =
            f.db.get_config(&format!("plugin.{ID}.consent"))
                .await
                .unwrap()
                .expect("migrated consent");
        let record: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(record["source"], "migration");
        assert_eq!(record["permissions"], json!(["tasks", "ytdlp"]));
        assert!(
            audit_actions(&f)
                .await
                .contains(&"consent.migrated".to_owned())
        );

        // 迁移只发生一次：之后新增的插件不会被自动授权。
        let other = f.root.join("other@demo");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("manifest.json"),
            json!({"identity":"other@demo","name":"O","version":"1.0.0","permissions":["tasks"]})
                .to_string(),
        )
        .unwrap();
        f.manager.load_all().await;
        let list = call(&f, "daemon.plugin.list", json!({})).await.unwrap();
        let other = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["identity"] == "other@demo")
            .unwrap();
        assert_eq!(other["consentPending"], json!(["tasks"]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_audit_record_validates_and_clear_logs_itself() {
        let f = fixture().await;
        write_plugin(&f.root.join(ID), &[]);
        f.manager.load_all().await;

        for entries in [
            json!([{"identity": ID, "action": "fetch", "target": "", "outcome": "ok", "detail": ""}]),
            json!([{"identity": "ghost@x", "action": "desktop.load", "target": "", "outcome": "ok", "detail": ""}]),
            json!([{"identity": ID, "action": "desktop.load", "target": "", "outcome": "weird", "detail": ""}]),
        ] {
            let err = call(
                &f,
                "daemon.plugin.audit.record",
                json!({ "entries": entries }),
            )
            .await
            .unwrap_err();
            assert_eq!(
                err.data.as_ref().unwrap().code,
                ApplicationErrorCode::InvalidArgument
            );
        }
        let too_many: Vec<Value> = (0..101)
            .map(|_| json!({"identity": ID, "action": "desktop.call", "target": "", "outcome": "ok", "detail": ""}))
            .collect();
        assert!(
            call(
                &f,
                "daemon.plugin.audit.record",
                json!({ "entries": too_many })
            )
            .await
            .is_err()
        );

        call(
            &f,
            "daemon.plugin.audit.record",
            json!({"entries": [{"identity": ID, "action": "desktop.load", "target": "ui/a.js", "outcome": "ok", "detail": "fs:all"}]}),
        )
        .await
        .unwrap();
        let out = call(&f, "daemon.plugin.audit.list", json!({ "identity": ID }))
            .await
            .unwrap();
        let entry = &out["entries"][0];
        assert_eq!(entry["origin"], "desktop");
        assert_eq!(entry["action"], "desktop.load");
        assert_eq!(entry["target"], "ui/a.js");
        assert!(out.get("nextBeforeId").is_none());

        call(&f, "daemon.plugin.audit.clear", json!({ "identity": ID }))
            .await
            .unwrap();
        let out = call(&f, "daemon.plugin.audit.list", json!({ "identity": ID }))
            .await
            .unwrap();
        let actions: Vec<_> = out["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap())
            .collect();
        assert_eq!(actions, ["audit.clear"]);

        call(&f, "daemon.plugin.audit.clear", json!({}))
            .await
            .unwrap();
        let out = call(&f, "daemon.plugin.audit.list", json!({}))
            .await
            .unwrap();
        assert_eq!(out["entries"][0]["identity"], "*");
        assert_eq!(out["entries"].as_array().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_audit_list_pages_with_before_id() {
        let f = fixture().await;
        write_plugin(&f.root.join(ID), &[]);
        f.manager.load_all().await;
        let entries: Vec<Value> = (0..5)
            .map(|i| json!({"identity": ID, "action": "desktop.call", "target": format!("m{i}"), "outcome": "ok", "detail": ""}))
            .collect();
        call(
            &f,
            "daemon.plugin.audit.record",
            json!({ "entries": entries }),
        )
        .await
        .unwrap();

        let first = call(&f, "daemon.plugin.audit.list", json!({ "limit": 2 }))
            .await
            .unwrap();
        assert_eq!(first["entries"].as_array().unwrap().len(), 2);
        let cursor = first["nextBeforeId"].as_i64().expect("more pages");
        let second = call(
            &f,
            "daemon.plugin.audit.list",
            json!({ "limit": 10, "beforeId": cursor }),
        )
        .await
        .unwrap();
        // 5 条上报 + 1 条迁移记录，首页取走 2 条。
        assert_eq!(second["entries"].as_array().unwrap().len(), 4);
        assert!(second.get("nextBeforeId").is_none());
    }
}
