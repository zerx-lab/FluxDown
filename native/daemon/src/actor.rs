//! 单线程 daemon actor：独占引擎、串行领域写入并排空所有管理器回流通道。

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use base64::Engine as _;
use fluxdown_engine::Engine;
use fluxdown_engine::download_manager::{
    CreateGroupSpec, FileExistsBehavior, NewTaskSpec, TaskDone,
};
#[cfg(feature = "plugins")]
use fluxdown_engine::download_manager::{ResolveOutcome, ResolvePreviewOutcome};
use fluxdown_engine::rss::RssValidateOutcome;
use fluxdown_engine::rss::model::RssSourceInfo;
use fluxdown_protocol::CreateTaskRequest;
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::selection::DaemonSelection;

/// 生产命令队列容量。
pub const COMMAND_CAPACITY: usize = 64;

/// actor 领域操作。
pub enum ActorOperation {
    CreateTask {
        request: Box<CreateTaskRequest>,
        torrent_file_bytes: Vec<u8>,
        hint_file_size: i64,
        unattended: bool,
    },
    PauseTask {
        task_id: String,
    },
    ResumeTask {
        task_id: String,
    },
    RenameTask {
        task_id: String,
        file_name: String,
    },
    DeleteTask {
        task_id: String,
        delete_files: bool,
    },
    PauseAll,
    ResumeAll,
    RescanFiles,
    SetTaskSeedLimits {
        task_id: String,
        ratio_limit_milli: i64,
        post_ratio_limit_milli: i64,
        seed_time_limit_minutes: i64,
        inactive_time_limit_minutes: i64,
        upload_limit_bps: i64,
    },
    CreateQueue {
        name: String,
        speed_limit_kbps: i64,
        upload_limit_kbps: i64,
        max_concurrent: i32,
        default_save_dir: String,
        default_segments: i32,
        default_user_agent: String,
    },
    UpdateQueue {
        queue_id: String,
        name: String,
        speed_limit_kbps: i64,
        upload_limit_kbps: i64,
        max_concurrent: i32,
        default_save_dir: String,
        default_segments: i32,
        default_user_agent: String,
    },
    DeleteQueue {
        queue_id: String,
    },
    StartQueue {
        queue_id: String,
    },
    StopQueue {
        queue_id: String,
    },
    SetQueueSchedule {
        queue_id: String,
        enabled: bool,
        start_time: String,
        stop_time: String,
        days: i32,
    },
    ReorderQueue {
        queue_id: String,
        task_ids: Vec<String>,
    },
    MoveToQueue {
        task_id: String,
        queue_id: String,
    },
    Boost {
        task_id: String,
    },
    TestProxy {
        proxy_type: String,
        host: String,
        port: String,
        username: String,
        password: String,
    },
    PatchConfig {
        expected_revision: u64,
        values: BTreeMap<String, String>,
    },
    ClearConnPolicy,
    SiteAuthDelete {
        site: String,
    },
    SiteAuthClear,
    CdnReportsPeek,
    CdnReportsAck {
        batch_id: String,
    },
    CdnConfigApply {
        values: BTreeMap<String, String>,
    },
    RefreshTrackerSubscription,
    RefreshEd2kServerSubscription,
    RefreshEd2kNodes,
    MigrationLinkExport,
    MigrationLinkAck {
        revision: u64,
    },
    MigrationGatewayExport,
    MigrationGatewayAck {
        revision: u64,
    },
    CreateGroup {
        spec: Box<CreateGroupSpec>,
    },
    PauseGroup {
        group_id: String,
    },
    ResumeGroup {
        group_id: String,
    },
    DeleteGroup {
        group_id: String,
        delete_files: bool,
    },
    #[cfg(feature = "plugins")]
    ResolvePreview {
        url: String,
        cookies: String,
        referrer: String,
        user_agent: String,
        extra_headers: HashMap<String, String>,
    },
    RssCreate {
        source: Box<RssSourceInfo>,
    },
    RssUpdate {
        source: Box<RssSourceInfo>,
    },
    RssDelete {
        source_id: String,
    },
    RssRefresh {
        source_id: String,
    },
    RssItemAction {
        source_id: String,
        guid: String,
        action: String,
    },
    RssValidate {
        url: String,
        cookies: String,
        user_agent: String,
        proxy_url: String,
    },
    WebhookDeliveries,
    WebhookClear,
    WebhookSimulate,
    WebhookTest {
        endpoint_json: String,
    },
}

/// actor 操作结果。
pub enum ActorResult {
    Unit,
    Created(String),
    Boolean(bool),
    ProxyLatency(i64),
    Config(fluxdown_protocol::DaemonConfigSnapshot),
    ConnPolicy(fluxdown_protocol::ConnPolicySummaryDto),
    SiteAuth(Vec<fluxdown_protocol::SiteAuthEntryDto>),
    CdnLease(Option<fluxdown_protocol::CdnReportLeaseDto>),
    TrackerRefresh(fluxdown_protocol::TrackerSubRefreshResponse),
    Ed2kRefresh(fluxdown_protocol::Ed2kServerSubRefreshResponse),
    LinkMigration(fluxdown_protocol::LinkMigrationExport),
    GatewayMigration(fluxdown_protocol::GatewayMigrationExport),
    #[cfg(feature = "plugins")]
    ResolvePreview(ResolvePreviewOutcome),
    RssValidation(Box<RssValidateOutcome>),
    WebhookDeliveries(Vec<fluxdown_engine::webhook::WebhookDelivery>),
    WebhookSimulation(usize),
    WebhookTest(Box<fluxdown_engine::webhook::WebhookDelivery>),
}

/// actor 领域错误。
#[derive(Debug, thiserror::Error)]
pub enum ActorError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("operation failed: {0}")]
    Operation(String),
    #[error("config revision conflict; current revision is {current}")]
    RevisionConflict { current: u64 },
    #[error("resource not found")]
    NotFound,
}

/// daemon actor 命令。
pub enum DaemonCommand {
    Execute {
        operation: ActorOperation,
        ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    },
    Shutdown {
        ack: oneshot::Sender<()>,
    },
}

/// 插件解析与重试后台任务合流后的 actor 事件。
pub enum PluginEvent {
    #[cfg(feature = "plugins")]
    Resolve(Box<ResolveOutcome>),
    #[cfg(feature = "plugins")]
    Retry { task_id: String, delay_ms: u64 },
}

/// off-actor 网络抓取回流，数据库提交只在 actor 内执行。
pub enum MaintenanceEvent {
    Tracker {
        outcome: fluxdown_engine::tracker_subscription::FetchOutcome,
        ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    },
    Ed2k {
        outcome: fluxdown_engine::ed2k::server_subscription::ServerFetchOutcome,
        ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    },
    Ed2kNodes {
        outcome: Result<Vec<u8>, String>,
        ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    },
}

/// 从 `DownloadManager` 一次性取出的全部宿主回流接收端。
pub struct EngineReceivers {
    pub done: mpsc::Receiver<TaskDone>,
    pub retry: mpsc::Receiver<String>,
    pub plugin: mpsc::UnboundedReceiver<PluginEvent>,
    pub missing_cleanup: mpsc::Receiver<Vec<String>>,
}

/// daemon actor 命令入口。
#[derive(Clone)]
pub struct DaemonActorHandle {
    commands: mpsc::Sender<DaemonCommand>,
}

impl DaemonActorHandle {
    /// 执行领域操作；命令发送或回执丢失统一视为 unavailable。
    pub async fn execute(&self, operation: ActorOperation) -> Result<ActorResult, ActorCallError> {
        let (ack, response) = oneshot::channel();
        self.commands
            .send(DaemonCommand::Execute { operation, ack })
            .await
            .map_err(|_| ActorCallError::Unavailable)?;
        response
            .await
            .map_err(|_| ActorCallError::Unavailable)?
            .map_err(ActorCallError::Operation)
    }

    /// 请求 actor 完成关闭收尾。
    pub async fn shutdown(&self) -> Result<(), ActorCallError> {
        let (ack, response) = oneshot::channel();
        self.commands
            .send(DaemonCommand::Shutdown { ack })
            .await
            .map_err(|_| ActorCallError::Unavailable)?;
        response.await.map_err(|_| ActorCallError::Unavailable)
    }
}

#[cfg(test)]
impl DaemonActorHandle {
    pub(crate) fn disconnected() -> Self {
        let (commands, receiver) = mpsc::channel(1);
        drop(receiver);
        Self { commands }
    }
}

/// actor 调用失败。
#[derive(Debug, thiserror::Error)]
pub enum ActorCallError {
    #[error("daemon actor is unavailable")]
    Unavailable,
    #[error(transparent)]
    Operation(ActorError),
}

/// 启动独占引擎的 actor。
pub fn spawn_actor(
    engine: Engine,
    receivers: EngineReceivers,
    selections: DaemonSelection,
    events: crate::event_hub::DaemonEventHub,
    cancel: CancellationToken,
) -> (DaemonActorHandle, tokio::task::JoinHandle<()>) {
    let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (maintenance_tx, maintenance_rx) = mpsc::unbounded_channel();
    let handle = DaemonActorHandle { commands };
    let task = tokio::spawn(run_actor(
        engine,
        command_rx,
        receivers,
        selections,
        events,
        maintenance_tx,
        maintenance_rx,
        cancel,
    ));
    (handle, task)
}

#[allow(
    clippy::too_many_arguments,
    reason = "single actor loop owns all engine and maintenance receivers"
)]
async fn run_actor(
    mut engine: Engine,
    mut commands: mpsc::Receiver<DaemonCommand>,
    mut receivers: EngineReceivers,
    selections: DaemonSelection,
    events: crate::event_hub::DaemonEventHub,
    maintenance_tx: mpsc::UnboundedSender<MaintenanceEvent>,
    mut maintenance_rx: mpsc::UnboundedReceiver<MaintenanceEvent>,
    cancel: CancellationToken,
) {
    engine.manager.load_queues().await;
    engine.manager.load_and_send_all_tasks().await;
    let mut rss_events = engine.manager.rss.take_event_rx();

    let mut file_scan = tokio::time::interval(Duration::from_secs(300));
    file_scan.set_missed_tick_behavior(MissedTickBehavior::Delay);
    file_scan.tick().await;
    let mut queue_schedule = tokio::time::interval(Duration::from_secs(20));
    queue_schedule.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut rss_poll = tokio::time::interval(Duration::from_secs(60));
    rss_poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut seeding = tokio::time::interval(fluxdown_engine::bt_seeding::SEEDING_EVAL_INTERVAL);
    seeding.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            Some(command) = commands.recv() => {
                match command {
                    DaemonCommand::Execute { operation, ack } => {
                        dispatch_operation(
                            operation,
                            ack,
                            &mut engine,
                            &events,
                            &maintenance_tx,
                        )
                        .await;
                    }
                    DaemonCommand::Shutdown { ack } => {
                        selections.resolve_all_defaults();
                        cancel.cancel();
                        let _ = ack.send(());
                        break;
                    }
                }
            }
            Some(done) = receivers.done.recv() => engine.manager.on_task_done(&done).await,
            Some(task_id) = receivers.retry.recv() => {
                if engine.manager.is_task_in_error(&task_id).await {
                    engine.manager.resume_task_auto(&task_id).await;
                }
            }
            Some(event) = receivers.plugin.recv() => {
                match event {
                    #[cfg(feature = "plugins")]
                    PluginEvent::Resolve(outcome) => engine.manager.on_resolve_ready(*outcome).await,
                    #[cfg(feature = "plugins")]
                    PluginEvent::Retry { task_id, delay_ms } => {
                        engine.manager.plugin_request_retry(&task_id, delay_ms).await;
                    }
                }
            }
            Some(event) = maintenance_rx.recv() => {
                commit_maintenance(event, &mut engine).await;
            }
            Some(ids) = receivers.missing_cleanup.recv() => {
                engine.manager.delete_tasks_batch(&ids, false).await;
                engine.manager.load_and_send_all_tasks().await;
            }
            _ = file_scan.tick() => engine.manager.spawn_file_scan(),
            _ = queue_schedule.tick() => {
                engine.manager.tick_queue_schedules().await;
                engine.manager.tick_system_proxy();
            }
            _ = rss_poll.tick() => engine.manager.tick_rss_sources(),
            event = receive_rss_event(&mut rss_events), if rss_events.is_some() => {
                if let Some(event) = event { engine.manager.on_rss_event(event).await; }
            }
            _ = seeding.tick() => engine.manager.tick_seeding_evaluation().await,
        }
    }

    engine.manager.shutdown().await;
    match tokio::time::timeout(Duration::from_secs(5), engine.flush_task_activity()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!(%error, "daemon task activity flush failed"),
        Err(_) => tracing::error!("daemon task activity flush timed out"),
    }
    selections.resolve_all_defaults();
    commands.close();
    while let Some(command) = commands.recv().await {
        match command {
            DaemonCommand::Execute { ack, .. } => {
                let _ = ack.send(Err(ActorError::Operation(
                    "daemon is shutting down".to_owned(),
                )));
            }
            DaemonCommand::Shutdown { ack } => {
                let _ = ack.send(());
            }
        }
    }
}

async fn dispatch_operation(
    operation: ActorOperation,
    ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    engine: &mut Engine,
    events: &crate::event_hub::DaemonEventHub,
    maintenance_tx: &mpsc::UnboundedSender<MaintenanceEvent>,
) {
    match operation {
        #[cfg(feature = "plugins")]
        ActorOperation::ResolvePreview {
            url,
            cookies,
            referrer,
            user_agent,
            extra_headers,
        } => {
            let receiver = engine.manager.spawn_resolve_preview(
                url,
                cookies,
                referrer,
                user_agent,
                extra_headers,
            );
            tokio::spawn(async move {
                let result = receiver
                    .await
                    .map(ActorResult::ResolvePreview)
                    .map_err(|_| {
                        ActorError::Operation("resolve preview worker dropped".to_owned())
                    });
                let _ = ack.send(result);
            });
        }
        ActorOperation::RefreshTrackerSubscription => {
            let config = match engine.db.get_all_config().await {
                Ok(config) => config,
                Err(error) => {
                    let _ = ack.send(Err(ActorError::Operation(format!("{error:#}"))));
                    return;
                }
            };
            let urls = config
                .get("bt_tracker_sub_urls")
                .cloned()
                .unwrap_or_else(fluxdown_engine::tracker_subscription::default_subscription_urls);
            let maintenance_tx = maintenance_tx.clone();
            tokio::spawn(async move {
                let outcome =
                    fluxdown_engine::tracker_subscription::fetch_subscriptions(&urls).await;
                let _ = maintenance_tx.send(MaintenanceEvent::Tracker { outcome, ack });
            });
        }
        ActorOperation::RefreshEd2kServerSubscription => {
            let config = match engine.db.get_all_config().await {
                Ok(config) => config,
                Err(error) => {
                    let _ = ack.send(Err(ActorError::Operation(format!("{error:#}"))));
                    return;
                }
            };
            let urls = config.get("ed2k_server_sub_urls").cloned().unwrap_or_else(
                fluxdown_engine::ed2k::server_subscription::default_server_met_urls,
            );
            let maintenance_tx = maintenance_tx.clone();
            tokio::spawn(async move {
                let outcome =
                    fluxdown_engine::ed2k::server_subscription::fetch_server_subscriptions(&urls)
                        .await;
                let _ = maintenance_tx.send(MaintenanceEvent::Ed2k { outcome, ack });
            });
        }
        ActorOperation::RefreshEd2kNodes => {
            let url = match engine.db.get_config("ed2k_nodes_dat_url").await {
                Ok(Some(url)) if !url.trim().is_empty() => url,
                Ok(_) => {
                    let _ = ack.send(Ok(ActorResult::Unit));
                    return;
                }
                Err(error) => {
                    let _ = ack.send(Err(ActorError::Operation(format!("{error:#}"))));
                    return;
                }
            };
            let maintenance_tx = maintenance_tx.clone();
            tokio::spawn(async move {
                let outcome = fluxdown_engine::ed2k::kad::fetch_nodes_dat(&url).await;
                let _ = maintenance_tx.send(MaintenanceEvent::Ed2kNodes { outcome, ack });
            });
        }
        ActorOperation::RssValidate {
            url,
            cookies,
            user_agent,
            proxy_url,
        } => {
            let future = engine
                .manager
                .rss_validate_future(url, cookies, user_agent, proxy_url);
            tokio::spawn(async move {
                let _ = ack.send(Ok(ActorResult::RssValidation(Box::new(future.await))));
            });
        }
        ActorOperation::WebhookTest { endpoint_json } => {
            let dispatcher = engine.manager.webhook();
            tokio::spawn(async move {
                let result =
                    serde_json::from_str::<fluxdown_engine::webhook::EndpointSpec>(&endpoint_json)
                        .map_err(|error| ActorError::InvalidArgument(error.to_string()));
                let result = match result {
                    Ok(spec) => Ok(ActorResult::WebhookTest(Box::new(
                        dispatcher.test_endpoint(spec).await,
                    ))),
                    Err(error) => Err(error),
                };
                let _ = ack.send(result);
            });
        }
        operation => {
            let result = execute_operation(operation, engine, events).await;
            let _ = ack.send(result);
        }
    }
}

async fn execute_operation(
    operation: ActorOperation,
    engine: &mut Engine,
    events: &crate::event_hub::DaemonEventHub,
) -> Result<ActorResult, ActorError> {
    match operation {
        ActorOperation::CreateTask {
            request,
            mut torrent_file_bytes,
            hint_file_size,
            unattended,
        } => {
            let request = *request;
            if torrent_file_bytes.is_empty() {
                torrent_file_bytes = decode_torrent_b64(request.torrent_b64.as_deref())?;
            }
            let mut save_dir = request.save_dir;
            if save_dir.trim().is_empty() {
                save_dir = engine
                    .db
                    .get_config("default_save_dir")
                    .await
                    .map_err(|error| ActorError::Operation(format!("{error:#}")))?
                    .unwrap_or_default();
            }
            if save_dir.trim().is_empty() {
                save_dir = fluxdown_engine::user_dirs::download_dir_or_cwd();
            }
            let queue_id = resolve_queue_id(&engine.db, request.queue_id).await?;
            let task_id = engine
                .manager
                .create_task(NewTaskSpec {
                    url: request.url,
                    save_dir,
                    file_name: request.file_name,
                    segments: request.segments,
                    cookies: request.cookies,
                    referrer: request.referrer,
                    hint_file_size,
                    torrent_file_bytes,
                    proxy_url: request.proxy_url,
                    user_agent: request.user_agent,
                    queue_id,
                    checksum: request.checksum,
                    ignore_tls_errors: request.ignore_tls_errors,
                    extra_headers: request.headers.unwrap_or_default(),
                    method: request.method,
                    body: request
                        .body
                        .map(fluxdown_engine_protocol::request_body_to_engine),
                    audio_url: request.audio_url,
                    start_paused: request.start_paused,
                    http_user: request.http_user,
                    http_password: request.http_password,
                    save_site_auth: request.save_site_auth,
                    unattended_selection: unattended,
                    ..Default::default()
                })
                .await
                .ok_or_else(|| ActorError::Operation("failed to persist task".to_owned()))?;
            engine.manager.load_and_send_all_tasks().await;
            return Ok(ActorResult::Created(task_id));
        }
        ActorOperation::PauseTask { task_id } => engine.manager.pause_task(&task_id).await,
        ActorOperation::ResumeTask { task_id } => engine.manager.resume_task(&task_id).await,
        ActorOperation::RenameTask { task_id, file_name } => engine
            .manager
            .rename_task(&task_id, &file_name)
            .await
            .map_err(ActorError::Operation)?,
        ActorOperation::DeleteTask {
            task_id,
            delete_files,
        } => {
            engine.manager.delete_task(&task_id, delete_files).await;
            engine.manager.load_and_send_all_tasks().await;
        }
        ActorOperation::PauseAll => {
            let ids = task_ids_by_status(&engine.db, &[0, 1, 5]).await?;
            engine.manager.batch_pause(&ids).await;
        }
        ActorOperation::ResumeAll => {
            engine.manager.resume_all_eligible().await;
        }
        ActorOperation::RescanFiles => engine.manager.spawn_file_scan(),
        ActorOperation::SetTaskSeedLimits {
            task_id,
            ratio_limit_milli,
            post_ratio_limit_milli,
            seed_time_limit_minutes,
            inactive_time_limit_minutes,
            upload_limit_bps,
        } => {
            engine
                .manager
                .set_task_seed_limits(
                    &task_id,
                    ratio_limit_milli,
                    post_ratio_limit_milli,
                    seed_time_limit_minutes,
                    inactive_time_limit_minutes,
                    upload_limit_bps,
                )
                .await;
        }
        ActorOperation::CreateQueue {
            name,
            speed_limit_kbps,
            upload_limit_kbps,
            max_concurrent,
            default_save_dir,
            default_segments,
            default_user_agent,
        } => {
            engine
                .manager
                .create_queue(
                    name,
                    speed_limit_kbps,
                    upload_limit_kbps,
                    max_concurrent,
                    default_save_dir,
                    default_segments,
                    default_user_agent,
                )
                .await
        }
        ActorOperation::UpdateQueue {
            queue_id,
            name,
            speed_limit_kbps,
            upload_limit_kbps,
            max_concurrent,
            default_save_dir,
            default_segments,
            default_user_agent,
        } => {
            engine
                .manager
                .update_queue(
                    queue_id,
                    name,
                    speed_limit_kbps,
                    upload_limit_kbps,
                    max_concurrent,
                    default_save_dir,
                    default_segments,
                    default_user_agent,
                )
                .await
        }
        ActorOperation::DeleteQueue { queue_id } => engine.manager.delete_queue(queue_id).await,
        ActorOperation::StartQueue { queue_id } => engine.manager.start_queue(queue_id).await,
        ActorOperation::StopQueue { queue_id } => engine.manager.stop_queue(queue_id).await,
        ActorOperation::SetQueueSchedule {
            queue_id,
            enabled,
            start_time,
            stop_time,
            days,
        } => {
            engine
                .manager
                .set_queue_schedule(queue_id, enabled, start_time, stop_time, days)
                .await
        }
        ActorOperation::ReorderQueue { queue_id, task_ids } => {
            engine.manager.reorder_queue_tasks(queue_id, task_ids).await
        }
        ActorOperation::MoveToQueue { task_id, queue_id } => {
            engine.manager.move_task_to_queue(task_id, queue_id).await
        }
        ActorOperation::Boost { task_id } => engine.manager.set_priority_task(task_id).await,
        ActorOperation::TestProxy {
            proxy_type,
            host,
            port,
            username,
            password,
        } => {
            let latency = engine
                .test_proxy_connection(&proxy_type, &host, &port, &username, &password)
                .await
                .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
            return Ok(ActorResult::ProxyLatency(latency));
        }
        ActorOperation::PatchConfig {
            expected_revision,
            values,
        } => {
            let snapshot = patch_config(engine, events, expected_revision, values).await?;
            return Ok(ActorResult::Config(snapshot));
        }
        ActorOperation::ClearConnPolicy => {
            return Ok(ActorResult::ConnPolicy(
                clear_conn_policy(engine, events).await?,
            ));
        }
        ActorOperation::SiteAuthDelete { site } => {
            return Ok(ActorResult::SiteAuth(
                delete_site_auth(&engine.db, &site).await?,
            ));
        }
        ActorOperation::SiteAuthClear => {
            return Ok(ActorResult::SiteAuth(
                save_site_auth(&engine.db, &BTreeMap::new()).await?,
            ));
        }
        ActorOperation::CdnReportsPeek => return cdn_reports_peek(engine).await,
        ActorOperation::CdnReportsAck { batch_id } => {
            let acknowledged = engine
                .db
                .ack_cdn_report_lease(&batch_id)
                .await
                .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
            return Ok(ActorResult::Boolean(acknowledged));
        }
        ActorOperation::CdnConfigApply { values } => {
            apply_cdn_config(engine, values).await?;
        }
        ActorOperation::MigrationLinkExport => {
            return Ok(ActorResult::LinkMigration(
                link_migration_export(engine).await?,
            ));
        }
        ActorOperation::MigrationLinkAck { revision } => {
            migration_ack(engine, "daemon_migration_link_acked", revision).await?;
        }
        ActorOperation::MigrationGatewayExport => {
            return Ok(ActorResult::GatewayMigration(
                gateway_migration_export(engine).await?,
            ));
        }
        ActorOperation::MigrationGatewayAck { revision } => {
            migration_ack(engine, "daemon_migration_gateway_acked", revision).await?;
        }
        ActorOperation::CreateGroup { spec } => {
            let mut spec = *spec;
            spec.queue_id = resolve_queue_id(&engine.db, spec.queue_id).await?;
            let group_id = engine
                .manager
                .create_task_group(spec)
                .await
                .ok_or_else(|| ActorError::Operation("failed to persist group".to_owned()))?;
            return Ok(ActorResult::Created(group_id));
        }
        ActorOperation::PauseGroup { group_id } => engine.manager.pause_group(&group_id).await,
        ActorOperation::ResumeGroup { group_id } => engine.manager.resume_group(&group_id).await,
        ActorOperation::DeleteGroup {
            group_id,
            delete_files,
        } => {
            engine.manager.delete_group(&group_id, delete_files).await;
            engine.manager.load_and_send_all_tasks().await;
        }
        #[cfg(feature = "plugins")]
        ActorOperation::ResolvePreview { .. } => unreachable!("handled off actor"),
        ActorOperation::RssCreate { source } => {
            let id = engine
                .manager
                .create_rss_source(*source)
                .await
                .ok_or_else(|| ActorError::InvalidArgument("RSS URL is required".to_owned()))?;
            return Ok(ActorResult::Created(id));
        }
        ActorOperation::RssUpdate { source } => {
            return Ok(ActorResult::Boolean(
                engine.manager.rss.update_source(*source).await,
            ));
        }
        ActorOperation::RssDelete { source_id } => {
            return Ok(ActorResult::Boolean(
                engine.manager.rss.delete_source(&source_id).await,
            ));
        }
        ActorOperation::RssRefresh { source_id } => {
            return Ok(ActorResult::Boolean(
                engine.manager.refresh_rss_source(&source_id),
            ));
        }
        ActorOperation::RssItemAction {
            source_id,
            guid,
            action,
        } => match action.as_str() {
            "download" => engine.manager.download_rss_item(&source_id, &guid).await,
            "ignore" => engine.manager.rss.ignore_item(&source_id, &guid).await,
            "readAll" => engine.manager.rss.mark_all_read(&source_id).await,
            _ => {
                return Err(ActorError::InvalidArgument(
                    "unknown RSS item action".to_owned(),
                ));
            }
        },
        ActorOperation::RefreshTrackerSubscription
        | ActorOperation::RefreshEd2kServerSubscription
        | ActorOperation::RefreshEd2kNodes
        | ActorOperation::RssValidate { .. }
        | ActorOperation::WebhookTest { .. } => unreachable!("handled off actor"),
        ActorOperation::WebhookDeliveries => {
            return Ok(ActorResult::WebhookDeliveries(engine.webhook_deliveries()));
        }
        ActorOperation::WebhookClear => engine.clear_webhook_deliveries().await,
        ActorOperation::WebhookSimulate => {
            return Ok(ActorResult::WebhookSimulation(
                engine.simulate_webhook_event(),
            ));
        }
    }
    Ok(ActorResult::Unit)
}

async fn link_migration_export(
    engine: &Engine,
) -> Result<fluxdown_protocol::LinkMigrationExport, ActorError> {
    ensure_migration_available(engine, "daemon_migration_link_acked").await?;
    let identity = engine
        .db
        .get_config("link.identity_secret")
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .map_or(serde_json::Value::Null, serde_json::Value::String);
    let roster = engine
        .db
        .link_load_devices()
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "fingerprint": row.fingerprint,
                "identityPubB64": base64::engine::general_purpose::STANDARD.encode(row.identity_pub),
                "name": row.name,
                "platform": row.platform,
                "linkSecretB64": base64::engine::general_purpose::STANDARD.encode(row.link_secret),
                "candidatesJson": row.candidates_json,
                "pairedAt": row.paired_at,
                "lastSeenAt": row.last_seen_at,
            })
        })
        .collect();
    Ok(fluxdown_protocol::LinkMigrationExport {
        revision: 1,
        identity,
        roster,
    })
}

async fn gateway_migration_export(
    engine: &Engine,
) -> Result<fluxdown_protocol::GatewayMigrationExport, ActorError> {
    ensure_migration_available(engine, "daemon_migration_gateway_acked").await?;
    let config = engine
        .db
        .get_all_config()
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    let enabled = |key: &str| config.get(key).is_some_and(|value| value == "true");
    Ok(fluxdown_protocol::GatewayMigrationExport {
        revision: 1,
        takeover_enabled: enabled("local_server_takeover_enabled"),
        jsonrpc_enabled: enabled("local_server_jsonrpc_enabled"),
        api_enabled: enabled("local_server_api_enabled"),
        mcp_enabled: enabled("local_server_mcp_enabled"),
        cors_enabled: enabled("local_server_cors_allow_all"),
        user_token_configured: config
            .get("local_server_token")
            .is_some_and(|value| !value.is_empty()),
        user_token: config
            .get("local_server_token")
            .cloned()
            .unwrap_or_default(),
    })
}

async fn ensure_migration_available(engine: &Engine, marker: &str) -> Result<(), ActorError> {
    let acknowledged = engine
        .db
        .get_config(marker)
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .is_some_and(|value| value == "1");
    if acknowledged {
        Err(ActorError::NotFound)
    } else {
        Ok(())
    }
}

async fn migration_ack(engine: &Engine, marker: &str, revision: u64) -> Result<(), ActorError> {
    if revision != 1 {
        return Err(ActorError::InvalidArgument(
            "migration revision does not match export".to_owned(),
        ));
    }
    ensure_migration_available(engine, marker).await?;
    engine
        .db
        .set_config(marker, "1")
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))
}

async fn commit_maintenance(event: MaintenanceEvent, engine: &mut Engine) {
    match event {
        MaintenanceEvent::Tracker { outcome, ack } => {
            let result = commit_tracker_refresh(outcome, engine).await;
            let _ = ack.send(result.map(ActorResult::TrackerRefresh));
        }
        MaintenanceEvent::Ed2kNodes { outcome, ack } => {
            let result = commit_ed2k_nodes(outcome, engine).await;
            let _ = ack.send(result);
        }
        MaintenanceEvent::Ed2k { outcome, ack } => {
            let result = commit_ed2k_refresh(outcome, engine).await;
            let _ = ack.send(result.map(ActorResult::Ed2kRefresh));
        }
    }
}

async fn commit_ed2k_nodes(
    outcome: Result<Vec<u8>, String>,
    engine: &mut Engine,
) -> Result<ActorResult, ActorError> {
    let bytes = outcome.map_err(ActorError::Operation)?;
    let values = BTreeMap::from([
        (
            "ed2k_nodes_dat_cache".to_owned(),
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ),
        (
            "ed2k_nodes_dat_updated_at".to_owned(),
            now_unix_secs().to_string(),
        ),
    ]);
    engine
        .db
        .set_config_batch_atomic(&values)
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    Ok(ActorResult::Unit)
}

async fn commit_tracker_refresh(
    outcome: fluxdown_engine::tracker_subscription::FetchOutcome,
    engine: &mut Engine,
) -> Result<fluxdown_protocol::TrackerSubRefreshResponse, ActorError> {
    let mut updated_at = engine
        .db
        .get_config("bt_tracker_sub_updated_at")
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    if outcome.is_success() {
        updated_at = now_unix_secs();
        let values = BTreeMap::from([
            (
                "bt_tracker_sub_cache".to_owned(),
                outcome.trackers.join("\n"),
            ),
            (
                "bt_tracker_sub_updated_at".to_owned(),
                updated_at.to_string(),
            ),
        ]);
        engine
            .db
            .set_config_batch_atomic(&values)
            .await
            .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
        let all = engine
            .db
            .get_all_config()
            .await
            .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
        engine
            .manager
            .set_bt_config(crate::config::bt_config_from_map(&all));
        engine.manager.invalidate_bt_session().await;
    }
    Ok(fluxdown_protocol::TrackerSubRefreshResponse {
        success: outcome.is_success(),
        tracker_count: outcome.trackers.len() as i64,
        ok_sources: outcome.ok_sources as i64,
        total_sources: outcome.total_sources as i64,
        updated_at,
        error: outcome.error,
    })
}

async fn commit_ed2k_refresh(
    outcome: fluxdown_engine::ed2k::server_subscription::ServerFetchOutcome,
    engine: &mut Engine,
) -> Result<fluxdown_protocol::Ed2kServerSubRefreshResponse, ActorError> {
    let mut updated_at = engine
        .db
        .get_config("ed2k_server_sub_updated_at")
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    if outcome.is_success() {
        updated_at = now_unix_secs();
        let values = BTreeMap::from([
            (
                "ed2k_server_sub_cache".to_owned(),
                outcome.servers.join(","),
            ),
            (
                "ed2k_server_sub_updated_at".to_owned(),
                updated_at.to_string(),
            ),
            (
                "ed2k_server_sub_cache_version".to_owned(),
                fluxdown_engine::ed2k::server_subscription::CACHE_FORMAT_VERSION.to_string(),
            ),
        ]);
        engine
            .db
            .set_config_batch_atomic(&values)
            .await
            .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    }
    Ok(fluxdown_protocol::Ed2kServerSubRefreshResponse {
        success: outcome.is_success(),
        server_count: outcome.servers.len() as i64,
        ok_sources: outcome.ok_sources as i64,
        total_sources: outcome.total_sources as i64,
        updated_at,
        error: outcome.error,
    })
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64)
}

async fn cdn_reports_peek(engine: &mut Engine) -> Result<ActorResult, ActorError> {
    if let Some(raw) = engine
        .db
        .get_config("cdn_report_lease")
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .filter(|value| !value.trim().is_empty())
    {
        let lease = serde_json::from_str::<fluxdown_protocol::CdnReportLeaseDto>(&raw)
            .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
        return Ok(ActorResult::CdnLease(Some(lease)));
    }
    let samples = fluxdown_engine::cdn::telemetry::take_all();
    if samples.is_empty() {
        return Ok(ActorResult::CdnLease(None));
    }
    let sample_values = samples
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ActorError::Operation(error.to_string()))?;
    let lease = fluxdown_protocol::CdnReportLeaseDto {
        batch_id: uuid::Uuid::new_v4().to_string(),
        samples: sample_values,
    };
    let lease_json =
        serde_json::to_string(&lease).map_err(|error| ActorError::Operation(error.to_string()))?;
    let stored = match engine.db.lease_cdn_reports(&lease_json).await {
        Ok(stored) => stored,
        Err(error) => {
            fluxdown_engine::cdn::telemetry::restore_front(samples);
            return Err(ActorError::Operation(format!("{error:#}")));
        }
    };
    let stored_lease = serde_json::from_str::<fluxdown_protocol::CdnReportLeaseDto>(&stored)
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    if stored_lease.batch_id != lease.batch_id {
        fluxdown_engine::cdn::telemetry::restore_front(samples);
    }
    Ok(ActorResult::CdnLease(Some(stored_lease)))
}

async fn apply_cdn_config(
    engine: &mut Engine,
    values: BTreeMap<String, String>,
) -> Result<(), ActorError> {
    const ALLOWED: [&str; 3] = [
        "cdn_resolver_endpoints",
        "cdn_ecs_subnets",
        "cdn_hints_base",
    ];
    if let Some(key) = values.keys().find(|key| !ALLOWED.contains(&key.as_str())) {
        return Err(ActorError::InvalidArgument(format!(
            "unsupported CDN config key: {key}"
        )));
    }
    engine
        .db
        .set_config_batch_atomic(&values)
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    if let Some(value) = values.get("cdn_resolver_endpoints") {
        engine.manager.set_cdn_resolver_endpoints(value);
    }
    if let Some(value) = values.get("cdn_ecs_subnets") {
        engine.manager.set_cdn_ecs_subnets(value);
    }
    if let Some(value) = values.get("cdn_hints_base") {
        engine.manager.set_cdn_hints_base(value);
    }
    Ok(())
}

async fn patch_config(
    engine: &mut Engine,
    events: &crate::event_hub::DaemonEventHub,
    expected_revision: u64,
    values: BTreeMap<String, String>,
) -> Result<fluxdown_protocol::DaemonConfigSnapshot, ActorError> {
    let mut merged = engine
        .db
        .get_all_config()
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    merged.extend(
        values
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    if values.keys().any(|key| key.starts_with("proxy_")) {
        let proxy = fluxdown_engine::proxy_config::ProxyConfig::from_config_map(&merged);
        fluxdown_engine::downloader::build_client(&proxy, "")
            .map_err(|error| ActorError::InvalidArgument(format!("{error:#}")))?;
    }
    let revision = engine
        .db
        .apply_config_patch_atomic(expected_revision, &values)
        .await
        .map_err(|error| match error {
            fluxdown_engine::db::ConfigPatchError::RevisionConflict { current, .. } => {
                ActorError::RevisionConflict { current }
            }
            other => ActorError::Operation(format!("{other:#}")),
        })?;
    apply_live_config(engine, &merged, values.keys()).await?;
    let snapshot = fluxdown_protocol::DaemonConfigSnapshot {
        revision,
        values: crate::config::public_config_values(&merged),
    };
    events.publish(fluxdown_protocol::DaemonEvent::ConfigChanged(
        snapshot.clone(),
    ));
    Ok(snapshot)
}

/// 清空引擎学习的域名连接策略（内存 + 持久化，与 hub 收到空
/// `domain_conn_caps` 时的处理同态），并以刷新后的公开配置广播
/// `ConfigChanged`（该只读键随快照可见）。用户配置 revision 不变。
async fn clear_conn_policy(
    engine: &mut Engine,
    events: &crate::event_hub::DaemonEventHub,
) -> Result<fluxdown_protocol::ConnPolicySummaryDto, ActorError> {
    engine.manager.clear_domain_conn_caps();
    engine
        .db
        .set_config(DOMAIN_CONN_CAPS_KEY, "")
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    let mut all = engine
        .db
        .get_all_config()
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    // 引擎的落盘是 fire-and-forget，快照以刚写入的空值为准。
    all.insert(DOMAIN_CONN_CAPS_KEY.to_owned(), String::new());
    events.publish(fluxdown_protocol::DaemonEvent::ConfigChanged(
        fluxdown_protocol::DaemonConfigSnapshot {
            revision: all
                .get("daemon_config_revision")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0),
            values: crate::config::public_config_values(&all),
        },
    ));
    Ok(fluxdown_protocol::ConnPolicySummaryDto { domain_count: 0 })
}

/// 引擎持久化域名连接策略的 config 键（引擎侧 `segment_coordinator` 私有常量）。
const DOMAIN_CONN_CAPS_KEY: &str = "domain_conn_caps";

/// 删除单个站点凭据；站点不存在返回 [`ActorError::NotFound`]。
async fn delete_site_auth(
    db: &fluxdown_engine::db::Db,
    site: &str,
) -> Result<Vec<fluxdown_protocol::SiteAuthEntryDto>, ActorError> {
    let json = db
        .get_config(fluxdown_engine::site_auth::SITE_AUTH_CONFIG_KEY)
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .unwrap_or_default();
    let mut store = fluxdown_engine::site_auth::parse_store(&json);
    if store.remove(site).is_none() {
        return Err(ActorError::NotFound);
    }
    save_site_auth(db, &store).await
}

/// 写回站点凭据表（与引擎 `apply_site_auth` 同一 `set_config` 路径：该键
/// 不属用户配置目录，不触碰 revision），返回脱敏后的列表。
async fn save_site_auth(
    db: &fluxdown_engine::db::Db,
    store: &BTreeMap<String, fluxdown_engine::site_auth::SiteCredential>,
) -> Result<Vec<fluxdown_protocol::SiteAuthEntryDto>, ActorError> {
    db.set_config(
        fluxdown_engine::site_auth::SITE_AUTH_CONFIG_KEY,
        &fluxdown_engine::site_auth::serialize_store(store),
    )
    .await
    .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    Ok(site_auth_entries(store))
}

/// 站点凭据表的脱敏投影（只含站点键与用户名，密码永不出 daemon）。
pub fn site_auth_entries(
    store: &BTreeMap<String, fluxdown_engine::site_auth::SiteCredential>,
) -> Vec<fluxdown_protocol::SiteAuthEntryDto> {
    store
        .iter()
        .map(|(site, credential)| fluxdown_protocol::SiteAuthEntryDto {
            site: site.clone(),
            user: credential.user.clone(),
        })
        .collect()
}

/// 请求未指定队列时套用设置项 `default_queue_id`（与 Flutter 新建对话框
/// 预选默认队列同义）；默认队列为空或已被删除时保持空串，由引擎归入
/// 内置主队列。
async fn resolve_queue_id(
    db: &fluxdown_engine::db::Db,
    requested: String,
) -> Result<String, ActorError> {
    if !requested.is_empty() {
        return Ok(requested);
    }
    let Some(preferred) = db
        .get_config("default_queue_id")
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .filter(|value| !value.is_empty())
    else {
        return Ok(requested);
    };
    if fluxdown_engine::model::is_builtin_queue(&preferred) {
        return Ok(preferred);
    }
    let exists = db
        .load_all_queues()
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .iter()
        .any(|queue| queue.queue_id == preferred);
    Ok(if exists { preferred } else { requested })
}

async fn apply_live_config<'a>(
    engine: &mut Engine,
    all: &HashMap<String, String>,
    keys: impl Iterator<Item = &'a String>,
) -> Result<(), ActorError> {
    let keys = keys.map(String::as_str).collect::<Vec<_>>();
    if keys.contains(&"max_concurrent_tasks")
        && let Some(value) = all
            .get("max_concurrent_tasks")
            .and_then(|value| value.parse::<usize>().ok())
    {
        engine.manager.set_max_concurrent(value).await;
    }
    if keys.contains(&"speed_limit_bytes")
        && let Some(value) = all
            .get("speed_limit_bytes")
            .and_then(|value| value.parse::<u64>().ok())
    {
        engine.manager.set_speed_limit(value);
    }
    if keys.contains(&"upload_limit_bytes")
        && let Some(value) = all
            .get("upload_limit_bytes")
            .and_then(|value| value.parse::<u64>().ok())
    {
        engine.manager.set_upload_speed_limit(value);
    }
    if keys.contains(&"default_save_dir")
        && let Some(value) = all.get("default_save_dir")
    {
        engine.manager.set_default_save_dir(value.clone());
    }
    if keys.contains(&"default_segments")
        && let Some(value) = all
            .get("default_segments")
            .and_then(|value| value.parse::<i32>().ok())
    {
        engine.manager.set_default_segments(value);
    }
    if keys.contains(&"auto_max_connections")
        && let Some(value) = all
            .get("auto_max_connections")
            .and_then(|value| value.parse::<i32>().ok())
    {
        engine.manager.set_auto_max_connections(value);
    }
    if keys.contains(&"cdn_multi_enabled") {
        engine.manager.set_cdn_multi_enabled(
            all.get("cdn_multi_enabled")
                .is_some_and(|value| value == "true"),
        );
    }
    if keys.contains(&"cdn_max_nodes")
        && let Some(value) = all
            .get("cdn_max_nodes")
            .and_then(|value| value.parse::<i32>().ok())
    {
        engine.manager.set_cdn_max_nodes(value);
    }
    if keys.contains(&"max_auto_retries")
        && let Some(value) = all
            .get("max_auto_retries")
            .and_then(|value| value.parse::<i32>().ok())
    {
        engine.manager.set_max_auto_retries(value);
    }
    if keys.contains(&"auto_retry_delay_secs")
        && let Some(value) = all
            .get("auto_retry_delay_secs")
            .and_then(|value| value.parse::<u64>().ok())
    {
        engine.manager.set_auto_retry_delay_secs(value);
    }
    if keys.contains(&"use_server_time") {
        engine.manager.set_use_server_time(
            all.get("use_server_time")
                .is_some_and(|value| value == "true"),
        );
    }
    if keys.contains(&"file_exists_behavior") {
        engine.manager.set_file_exists_behavior(
            all.get("file_exists_behavior")
                .map(|value| FileExistsBehavior::from_config_str(value))
                .unwrap_or(FileExistsBehavior::Rename),
        );
    }
    if keys.contains(&"file_missing_action") {
        engine.manager.set_missing_file_auto_delete(
            all.get("file_missing_action")
                .is_some_and(|value| value == "delete"),
        );
    }
    if keys.contains(&"global_user_agent")
        && let Some(value) = all.get("global_user_agent")
    {
        engine
            .manager
            .set_user_agent(value.clone())
            .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    }
    if keys.iter().any(|key| key.starts_with("proxy_")) {
        engine
            .manager
            .set_proxy_config(fluxdown_engine::proxy_config::ProxyConfig::from_config_map(
                all,
            ))
            .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    }
    if keys.iter().any(|key| key.starts_with("bt_")) {
        engine
            .manager
            .set_bt_config(crate::config::bt_config_from_map(all));
        if keys.iter().any(|key| {
            matches!(
                *key,
                "bt_enable_dht"
                    | "bt_enable_upnp"
                    | "bt_port_start"
                    | "bt_port_end"
                    | "bt_custom_trackers"
                    | "bt_tracker_sub_enabled"
                    | "bt_tracker_sub_urls"
                    | "bt_mse_mode"
            )
        }) {
            engine.manager.invalidate_bt_session().await;
        }
    }
    if keys.contains(&"webhook.endpoints")
        && let Some(value) = all.get("webhook.endpoints")
    {
        engine.manager.set_webhook_endpoints(value);
    }
    Ok(())
}

fn decode_torrent_b64(value: Option<&str>) -> Result<Vec<u8>, ActorError> {
    match value {
        Some(encoded) if !encoded.is_empty() => base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| ActorError::InvalidArgument(format!("invalid torrentB64: {error}"))),
        _ => Ok(Vec::new()),
    }
}

async fn task_ids_by_status(
    db: &fluxdown_engine::db::Db,
    statuses: &[i32],
) -> Result<Vec<String>, ActorError> {
    Ok(db
        .load_all_tasks()
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .into_iter()
        .filter(|task| statuses.contains(&task.status))
        .map(|task| task.task_id)
        .collect())
}

async fn receive_rss_event(
    receiver: &mut Option<mpsc::UnboundedReceiver<fluxdown_engine::rss::RssEvent>>,
) -> Option<fluxdown_engine::rss::RssEvent> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fluxdown_engine::site_auth::{SITE_AUTH_CONFIG_KEY, SiteCredential, serialize_store};

    use super::{ActorError, delete_site_auth, resolve_queue_id, save_site_auth};

    async fn open_db() -> (fluxdown_engine::db::Db, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_actor_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create actor test dir");
        let db = fluxdown_engine::db::Db::open(&dir)
            .await
            .expect("open actor test db");
        (db, dir)
    }

    #[tokio::test]
    async fn default_queue_applies_only_to_unspecified_and_existing_queues() {
        let (db, dir) = open_db().await;
        db.init_default_config("/tmp").await.expect("seed config");
        db.seed_builtin_queues().await.expect("seed queues");

        assert_eq!(
            resolve_queue_id(&db, "explicit".to_owned())
                .await
                .expect("explicit passthrough"),
            "explicit"
        );
        assert_eq!(
            resolve_queue_id(&db, String::new())
                .await
                .expect("builtin default"),
            fluxdown_engine::model::MAIN_QUEUE_ID
        );

        db.insert_queue("custom", "Custom", 0, 0, 0, "", 5, 0, "")
            .await
            .expect("insert queue");
        db.set_config("default_queue_id", "custom")
            .await
            .expect("set default queue");
        assert_eq!(
            resolve_queue_id(&db, String::new())
                .await
                .expect("existing named default"),
            "custom"
        );

        db.set_config("default_queue_id", "deleted")
            .await
            .expect("set stale default");
        assert_eq!(
            resolve_queue_id(&db, String::new())
                .await
                .expect("stale default falls back"),
            "",
            "engine maps '' to the builtin main queue"
        );

        drop(db);
        let _ = tokio::fs::remove_dir_all(dir).await;
    }

    #[tokio::test]
    async fn site_auth_delete_and_clear_never_expose_passwords() {
        let (db, dir) = open_db().await;
        let store = BTreeMap::from([
            (
                "a.example".to_owned(),
                SiteCredential {
                    user: "alice".to_owned(),
                    pass: "s3cret".to_owned(),
                },
            ),
            (
                "b.example:8443".to_owned(),
                SiteCredential {
                    user: "bob".to_owned(),
                    pass: "hunter2".to_owned(),
                },
            ),
        ]);
        db.set_config(SITE_AUTH_CONFIG_KEY, &serialize_store(&store))
            .await
            .expect("seed credentials");

        let after_delete = delete_site_auth(&db, "a.example")
            .await
            .expect("delete known site");
        assert_eq!(after_delete.len(), 1);
        assert_eq!(after_delete[0].site, "b.example:8443");
        assert_eq!(after_delete[0].user, "bob");
        let json = serde_json::to_string(&after_delete).expect("serialize dto");
        assert!(!json.contains("hunter2"), "password must not leak: {json}");

        assert!(matches!(
            delete_site_auth(&db, "a.example").await,
            Err(ActorError::NotFound)
        ));

        let cleared = save_site_auth(&db, &BTreeMap::new())
            .await
            .expect("clear store");
        assert!(cleared.is_empty());
        assert_eq!(
            db.get_config(SITE_AUTH_CONFIG_KEY)
                .await
                .expect("read store")
                .as_deref(),
            Some("{}")
        );

        drop(db);
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
}
