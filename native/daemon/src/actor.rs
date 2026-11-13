//! 单线程 daemon actor：独占引擎、串行领域写入并排空所有管理器回流通道。

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use base64::Engine as _;
use fluxdown_engine::Engine;
use fluxdown_engine::download_manager::{
    BT_DISABLED_MESSAGE, BatchItemOutcome, BoostError, CreateGroupSpec, CreateRejection,
    FileExistsBehavior, NewTaskSpec, TaskDone, UNSUPPORTED_PROTOCOL_MESSAGE,
    rejects_unsupported_protocol,
};
#[cfg(feature = "plugins")]
use fluxdown_engine::download_manager::{ResolveOutcome, ResolvePreviewOutcome};
use fluxdown_engine::rss::RssValidateOutcome;
use fluxdown_engine::rss::model::RssSourceInfo;
use fluxdown_protocol::{
    ApplicationErrorCode, CreateTaskItem, CreateTaskOutcome, CreateTaskRequest, RpcErrorData,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::selection::DaemonSelection;

/// 生产命令队列容量。
pub const COMMAND_CAPACITY: usize = 64;

/// 本机做种任务的登记选项（`daemon.task.seedLocal`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeedLocalOptions {
    /// 任务级做种限制全部「不限制」。
    pub unlimited: bool,
    /// 以超级做种模式做种。
    pub super_seeding: bool,
    /// 首次挂载不做哈希校验（数据可信或种子刚由这份数据生成）。
    pub skip_check: bool,
}

/// actor 领域操作。
pub enum ActorOperation {
    CreateTask {
        request: Box<CreateTaskRequest>,
        torrent_file_bytes: Vec<u8>,
        hint_file_size: i64,
        /// 参照文件名（浏览器捕获的原始名），见 `DaemonCreateTaskParams::name_hint`。
        name_hint: String,
        unattended: bool,
        /// 分类目录路由，见 `DaemonCreateTaskParams::category_route`。
        category_route: Option<fluxdown_protocol::CategoryRouteDto>,
    },
    /// BT 任务实况（`daemon.task.btStatus`）：peer 与 tracker 状态。
    BtStatus {
        task_id: String,
    },
    /// BT 监听端口与端口映射实况（`daemon.bt.portStatus`）。
    BtPortStatus,
    /// 立即向 tracker 重新汇报（`daemon.task.reannounce`）。
    ReannounceTask {
        task_id: String,
    },
    /// BT 任务的文件表（`daemon.task.btFiles`）。
    BtTaskFiles {
        task_id: String,
    },
    /// 修改 BT 任务的文件优先级（`daemon.task.setFilePriorities`）：`(文件下标, 级别)`。
    SetTaskFilePriorities {
        task_id: String,
        priorities: Vec<(u32, u8)>,
    },
    /// 重命名 BT 任务里的一个文件（`daemon.task.renameBtFile`）。
    RenameBtFile {
        task_id: String,
        index: u32,
        path: String,
    },
    /// 强制重新校验未完成的 BT 任务（`daemon.task.recheck`）。
    RecheckTask {
        task_id: String,
    },
    /// 把 BT 任务的数据移动到新的保存目录（`daemon.task.moveStorage`，搬移在 actor 之外）。
    MoveTaskStorage {
        task_id: String,
        save_dir: String,
    },
    /// eD2K 任务实况（`daemon.task.ed2kStatus`）：源、服务器、Kad 节点与本机网络状态。
    Ed2kStatus {
        task_id: String,
    },
    /// BT 任务的分享材料（`daemon.task.torrentShare`）。
    TorrentShare {
        task_id: String,
        include_torrent: bool,
    },
    /// 本机做种（`daemon.task.seedLocal`）：`torrent_bytes` 已由服务层在 actor 之外生成 / 读取。
    SeedLocal {
        path: String,
        torrent_bytes: Vec<u8>,
        options: SeedLocalOptions,
    },
    /// 批量建任务（`daemon.task.createMany`）：保存目录与队列只解析一次，
    /// 整块一个事务落库，按 `batch_id` 幂等。
    CreateTasks {
        batch_id: String,
        /// 整块共享的选项（`url` / `fileName` / `checksum` 已由服务层校验为空）。
        common: Box<CreateTaskRequest>,
        items: Vec<CreateTaskItem>,
        /// 分类目录路由，对每一条生效，见 `DaemonCreateTasksParams::category_route`。
        category_route: Option<fluxdown_protocol::CategoryRouteDto>,
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
    ChangeTaskUrl {
        task_id: String,
        url: String,
    },
    SetTaskSegments {
        task_id: String,
        segments: i32,
    },
    SetTaskPiecePicking {
        task_id: String,
        sequential: Option<bool>,
        first_last_pieces_first: Option<bool>,
    },
    /// `daemon.task.setBtLimits`：`None` = 该方向不变，`0` = 不限。
    SetTaskBtLimits {
        task_id: String,
        download_limit_bps: Option<i64>,
        upload_limit_bps: Option<i64>,
    },
    /// `daemon.task.setSuperSeeding`。
    SetTaskSuperSeeding {
        task_id: String,
        enabled: bool,
    },
    DeleteTask {
        task_id: String,
        delete_files: bool,
    },
    PauseAll,
    ResumeAll,
    RescanFiles,
    PauseTasks {
        task_ids: Vec<String>,
    },
    ResumeTasks {
        task_ids: Vec<String>,
    },
    DeleteTasks {
        task_ids: Vec<String>,
        delete_files: bool,
    },
    SetTaskSeedLimits {
        task_id: String,
        ratio_limit_milli: i64,
        post_ratio_limit_milli: i64,
        seed_time_limit_minutes: i64,
        inactive_time_limit_minutes: i64,
        upload_limit_bps: i64,
    },
    /// 已完成 BT 任务的完整性校验（off-actor 哈希）。
    VerifyTask {
        task_id: String,
    },
    /// 已完成任务落盘文件的哈希（off-actor 读盘，actor 内写回）。
    HashTask {
        task_id: String,
        algorithm: fluxdown_engine::file_hash::HashAlgorithm,
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
    SiteAuthSave {
        site: String,
        user: String,
        pass: String,
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
    /// 刷新 Kad 节点（nodes.dat）订阅（`daemon.ed2k.nodesSubscription.refresh` 与启动时）。
    RefreshEd2kNodes,
    /// HighID / LowID 检测（`daemon.ed2k.idCheck`）。
    Ed2kIdCheck {
        params: fluxdown_protocol::Ed2kIdCheckParams,
    },
    /// Kad 引导（`daemon.ed2k.kadBootstrap`）。
    Ed2kKadBootstrap {
        params: fluxdown_protocol::Ed2kKadBootstrapParams,
    },
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
    /// 订阅列表：重读库（未读计数）并叠加内存运行态（上次检查时间等只在内存前进）。
    RssListSources,
    RssValidate {
        provider_id: String,
        provider_config: String,
        url: String,
        cookies: String,
        user_agent: String,
        proxy_url: String,
    },
    /// 剪贴板识别用的只读 HEAD 探测（off-actor）。
    LinkProbe {
        url: String,
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
    /// 与 `CreateTasks` 的 `items` 逐条同序的结果。
    CreatedMany(Vec<CreateTaskOutcome>),
    Boolean(bool),
    ProxyLatency(i64),
    Config(fluxdown_protocol::DaemonConfigSnapshot),
    ConnPolicy(fluxdown_protocol::ConnPolicySummaryDto),
    SiteAuth(Vec<fluxdown_protocol::SiteAuthEntryDto>),
    SiteAuthEntry(fluxdown_protocol::SiteAuthEntryDto),
    CdnLease(Option<fluxdown_protocol::CdnReportLeaseDto>),
    TrackerRefresh(fluxdown_protocol::TrackerSubRefreshResponse),
    Ed2kRefresh(fluxdown_protocol::Ed2kServerSubRefreshResponse),
    Ed2kNodesRefresh(fluxdown_protocol::Ed2kNodesSubRefreshResponse),
    Ed2kNetwork(Box<fluxdown_protocol::Ed2kNetworkDto>),
    Ed2kStatus(Box<fluxdown_protocol::Ed2kStatusDto>),
    Ed2kKadBootstrap(Box<fluxdown_protocol::Ed2kKadBootstrapDto>),
    LinkMigration(fluxdown_protocol::LinkMigrationExport),
    GatewayMigration(fluxdown_protocol::GatewayMigrationExport),
    #[cfg(feature = "plugins")]
    ResolvePreview(ResolvePreviewOutcome),
    RssValidation(Box<RssValidateOutcome>),
    LinkProbe(fluxdown_protocol::LinkProbeResult),
    RssSources(Vec<RssSourceInfo>),
    WebhookDeliveries(Vec<fluxdown_engine::webhook::WebhookDelivery>),
    WebhookSimulation(usize),
    WebhookTest(Box<fluxdown_engine::webhook::WebhookDelivery>),
    TaskVerify(fluxdown_protocol::TaskVerifyResult),
    TaskHash(fluxdown_protocol::TaskHashResult),
    SeedLocal(fluxdown_protocol::SeedLocalResultDto),
    TorrentShare(Box<fluxdown_protocol::TorrentShareDto>),
    BtStatus(Box<fluxdown_protocol::BtStatusDto>),
    BtPortStatus(Box<fluxdown_protocol::BtPortStatusDto>),
    BtFiles(Box<fluxdown_protocol::BtFilesDto>),
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
    /// 资源正被另一操作占用（如同一任务的校验已在进行）。
    #[error("conflict: {0}")]
    Conflict(String),
    /// 任务当前状态不允许该操作（如对已完成 / 失败任务设优先下载）。
    #[error("task state conflict: {0}")]
    TaskStateConflict(String),
    /// 删除被拒：任务文件删不掉，任务记录已保留（`in_use` = 文件正被其他程序占用）。
    #[error("{message}")]
    TaskFilesKept { in_use: bool, message: String },
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
    #[cfg(feature = "bt")]
    Tracker {
        outcome: fluxdown_engine::tracker_subscription::FetchOutcome,
        ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    },
    #[cfg(feature = "ed2k")]
    Ed2k {
        outcome: fluxdown_engine::ed2k::server_subscription::ServerFetchOutcome,
        ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    },
    #[cfg(feature = "ed2k")]
    Ed2kNodes {
        outcome: fluxdown_engine::ed2k::kad::NodesFetchOutcome,
        ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    },
    #[cfg(feature = "ed2k")]
    Ed2kKadBootstrap {
        report: fluxdown_engine::ed2k::kad::BootstrapReport,
        ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    },
    /// `daemon.task.hash` 的计算结果：写回与任务行发布在 actor 内。
    FileHash {
        outcome: fluxdown_engine::file_hash::FileHashOutcome,
        ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    },
    /// `daemon.task.moveStorage` 在等下载循环退出：稍后再试一次。
    #[cfg(feature = "bt")]
    BtMoveRetry(BtMoveRequest),
    /// `daemon.task.moveStorage` 的搬移结果：落库与恢复任务状态在 actor 内。
    #[cfg(feature = "bt")]
    BtMoveDone {
        outcome: fluxdown_engine::download_manager::BtMoveOutcome,
        ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    },
}

/// 一次 `daemon.task.moveStorage` 请求（等待下载循环退出期间在 actor 与定时重试之间往返）。
#[cfg(feature = "bt")]
pub struct BtMoveRequest {
    task_id: String,
    save_dir: String,
    /// 任务是为这次搬移才暂停的：搬完（或放弃）后恢复下载。
    resume_after: bool,
    /// 等下载循环退出的截止时间。
    deadline: tokio::time::Instant,
    ack: oneshot::Sender<Result<ActorResult, ActorError>>,
}

/// `daemon.task.moveStorage` 等下载循环退出的上限：暂停后的收尾正常在一秒内结束。
#[cfg(feature = "bt")]
const BT_MOVE_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// 等下载循环退出时两次检查的间隔。
#[cfg(feature = "bt")]
const BT_MOVE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// 从 `DownloadManager` 一次性取出的全部宿主回流接收端。
pub struct EngineReceivers {
    pub done: mpsc::Receiver<TaskDone>,
    pub retry: mpsc::Receiver<String>,
    pub plugin: mpsc::UnboundedReceiver<PluginEvent>,
    pub missing_cleanup: mpsc::Receiver<Vec<String>>,
    /// 「下载完成后计算哈希」的结果回流。
    pub file_hash: mpsc::UnboundedReceiver<fluxdown_engine::file_hash::FileHashOutcome>,
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

    pub(crate) fn test_channel() -> (Self, mpsc::Receiver<DaemonCommand>) {
        let (commands, receiver) = mpsc::channel(1);
        (Self { commands }, receiver)
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
    selections.spawn_task_watcher(cancel.clone());
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
    publish_webhook_history(&engine, &events);
    if let Ok(config) = engine.db.get_all_config().await {
        apply_log_limit(&config);
    }
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
    let mut seeding_upload =
        tokio::time::interval(fluxdown_engine::bt_seeding::SEEDING_UPLOAD_SAMPLE_INTERVAL);
    seeding_upload.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut queue_positions = tokio::time::interval(Duration::from_millis(250));
    queue_positions.set_missed_tick_behavior(MissedTickBehavior::Skip);

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
                        selections.resolve_all_for_shutdown();
                        cancel.cancel();
                        if ack.send(()).is_err() {
                            tracing::debug!("daemon shutdown caller disconnected before acknowledgement");
                        }
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
                commit_maintenance(event, &mut engine, &events, &maintenance_tx).await;
            }
            Some(outcome) = receivers.file_hash.recv() => {
                let task_id = outcome.task_id().to_owned();
                if let Err(error) = engine.manager.commit_file_hash(outcome).await {
                    log_auto_file_hash_error(&task_id, &error);
                }
            }
            Some(ids) = receivers.missing_cleanup.recv() => {
                // 删除事实由 delete_tasks_batch 自己发布（TasksDeleted），无需再推全量快照。
                if let Err(error) = batch_delete_result(
                    engine.manager.delete_tasks_batch(&ids, false).await,
                ) {
                    tracing::warn!(%error, "removing tasks whose files vanished failed");
                }
            }
            _ = file_scan.tick() => {
                // 完全空闲且 idle_file_scan 关闭时跳过，避免周期性唤醒 NAS 硬盘。
                if engine.manager.should_run_idle_scan() {
                    engine.manager.spawn_file_scan();
                }
            }
            _ = queue_schedule.tick() => engine.manager.tick_queue_schedules().await,
            _ = queue_positions.tick() => engine.manager.flush_queue_positions(),
            _ = rss_poll.tick() => engine.manager.tick_rss_sources(),
            event = receive_rss_event(&mut rss_events), if rss_events.is_some() => {
                if let Some(event) = event { engine.manager.on_rss_event(event).await; }
            }
            _ = seeding.tick() => engine.manager.tick_seeding_evaluation().await,
            _ = seeding_upload.tick() => engine.manager.tick_seeding_upload().await,
        }
    }

    engine.manager.shutdown().await;
    match tokio::time::timeout(Duration::from_secs(5), engine.flush_task_activity()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!(%error, "daemon task activity flush failed"),
        Err(_) => tracing::error!("daemon task activity flush timed out"),
    }
    match tokio::time::timeout(Duration::from_secs(5), engine.db.flush_speed_history()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!(%error, "daemon speed history flush failed"),
        Err(_) => tracing::error!("daemon speed history flush timed out"),
    }
    selections.resolve_all_for_shutdown();
    commands.close();
    while let Some(command) = commands.recv().await {
        match command {
            DaemonCommand::Execute { ack, .. } => {
                if ack
                    .send(Err(ActorError::Operation(
                        "daemon is shutting down".to_owned(),
                    )))
                    .is_err()
                {
                    tracing::debug!("daemon caller disconnected during shutdown");
                }
            }
            DaemonCommand::Shutdown { ack } => {
                if ack.send(()).is_err() {
                    tracing::debug!("daemon shutdown caller disconnected before acknowledgement");
                }
            }
        }
    }
}

/// 本构建没有对应协议后端（`bt` / `ed2k` feature 关闭）：明确拒绝 tracker / eD2K
/// 订阅这类只对该协议有意义的维护操作，而不是静默成功。
#[cfg(not(all(feature = "bt", feature = "ed2k")))]
fn reject_unsupported_maintenance(ack: oneshot::Sender<Result<ActorResult, ActorError>>) {
    if ack
        .send(Err(ActorError::InvalidArgument(
            UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
        )))
        .is_err()
    {
        tracing::debug!("maintenance caller disconnected before unsupported-protocol rejection");
    }
}

async fn dispatch_operation(
    operation: ActorOperation,
    ack: oneshot::Sender<Result<ActorResult, ActorError>>,
    engine: &mut Engine,
    events: &crate::event_hub::DaemonEventHub,
    #[cfg_attr(
        not(any(feature = "bt", feature = "ed2k")),
        allow(unused_variables)
    )]
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
                if let Err(result) = ack.send(result) {
                    match result {
                        Err(ActorError::Operation(error)) => {
                            tracing::warn!(%error, "resolve preview failed after caller disconnected");
                        }
                        _ => tracing::debug!("resolve preview caller disconnected before result"),
                    }
                }
            });
        }
        ActorOperation::RefreshTrackerSubscription => {
            #[cfg(feature = "bt")]
            {
                let config = match engine.db.get_all_config().await {
                    Ok(config) => config,
                    Err(error) => {
                        if ack
                            .send(Err(ActorError::Operation(format!("{error:#}"))))
                            .is_err()
                        {
                            tracing::warn!(%error, "tracker refresh config read failed after caller disconnected");
                        }
                        return;
                    }
                };
                let urls = config.get("bt_tracker_sub_urls").cloned().unwrap_or_else(
                    fluxdown_engine::tracker_subscription::default_subscription_urls,
                );
                let proxy = engine.manager.proxy_config().clone();
                let maintenance_tx = maintenance_tx.clone();
                tokio::spawn(async move {
                    let outcome =
                        fluxdown_engine::tracker_subscription::fetch_subscriptions(&urls, &proxy)
                            .await;
                    if maintenance_tx
                        .send(MaintenanceEvent::Tracker { outcome, ack })
                        .is_err()
                    {
                        // Actor 已关闭，未提交的结果丢弃并关闭回执，等待者收到 Unavailable。
                        tracing::debug!("daemon actor closed before tracker refresh commit");
                    }
                });
            }
            #[cfg(not(feature = "bt"))]
            reject_unsupported_maintenance(ack);
        }
        ActorOperation::RefreshEd2kServerSubscription => {
            #[cfg(feature = "ed2k")]
            {
                let config = match engine.db.get_all_config().await {
                    Ok(config) => config,
                    Err(error) => {
                        if ack
                            .send(Err(ActorError::Operation(format!("{error:#}"))))
                            .is_err()
                        {
                            tracing::warn!(%error, "ED2K refresh config read failed after caller disconnected");
                        }
                        return;
                    }
                };
                let urls = config.get("ed2k_server_sub_urls").cloned().unwrap_or_else(
                    fluxdown_engine::ed2k::server_subscription::default_server_met_urls,
                );
                let proxy = engine.manager.proxy_config().clone();
                let maintenance_tx = maintenance_tx.clone();
                tokio::spawn(async move {
                    let outcome =
                        fluxdown_engine::ed2k::server_subscription::fetch_server_subscriptions(
                            &urls, &proxy,
                        )
                        .await;
                    if maintenance_tx
                        .send(MaintenanceEvent::Ed2k { outcome, ack })
                        .is_err()
                    {
                        // Actor 已关闭，未提交的结果丢弃并关闭回执，等待者收到 Unavailable。
                        tracing::debug!("daemon actor closed before ED2K refresh commit");
                    }
                });
            }
            #[cfg(not(feature = "ed2k"))]
            reject_unsupported_maintenance(ack);
        }
        ActorOperation::RefreshEd2kNodes => {
            #[cfg(feature = "ed2k")]
            {
                let urls = match engine.db.get_config("ed2k_nodes_dat_url").await {
                    Ok(urls) => urls.unwrap_or_default(),
                    Err(error) => {
                        if ack
                            .send(Err(ActorError::Operation(format!("{error:#}"))))
                            .is_err()
                        {
                            tracing::warn!(%error, "ED2K nodes config read failed after caller disconnected");
                        }
                        return;
                    }
                };
                let proxy = engine.manager.proxy_config().clone();
                let maintenance_tx = maintenance_tx.clone();
                tokio::spawn(async move {
                    let outcome =
                        fluxdown_engine::ed2k::kad::fetch_nodes_subscriptions(&urls, &proxy).await;
                    if maintenance_tx
                        .send(MaintenanceEvent::Ed2kNodes { outcome, ack })
                        .is_err()
                    {
                        // Actor 已关闭，未提交的结果丢弃并关闭回执，等待者收到 Unavailable。
                        tracing::debug!("daemon actor closed before ED2K nodes commit");
                    }
                });
            }
            #[cfg(not(feature = "ed2k"))]
            reject_unsupported_maintenance(ack);
        }
        ActorOperation::Ed2kIdCheck { params } => {
            #[cfg(feature = "ed2k")]
            {
                use fluxdown_engine::ed2k::client::IdCheckTarget;
                use fluxdown_protocol::Ed2kIdCheckTarget;
                let target = match params.target {
                    Ed2kIdCheckTarget::Auto => IdCheckTarget::Auto,
                    Ed2kIdCheckTarget::Session => IdCheckTarget::Session,
                    Ed2kIdCheckTarget::List => IdCheckTarget::List,
                    Ed2kIdCheckTarget::Server => IdCheckTarget::Server(params.server),
                };
                // 检测要等 UPnP 映射与服务器登录，可达数十秒：只在 actor 内取一份库句柄。
                let db = engine.db.clone();
                tokio::spawn(async move {
                    let result = fluxdown_engine::ed2k::check_id(db, target)
                        .await
                        .map(|view| ActorResult::Ed2kNetwork(Box::new(ed2k_network_to_dto(view))))
                        .map_err(|error| ActorError::Operation(format!("{error:#}")));
                    if let Err(result) = ack.send(result) {
                        match result {
                            Err(ActorError::Operation(error)) => {
                                tracing::warn!(%error, "ED2K id check failed after caller disconnected");
                            }
                            _ => tracing::debug!("ED2K id check caller disconnected before result"),
                        }
                    }
                });
            }
            #[cfg(not(feature = "ed2k"))]
            {
                let _ = params;
                reject_unsupported_maintenance(ack);
            }
        }
        ActorOperation::Ed2kKadBootstrap { params } => {
            #[cfg(feature = "ed2k")]
            {
                use fluxdown_engine::ed2k::kad::BootstrapSource;
                use fluxdown_protocol::Ed2kKadBootstrapSource;
                let source = match params.source {
                    Ed2kKadBootstrapSource::Known => BootstrapSource::Known,
                    Ed2kKadBootstrapSource::Url => BootstrapSource::Url(params.url),
                    Ed2kKadBootstrapSource::Node => BootstrapSource::Node {
                        host: params.host,
                        port: params.port,
                    },
                };
                // actor 内只读配置；取 nodes.dat、解析域名与 UDP 握手都在 actor 之外。
                let inputs = async {
                    let cached = fluxdown_engine::ed2k::load_nodes_dat(&engine.db).await?;
                    let urls = engine
                        .db
                        .get_config("ed2k_nodes_dat_url")
                        .await?
                        .unwrap_or_default();
                    let listen_port = engine
                        .db
                        .get_config("ed2k_listen_port")
                        .await?
                        .and_then(|value| value.parse::<u16>().ok())
                        .unwrap_or(0);
                    Ok::<_, fluxdown_engine::db::DbError>((cached, urls, listen_port))
                }
                .await;
                let (cached, urls, listen_port) = match inputs {
                    Ok(inputs) => inputs,
                    Err(error) => {
                        if ack
                            .send(Err(ActorError::Operation(format!("{error:#}"))))
                            .is_err()
                        {
                            tracing::warn!(%error, "Kad bootstrap config read failed after caller disconnected");
                        }
                        return;
                    }
                };
                let proxy = engine.manager.proxy_config().clone();
                let maintenance_tx = maintenance_tx.clone();
                tokio::spawn(async move {
                    let report = fluxdown_engine::ed2k::kad::bootstrap(
                        source,
                        &cached,
                        &urls,
                        listen_port,
                        &proxy,
                    )
                    .await;
                    if maintenance_tx
                        .send(MaintenanceEvent::Ed2kKadBootstrap { report, ack })
                        .is_err()
                    {
                        // Actor 已关闭，未提交的结果丢弃并关闭回执，等待者收到 Unavailable。
                        tracing::debug!("daemon actor closed before Kad bootstrap commit");
                    }
                });
            }
            #[cfg(not(feature = "ed2k"))]
            {
                let _ = params;
                reject_unsupported_maintenance(ack);
            }
        }
        ActorOperation::RssValidate {
            provider_id,
            provider_config,
            url,
            cookies,
            user_agent,
            proxy_url,
        } => {
            let future = engine.manager.rss_validate_future(
                provider_id,
                provider_config,
                url,
                cookies,
                user_agent,
                proxy_url,
            );
            tokio::spawn(async move {
                if ack
                    .send(Ok(ActorResult::RssValidation(Box::new(future.await))))
                    .is_err()
                {
                    tracing::debug!("RSS validation caller disconnected before result");
                }
            });
        }
        ActorOperation::LinkProbe { url } => {
            let future = engine.manager.link_probe_future(url);
            tokio::spawn(async move {
                let result = link_probe_to_dto(future.await);
                if ack.send(Ok(ActorResult::LinkProbe(result))).is_err() {
                    tracing::debug!("link probe caller disconnected before result");
                }
            });
        }
        #[cfg_attr(not(feature = "bt"), allow(unused_variables))]
        ActorOperation::VerifyTask { task_id } => {
            #[cfg(feature = "bt")]
            {
                let result = match engine.manager.bt_integrity_check_future(&task_id).await {
                    Ok(future) => {
                        tokio::spawn(async move {
                            let result = future
                                .await
                                .map(|report| ActorResult::TaskVerify(task_verify_to_dto(report)))
                                .map_err(integrity_error);
                            if ack.send(result).is_err() {
                                tracing::debug!(
                                    "integrity check caller disconnected before result"
                                );
                            }
                        });
                        return;
                    }
                    Err(error) => Err(integrity_error(error)),
                };
                if ack.send(result).is_err() {
                    tracing::debug!("integrity check caller disconnected before rejection");
                }
            }
            #[cfg(not(feature = "bt"))]
            {
                if ack
                    .send(Err(ActorError::InvalidArgument(
                        UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                    )))
                    .is_err()
                {
                    tracing::debug!("integrity check caller disconnected before rejection");
                }
            }
        }
        ActorOperation::HashTask { task_id, algorithm } => {
            match engine.manager.file_hash_future(&task_id, algorithm).await {
                Ok(future) => {
                    let maintenance_tx = maintenance_tx.clone();
                    tokio::spawn(async move {
                        let outcome = future.await;
                        if maintenance_tx
                            .send(MaintenanceEvent::FileHash { outcome, ack })
                            .is_err()
                        {
                            // Actor 已关闭，结果不写回；回执随事件丢弃，等待者收到 Unavailable。
                            tracing::debug!("daemon actor closed before file hash commit");
                        }
                    });
                }
                Err(error) => {
                    if ack.send(Err(file_hash_error(error))).is_err() {
                        tracing::debug!("file hash caller disconnected before rejection");
                    }
                }
            }
        }
        ActorOperation::TestProxy {
            proxy_type,
            host,
            port,
            username,
            password,
        } => {
            // 连通性测试最长数十秒且不依赖引擎状态，不能占用 actor 循环。
            tokio::spawn(async move {
                let result = fluxdown_engine::proxy_config::test_proxy_connection(
                    &proxy_type,
                    &host,
                    &port,
                    &username,
                    &password,
                )
                .await
                .map(ActorResult::ProxyLatency)
                .map_err(|error| ActorError::Operation(format!("{error:#}")));
                if let Err(result) = ack.send(result) {
                    match result {
                        Err(ActorError::Operation(error)) => {
                            tracing::warn!(%error, "proxy test failed after caller disconnected");
                        }
                        _ => tracing::debug!("proxy test caller disconnected before result"),
                    }
                }
            });
        }
        ActorOperation::MoveTaskStorage { task_id, save_dir } => {
            #[cfg(feature = "bt")]
            start_bt_move(
                engine,
                maintenance_tx,
                BtMoveRequest {
                    task_id,
                    save_dir,
                    resume_after: false,
                    deadline: tokio::time::Instant::now() + BT_MOVE_STOP_TIMEOUT,
                    ack,
                },
            )
            .await;
            #[cfg(not(feature = "bt"))]
            {
                let _ = (task_id, save_dir);
                if ack
                    .send(Err(ActorError::InvalidArgument(
                        UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                    )))
                    .is_err()
                {
                    tracing::debug!("storage move caller disconnected before rejection");
                }
            }
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
                if let Err(result) = ack.send(result) {
                    match result {
                        Err(ActorError::Operation(error)) => {
                            tracing::warn!(%error, "webhook test failed after caller disconnected");
                        }
                        _ => tracing::debug!("webhook test caller disconnected before result"),
                    }
                }
            });
        }
        operation => {
            let result = execute_operation(operation, engine, events).await;
            if let Err(result) = ack.send(result) {
                match result {
                    Err(ActorError::Operation(error)) => {
                        tracing::warn!(%error, "daemon operation failed after caller disconnected");
                    }
                    _ => tracing::debug!("daemon operation caller disconnected before result"),
                }
            }
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
            name_hint,
            unattended,
            category_route,
        } => {
            let request = *request;
            if torrent_file_bytes.is_empty() {
                torrent_file_bytes = decode_torrent_b64(request.torrent_b64.as_deref())?;
            }
            // BT 被禁用：在落库前回报明确原因，而不是让 create_task 的无原因 `None`
            // 变成误导性的「failed to persist task」。
            if engine
                .manager
                .rejects_new_bt_task(&request.url, &torrent_file_bytes)
            {
                return Err(ActorError::InvalidArgument(BT_DISABLED_MESSAGE.to_owned()));
            }
            if rejects_unsupported_protocol(&request.url) {
                return Err(ActorError::InvalidArgument(
                    UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                ));
            }
            let save_dir = resolve_save_dir(&engine.db, request.save_dir).await?;
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
                    name_hint,
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
                    category_route: category_route
                        .map(fluxdown_engine_protocol::category_route_to_engine),
                    ..Default::default()
                })
                .await
                .ok_or_else(|| ActorError::Operation("failed to persist task".to_owned()))?;
            return Ok(ActorResult::Created(task_id));
        }
        ActorOperation::SeedLocal {
            path,
            torrent_bytes,
            options,
        } => {
            #[cfg(feature = "bt")]
            {
                use fluxdown_engine::bt_local_seed::{LocalSeedError, plan_local_seed};
                let local_error = |error: LocalSeedError| match error {
                    LocalSeedError::InvalidArgument(message) => {
                        ActorError::InvalidArgument(message)
                    }
                    LocalSeedError::Operation(message) => ActorError::Operation(message),
                    error @ LocalSeedError::Duplicate { .. } => {
                        ActorError::Operation(error.to_string())
                    }
                };
                let plan = plan_local_seed(std::path::Path::new(&path), torrent_bytes)
                    .map_err(local_error)?;
                let queue_id = resolve_queue_id(&engine.db, String::new()).await?;
                let created = engine
                    .manager
                    .create_local_seed(
                        plan,
                        queue_id,
                        fluxdown_engine::bt_local_seed::LocalSeedOptions {
                            unlimited: options.unlimited,
                            super_seeding: options.super_seeding,
                            skip_check: options.skip_check,
                        },
                    )
                    .await;
                return match created {
                    Ok(task_id) => Ok(ActorResult::SeedLocal(
                        fluxdown_protocol::SeedLocalResultDto {
                            task_id,
                            duplicate: false,
                            existing_name: String::new(),
                            torrent_path: String::new(),
                        },
                    )),
                    Err(LocalSeedError::Duplicate { task_id, name }) => Ok(ActorResult::SeedLocal(
                        fluxdown_protocol::SeedLocalResultDto {
                            task_id,
                            duplicate: true,
                            existing_name: name,
                            torrent_path: String::new(),
                        },
                    )),
                    Err(error) => Err(local_error(error)),
                };
            }
            #[cfg(not(feature = "bt"))]
            {
                let _ = (path, torrent_bytes, options);
                return Err(ActorError::InvalidArgument(
                    UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                ));
            }
        }
        ActorOperation::BtStatus { task_id } => {
            #[cfg(feature = "bt")]
            {
                use fluxdown_engine::bt_local_seed::LocalSeedError;
                let view =
                    engine
                        .manager
                        .bt_status(&task_id)
                        .await
                        .map_err(|error| match error {
                            LocalSeedError::InvalidArgument(message) => {
                                ActorError::InvalidArgument(message)
                            }
                            other => ActorError::Operation(other.to_string()),
                        })?;
                return Ok(ActorResult::BtStatus(Box::new(bt_status_to_dto(view))));
            }
            #[cfg(not(feature = "bt"))]
            {
                let _ = task_id;
                return Err(ActorError::InvalidArgument(
                    UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                ));
            }
        }
        ActorOperation::BtPortStatus => {
            #[cfg(feature = "bt")]
            {
                let view = engine.manager.bt_port_status();
                return Ok(ActorResult::BtPortStatus(Box::new(bt_port_status_to_dto(
                    view,
                ))));
            }
            // 没有 BT 后端的构建：会话永不运行。
            #[cfg(not(feature = "bt"))]
            {
                return Ok(ActorResult::BtPortStatus(Box::default()));
            }
        }
        ActorOperation::ReannounceTask { task_id } => {
            #[cfg(feature = "bt")]
            {
                engine
                    .manager
                    .reannounce_task(&task_id)
                    .await
                    .map_err(bt_task_op_error)?;
            }
            #[cfg(not(feature = "bt"))]
            {
                let _ = task_id;
                return Err(ActorError::InvalidArgument(
                    UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                ));
            }
        }
        ActorOperation::BtTaskFiles { task_id } => {
            #[cfg(feature = "bt")]
            {
                let view = engine
                    .manager
                    .bt_task_files(&task_id)
                    .await
                    .map_err(bt_task_op_error)?;
                return Ok(ActorResult::BtFiles(Box::new(
                    fluxdown_protocol::BtFilesDto {
                        editable: view.editable,
                        files: view
                            .files
                            .into_iter()
                            .map(|file| fluxdown_protocol::BtTaskFileDto {
                                index: file.index,
                                path: file.path,
                                size: file.size,
                                downloaded: file.downloaded,
                                priority: file.priority,
                            })
                            .collect(),
                    },
                )));
            }
            #[cfg(not(feature = "bt"))]
            {
                let _ = task_id;
                return Err(ActorError::InvalidArgument(
                    UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                ));
            }
        }
        ActorOperation::SetTaskFilePriorities {
            task_id,
            priorities,
        } => {
            #[cfg(feature = "bt")]
            {
                engine
                    .manager
                    .set_task_file_priorities(&task_id, &priorities)
                    .await
                    .map_err(bt_task_op_error)?;
                // 选择变了任务大小也变：补一次快照让客户端看到新的总量。
                engine.manager.send_tasks_snapshot().await;
            }
            #[cfg(not(feature = "bt"))]
            {
                let _ = (task_id, priorities);
                return Err(ActorError::InvalidArgument(
                    UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                ));
            }
        }
        ActorOperation::RenameBtFile {
            task_id,
            index,
            path,
        } => {
            #[cfg(feature = "bt")]
            {
                engine
                    .manager
                    .rename_bt_file(&task_id, index, &path)
                    .await
                    .map_err(bt_task_op_error)?;
            }
            #[cfg(not(feature = "bt"))]
            {
                let _ = (task_id, index, path);
                return Err(ActorError::InvalidArgument(
                    UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                ));
            }
        }
        ActorOperation::RecheckTask { task_id } => {
            #[cfg(feature = "bt")]
            {
                engine
                    .manager
                    .recheck_bt_task(&task_id)
                    .await
                    .map_err(bt_task_op_error)?;
            }
            #[cfg(not(feature = "bt"))]
            {
                let _ = task_id;
                return Err(ActorError::InvalidArgument(
                    UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                ));
            }
        }
        ActorOperation::Ed2kStatus { task_id } => {
            #[cfg(feature = "ed2k")]
            {
                use fluxdown_engine::ed2k::status::StatusError;
                let view =
                    engine
                        .manager
                        .ed2k_status(&task_id)
                        .await
                        .map_err(|error| match error {
                            StatusError::InvalidArgument(message) => {
                                ActorError::InvalidArgument(message)
                            }
                            StatusError::Operation(message) => ActorError::Operation(message),
                        })?;
                return Ok(ActorResult::Ed2kStatus(Box::new(ed2k_status_to_dto(view))));
            }
            #[cfg(not(feature = "ed2k"))]
            {
                let _ = task_id;
                return Err(ActorError::InvalidArgument(
                    UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                ));
            }
        }
        ActorOperation::TorrentShare {
            task_id,
            include_torrent,
        } => {
            #[cfg(feature = "bt")]
            {
                use fluxdown_engine::bt_local_seed::LocalSeedError;
                let (share, torrent) = engine
                    .manager
                    .torrent_share(&task_id, include_torrent)
                    .await
                    .map_err(|error| match error {
                        LocalSeedError::InvalidArgument(message) => {
                            ActorError::InvalidArgument(message)
                        }
                        error @ LocalSeedError::Duplicate { .. } => {
                            ActorError::Operation(error.to_string())
                        }
                        LocalSeedError::Operation(message) => ActorError::Operation(message),
                    })?;
                return Ok(ActorResult::TorrentShare(Box::new(
                    fluxdown_protocol::TorrentShareDto {
                        info_hash: share.info_hash,
                        name: share.name,
                        magnet: share.magnet,
                        torrent_b64: torrent
                            .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes)),
                        info_hash_v2: share.info_hash_v2,
                    },
                )));
            }
            #[cfg(not(feature = "bt"))]
            {
                let _ = (task_id, include_torrent);
                return Err(ActorError::InvalidArgument(
                    UNSUPPORTED_PROTOCOL_MESSAGE.to_owned(),
                ));
            }
        }
        ActorOperation::CreateTasks {
            batch_id,
            common,
            items,
            category_route,
        } => {
            let common = *common;
            let save_dir = resolve_save_dir(&engine.db, common.save_dir).await?;
            let queue_id = resolve_queue_id(&engine.db, common.queue_id).await?;
            let extra_headers = common.headers.unwrap_or_default();
            let engine_route =
                category_route.map(fluxdown_engine_protocol::category_route_to_engine);
            let specs = items
                .into_iter()
                .map(|item| NewTaskSpec {
                    url: item.url,
                    save_dir: save_dir.clone(),
                    file_name: item.file_name,
                    segments: common.segments,
                    cookies: common.cookies.clone(),
                    referrer: common.referrer.clone(),
                    proxy_url: common.proxy_url.clone(),
                    user_agent: common.user_agent.clone(),
                    queue_id: queue_id.clone(),
                    checksum: item.checksum,
                    ignore_tls_errors: common.ignore_tls_errors,
                    extra_headers: extra_headers.clone(),
                    start_paused: common.start_paused,
                    http_user: common.http_user.clone(),
                    http_password: common.http_password.clone(),
                    save_site_auth: common.save_site_auth,
                    category_route: engine_route.clone(),
                    ..Default::default()
                })
                .collect();
            let outcomes = engine
                .manager
                .create_tasks_batch(&batch_id, specs)
                .await
                .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
            return Ok(ActorResult::CreatedMany(
                outcomes.into_iter().map(create_task_outcome).collect(),
            ));
        }
        ActorOperation::PauseTask { task_id } => engine.manager.pause_task(&task_id).await,
        ActorOperation::ResumeTask { task_id } => {
            // BT 被禁用时恢复 BT 任务会被引擎静默拒绝（任务保持原状）；
            // 在此显式回报原因，客户端才不会收到无信息的 `ok`。
            let rejected = engine
                .manager
                .bt_disabled_resume_rejections(std::slice::from_ref(&task_id))
                .await
                .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
            if !rejected.is_empty() {
                return Err(ActorError::InvalidArgument(BT_DISABLED_MESSAGE.to_owned()));
            }
            engine.manager.resume_task(&task_id).await;
        }
        ActorOperation::RenameTask { task_id, file_name } => engine
            .manager
            .rename_task(&task_id, &file_name)
            .await
            .map_err(ActorError::Operation)?,
        ActorOperation::ChangeTaskUrl { task_id, url } => engine
            .manager
            .change_task_url(&task_id, &url)
            .await
            .map_err(ActorError::Operation)?,
        ActorOperation::SetTaskSegments { task_id, segments } => {
            let updated = engine
                .manager
                .set_task_segments(&task_id, segments)
                .await
                .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
            if !updated {
                return Err(ActorError::NotFound);
            }
            // 非活跃任务引擎只改库不推事件：补一次快照让客户端看到新线程数。
            engine.manager.send_tasks_snapshot().await;
        }
        ActorOperation::SetTaskPiecePicking {
            task_id,
            sequential,
            first_last_pieces_first,
        } => {
            let updated = engine
                .manager
                .set_task_piece_picking(&task_id, sequential, first_last_pieces_first)
                .await
                .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
            if !updated {
                return Err(ActorError::NotFound);
            }
            // 引擎只改库与会话、不推事件：补一次快照让客户端看到新的选片方式。
            engine.manager.send_tasks_snapshot().await;
        }
        ActorOperation::SetTaskBtLimits {
            task_id,
            download_limit_bps,
            upload_limit_bps,
        } => {
            let updated = engine
                .manager
                .set_task_bt_limits(&task_id, download_limit_bps, upload_limit_bps)
                .await
                .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
            if !updated {
                return Err(ActorError::NotFound);
            }
            // 引擎只改库与会话、不推事件：补一次快照让客户端看到新的限速。
            engine.manager.send_tasks_snapshot().await;
        }
        ActorOperation::SetTaskSuperSeeding { task_id, enabled } => {
            let updated = engine
                .manager
                .set_task_super_seeding(&task_id, enabled)
                .await
                .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
            if !updated {
                return Err(ActorError::NotFound);
            }
            // 引擎只改库与会话、不推事件：补一次快照让客户端看到新的开关。
            engine.manager.send_tasks_snapshot().await;
        }
        ActorOperation::DeleteTask {
            task_id,
            delete_files,
        } => {
            let result = engine.manager.delete_task(&task_id, delete_files).await;
            // 失败时任务也已被取消 / 移出排队，同样推送快照。
            engine.manager.send_tasks_snapshot().await;
            result.map_err(delete_task_error)?;
        }
        ActorOperation::PauseAll => {
            let mut ids = pause_all_task_ids(&engine.db).await?;
            ids.extend(engine.manager.retry_pending_task_ids());
            // 重建连接 flush 窗口内的任务库里已是暂停态，不在上面的列表里。
            ids.extend(engine.manager.pending_resume_task_ids());
            engine.manager.batch_pause_all(&ids).await;
        }
        ActorOperation::PauseTasks { task_ids } => {
            let ids = existing_task_ids(&engine.db, task_ids).await?;
            engine.manager.pause_selected(&ids).await;
        }
        ActorOperation::ResumeTasks { task_ids } => {
            let ids = existing_task_ids(&engine.db, task_ids).await?;
            // 批量语义：BT 禁用时 BT 任务被跳过、其余照常恢复；但整批都被拒
            // 时必须回报原因，否则用户只看到「成功」而什么都没发生。
            let rejected = engine
                .manager
                .bt_disabled_resume_rejections(&ids)
                .await
                .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
            if !ids.is_empty() && rejected.len() == ids.len() {
                return Err(ActorError::InvalidArgument(BT_DISABLED_MESSAGE.to_owned()));
            }
            engine.manager.batch_resume(&ids).await;
        }
        ActorOperation::DeleteTasks {
            task_ids,
            delete_files,
        } => {
            let ids = existing_task_ids(&engine.db, task_ids).await?;
            // 删除事实由 delete_tasks_batch 经 TasksDeleted 发布：大批量删除不再逐块推全量快照。
            batch_delete_result(engine.manager.delete_tasks_batch(&ids, delete_files).await)?;
        }
        ActorOperation::ResumeAll => {
            engine.manager.resume_all_eligible().await;
        }
        ActorOperation::RescanFiles => {
            engine.manager.spawn_file_scan();
            engine.manager.purge_expired_completed_records(false).await;
        }
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
        ActorOperation::Boost { task_id } => engine
            .manager
            .set_priority_task(task_id)
            .await
            .map_err(boost_error)?,
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
        ActorOperation::SiteAuthSave { site, user, pass } => {
            return Ok(ActorResult::SiteAuthEntry(
                upsert_site_auth(&engine.db, site, user, pass).await?,
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
        } => batch_delete_result(engine.manager.delete_group(&group_id, delete_files).await)?,
        #[cfg(feature = "plugins")]
        ActorOperation::ResolvePreview { .. } => unreachable!("handled off actor"),
        ActorOperation::RssListSources => {
            engine.manager.rss.load().await;
            return Ok(ActorResult::RssSources(
                engine.manager.rss.sources().to_vec(),
            ));
        }
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
            use fluxdown_engine::rss::RssRefreshOutcome;
            // 已在抓取中同样算成功：结果稍后经 RSS 事件到达，只有订阅不存在才是 NotFound。
            return Ok(ActorResult::Boolean(!matches!(
                engine.manager.refresh_rss_source(&source_id),
                RssRefreshOutcome::NotFound
            )));
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
        | ActorOperation::Ed2kIdCheck { .. }
        | ActorOperation::Ed2kKadBootstrap { .. }
        | ActorOperation::RssValidate { .. }
        | ActorOperation::LinkProbe { .. }
        | ActorOperation::VerifyTask { .. }
        | ActorOperation::HashTask { .. }
        | ActorOperation::WebhookTest { .. }
        | ActorOperation::TestProxy { .. }
        | ActorOperation::MoveTaskStorage { .. } => unreachable!("handled off actor"),
        ActorOperation::WebhookDeliveries => {
            return Ok(ActorResult::WebhookDeliveries(engine.webhook_deliveries()));
        }
        ActorOperation::WebhookClear => {
            engine.clear_webhook_deliveries().await;
            events.publish(fluxdown_protocol::DaemonEvent::WebhooksCleared);
        }
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

/// 以库里最新的公开配置广播 `ConfigChanged`（用户配置 revision 不变）。宿主自己写入的只读键
/// （订阅缓存、条数、更新时间）只有经它才到达已连接的客户端。
fn publish_public_config(all: &HashMap<String, String>, events: &crate::event_hub::DaemonEventHub) {
    events.publish(fluxdown_protocol::DaemonEvent::ConfigChanged(
        fluxdown_protocol::DaemonConfigSnapshot {
            revision: all
                .get("daemon_config_revision")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0),
            values: crate::config::public_config_values(all),
        },
    ));
}

/// 订阅刷新落库后把新的缓存 / 更新时间推给客户端；读库失败只记日志（数据已落库，
/// 客户端下次取快照时仍会看到）。
#[cfg(any(feature = "bt", feature = "ed2k"))]
async fn publish_subscription_config(engine: &Engine, events: &crate::event_hub::DaemonEventHub) {
    match engine.db.get_all_config().await {
        Ok(all) => publish_public_config(&all, events),
        Err(error) => {
            tracing::warn!(%error, "config read failed after subscription refresh; clients keep the previous snapshot");
        }
    }
}

async fn commit_maintenance(
    event: MaintenanceEvent,
    engine: &mut Engine,
    #[cfg_attr(not(any(feature = "bt", feature = "ed2k")), allow(unused_variables))]
    events: &crate::event_hub::DaemonEventHub,
    #[cfg_attr(not(feature = "bt"), allow(unused_variables))]
    maintenance_tx: &mpsc::UnboundedSender<MaintenanceEvent>,
) {
    match event {
        #[cfg(feature = "bt")]
        MaintenanceEvent::Tracker { outcome, ack } => {
            let result = commit_tracker_refresh(outcome, engine).await;
            if result.as_ref().is_ok_and(|response| response.success) {
                publish_subscription_config(engine, events).await;
            }
            if let Err(result) = ack.send(result.map(ActorResult::TrackerRefresh)) {
                match result {
                    Err(ActorError::Operation(error)) => {
                        tracing::warn!(%error, "tracker refresh commit failed after caller disconnected");
                    }
                    _ => {
                        tracing::debug!("tracker refresh commit caller disconnected before result")
                    }
                }
            }
        }
        #[cfg(feature = "ed2k")]
        MaintenanceEvent::Ed2kNodes { outcome, ack } => {
            let result = commit_ed2k_nodes(outcome, engine).await;
            if result.as_ref().is_ok_and(|response| response.success) {
                publish_subscription_config(engine, events).await;
            }
            if let Err(result) = ack.send(result.map(ActorResult::Ed2kNodesRefresh)) {
                match result {
                    Err(ActorError::Operation(error)) => {
                        tracing::warn!(%error, "ED2K nodes commit failed after caller disconnected");
                    }
                    _ => tracing::debug!("ED2K nodes commit caller disconnected before result"),
                }
            }
        }
        #[cfg(feature = "ed2k")]
        MaintenanceEvent::Ed2kKadBootstrap { report, ack } => {
            let result = commit_ed2k_kad_bootstrap(report, engine, events).await;
            if let Err(result) =
                ack.send(result.map(|dto| ActorResult::Ed2kKadBootstrap(Box::new(dto))))
            {
                match result {
                    Err(ActorError::Operation(error)) => {
                        tracing::warn!(%error, "Kad bootstrap commit failed after caller disconnected");
                    }
                    _ => tracing::debug!("Kad bootstrap commit caller disconnected before result"),
                }
            }
        }
        #[cfg(feature = "ed2k")]
        MaintenanceEvent::Ed2k { outcome, ack } => {
            let result = commit_ed2k_refresh(outcome, engine).await;
            if result.as_ref().is_ok_and(|response| response.success) {
                publish_subscription_config(engine, events).await;
            }
            if let Err(result) = ack.send(result.map(ActorResult::Ed2kRefresh)) {
                match result {
                    Err(ActorError::Operation(error)) => {
                        tracing::warn!(%error, "ED2K refresh commit failed after caller disconnected");
                    }
                    _ => tracing::debug!("ED2K refresh commit caller disconnected before result"),
                }
            }
        }
        MaintenanceEvent::FileHash { outcome, ack } => {
            let result = engine
                .manager
                .commit_file_hash(outcome)
                .await
                .map(|file_hash| {
                    ActorResult::TaskHash(fluxdown_protocol::TaskHashResult { file_hash })
                })
                .map_err(file_hash_error);
            if ack.send(result).is_err() {
                tracing::debug!("file hash caller disconnected before result");
            }
        }
        #[cfg(feature = "bt")]
        MaintenanceEvent::BtMoveRetry(request) => {
            start_bt_move(engine, maintenance_tx, request).await;
        }
        #[cfg(feature = "bt")]
        MaintenanceEvent::BtMoveDone { outcome, ack } => {
            let result = engine
                .manager
                .finish_bt_move(outcome)
                .await
                .map(|()| ActorResult::Unit)
                .map_err(bt_task_op_error);
            if let Err(result) = ack.send(result) {
                match result {
                    Err(error) => {
                        tracing::warn!(%error, "storage move failed after caller disconnected");
                    }
                    Ok(_) => tracing::debug!("storage move caller disconnected before result"),
                }
            }
        }
    }
}

/// 开始（或在等下载循环退出后再试）一次 BT 数据搬移；结果经维护通道回到 actor。
#[cfg(feature = "bt")]
async fn start_bt_move(
    engine: &mut Engine,
    maintenance_tx: &mpsc::UnboundedSender<MaintenanceEvent>,
    request: BtMoveRequest,
) {
    use fluxdown_engine::download_manager::BtMoveStart;

    let BtMoveRequest {
        task_id,
        save_dir,
        resume_after,
        deadline,
        ack,
    } = request;
    let reply = |ack: oneshot::Sender<Result<ActorResult, ActorError>>,
                 result: Result<ActorResult, ActorError>| {
        if ack.send(result).is_err() {
            tracing::debug!("storage move caller disconnected before result");
        }
    };
    match engine
        .manager
        .begin_bt_move(&task_id, &save_dir, resume_after)
        .await
    {
        Ok(BtMoveStart::Unchanged) => reply(ack, Ok(ActorResult::Unit)),
        Ok(BtMoveStart::Wait { resume_after }) => {
            if tokio::time::Instant::now() >= deadline {
                // 放弃搬移：为它暂停的下载恢复原状。
                if resume_after {
                    engine.manager.resume_task(&task_id).await;
                }
                reply(
                    ack,
                    Err(ActorError::TaskStateConflict(
                        "the download did not stop in time; try again".to_owned(),
                    )),
                );
                return;
            }
            let tx = maintenance_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(BT_MOVE_RETRY_DELAY).await;
                let retry = BtMoveRequest {
                    task_id,
                    save_dir,
                    resume_after,
                    deadline,
                    ack,
                };
                if tx.send(MaintenanceEvent::BtMoveRetry(retry)).is_err() {
                    tracing::debug!("daemon actor closed while a storage move waited");
                }
            });
        }
        Ok(BtMoveStart::Started(job)) => {
            let tx = maintenance_tx.clone();
            tokio::spawn(async move {
                let outcome = job.await;
                if tx
                    .send(MaintenanceEvent::BtMoveDone { outcome, ack })
                    .is_err()
                {
                    // 搬移日志还在：下次启动时按它把新位置落库。
                    tracing::debug!("daemon actor closed before a storage move was committed");
                }
            });
        }
        Err(error) => {
            if resume_after {
                engine.manager.resume_task(&task_id).await;
            }
            reply(ack, Err(bt_task_op_error(error)));
        }
    }
}

/// Kad 引导的提交：把确认存活 / 新告知 / 从网址取到的节点并入已知节点缓存（新的在前），
/// 缓存有变化时广播公开配置。订阅更新时间不动——它只表示订阅地址上次拉取成功的时刻。
#[cfg(feature = "ed2k")]
async fn commit_ed2k_kad_bootstrap(
    report: fluxdown_engine::ed2k::kad::BootstrapReport,
    engine: &mut Engine,
    events: &crate::event_hub::DaemonEventHub,
) -> Result<fluxdown_protocol::Ed2kKadBootstrapDto, ActorError> {
    let db_error =
        |error: fluxdown_engine::db::DbError| ActorError::Operation(format!("{error:#}"));
    let mut node_count = engine
        .db
        .get_config("ed2k_nodes_dat_count")
        .await
        .map_err(db_error)?
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    if !report.contacts.is_empty() {
        let cached = fluxdown_engine::ed2k::load_nodes_dat(&engine.db)
            .await
            .map_err(db_error)?;
        let (nodes_dat, merged) =
            fluxdown_engine::ed2k::kad::merge_into_cache(report.contacts, &cached);
        node_count = u32::try_from(merged).unwrap_or(u32::MAX);
        let values = BTreeMap::from([
            (
                "ed2k_nodes_dat_cache".to_owned(),
                base64::engine::general_purpose::STANDARD.encode(&nodes_dat),
            ),
            ("ed2k_nodes_dat_count".to_owned(), node_count.to_string()),
        ]);
        engine
            .db
            .set_config_batch_atomic(&values)
            .await
            .map_err(db_error)?;
        publish_subscription_config(engine, events).await;
    }
    Ok(fluxdown_protocol::Ed2kKadBootstrapDto {
        contacted: report.contacted,
        responded: report.responded,
        discovered: report.discovered,
        node_count,
        nodes: report.nodes.into_iter().map(ed2k_kad_node_to_dto).collect(),
        error: report.error,
    })
}

#[cfg(feature = "ed2k")]
fn ed2k_kad_node_to_dto(
    node: fluxdown_engine::ed2k::status::KadNodeRow,
) -> fluxdown_protocol::Ed2kKadNodeDto {
    use fluxdown_engine::ed2k::status::{KadNodeOrigin, KadNodeState};
    use fluxdown_protocol::{Ed2kKadNodeOrigin, Ed2kKadNodeState};
    fluxdown_protocol::Ed2kKadNodeDto {
        addr: node.addr,
        id: node.id,
        origin: match node.origin {
            KadNodeOrigin::Bootstrap => Ed2kKadNodeOrigin::Bootstrap,
            KadNodeOrigin::Discovered => Ed2kKadNodeOrigin::Discovered,
        },
        state: match node.state {
            KadNodeState::Known => Ed2kKadNodeState::Known,
            KadNodeState::Queried => Ed2kKadNodeState::Queried,
            KadNodeState::Responded => Ed2kKadNodeState::Responded,
        },
        tcp_port: node.tcp_port,
        version: node.version,
    }
}

#[cfg(feature = "ed2k")]
async fn commit_ed2k_nodes(
    outcome: fluxdown_engine::ed2k::kad::NodesFetchOutcome,
    engine: &mut Engine,
) -> Result<fluxdown_protocol::Ed2kNodesSubRefreshResponse, ActorError> {
    let db_error =
        |error: fluxdown_engine::db::DbError| ActorError::Operation(format!("{error:#}"));
    let config_i64 = |value: Option<String>| value.and_then(|value| value.parse::<i64>().ok());
    let mut updated_at = config_i64(
        engine
            .db
            .get_config("ed2k_nodes_dat_updated_at")
            .await
            .map_err(db_error)?,
    )
    .unwrap_or(0);
    let mut node_count = config_i64(
        engine
            .db
            .get_config("ed2k_nodes_dat_count")
            .await
            .map_err(db_error)?,
    )
    .unwrap_or(0);
    // 全部源失败时保留旧缓存：过期的引导节点仍可能有存活的。
    if outcome.is_success() {
        updated_at = now_unix_secs();
        node_count = i64::try_from(outcome.node_count).unwrap_or(i64::MAX);
        let values = BTreeMap::from([
            (
                "ed2k_nodes_dat_cache".to_owned(),
                base64::engine::general_purpose::STANDARD.encode(&outcome.nodes_dat),
            ),
            ("ed2k_nodes_dat_count".to_owned(), node_count.to_string()),
            (
                "ed2k_nodes_dat_updated_at".to_owned(),
                updated_at.to_string(),
            ),
        ]);
        engine
            .db
            .set_config_batch_atomic(&values)
            .await
            .map_err(db_error)?;
    }
    Ok(fluxdown_protocol::Ed2kNodesSubRefreshResponse {
        success: outcome.is_success(),
        node_count,
        ok_sources: i64::try_from(outcome.ok_sources).unwrap_or(i64::MAX),
        total_sources: i64::try_from(outcome.total_sources).unwrap_or(i64::MAX),
        updated_at,
        error: outcome.error,
    })
}

#[cfg(feature = "ed2k")]
fn ed2k_network_to_dto(
    view: fluxdown_engine::ed2k::status::NetworkView,
) -> fluxdown_protocol::Ed2kNetworkDto {
    use fluxdown_engine::ed2k::status::{IdKind, UpnpState};
    use fluxdown_protocol::{Ed2kIdKind, Ed2kUpnpStatus};
    fluxdown_protocol::Ed2kNetworkDto {
        id: match view.client.id {
            IdKind::None => Ed2kIdKind::None,
            IdKind::Low => Ed2kIdKind::Low,
            IdKind::High => Ed2kIdKind::High,
        },
        client_id: view.client.client_id,
        server: view.client.server,
        listen_port: view.client.listen_port,
        upnp: match view.client.upnp {
            UpnpState::Disabled => Ed2kUpnpStatus::Disabled,
            UpnpState::Pending => Ed2kUpnpStatus::Pending,
            UpnpState::Mapped => Ed2kUpnpStatus::Mapped,
            UpnpState::Failed => Ed2kUpnpStatus::Failed,
        },
        external_ip: view.client.external_ip,
        upnp_error: view.client.upnp_error,
        upnp_enabled: view.upnp_enabled,
        kad_enabled: view.kad_enabled,
        kad_nodes: view.kad_nodes,
        server_count: view.server_count,
        error: view.error,
    }
}

/// 引擎的 eD2K 任务实况 → wire DTO（引擎不依赖 protocol，转换放在 daemon 边界）。
#[cfg(feature = "ed2k")]
fn ed2k_status_to_dto(
    view: fluxdown_engine::ed2k::status::Ed2kStatusView,
) -> fluxdown_protocol::Ed2kStatusDto {
    use fluxdown_engine::ed2k::status::{
        KadPhase, PeerOrigin, PeerState, ServerOrigin, ServerState, TaskPhase,
    };
    use fluxdown_protocol::{
        Ed2kKadDto, Ed2kKadPhase, Ed2kPeerCountsDto, Ed2kPeerDto, Ed2kPeerOrigin, Ed2kPeerState,
        Ed2kServerDto, Ed2kServerSource, Ed2kServerStatus, Ed2kTaskPhase,
    };
    let task = view.task;
    fluxdown_protocol::Ed2kStatusDto {
        phase: match task.phase {
            TaskPhase::Idle => Ed2kTaskPhase::Idle,
            TaskPhase::Searching => Ed2kTaskPhase::Searching,
            TaskPhase::Downloading => Ed2kTaskPhase::Downloading,
            TaskPhase::Waiting => Ed2kTaskPhase::Waiting,
            TaskPhase::Verifying => Ed2kTaskPhase::Verifying,
        },
        next_search_at: task.next_search_at,
        file_hash: view.file_hash,
        parts_total: view.parts_total,
        parts_done: task.parts_done,
        max_sources: task.max_sources,
        segments: view.segments,
        network: ed2k_network_to_dto(view.network),
        peer_counts: Ed2kPeerCountsDto {
            total: task.peer_counts.total,
            transferring: task.peer_counts.transferring,
            connected: task.peer_counts.connected,
            queued: task.peer_counts.queued,
            connecting: task.peer_counts.connecting,
            cooldown: task.peer_counts.cooldown,
            failed: task.peer_counts.failed,
            banned: task.peer_counts.banned,
        },
        peers: task
            .peers
            .into_iter()
            .map(|peer| Ed2kPeerDto {
                addr: peer.addr,
                low_id: peer.low_id,
                origin: match peer.origin {
                    PeerOrigin::Server => Ed2kPeerOrigin::Server,
                    PeerOrigin::Kad => Ed2kPeerOrigin::Kad,
                },
                state: match peer.state {
                    PeerState::Known => Ed2kPeerState::Known,
                    PeerState::Connecting => Ed2kPeerState::Connecting,
                    PeerState::Queued => Ed2kPeerState::Queued,
                    PeerState::Transferring => Ed2kPeerState::Transferring,
                    PeerState::Connected => Ed2kPeerState::Connected,
                    PeerState::Cooldown => Ed2kPeerState::Cooldown,
                    PeerState::Failed => Ed2kPeerState::Failed,
                    PeerState::Banned => Ed2kPeerState::Banned,
                },
                queue_rank: peer.queue_rank,
                client: peer.client,
                nickname: peer.nickname,
                downloaded: peer.downloaded,
                down_bps: peer.down_bps,
                parts: peer.parts,
                part: peer.part,
                message: peer.message,
                retry_at: peer.retry_at,
            })
            .collect(),
        servers: task
            .servers
            .into_iter()
            .map(|server| Ed2kServerDto {
                addr: server.addr,
                source: match server.origin {
                    ServerOrigin::Manual => Ed2kServerSource::Manual,
                    ServerOrigin::Subscription => Ed2kServerSource::Subscription,
                },
                status: match server.state {
                    ServerState::Querying => Ed2kServerStatus::Querying,
                    ServerState::Working => Ed2kServerStatus::Working,
                    ServerState::NotWorking => Ed2kServerStatus::NotWorking,
                },
                connected: server.connected,
                sources: server.sources,
                message: server.message,
                last_query_at: server.last_query_at,
            })
            .collect(),
        kad: Ed2kKadDto {
            phase: match task.kad.phase {
                KadPhase::Idle => Ed2kKadPhase::Idle,
                KadPhase::Bootstrapping => Ed2kKadPhase::Bootstrapping,
                KadPhase::FindingNodes => Ed2kKadPhase::FindingNodes,
                KadPhase::Searching => Ed2kKadPhase::Searching,
                KadPhase::Done => Ed2kKadPhase::Done,
            },
            bootstrap_nodes: task.kad.bootstrap_nodes,
            responded: task.kad.responded,
            discovered: task.kad.discovered,
            sources: task.kad.sources,
            last_search_at: task.kad.last_search_at,
            nodes: task
                .kad
                .nodes
                .into_iter()
                .map(ed2k_kad_node_to_dto)
                .collect(),
        },
    }
}

fn file_hash_error(error: fluxdown_engine::file_hash::FileHashError) -> ActorError {
    use fluxdown_engine::file_hash::FileHashError;
    match error {
        FileHashError::NotFound => ActorError::NotFound,
        FileHashError::NotEligible => ActorError::InvalidArgument(error.to_string()),
        FileHashError::Busy => ActorError::Conflict(error.to_string()),
        // 取消只来自删除 / 重新下载 / 关停：对调用方即「任务已变化」，不是计算失败。
        FileHashError::Stale | FileHashError::Cancelled => {
            ActorError::TaskStateConflict(error.to_string())
        }
        FileHashError::Failed(_) => ActorError::Operation(error.to_string()),
    }
}

/// 自动哈希没有调用方等待：任务在计算期间被删除 / 重新下载、产物是目录属正常，只记调试日志。
fn log_auto_file_hash_error(task_id: &str, error: &fluxdown_engine::file_hash::FileHashError) {
    use fluxdown_engine::file_hash::FileHashError;
    match error {
        FileHashError::Stale
        | FileHashError::Cancelled
        | FileHashError::NotFound
        | FileHashError::NotEligible => {
            tracing::debug!(task_id, %error, "auto file hash discarded");
        }
        FileHashError::Busy | FileHashError::Failed(_) => {
            tracing::warn!(task_id, %error, "auto file hash failed");
        }
    }
}

#[cfg(feature = "bt")]
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
        // 新列表只经 `set_bt_config` 热生效于此后加入的种子（对齐 qBittorrent
        // 「自动添加到新下载」），不重建 BT 会话、不打断做种与下载。
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

#[cfg(feature = "ed2k")]
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

/// 引擎 BT 实况 → wire DTO（引擎不依赖 protocol，转换放在 daemon 边界）。
#[cfg(feature = "bt")]
fn bt_status_to_dto(
    view: fluxdown_engine::bt_status::BtStatusView,
) -> fluxdown_protocol::BtStatusDto {
    use fluxdown_engine::bt_status::{TrackerSource, TrackerState, WebSeedState};
    use fluxdown_protocol::{BtTrackerDto, BtTrackerSource, BtTrackerStatus};
    fluxdown_protocol::BtStatusDto {
        dht_enabled: view.dht_enabled,
        peer_counts: fluxdown_protocol::BtPeerCountsDto {
            live: view.counts.live,
            connecting: view.counts.connecting,
            queued: view.counts.queued,
            seen: view.counts.seen,
            dead: view.counts.dead,
        },
        trackers: view
            .trackers
            .into_iter()
            .map(|row| BtTrackerDto {
                url: row.url,
                source: match row.source {
                    TrackerSource::Torrent => BtTrackerSource::Torrent,
                    TrackerSource::Session => BtTrackerSource::Session,
                },
                status: match row.state {
                    TrackerState::NotContacted => BtTrackerStatus::NotContacted,
                    TrackerState::Updating => BtTrackerStatus::Updating,
                    TrackerState::Working => BtTrackerStatus::Working,
                    TrackerState::NotWorking => BtTrackerStatus::NotWorking,
                },
                peers: row.peers,
                message: row.message,
                detail: row.detail,
                last_ok_at: row.last_ok_at,
                next_announce_at: row.next_announce_at,
                tier: row.tier,
                seeders: row.seeders,
                leechers: row.leechers,
                downloaded: row.downloaded,
            })
            .collect(),
        peers: view.peers.into_iter().map(peer_row_to_dto).collect(),
        web_seeds: view
            .web_seeds
            .into_iter()
            .map(|seed| fluxdown_protocol::BtWebSeedDto {
                url: seed.url,
                state: match seed.state {
                    WebSeedState::Idle => "idle",
                    WebSeedState::Downloading => "downloading",
                    WebSeedState::BackingOff => "backingOff",
                    WebSeedState::Disabled => "disabled",
                }
                .to_owned(),
                downloaded: seed.downloaded,
                message: seed.message,
            })
            .collect(),
        info_hash: view.info_hash,
        info_hash_v2: view.info_hash_v2,
    }
}

/// 引擎端口映射实况 → wire DTO。
#[cfg(feature = "bt")]
fn bt_port_status_to_dto(
    view: fluxdown_engine::bt_status::BtPortStatusView,
) -> fluxdown_protocol::BtPortStatusDto {
    use fluxdown_engine::bt_status::{PortMappingMethod, PortMappingState};
    fluxdown_protocol::BtPortStatusDto {
        running: view.running,
        listen_port: u32::from(view.listen_port),
        mappings: view
            .mappings
            .into_iter()
            .map(|row| {
                let (state, message) = match row.state {
                    PortMappingState::Pending => ("pending", String::new()),
                    PortMappingState::Mapped => ("mapped", String::new()),
                    PortMappingState::Failed(reason) => ("failed", reason),
                };
                fluxdown_protocol::BtPortMappingDto {
                    protocol: if row.udp { "udp" } else { "tcp" }.to_owned(),
                    internal_port: u32::from(row.internal_port),
                    external_port: row.external_port.map_or(0, u32::from),
                    method: match row.method {
                        Some(PortMappingMethod::Upnp) => "upnp",
                        Some(PortMappingMethod::NatPmp) => "natPmp",
                        Some(PortMappingMethod::Pcp) => "pcp",
                        None => "",
                    }
                    .to_owned(),
                    external_ip: row.external_ip.map(|ip| ip.to_string()).unwrap_or_default(),
                    state: state.to_owned(),
                    message,
                }
            })
            .collect(),
    }
}

/// 引擎 BT 任务级操作错误 → actor 领域错误。
#[cfg(feature = "bt")]
fn bt_task_op_error(error: fluxdown_engine::bt_task_files::BtTaskOpError) -> ActorError {
    use fluxdown_engine::bt_task_files::BtTaskOpError;
    match error {
        BtTaskOpError::NotFound => ActorError::NotFound,
        BtTaskOpError::InvalidArgument(message) => ActorError::InvalidArgument(message),
        BtTaskOpError::StateConflict(message) => ActorError::TaskStateConflict(message),
        BtTaskOpError::Operation(message) => ActorError::Operation(message),
        BtTaskOpError::Conflict(message) => ActorError::Conflict(message),
    }
}

/// 一个 peer 行 → wire DTO；防吸血研判（若该连接已被观察到）一并带出。
#[cfg(feature = "bt")]
fn peer_row_to_dto(peer: fluxdown_engine::bt_status::PeerRow) -> fluxdown_protocol::BtPeerDto {
    let mut dto = fluxdown_protocol::BtPeerDto {
        addr: peer.addr,
        client: peer.client,
        transport: peer.transport,
        downloaded: peer.downloaded,
        uploaded: peer.uploaded,
        down_bps: peer.down_bps,
        up_bps: peer.up_bps,
        country: peer.country,
        ..fluxdown_protocol::BtPeerDto::default()
    };
    if let Some(insight) = peer.insight {
        dto.ip_group = insight.ip_group;
        dto.group_delivered_bytes = insight.group_delivered_bytes;
        dto.suspicion = Some(insight.suspicion.to_owned());
        dto.rules = insight
            .rules
            .iter()
            .map(|rule| (*rule).to_owned())
            .collect();
        dto.action = insight.action.to_owned();
        dto.observe_only = insight.observe_only;
    }
    dto
}

#[cfg(feature = "bt")]
fn task_verify_to_dto(
    report: fluxdown_engine::bt_verify::BtIntegrityReport,
) -> fluxdown_protocol::TaskVerifyResult {
    use fluxdown_engine::bt_verify::BtIntegrityStatus;
    fluxdown_protocol::TaskVerifyResult {
        status: match report.status {
            BtIntegrityStatus::Intact => fluxdown_protocol::TaskVerifyStatus::Intact,
            BtIntegrityStatus::Damaged => fluxdown_protocol::TaskVerifyStatus::Damaged,
            BtIntegrityStatus::Unverifiable => fluxdown_protocol::TaskVerifyStatus::Unverifiable,
        },
        total_pieces: report.total_pieces,
        verified_pieces: report.verified_pieces,
        bad_pieces: report.bad_pieces,
        skipped_pieces: report.skipped_pieces,
        damaged_files: report.damaged_files,
    }
}

#[cfg(feature = "bt")]
fn integrity_error(error: fluxdown_engine::bt_verify::BtIntegrityError) -> ActorError {
    use fluxdown_engine::bt_verify::BtIntegrityError;
    match error {
        BtIntegrityError::NotFound => ActorError::NotFound,
        BtIntegrityError::NotEligible => ActorError::InvalidArgument(error.to_string()),
        BtIntegrityError::Busy => ActorError::Conflict(error.to_string()),
        BtIntegrityError::Cancelled | BtIntegrityError::Internal(_) => {
            ActorError::Operation(error.to_string())
        }
    }
}

/// 引擎探测结果 → wire DTO（引擎不依赖 protocol，转换放在 daemon 边界）。
fn link_probe_to_dto(
    probe: fluxdown_engine::meta_prober::LinkProbe,
) -> fluxdown_protocol::LinkProbeResult {
    use fluxdown_engine::meta_prober::LinkVerdict;
    use fluxdown_protocol::LinkProbeVerdict;
    fluxdown_protocol::LinkProbeResult {
        verdict: match probe.verdict {
            LinkVerdict::Resource => LinkProbeVerdict::Resource,
            LinkVerdict::NotResource => LinkProbeVerdict::NotResource,
            LinkVerdict::Unknown => LinkProbeVerdict::Unknown,
        },
        final_url: probe.final_url,
        file_name: probe.file_name,
        mime: probe.mime,
        total_bytes: probe.total_bytes,
    }
}

#[cfg(any(feature = "bt", feature = "ed2k"))]
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64)
}

async fn cdn_reports_peek(engine: &mut Engine) -> Result<ActorResult, ActorError> {
    if let Some(raw) = engine
        .db
        .peek_cdn_report_lease()
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
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
    // 只写与当前值不同的键：未变化时不产生任何 DB 写（避免空闲周期 fsync 唤醒硬盘）。
    let current = engine
        .db
        .get_all_config()
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    let changed: BTreeMap<String, String> = values
        .into_iter()
        .filter(|(key, value)| current.get(key) != Some(value))
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    let values = changed;
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
    if let Some(raw) = values.get(fluxdown_engine::proxy_rules::PROXY_RULES_KEY)
        && let Some(first) = fluxdown_engine::proxy_rules::validate(raw).first()
    {
        return Err(ActorError::InvalidArgument(first.message()));
    }
    if values
        .keys()
        .any(|key| key.starts_with("proxy_") || key == "global_user_agent")
    {
        // 与引擎实际应用时一致地构建 client：非法代理或 UA（HeaderValue 规则）
        // 必须在提交前被拒绝，否则毒值落库后会导致下次启动失败。
        let proxy = fluxdown_engine::proxy_config::ProxyConfig::from_config_map(&merged);
        let user_agent = merged
            .get("global_user_agent")
            .map(String::as_str)
            .unwrap_or_default();
        fluxdown_engine::downloader::build_client(&proxy, user_agent)
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
    // DB 与 revision 已提交：即便在线应用失败，也必须广播已提交快照，
    // 否则事件快照与 DB revision 分叉，后续 patch 会一直冲突。
    let applied = apply_live_config(engine, &merged, values.keys()).await;
    let snapshot = fluxdown_protocol::DaemonConfigSnapshot {
        revision,
        values: crate::config::public_config_values(&merged),
    };
    events.publish(fluxdown_protocol::DaemonEvent::ConfigChanged(
        snapshot.clone(),
    ));
    applied?;
    Ok(snapshot)
}

/// 清空引擎学习的域名连接策略（内存 + 持久化），并以刷新后的公开配置广播
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
    publish_public_config(&all, events);
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

/// 站点键归一化：完整 URL 直接取 `site_key`，裸 `host` / `host:port` 补 `https://` 再取；
/// 非 http(s) 或无法解析返回 `None`（旧 headless 宿主 `normalize_site` 的收紧版：带 scheme 的
/// 输入不再套 `https://` 二次解析，避免 `ftp://host` 被误当成站点 `ftp`）。
pub fn normalize_site(input: &str) -> Option<String> {
    let input = input.trim();
    let direct = fluxdown_engine::site_auth::site_key(input);
    if input.contains("://") {
        return direct;
    }
    direct.or_else(|| fluxdown_engine::site_auth::site_key(&format!("https://{input}")))
}

/// 新增或覆盖单站点凭据；`site` 必须已归一化，`user` 必须已去空白且非空。
async fn upsert_site_auth(
    db: &fluxdown_engine::db::Db,
    site: String,
    user: String,
    pass: String,
) -> Result<fluxdown_protocol::SiteAuthEntryDto, ActorError> {
    let json = db
        .get_config(fluxdown_engine::site_auth::SITE_AUTH_CONFIG_KEY)
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .unwrap_or_default();
    let mut store = fluxdown_engine::site_auth::parse_store(&json);
    store.insert(
        site.clone(),
        fluxdown_engine::site_auth::SiteCredential {
            user: user.clone(),
            pass,
        },
    );
    save_site_auth(db, &store).await?;
    Ok(fluxdown_protocol::SiteAuthEntryDto { site, user })
}

/// 请求未指定队列时套用设置项 `default_queue_id`；默认队列为空或已被删除时保持空串，由引擎归入
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

/// 请求未指定保存目录时依次套用设置项 `default_save_dir` 与系统下载目录。
async fn resolve_save_dir(
    db: &fluxdown_engine::db::Db,
    requested: String,
) -> Result<String, ActorError> {
    if !requested.trim().is_empty() {
        return Ok(requested);
    }
    let configured = db
        .get_config("default_save_dir")
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .unwrap_or_default();
    if configured.trim().is_empty() {
        Ok(fluxdown_engine::user_dirs::download_dir_or_cwd())
    } else {
        Ok(configured)
    }
}

/// 引擎单项建任务结果 → wire 结果。逐项拒绝都是参数问题（`InvalidArgument`），
/// `field` 指向出问题的条目字段；`RpcErrorData` 不带文案，细节只进日志。
fn create_task_outcome(outcome: BatchItemOutcome) -> CreateTaskOutcome {
    match outcome {
        BatchItemOutcome::Created(task_id) => CreateTaskOutcome {
            task_id: Some(task_id),
            error: None,
        },
        BatchItemOutcome::Rejected(rejection) => {
            let field = match rejection {
                CreateRejection::BtDisabled
                | CreateRejection::UnsupportedProtocol
                | CreateRejection::InvalidThunderLink(_) => "url",
                CreateRejection::InvalidChecksum(_) => "checksum",
                CreateRejection::InvalidHeaders(_) => "headers",
            };
            CreateTaskOutcome {
                task_id: None,
                error: Some(RpcErrorData {
                    code: ApplicationErrorCode::InvalidArgument,
                    retryable: false,
                    field: Some(field.to_owned()),
                    revision: None,
                    reason: None,
                }),
            }
        }
    }
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
    if keys.contains(&"multi_nic_enabled") {
        engine.manager.set_multi_nic_enabled(
            all.get("multi_nic_enabled")
                .is_some_and(|value| value == "true"),
        );
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
    if keys.contains(&"idle_file_scan") {
        engine.manager.set_idle_file_scan(
            all.get("idle_file_scan")
                .is_some_and(|value| value == "true" || value == "1"),
        );
    }
    if keys.contains(&"hash_after_download") || keys.contains(&"hash_algorithm") {
        engine
            .manager
            .set_hash_after_download(crate::runtime::hash_after_download(all));
    }
    if keys.contains(&"completed_record_retention_days") {
        engine.manager.set_completed_record_retention_days(
            all.get("completed_record_retention_days")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0),
        );
        // 设置变更立即生效（跳过去抖）；清理只删记录，失败只记日志。
        engine.manager.purge_expired_completed_records(true).await;
    }
    if keys.contains(&"global_user_agent")
        && let Some(value) = all.get("global_user_agent")
    {
        engine
            .manager
            .set_user_agent(value.clone())
            .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    }
    let rules_key = fluxdown_engine::proxy_rules::PROXY_RULES_KEY;
    if keys
        .iter()
        .any(|key| key.starts_with("proxy_") && *key != rules_key)
    {
        engine
            .manager
            .set_proxy_config(fluxdown_engine::proxy_config::ProxyConfig::from_config_map(
                all,
            ))
            .await
            .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    } else if keys.contains(&rules_key) {
        engine.manager.set_proxy_rules(
            fluxdown_engine::proxy_config::ProxyConfig::from_config_map(all).rules,
        );
    }
    let bt_session_rebuild = crate::config::bt_session_rebuild_needed(&keys, all);
    if bt_session_rebuild || keys.iter().any(|key| key.starts_with("bt_")) {
        engine
            .manager
            .set_bt_config(crate::config::bt_config_from_map(all));
        // 只有会话级设置需要重建会话；tracker 列表（自定义 / 订阅开关与地址）与连接数上限
        // 已由上面的 `set_bt_config` 热生效。
        if bt_session_rebuild {
            engine.manager.invalidate_bt_session().await;
        }
    }
    // eD2K 设置热生效：UPnP 开关立即建立 / 撤销映射；服务器列表、Kad 开关与引导节点
    // 订阅由各任务在下一轮找源前重读。监听端口只在监听器首次绑定时读取。
    #[cfg(feature = "ed2k")]
    if keys.iter().any(|key| key.starts_with("ed2k_")) {
        fluxdown_engine::ed2k::apply_live_settings(&engine.db)
            .await
            .map_err(|error| ActorError::Operation(format!("{error:#}")))?;
    }
    // 防吸血配置全部热生效：不在监听类键之列，不重建 BT 会话。
    #[cfg(feature = "bt")]
    if keys.iter().any(|key| key.starts_with("bt_guard_")) {
        engine
            .manager
            .set_bt_guard_config(crate::config::bt_guard_config_from_map(all));
    }
    if keys.contains(&"webhook.endpoints")
        && let Some(value) = all.get("webhook.endpoints")
    {
        engine.manager.set_webhook_endpoints(value);
    }
    if keys.contains(&"log_max_size_mb") {
        apply_log_limit(all);
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

/// 把持久化的日志大小上限（MB，缺省 / 非法 = 目录默认值）应用到引擎 logger。
fn apply_log_limit(config: &HashMap<String, String>) {
    fluxdown_engine::logger::set_max_total_bytes(log_limit_bytes(config));
}

fn log_limit_bytes(config: &HashMap<String, String>) -> u64 {
    const KEY: &str = "log_max_size_mb";
    let default_mb = fluxdown_protocol::daemon_config_default(KEY)
        .parse::<u64>()
        .unwrap_or(10);
    let mb = config
        .get(KEY)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|mb| (1..=1024).contains(mb))
        .unwrap_or(default_mb);
    mb * 1024 * 1024
}

/// 把引擎持久化的投递历史并入 daemon 投影（合并语义，不覆盖启动后已到达的增量）。
fn publish_webhook_history(engine: &Engine, events: &crate::event_hub::DaemonEventHub) {
    let history: Vec<_> = engine
        .webhook_deliveries()
        .into_iter()
        .map(fluxdown_engine_protocol::webhook_delivery_to_dto)
        .collect();
    if !history.is_empty() {
        events.publish(fluxdown_protocol::DaemonEvent::WebhooksChanged(history));
    }
}

/// 批量删除结果 → 操作结果：预读 / 提交失败，或有任务因文件清理失败而保留（其余已删除）
/// 时报错，调用方据此向用户提示。
fn batch_delete_result(
    result: Result<
        fluxdown_engine::download_manager::BatchDeleteOutcome,
        fluxdown_engine::db::DbError,
    >,
) -> Result<(), ActorError> {
    match result {
        Ok(outcome) if outcome.failed == 0 => Ok(()),
        Ok(outcome) => Err(ActorError::TaskFilesKept {
            in_use: outcome.files_in_use,
            message: format!(
                "{} task(s) kept because their files could not be removed ({} deleted)",
                outcome.failed, outcome.deleted
            ),
        }),
        Err(error) => Err(ActorError::Operation(format!("{error:#}"))),
    }
}

/// 单任务删除被拒 → 操作结果：文件清理失败回报「任务已保留」及是否被占用，其余为操作失败。
fn delete_task_error(error: fluxdown_engine::download_manager::DeleteTaskError) -> ActorError {
    use fluxdown_engine::download_manager::DeleteTaskError;
    match &error {
        DeleteTaskError::Cleanup(_) => ActorError::TaskFilesKept {
            in_use: error.is_file_in_use(),
            message: format!("task kept: {error}"),
        },
        DeleteTaskError::Record(_) => ActorError::Operation(error.to_string()),
    }
}

/// 设优先下载被拒 → 操作结果：已完成 / 失败任务回报状态冲突，客户端据此提示。
fn boost_error(error: BoostError) -> ActorError {
    match error {
        BoostError::NotFound => ActorError::NotFound,
        BoostError::Completed | BoostError::Ended => {
            ActorError::TaskStateConflict(error.to_string())
        }
        BoostError::Db(error) => ActorError::Operation(format!("{error:#}")),
    }
}

/// 去重并剔除不存在的任务 id（批量接口「未知 id 忽略」）。
async fn existing_task_ids(
    db: &fluxdown_engine::db::Db,
    mut task_ids: Vec<String>,
) -> Result<Vec<String>, ActorError> {
    let mut seen = std::collections::HashSet::new();
    task_ids.retain(|id| seen.insert(id.clone()));
    if task_ids.is_empty() {
        return Ok(task_ids);
    }
    let known: std::collections::HashSet<String> = db
        .load_tasks_by_ids(&task_ids)
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .into_iter()
        .map(|task| task.task_id)
        .collect();
    task_ids.retain(|id| known.contains(id));
    Ok(task_ids)
}

/// 「全部暂停」的作用对象（对齐 qBittorrent Stop all）：下载中 / 排队 / 准备中，
/// 以及做种中 / 排队做种的已完成 BT 任务（暂停 = 停止做种）。等待自动重试的失败任务
/// 由调用方经 `retry_pending_task_ids` 追加（那是引擎内存态，库里看不出）。
async fn pause_all_task_ids(db: &fluxdown_engine::db::Db) -> Result<Vec<String>, ActorError> {
    use fluxdown_engine::bt_seeding::{SEEDING_STATUS_ACTIVE, SEEDING_STATUS_QUEUED};
    Ok(db
        .load_all_tasks()
        .await
        .map_err(|error| ActorError::Operation(format!("{error:#}")))?
        .into_iter()
        .filter(|task| {
            matches!(task.status, 0 | 1 | 5)
                || (task.status == 3
                    && matches!(
                        task.seeding_status,
                        SEEDING_STATUS_ACTIVE | SEEDING_STATUS_QUEUED
                    ))
        })
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

    use super::{
        ActorError, ActorOperation, BT_DISABLED_MESSAGE, delete_site_auth, execute_operation,
        pause_all_task_ids, resolve_queue_id, save_site_auth,
    };

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
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor test directory removal failed: {error}");
        }
    }

    #[tokio::test]
    async fn pause_all_targets_active_downloads_and_live_seeders_only() {
        let (db, dir) = open_db().await;
        // (id, status, seeding_status)
        let rows = [
            ("pending", 0, 0),
            ("downloading", 1, 0),
            ("paused", 2, 0),
            ("completed", 3, 0),
            ("failed", 4, 0),
            ("preparing", 5, 0),
            ("seeding", 3, 1),
            ("seed-queued", 3, 8),
            ("seed-user-stopped", 3, 4),
            ("seed-ratio-reached", 3, 2),
        ];
        for (id, status, seeding) in rows {
            db.insert_task(
                id,
                "magnet:?xt=urn:btih:x",
                id,
                "/tmp",
                1,
                0,
                "",
                "",
                "",
                status,
            )
            .await
            .expect("insert task");
            if seeding != 0 {
                db.update_task_seeding_status(id, seeding, "")
                    .await
                    .expect("set seeding status");
            }
        }
        let mut ids = pause_all_task_ids(&db).await.expect("pause-all ids");
        ids.sort();
        assert_eq!(
            ids,
            [
                "downloading",
                "pending",
                "preparing",
                "seed-queued",
                "seeding"
            ]
        );

        drop(db);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor test directory removal failed: {error}");
        }
    }

    #[tokio::test]
    async fn site_auth_delete_and_clear_never_expose_passwords() {
        let (db, dir) = open_db().await;
        let store = BTreeMap::from([
            (
                "https://a.example".to_owned(),
                SiteCredential {
                    user: "alice".to_owned(),
                    pass: "s3cret".to_owned(),
                },
            ),
            (
                "https://b.example:8443".to_owned(),
                SiteCredential {
                    user: "bob".to_owned(),
                    pass: "hunter2".to_owned(),
                },
            ),
        ]);
        db.set_config(SITE_AUTH_CONFIG_KEY, &serialize_store(&store))
            .await
            .expect("seed credentials");

        let after_delete = delete_site_auth(&db, "https://a.example")
            .await
            .expect("delete known site");
        assert_eq!(after_delete.len(), 1);
        assert_eq!(after_delete[0].site, "https://b.example:8443");
        assert_eq!(after_delete[0].user, "bob");
        let json = serde_json::to_string(&after_delete).expect("serialize dto");
        assert!(!json.contains("hunter2"), "password must not leak: {json}");

        assert!(matches!(
            delete_site_auth(&db, "https://a.example").await,
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
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor test directory removal failed: {error}");
        }
    }

    #[test]
    fn normalize_site_accepts_bare_hosts_ports_and_urls_but_rejects_other_schemes() {
        assert_eq!(
            super::normalize_site("Example.COM").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            super::normalize_site(" example.com:8443 ").as_deref(),
            Some("https://example.com:8443")
        );
        assert_eq!(
            super::normalize_site("https://example.com:443/a.bin").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            super::normalize_site("http://nas.local:8080/a.bin").as_deref(),
            Some("http://nas.local:8080")
        );
        assert_eq!(super::normalize_site("ftp://example.com/a.bin"), None);
        assert_eq!(super::normalize_site(""), None);
    }

    #[tokio::test]
    async fn upsert_site_auth_overwrites_same_site_and_keeps_other_entries() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_site_auth_upsert_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir).await.expect("create dir");
        let db = fluxdown_engine::db::Db::open(&dir).await.expect("open db");

        let first =
            super::upsert_site_auth(&db, "https://a.example".into(), "alice".into(), "p1".into())
                .await
                .expect("insert");
        assert_eq!(
            (first.site.as_str(), first.user.as_str()),
            ("https://a.example", "alice")
        );
        super::upsert_site_auth(&db, "https://b.example".into(), "bob".into(), "p2".into())
            .await
            .expect("insert second");
        super::upsert_site_auth(
            &db,
            "https://a.example".into(),
            "alice2".into(),
            "p3".into(),
        )
        .await
        .expect("overwrite");

        let json = db
            .get_config(SITE_AUTH_CONFIG_KEY)
            .await
            .expect("read store")
            .unwrap_or_default();
        let store = fluxdown_engine::site_auth::parse_store(&json);
        assert_eq!(store.len(), 2);
        assert_eq!(store["https://a.example"].user, "alice2");
        assert_eq!(store["https://a.example"].pass, "p3");
        assert_eq!(store["https://b.example"].pass, "p2");

        drop(db);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("site auth upsert test directory removal failed: {error}");
        }
    }

    #[tokio::test]
    async fn batch_ids_are_deduplicated_and_unknown_ids_dropped() {
        let (db, dir) = open_db().await;
        db.init_default_config("/tmp").await.expect("seed config");
        db.seed_builtin_queues().await.expect("seed queues");
        for id in ["t1", "t2"] {
            db.insert_task(id, "https://example.com/a", id, "/tmp", 1, 0, "", "", "", 2)
                .await
                .expect("insert task");
        }

        let ids =
            super::existing_task_ids(&db, ["t2", "ghost", "t1", "t2"].map(str::to_owned).to_vec())
                .await
                .expect("filter ids");
        assert_eq!(ids, ["t2", "t1"]);
        assert!(
            super::existing_task_ids(&db, Vec::new())
                .await
                .expect("empty list")
                .is_empty()
        );

        drop(db);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor batch ids test directory removal failed: {error}");
        }
    }

    /// BT 被禁用时：新建 BT 任务与（整批）恢复 BT 任务必须带着明确原因失败，
    /// 而不是「failed to persist task」或无信息的成功。
    #[tokio::test]
    async fn bt_disabled_create_and_resume_report_the_reason() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_bt_disabled_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create bt disabled test dir");
        let dir_text = dir.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig {
                    enabled: false,
                    ..Default::default()
                },
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        let magnet = "magnet:?xt=urn:btih:da39a3ee5e6b4b0d3255bfef95601890afd80709&dn=t";
        let create = |url: &str, torrent: Vec<u8>| ActorOperation::CreateTask {
            request: Box::new(
                serde_json::from_value(serde_json::json!({ "url": url })).expect("create request"),
            ),
            torrent_file_bytes: torrent,
            hint_file_size: 0,
            name_hint: String::new(),
            unattended: false,
            category_route: None,
        };
        let reason = |result: Result<super::ActorResult, ActorError>| match result {
            Err(ActorError::InvalidArgument(message)) => message,
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("operation must be rejected"),
        };

        // 新建：magnet 与种子字节都带明确原因，且不落库。
        assert_eq!(
            reason(execute_operation(create(magnet, Vec::new()), &mut engine, &events).await),
            BT_DISABLED_MESSAGE
        );
        assert_eq!(
            reason(execute_operation(create("", vec![1, 2, 3]), &mut engine, &events).await),
            BT_DISABLED_MESSAGE
        );
        assert!(
            engine
                .db
                .load_all_tasks()
                .await
                .expect("load tasks")
                .is_empty(),
            "rejected BT creation must not persist a task"
        );

        // 恢复：单任务与「整批都是 BT」的批量均带原因失败，任务保持暂停。
        engine
            .db
            .insert_task("bt-1", magnet, "t", &dir_text, 1, 0, "", "", "", 0)
            .await
            .expect("insert BT task");
        engine
            .db
            .update_task_status("bt-1", 2, "")
            .await
            .expect("pause BT task");
        assert_eq!(
            reason(
                execute_operation(
                    ActorOperation::ResumeTask {
                        task_id: "bt-1".to_owned()
                    },
                    &mut engine,
                    &events
                )
                .await
            ),
            BT_DISABLED_MESSAGE
        );
        assert_eq!(
            reason(
                execute_operation(
                    ActorOperation::ResumeTasks {
                        task_ids: vec!["bt-1".to_owned()]
                    },
                    &mut engine,
                    &events
                )
                .await
            ),
            BT_DISABLED_MESSAGE
        );
        let task = engine
            .db
            .load_task_by_id("bt-1")
            .await
            .expect("load task")
            .expect("task exists");
        assert_eq!(task.status, 2, "rejected resume leaves the task paused");

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor bt disabled test directory removal failed: {error}");
        }
    }

    /// 防吸血的 19 个设置键全部热生效：patch 之后 guard 配置立即变化，只读节奏（BT 会话）不被触碰；
    /// 其余 `bt_*` 键的 patch 不影响 guard 配置。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn bt_guard_settings_apply_live_through_a_config_patch() {
        use fluxdown_engine::bt_guard::{DownloadingMaxAction, RuleMode};
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_guard_patch_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create guard patch test dir");
        let dir_text = dir.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig::default(),
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        assert_eq!(
            *engine.manager.bt_guard().config(),
            fluxdown_engine::bt_guard::BtGuardConfig::default()
        );

        let patch = |pairs: &[(&str, &str)]| ActorOperation::PatchConfig {
            expected_revision: 0,
            values: pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        };
        let revision_of = |result: Result<super::ActorResult, ActorError>| match result {
            Ok(super::ActorResult::Config(snapshot)) => snapshot.revision,
            Ok(_) => panic!("unexpected actor result"),
            Err(error) => panic!("patch failed: {error}"),
        };
        let first = revision_of(
            execute_operation(
                patch(&[
                    ("bt_guard_enabled", "false"),
                    ("bt_guard_rule_excessive", "enforce"),
                    ("bt_guard_rule_rerequest", "off"),
                    ("bt_guard_rule_rewind", "enforce"),
                    ("bt_guard_rule_lag", "off"),
                    ("bt_guard_rule_fingerprint", "enforce"),
                    ("bt_guard_cloud_mode", "enforce"),
                    ("bt_guard_excess_ratio_pct", "350"),
                    ("bt_guard_min_torrent_mib", "0"),
                    ("bt_guard_rewind_tolerance_pct", "15"),
                    ("bt_guard_throttle_kib", "128"),
                    ("bt_guard_ban_base_hours", "2"),
                    ("bt_guard_ban_max_days", "9"),
                    ("bt_guard_downloading_max_action", "ban"),
                    ("bt_guard_skip_private", "false"),
                    ("bt_guard_skip_lan", "false"),
                    ("bt_guard_ipv6_prefix", "48"),
                    ("bt_guard_filter_urls", "https://a.test/list.txt"),
                    ("bt_guard_cloud_enabled", "false"),
                    ("bt_guard_report_enabled", "true"),
                ]),
                &mut engine,
                &events,
            )
            .await,
        );
        let config = engine.manager.bt_guard().config();
        assert!(!config.enabled);
        assert_eq!(config.rule_excessive, RuleMode::Enforce);
        assert_eq!(config.rule_rerequest, RuleMode::Off);
        assert_eq!(config.rule_rewind, RuleMode::Enforce);
        assert_eq!(config.rule_lag, RuleMode::Off);
        assert_eq!(config.rule_fingerprint, RuleMode::Enforce);
        assert_eq!(config.cloud_mode, RuleMode::Enforce);
        assert_eq!(config.excess_ratio_pct, 350);
        assert_eq!(config.min_torrent_mib, 0);
        assert_eq!(config.rewind_tolerance_pct, 15);
        assert_eq!(config.throttle_kib, 128);
        assert_eq!(config.ban_base_hours, 2);
        assert_eq!(config.ban_max_days, 9);
        assert_eq!(config.downloading_max_action, DownloadingMaxAction::Ban);
        assert!(!config.skip_private && !config.skip_lan && !config.cloud_enabled);
        assert_eq!(config.ipv6_prefix, 48);
        assert_eq!(config.filter_urls, "https://a.test/list.txt");
        assert!(config.report_enabled);
        assert!(
            !engine.manager.bt_guard().session_active(),
            "patching guard settings never creates or rebuilds a BT session"
        );

        // 其余 bt_* 键的 patch 不改 guard 配置。
        let mut op = patch(&[("bt_seed_max_active", "3")]);
        if let ActorOperation::PatchConfig {
            expected_revision, ..
        } = &mut op
        {
            *expected_revision = first;
        }
        revision_of(execute_operation(op, &mut engine, &events).await);
        assert_eq!(*engine.manager.bt_guard().config(), *config);

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor guard patch test directory removal failed: {error}");
        }
    }

    /// peer 行 → DTO：有防吸血研判时带出全部新字段，没有时保持旧字段与默认值。
    #[cfg(feature = "bt")]
    #[test]
    fn peer_rows_carry_the_guard_insight_into_the_dto() {
        use fluxdown_engine::bt_guard::PeerInsight;
        let row = |insight| fluxdown_engine::bt_status::PeerRow {
            addr: "203.0.113.9:6881".to_owned(),
            client: "Xunlei".to_owned(),
            transport: "tcp".to_owned(),
            downloaded: 1,
            uploaded: 2,
            down_bps: 3,
            up_bps: 4,
            country: Some("CN".to_owned()),
            insight,
        };
        let plain = super::peer_row_to_dto(row(None));
        assert_eq!(plain.addr, "203.0.113.9:6881");
        assert_eq!((plain.downloaded, plain.uploaded), (1, 2));
        assert_eq!(plain.country.as_deref(), Some("CN"));
        assert_eq!(plain.action, "allow");
        assert!(plain.ip_group.is_empty() && plain.suspicion.is_none() && plain.rules.is_empty());
        assert!(!plain.observe_only);

        let flagged = super::peer_row_to_dto(row(Some(PeerInsight {
            ip_group: "203.0.113.9".to_owned(),
            group_delivered_bytes: 4096,
            suspicion: "strong",
            rules: vec!["excessive", "rerequest"],
            action: "banned",
            observe_only: true,
        })));
        assert_eq!(flagged.ip_group, "203.0.113.9");
        assert_eq!(flagged.group_delivered_bytes, 4096);
        assert_eq!(flagged.suspicion.as_deref(), Some("strong"));
        assert_eq!(flagged.rules, vec!["excessive", "rerequest"]);
        assert_eq!(flagged.action, "banned");
        assert!(flagged.observe_only);
    }

    /// `daemon.task.setSegments`：暂停任务改线程数后落库且状态不变；不存在 / 已完成任务
    /// 映射为 `NotFound`，不改库。
    #[tokio::test]
    async fn set_task_segments_updates_paused_task_and_rejects_missing_or_completed() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_set_segments_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create set segments test dir");
        let dir_text = dir.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig::default(),
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        let set = |task_id: &str, segments: i32| ActorOperation::SetTaskSegments {
            task_id: task_id.to_owned(),
            segments,
        };
        for (id, status) in [("paused", 2), ("done", 3)] {
            engine
                .db
                .insert_task(
                    id,
                    "https://example.invalid/a.bin",
                    "a.bin",
                    &dir_text,
                    8,
                    0,
                    "",
                    "",
                    "",
                    status,
                )
                .await
                .expect("insert task");
        }

        execute_operation(set("paused", 1), &mut engine, &events)
            .await
            .expect("paused task accepts new segments");
        let paused = engine
            .db
            .load_task_by_id("paused")
            .await
            .expect("load paused")
            .expect("paused exists");
        assert_eq!((paused.segments, paused.status), (1, 2));

        for id in ["missing", "done"] {
            assert!(matches!(
                execute_operation(set(id, 4), &mut engine, &events).await,
                Err(ActorError::NotFound)
            ));
        }
        let done = engine
            .db
            .load_task_by_id("done")
            .await
            .expect("load done")
            .expect("done exists");
        assert_eq!(done.segments, 8, "completed task keeps its segments");

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor set segments test directory removal failed: {error}");
        }
    }

    /// `daemon.task.ed2kStatus` 对未在运行的任务给出块进度 / 线程数设置 / 网络设置；改线程数
    /// 不动任务状态；`daemon.ed2k.nodesSubscription.refresh` 的提交只在成功时替换缓存。
    #[cfg(feature = "ed2k")]
    #[tokio::test]
    async fn ed2k_status_reads_idle_task_and_nodes_commit_keeps_cache_on_failure() {
        const LINK: &str = "ed2k://|file|a.bin|20000000|0123456789ABCDEF0123456789ABCDEF|/";
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_ed2k_status_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create ed2k status test dir");
        let dir_text = dir.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig::default(),
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        for (id, url) in [("ed2k", LINK), ("http", "https://example.invalid/a.bin")] {
            engine
                .db
                .insert_task(id, url, "a.bin", &dir_text, 0, 0, "", "", "", 2)
                .await
                .expect("insert task");
        }
        let status = |task_id: &str| ActorOperation::Ed2kStatus {
            task_id: task_id.to_owned(),
        };

        engine
            .db
            .set_config_batch_atomic(&BTreeMap::from([
                (
                    "ed2k_server_list".to_owned(),
                    "1.1.1.1:4661,2.2.2.2:4661".to_owned(),
                ),
                (
                    "ed2k_server_sub_cache".to_owned(),
                    "2.2.2.2:4661,3.3.3.3:4661".to_owned(),
                ),
            ]))
            .await
            .expect("seed servers");
        let Ok(super::ActorResult::Ed2kStatus(idle)) =
            execute_operation(status("ed2k"), &mut engine, &events).await
        else {
            panic!("ed2k status of a paused task");
        };
        assert_eq!(idle.phase, fluxdown_protocol::Ed2kTaskPhase::Idle);
        assert_eq!(idle.file_hash, "0123456789abcdef0123456789abcdef");
        // 20 MB / 9.28 MB 每块 = 3 块，尚无已校验块。
        assert_eq!((idle.parts_total, idle.parts_done), (3, 0));
        assert_eq!((idle.segments, idle.max_sources), (0, 4));
        assert!(idle.peers.is_empty() && idle.servers.is_empty());
        assert_eq!(idle.network.id, fluxdown_protocol::Ed2kIdKind::None);
        assert!(idle.network.kad_enabled && idle.network.upnp_enabled);
        // 手填与订阅列表合并去重。
        assert_eq!(idle.network.server_count, 3);
        assert_eq!(idle.network.kad_nodes, 0);

        for id in ["http", "missing"] {
            assert!(matches!(
                execute_operation(status(id), &mut engine, &events).await,
                Err(ActorError::InvalidArgument(_))
            ));
        }

        execute_operation(
            ActorOperation::SetTaskSegments {
                task_id: "ed2k".to_owned(),
                segments: 6,
            },
            &mut engine,
            &events,
        )
        .await
        .expect("segments accepted");
        let Ok(super::ActorResult::Ed2kStatus(resized)) =
            execute_operation(status("ed2k"), &mut engine, &events).await
        else {
            panic!("ed2k status after resizing");
        };
        assert_eq!((resized.segments, resized.max_sources), (6, 6));
        let task = engine
            .db
            .load_task_by_id("ed2k")
            .await
            .expect("load task")
            .expect("task exists");
        assert_eq!(task.status, 2, "resizing does not resume a paused task");

        let fetched = |ok_sources: usize, nodes_dat: Vec<u8>, node_count: usize| {
            fluxdown_engine::ed2k::kad::NodesFetchOutcome {
                nodes_dat,
                node_count,
                ok_sources,
                total_sources: 2,
                error: if ok_sources == 0 {
                    "a: timeout; b: http status 404".to_owned()
                } else {
                    String::new()
                },
            }
        };
        // 经 actor 的提交路径：落库后必须把新的只读键广播给已连接的客户端。
        let (ack, committed) = tokio::sync::oneshot::channel();
        super::commit_maintenance(
            super::MaintenanceEvent::Ed2kNodes {
                outcome: fetched(1, vec![1, 2, 3], 42),
                ack,
            },
            &mut engine,
            &events,
            &tokio::sync::mpsc::unbounded_channel().0,
        )
        .await;
        let Ok(Ok(super::ActorResult::Ed2kNodesRefresh(committed))) = committed.await else {
            panic!("nodes commit acknowledged");
        };
        assert_eq!(
            events.inspect(|snapshot| snapshot.config.values.get("ed2k_nodes_dat_count").cloned()),
            Some("42".to_owned()),
            "clients learn the new count through ConfigChanged"
        );
        assert!(committed.success);
        assert_eq!((committed.node_count, committed.ok_sources), (42, 1));
        assert!(committed.updated_at > 0);

        let failed = super::commit_ed2k_nodes(fetched(0, Vec::new(), 0), &mut engine)
            .await
            .expect("commit failed fetch");
        assert!(!failed.success);
        assert_eq!(failed.error, "a: timeout; b: http status 404");
        // 失败不清空缓存：条数与更新时间沿用上次成功的值。
        assert_eq!(
            (failed.node_count, failed.updated_at),
            (42, committed.updated_at)
        );
        let cache = engine
            .db
            .get_config("ed2k_nodes_dat_cache")
            .await
            .expect("read cache");
        assert_eq!(cache.as_deref(), Some("AQID"));
        let Ok(super::ActorResult::Ed2kStatus(cached)) =
            execute_operation(status("ed2k"), &mut engine, &events).await
        else {
            panic!("ed2k status after nodes commit");
        };
        assert_eq!(cached.network.kad_nodes, 42);

        // Kad 引导的提交：存活节点并入已知节点缓存并广播新条数；没有节点可并入时缓存不动。
        let contact = |last: u8| fluxdown_engine::ed2k::kad::proto::Contact {
            id: fluxdown_engine::ed2k::kad::proto::KadId([last; 16]),
            endpoint: fluxdown_engine::ed2k::kad::proto::Endpoint {
                ip: std::net::Ipv4Addr::new(10, 0, 0, last),
                udp_port: 4672,
                tcp_port: 4662,
            },
            version: 9,
        };
        let bootstrapped = super::commit_ed2k_kad_bootstrap(
            fluxdown_engine::ed2k::kad::BootstrapReport {
                contacted: 2,
                responded: 1,
                discovered: 1,
                contacts: vec![contact(1), contact(2)],
                ..Default::default()
            },
            &mut engine,
            &events,
        )
        .await
        .expect("commit bootstrap");
        assert_eq!((bootstrapped.responded, bootstrapped.node_count), (1, 2));
        assert_eq!(
            events.inspect(|snapshot| snapshot.config.values.get("ed2k_nodes_dat_count").cloned()),
            Some("2".to_owned())
        );
        let unreachable = super::commit_ed2k_kad_bootstrap(
            fluxdown_engine::ed2k::kad::BootstrapReport {
                contacted: 2,
                ..Default::default()
            },
            &mut engine,
            &events,
        )
        .await
        .expect("commit unreachable bootstrap");
        assert_eq!((unreachable.responded, unreachable.node_count), (0, 2));

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor ed2k status test directory removal failed: {error}");
        }
    }

    /// `daemon.task.delete`：文件删不掉时回报「任务已保留」而不是成功，任务记录与文件都在
    /// （#986：此前单任务删除吞掉清理失败，客户端误以为已删除）。
    #[tokio::test]
    async fn delete_task_reports_kept_task_when_files_cannot_be_removed() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_delete_kept_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        // 「文件」实为非空目录：remove_file 必然失败（非占用类错误）。
        tokio::fs::create_dir_all(dir.join("locked.bin"))
            .await
            .expect("create undeletable target");
        tokio::fs::write(dir.join("locked.bin").join("payload"), b"keep")
            .await
            .expect("write payload");
        let dir_text = dir.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig::default(),
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        engine
            .db
            .insert_task(
                "done",
                "https://example.invalid/locked.bin",
                "locked.bin",
                &dir_text,
                8,
                0,
                "",
                "",
                "",
                3,
            )
            .await
            .expect("insert task");

        let result = execute_operation(
            ActorOperation::DeleteTask {
                task_id: "done".to_owned(),
                delete_files: true,
            },
            &mut engine,
            &events,
        )
        .await;
        assert!(
            matches!(result, Err(ActorError::TaskFilesKept { in_use: false, .. })),
            "failed file removal must be reported as a kept task"
        );
        assert!(
            engine
                .db
                .load_task_by_id("done")
                .await
                .expect("load task")
                .is_some(),
            "task record is kept"
        );
        assert!(dir.join("locked.bin").join("payload").exists());

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor delete kept test directory removal failed: {error}");
        }
    }

    /// `daemon.task.setPiecePicking`：BT 任务按缺省字段做部分更新并落库；不存在 / 非 BT
    /// 任务映射为 `NotFound`，不改库。
    #[tokio::test]
    async fn set_task_piece_picking_updates_bt_tasks_only() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_piece_picking_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create piece picking test dir");
        let dir_text = dir.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig::default(),
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        let set = |task_id: &str, sequential: Option<bool>, first_last: Option<bool>| {
            ActorOperation::SetTaskPiecePicking {
                task_id: task_id.to_owned(),
                sequential,
                first_last_pieces_first: first_last,
            }
        };
        for (id, url) in [
            (
                "bt",
                "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
            ),
            ("http", "https://example.invalid/a.bin"),
        ] {
            engine
                .db
                .insert_task(id, url, "a.bin", &dir_text, 0, 0, "", "", "", 2)
                .await
                .expect("insert task");
        }
        let picking = |task: fluxdown_engine::model::TaskInfo| {
            (task.bt_sequential, task.bt_first_last_pieces_first)
        };

        execute_operation(set("bt", Some(true), None), &mut engine, &events)
            .await
            .expect("BT task accepts sequential");
        execute_operation(set("bt", None, Some(true)), &mut engine, &events)
            .await
            .expect("BT task accepts first and last pieces first");
        let bt = engine
            .db
            .load_task_by_id("bt")
            .await
            .expect("load bt")
            .expect("bt exists");
        assert_eq!(picking(bt), (true, true));

        for id in ["missing", "http"] {
            assert!(matches!(
                execute_operation(set(id, Some(true), Some(true)), &mut engine, &events).await,
                Err(ActorError::NotFound)
            ));
        }
        let http = engine
            .db
            .load_task_by_id("http")
            .await
            .expect("load http")
            .expect("http exists");
        assert_eq!(picking(http), (false, false), "non-BT task is untouched");

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor piece picking test directory removal failed: {error}");
        }
    }

    /// `daemon.task.setBtLimits`：BT 任务按缺省字段做部分更新并落库（上传限速与做种限制
    /// 共用一列）；不存在 / 非 BT 任务映射为 `NotFound`，不改库。
    #[tokio::test]
    async fn set_task_bt_limits_updates_bt_tasks_only() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_bt_limits_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create bt limits test dir");
        let dir_text = dir.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig::default(),
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        let set = |task_id: &str, download: Option<i64>, upload: Option<i64>| {
            ActorOperation::SetTaskBtLimits {
                task_id: task_id.to_owned(),
                download_limit_bps: download,
                upload_limit_bps: upload,
            }
        };
        for (id, url) in [
            (
                "bt",
                "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
            ),
            ("http", "https://example.invalid/a.bin"),
        ] {
            engine
                .db
                .insert_task(id, url, "a.bin", &dir_text, 0, 0, "", "", "", 2)
                .await
                .expect("insert task");
        }
        let limits = |task: fluxdown_engine::model::TaskInfo| {
            (task.bt_download_limit_bps, task.seed_upload_limit_bps)
        };

        execute_operation(set("bt", Some(512_000), None), &mut engine, &events)
            .await
            .expect("BT task accepts a download limit");
        execute_operation(set("bt", None, Some(64_000)), &mut engine, &events)
            .await
            .expect("BT task accepts an upload limit");
        let bt = engine
            .db
            .load_task_by_id("bt")
            .await
            .expect("load bt")
            .expect("bt exists");
        assert_eq!(limits(bt), (512_000, 64_000));

        execute_operation(set("bt", Some(0), None), &mut engine, &events)
            .await
            .expect("zero lifts the download limit");
        let bt = engine
            .db
            .load_task_by_id("bt")
            .await
            .expect("load bt")
            .expect("bt exists");
        assert_eq!(limits(bt), (0, 64_000), "the upload limit is left alone");

        for id in ["missing", "http"] {
            assert!(matches!(
                execute_operation(set(id, Some(1024), Some(1024)), &mut engine, &events).await,
                Err(ActorError::NotFound)
            ));
        }
        let http = engine
            .db
            .load_task_by_id("http")
            .await
            .expect("load http")
            .expect("http exists");
        assert_eq!(limits(http), (0, 0), "non-BT task is untouched");

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor bt limits test directory removal failed: {error}");
        }
    }

    /// `daemon.task.verify`：已完成的 .torrent 任务按 DB 种子字节校验落盘文件并经回执给出
    /// 结论；改坏数据后报告损坏文件；非 BT / 不存在的任务分别映射为参数错误 / NotFound。
    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn verify_task_reports_integrity_and_rejects_ineligible_tasks() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_verify_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create verify test dir");
        let dir_text = dir.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig::default(),
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        let (maintenance_tx, _maintenance_rx) = tokio::sync::mpsc::unbounded_channel();

        // 单文件种子 "hello.txt"，内容 "hello"，一个 piece（SHA1 = aaf4c61d…）。
        let mut torrent =
            b"d4:infod6:lengthi5e4:name9:hello.txt12:piece lengthi16384e6:pieces20:".to_vec();
        torrent.extend_from_slice(&[
            0xaa, 0xf4, 0xc6, 0x1d, 0xdc, 0xc5, 0xe8, 0xa2, 0xda, 0xbe, 0xde, 0x0f, 0x3b, 0x48,
            0x2c, 0xd9, 0xae, 0xa9, 0x43, 0x4d,
        ]);
        torrent.extend_from_slice(b"ee");
        for (id, url) in [
            ("bt", "torrent-file://hello.torrent"),
            ("http", "https://example.invalid/a.bin"),
        ] {
            engine
                .db
                .insert_task(id, url, "hello.txt", &dir_text, 1, 0, "", "", "", 3)
                .await
                .expect("insert task");
        }
        engine
            .db
            .save_torrent_file_bytes("bt", &torrent)
            .await
            .expect("save torrent bytes");
        tokio::fs::write(dir.join("hello.txt"), b"hello")
            .await
            .expect("write landed file");

        let mut verify = async |task_id: &str| {
            let (ack, response) = tokio::sync::oneshot::channel();
            super::dispatch_operation(
                ActorOperation::VerifyTask {
                    task_id: task_id.to_owned(),
                },
                ack,
                &mut engine,
                &events,
                &maintenance_tx,
            )
            .await;
            response.await.expect("verify acknowledged")
        };
        match verify("bt").await {
            Ok(super::ActorResult::TaskVerify(result)) => {
                assert_eq!(result.status, fluxdown_protocol::TaskVerifyStatus::Intact);
                assert_eq!((result.total_pieces, result.verified_pieces), (1, 1));
            }
            _ => panic!("intact data must verify"),
        }
        tokio::fs::write(dir.join("hello.txt"), b"jello")
            .await
            .expect("corrupt landed file");
        match verify("bt").await {
            Ok(super::ActorResult::TaskVerify(result)) => {
                assert_eq!(result.status, fluxdown_protocol::TaskVerifyStatus::Damaged);
                assert_eq!(result.damaged_files, vec!["hello.txt".to_owned()]);
            }
            _ => panic!("corrupted data must be reported"),
        }
        assert!(matches!(
            verify("http").await,
            Err(ActorError::InvalidArgument(_))
        ));
        assert!(matches!(verify("missing").await, Err(ActorError::NotFound)));

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor verify test directory removal failed: {error}");
        }
    }

    /// `daemon.task.hash`：已完成单文件任务经 off-actor 计算、maintenance 回流写回并返回
    /// `algo=hex`；未完成 / 目录产物 / 不存在分别映射为参数错误 / 参数错误 / NotFound；
    /// 计算期间被重新下载的结果不写回（TaskStateConflict），哈希记录随重新下载清空。
    #[tokio::test]
    async fn hash_task_records_digest_and_discards_results_of_restarted_tasks() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_hash_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(dir.join("folder"))
            .await
            .expect("create hash test dir");
        let dir_text = dir.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig::default(),
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        let (maintenance_tx, mut maintenance_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::fs::write(dir.join("hello.txt"), b"hello")
            .await
            .expect("write landed file");
        for (id, name, status) in [
            ("done", "hello.txt", 3),
            ("again", "hello.txt", 3),
            ("paused", "hello.txt", 2),
            ("folder", "folder", 3),
        ] {
            engine
                .db
                .insert_task(
                    id,
                    "https://example.invalid/a",
                    name,
                    &dir_text,
                    1,
                    0,
                    "",
                    "",
                    "",
                    1,
                )
                .await
                .expect("insert task");
            engine
                .db
                .update_task_status(id, status, "")
                .await
                .expect("set status");
        }

        let sha256 = fluxdown_engine::file_hash::HashAlgorithm::Sha256;
        let (ack, response) = tokio::sync::oneshot::channel();
        super::dispatch_operation(
            ActorOperation::HashTask {
                task_id: "done".to_owned(),
                algorithm: sha256,
            },
            ack,
            &mut engine,
            &events,
            &maintenance_tx,
        )
        .await;
        let event = maintenance_rx
            .recv()
            .await
            .expect("hash result reflows to actor");
        super::commit_maintenance(event, &mut engine, &events, &maintenance_tx).await;
        let expected =
            "sha256=2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned();
        match response.await.expect("hash acknowledged") {
            Ok(super::ActorResult::TaskHash(result)) => assert_eq!(result.file_hash, expected),
            _ => panic!("completed single-file task must be hashed"),
        }
        let done = engine
            .db
            .load_task_by_id("done")
            .await
            .expect("load done")
            .expect("done exists");
        assert_eq!(done.file_hash, expected);

        // 计算期间任务被重新下载：结果作废，不写回。
        let job = engine
            .manager
            .file_hash_future("again", sha256)
            .await
            .expect("eligible");
        engine
            .db
            .update_task_status("again", 1, "")
            .await
            .expect("restart");
        assert_eq!(
            engine.manager.commit_file_hash(job.await).await,
            Err(fluxdown_engine::file_hash::FileHashError::Stale)
        );
        // 已记录的哈希随重新下载清空。
        engine
            .db
            .update_task_status("done", 1, "")
            .await
            .expect("restart done");
        let restarted = engine
            .db
            .load_task_by_id("done")
            .await
            .expect("load restarted")
            .expect("restarted exists");
        assert_eq!(restarted.file_hash, "");

        // 同步拒绝（DB 判定）直接回执；目录产物在读盘线程里判定，经 maintenance 回流后回执。
        let mut reject = async |task_id: &str| {
            let (ack, mut response) = tokio::sync::oneshot::channel();
            super::dispatch_operation(
                ActorOperation::HashTask {
                    task_id: task_id.to_owned(),
                    algorithm: sha256,
                },
                ack,
                &mut engine,
                &events,
                &maintenance_tx,
            )
            .await;
            tokio::select! {
                result = &mut response => result.expect("rejection acknowledged"),
                Some(event) = maintenance_rx.recv() => {
                    super::commit_maintenance(event, &mut engine, &events, &maintenance_tx).await;
                    response.await.expect("rejection acknowledged")
                }
            }
        };
        for id in ["paused", "folder", "again"] {
            assert!(
                matches!(reject(id).await, Err(ActorError::InvalidArgument(_))),
                "{id} is not eligible"
            );
        }
        assert!(matches!(reject("missing").await, Err(ActorError::NotFound)));

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("actor hash test directory removal failed: {error}");
        }
    }

    /// `daemon.task.createMany`：保存目录 / 队列只解析一次，逐项拒绝映射为带字段的
    /// `InvalidArgument`，其余项照常创建；同 `batchId` 重放返回首次结果且不再建任务。
    #[tokio::test]
    async fn create_many_maps_item_rejections_and_replays_idempotently() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_create_many_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create createMany test dir");
        let dir_text = dir.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig::default(),
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        let item = |url: &str, checksum: &str| fluxdown_protocol::CreateTaskItem {
            url: url.to_owned(),
            file_name: String::new(),
            checksum: checksum.to_owned(),
        };
        let operation = || ActorOperation::CreateTasks {
            batch_id: "batch-1".to_owned(),
            common: Box::new(
                serde_json::from_value(serde_json::json!({
                    "url": "",
                    "startPaused": true,
                    "saveDir": dir_text,
                }))
                .expect("common request"),
            ),
            items: vec![
                item("http://127.0.0.1:1/a.bin", ""),
                item("thunder://not-base64!!", ""),
                item("http://127.0.0.1:1/b.bin", "md5=zz"),
                item("http://127.0.0.1:1/c.bin", ""),
            ],
            category_route: None,
        };
        async fn run(
            engine: &mut fluxdown_engine::Engine,
            events: &crate::event_hub::DaemonEventHub,
            operation: ActorOperation,
        ) -> Vec<fluxdown_protocol::CreateTaskOutcome> {
            match execute_operation(operation, engine, events).await {
                Ok(super::ActorResult::CreatedMany(results)) => results,
                Ok(_) => panic!("unexpected actor result"),
                Err(error) => panic!("createMany failed: {error}"),
            }
        }

        let first = run(&mut engine, &events, operation()).await;
        assert_eq!(first.len(), 4);
        assert!(first[0].task_id.is_some() && first[0].error.is_none());
        assert!(first[3].task_id.is_some() && first[3].error.is_none());
        for (index, field) in [(1, "url"), (2, "checksum")] {
            let error = first[index].error.as_ref().expect("rejection error");
            assert!(first[index].task_id.is_none());
            assert_eq!(
                error.code,
                fluxdown_protocol::ApplicationErrorCode::InvalidArgument
            );
            assert!(!error.retryable);
            assert_eq!(error.field.as_deref(), Some(field));
        }
        let stored = engine.db.load_all_tasks().await.expect("load tasks");
        assert_eq!(stored.len(), 2);
        assert!(
            stored
                .iter()
                .all(|task| task.save_dir == dir_text && task.status == 2)
        );

        let replay = run(&mut engine, &events, operation()).await;
        assert_eq!(
            replay, first,
            "the replay returns the first results verbatim"
        );
        assert_eq!(
            engine.db.load_all_tasks().await.expect("load tasks").len(),
            2,
            "the replay creates nothing"
        );

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("createMany test directory removal failed: {error}");
        }
    }

    /// `daemon.task.createMany` 携带分类路由：按条目初始名命中分类的条目落进分类目录，
    /// 未命中的条目留在基准目录（`common.saveDir`）。
    #[tokio::test]
    async fn create_many_applies_category_route_per_item() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_create_many_route_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let base = dir.join("base");
        let images = dir.join("images");
        for path in [&base, &images] {
            tokio::fs::create_dir_all(path)
                .await
                .expect("create createMany route test dir");
        }
        let dir_text = dir.to_string_lossy().into_owned();
        let base_text = base.to_string_lossy().into_owned();
        let images_text = images.to_string_lossy().into_owned();
        let mut engine = fluxdown_engine::Engine::new(
            fluxdown_engine::EngineConfig {
                max_concurrent: 1,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir_text.clone(),
                app_data_dir: dir_text.clone(),
                bt_config: fluxdown_engine::bt_config::BtConfig::default(),
                proxy_config: fluxdown_engine::proxy_config::ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(dir.clone()),
                database_url: None,
            },
            std::sync::Arc::new(fluxdown_engine::NoopSink),
            std::sync::Arc::new(fluxdown_engine::NoopSelection),
        )
        .await
        .expect("construct engine");
        let events =
            crate::event_hub::DaemonEventHub::new(fluxdown_protocol::DaemonSnapshot::default(), 32);
        let item = |url: &str| fluxdown_protocol::CreateTaskItem {
            url: url.to_owned(),
            file_name: String::new(),
            checksum: String::new(),
        };
        let operation = ActorOperation::CreateTasks {
            batch_id: "route-batch".to_owned(),
            common: Box::new(
                serde_json::from_value(serde_json::json!({
                    "url": "",
                    "startPaused": true,
                    "saveDir": base_text,
                }))
                .expect("common request"),
            ),
            items: vec![
                item("http://127.0.0.1:1/photo.jpg"),
                item("http://127.0.0.1:1/notes.txt"),
            ],
            category_route: Some(fluxdown_protocol::CategoryRouteDto {
                rules: vec![fluxdown_protocol::CategoryRuleDto {
                    match_mode: "extension".to_owned(),
                    extensions: vec!["jpg".to_owned()],
                    regex_pattern: String::new(),
                    url_pattern: String::new(),
                    save_dir: images_text.clone(),
                }],
                other_dir: String::new(),
            }),
        };
        let results = match execute_operation(operation, &mut engine, &events).await {
            Ok(super::ActorResult::CreatedMany(results)) => results,
            Ok(_) => panic!("unexpected actor result"),
            Err(error) => panic!("createMany failed: {error}"),
        };
        assert!(
            results
                .iter()
                .all(|r| r.task_id.is_some() && r.error.is_none())
        );
        let stored = engine.db.load_all_tasks().await.expect("load tasks");
        assert_eq!(stored.len(), 2);
        let save_dir_of = |url_tail: &str| {
            stored
                .iter()
                .find(|task| task.url.ends_with(url_tail))
                .map(|task| task.save_dir.clone())
        };
        assert_eq!(
            save_dir_of("photo.jpg").as_deref(),
            Some(images_text.as_str())
        );
        assert_eq!(
            save_dir_of("notes.txt").as_deref(),
            Some(base_text.as_str())
        );

        drop(engine);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await {
            eprintln!("createMany route test directory removal failed: {error}");
        }
    }

    #[test]
    fn log_limit_falls_back_to_catalog_default_for_missing_or_invalid_values() {
        let limit = |value: Option<&str>| {
            let config = value
                .map(|v| {
                    std::collections::HashMap::from([("log_max_size_mb".to_owned(), v.to_owned())])
                })
                .unwrap_or_default();
            super::log_limit_bytes(&config)
        };
        assert_eq!(limit(Some("64")), 64 * 1024 * 1024);
        assert_eq!(limit(Some(" 1024 ")), 1024 * 1024 * 1024);
        for fallback in [None, Some("0"), Some("1025"), Some("abc"), Some("-3")] {
            assert_eq!(limit(fallback), 10 * 1024 * 1024, "{fallback:?}");
        }
    }
}
