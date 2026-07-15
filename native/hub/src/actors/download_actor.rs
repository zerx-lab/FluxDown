use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fluxdown_engine::bt_downloader::BtConfig;
use fluxdown_engine::db::Db;
use fluxdown_engine::download_manager::{self, TaskDone};
use fluxdown_engine::events::EventSink;
use fluxdown_engine::plugin::{PluginError, PluginManager};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::selection::HostSelection;
use fluxdown_engine::{Engine, EngineConfig};
use rinf::{DartSignal, RustSignal};
use tokio::sync::{broadcast, mpsc};

use crate::api_host::{ApiCommand, HubApiHost, LiveSpeedMap};
use crate::file_association;
use crate::logger::log_info;
use crate::native_messaging::{self};
use crate::protocol_registry;
use crate::rinf_selection::RinfHostSelection;
use crate::rinf_sink::RinfEventSink;
use crate::signals::{
    BatchControlTask, BatchCreateTask, CheckFileAssociation, CheckForUpdate, CheckUrlProtocol,
    ConfigEntry, ConfigLoaded, ConfirmExternalDownload, ControlTask, CreateQueue, CreateTask,
    DeleteQueue, DetectSystemProxy, DownloadUpdate, Ed2kServerSubscriptionResult,
    ExternalDownloadRequest, FfmpegInstallProgress, FfmpegInstallResult, FfmpegStatusReport,
    FfmpegVersionList, FileAssociationStatus, IgnorePluginRetry, InstallFfmpeg,
    InstallMarketPlugin, InstallPlugin, InstallUpdate, InstallYtdlp, MarketEntrySignal,
    MarketIndexLoaded, MoveTaskToQueue, OpenFile, PluginList, PluginOpResult, ProbeTorrentMeta,
    ProxyTestResult, RequestAllQueues, RequestAllTasks, RequestConfig, RequestFfmpegStatus,
    RequestFfmpegVersions, RequestMarketIndex, RequestPlugins, RequestUpdateFailureMarker,
    RequestYtdlpStatus, RequestYtdlpVersions, RescanFiles, RevealFile, SaveConfig,
    SavePluginSettings, SelectBtFiles, SelectHlsQuality, SelectResolveVariant, SetFileAssociation,
    SetPluginEnabled, SetPriorityTask, SetUrlProtocol, SystemProxyInfo, TestProxyConnection,
    TrackerSubscriptionResult, UninstallFfmpeg, UninstallPlugin, UninstallYtdlp, UpdateCheckResult,
    UpdateEd2kServerSubscription, UpdateFailureMarker, UpdateQueue, UpdateTrackerSubscription,
    UrlProtocolStatus, YtdlpInstallProgress, YtdlpInstallResult, YtdlpStatusReport,
    YtdlpVersionList,
};
use crate::updater;
use fluxdown_api::server::{ApiServerConfig, ApiServerHandle, spawn_api_server};
use fluxdown_api::service::TaskEvent;

/// Compute default save directory (platform-dependent).
fn default_save_dir() -> String {
    // Android：应用专属外部目录（免权限可写）；公共 Download 目录需
    // SAF / All-files 权限，由 Dart 侧引导用户选择后经配置下发。
    #[cfg(target_os = "android")]
    {
        if let Some(pkg) = fluxdown_engine::data_dir::android_package_name() {
            return format!("/storage/emulated/0/Android/data/{pkg}/files/Download");
        }
    }
    if cfg!(target_os = "windows")
        && let Some(profile) = std::env::var_os("USERPROFILE")
    {
        let mut p = PathBuf::from(profile);
        p.push("Downloads");
        return p.to_string_lossy().into_owned();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push("Downloads");
        return p.to_string_lossy().into_owned();
    }
    ".".to_string()
}

/// Build a [`BtConfig`] from the raw config key-value map.
///
/// When the tracker subscription feature is disabled, the cached
/// subscription trackers are excluded so only the user's own list is used.
fn bt_config_from_map(cfg: &HashMap<String, String>) -> BtConfig {
    let sub_enabled = cfg
        .get("bt_tracker_sub_enabled")
        .map(|v| v == "true")
        .unwrap_or(true);
    BtConfig {
        enable_dht: cfg
            .get("bt_enable_dht")
            .map(|v| v == "true")
            .unwrap_or(true),
        enable_upnp: cfg
            .get("bt_enable_upnp")
            .map(|v| v == "true")
            .unwrap_or(true),
        port_start: cfg
            .get("bt_port_start")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(6881),
        port_end: cfg
            .get("bt_port_end")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(6891),
        custom_trackers: cfg.get("bt_custom_trackers").cloned().unwrap_or_default(),
        subscription_trackers: if sub_enabled {
            cfg.get("bt_tracker_sub_cache").cloned().unwrap_or_default()
        } else {
            String::new()
        },
    }
}

/// Spawn a background task that fetches all tracker subscription sources,
/// persists the deduped result to the config table, then reports the outcome
/// back to the actor loop (which updates the BtConfig and notifies Dart).
fn spawn_tracker_sub_refresh(
    db: Db,
    tx: mpsc::Sender<fluxdown_engine::tracker_subscription::FetchOutcome>,
) {
    tokio::spawn(async move {
        let cfg = db.get_all_config().await.unwrap_or_default();
        let urls = cfg
            .get("bt_tracker_sub_urls")
            .cloned()
            .unwrap_or_else(fluxdown_engine::tracker_subscription::default_subscription_urls);
        let outcome = fluxdown_engine::tracker_subscription::fetch_subscriptions(&urls).await;
        if outcome.is_success() {
            let now = chrono::Utc::now().timestamp();
            if let Err(e) = db
                .set_config("bt_tracker_sub_cache", &outcome.trackers.join("\n"))
                .await
            {
                log_info!("[actor] failed to save tracker sub cache: {}", e);
            }
            if let Err(e) = db
                .set_config("bt_tracker_sub_updated_at", &now.to_string())
                .await
            {
                log_info!("[actor] failed to save tracker sub timestamp: {}", e);
            }
        }
        let _ = tx.send(outcome).await;
    });
}

/// Spawn a background task that fetches all ED2K server.met subscription
/// sources, persists the deduped `ip:port` list to the config table, then
/// reports the outcome back to the actor loop (which notifies Dart).
///
/// Unlike BT trackers, the ED2K server list is read fresh at each download's
/// find-sources step, so no shared session needs invalidating here.
fn spawn_ed2k_server_sub_refresh(
    db: Db,
    tx: mpsc::Sender<fluxdown_engine::ed2k::server_subscription::ServerFetchOutcome>,
) {
    tokio::spawn(async move {
        let cfg = db.get_all_config().await.unwrap_or_default();
        let urls = cfg
            .get("ed2k_server_sub_urls")
            .cloned()
            .unwrap_or_else(fluxdown_engine::ed2k::server_subscription::default_server_met_urls);
        let outcome =
            fluxdown_engine::ed2k::server_subscription::fetch_server_subscriptions(&urls).await;
        if outcome.is_success() {
            let now = chrono::Utc::now().timestamp();
            if let Err(e) = db
                .set_config("ed2k_server_sub_cache", &outcome.servers.join(","))
                .await
            {
                log_info!("[actor] failed to save ed2k server sub cache: {}", e);
            }
            if let Err(e) = db
                .set_config("ed2k_server_sub_updated_at", &now.to_string())
                .await
            {
                log_info!("[actor] failed to save ed2k server sub timestamp: {}", e);
            }
            if let Err(e) = db
                .set_config(
                    "ed2k_server_sub_cache_version",
                    &fluxdown_engine::ed2k::server_subscription::CACHE_FORMAT_VERSION.to_string(),
                )
                .await
            {
                log_info!(
                    "[actor] failed to save ed2k server sub cache version: {}",
                    e
                );
            }
        }
        let _ = tx.send(outcome).await;
    });
}

/// Kad nodes.dat 刷新间隔（秒）：24 小时。
const ED2K_NODES_DAT_REFRESH_SECS: i64 = 24 * 60 * 60;

/// Spawn a fire-and-forget task that downloads `nodes.dat` from the configured
/// URL and caches it (base64) in the config table for Kad bootstrap.
///
/// Binary blob with no Dart-visible state, so no result channel — failures are
/// logged and tolerated (Kad simply stays inactive until a later refresh).
fn spawn_ed2k_nodes_dat_refresh(db: Db) {
    tokio::spawn(async move {
        use base64::Engine as _;
        let cfg = db.get_all_config().await.unwrap_or_default();
        let url = cfg.get("ed2k_nodes_dat_url").cloned().unwrap_or_default();
        if url.is_empty() {
            return;
        }
        match fluxdown_engine::ed2k::kad::fetch_nodes_dat(&url).await {
            Ok(bytes) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let now = chrono::Utc::now().timestamp();
                if let Err(e) = db.set_config("ed2k_nodes_dat_cache", &encoded).await {
                    log_info!("[actor] failed to save ed2k nodes.dat cache: {}", e);
                }
                if let Err(e) = db
                    .set_config("ed2k_nodes_dat_updated_at", &now.to_string())
                    .await
                {
                    log_info!("[actor] failed to save ed2k nodes.dat timestamp: {}", e);
                }
                log_info!("[actor] ed2k nodes.dat refreshed ({} bytes)", bytes.len());
            }
            Err(e) => log_info!("[actor] ed2k nodes.dat refresh failed: {}", e),
        }
    });
}

/// Read initial config values from DB to pass to DownloadManager.
async fn load_initial_config(
    db: &Db,
) -> (usize, u64, String, BtConfig, ProxyConfig, String, i32, i32) {
    let config = db.get_all_config().await.unwrap_or_default();
    let max_concurrent = config
        .get("max_concurrent_tasks")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5);
    let speed_limit_bytes = config
        .get("speed_limit_bytes")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let save_dir = config
        .get("default_save_dir")
        .cloned()
        .unwrap_or_else(default_save_dir);
    let bt_config = bt_config_from_map(&config);

    let proxy_config = ProxyConfig::from_config_map(&config);
    let user_agent = config.get("global_user_agent").cloned().unwrap_or_default();
    let default_segments = config
        .get("default_segments")
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(0);
    // Auto 模式最大连接数上限。老库无此 key → 默认 16。
    let auto_max_connections = config
        .get("auto_max_connections")
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(16);

    (
        max_concurrent,
        speed_limit_bytes,
        save_dir,
        bt_config,
        proxy_config,
        user_agent,
        default_segments,
        auto_max_connections,
    )
}

pub async fn run(db_dir: PathBuf) {
    let db = match Db::open(&db_dir).await {
        Ok(db) => db,
        Err(e) => {
            log_info!("Failed to open database: {}", e);
            return;
        }
    };

    // Initialize default config values in DB (no-op if already set)
    if let Err(e) = db.init_default_config(&default_save_dir()).await {
        log_info!("Failed to init default config: {}", e);
    }

    // Load persisted config to initialize the manager with correct limits.
    let (
        max_concurrent,
        speed_limit_bps,
        save_dir,
        mut bt_config,
        proxy_config,
        user_agent,
        default_segments,
        auto_max_connections,
    ) = load_initial_config(&db).await;
    log_info!(
        "[actor] proxy config: mode={}, type={}, host={}, port={}",
        proxy_config.mode.as_str(),
        proxy_config.proxy_type.as_str(),
        proxy_config.host,
        proxy_config.port,
    );

    // Populate default tracker list on first launch (when DB value is empty).
    if bt_config.custom_trackers.trim().is_empty() {
        let defaults = fluxdown_engine::bt_downloader::default_tracker_list();
        if let Err(e) = db.set_config("bt_custom_trackers", &defaults).await {
            log_info!("[actor] failed to save default trackers: {}", e);
        }
        bt_config.custom_trackers = defaults;
    }
    log_info!(
        "[actor] init config: max_concurrent={}, speed_limit_bps={}, save_dir={}, \
         bt: dht={}, upnp={}, ports={}-{}, custom_trackers={} lines, subscription_trackers={} lines",
        max_concurrent,
        speed_limit_bps,
        save_dir,
        bt_config.enable_dht,
        bt_config.enable_upnp,
        bt_config.port_start,
        bt_config.port_end,
        bt_config.custom_trackers.lines().count(),
        bt_config.subscription_trackers.lines().count(),
    );

    let app_data_dir = db_dir.to_string_lossy().into_owned();

    // 引擎事件接收端与选择接口:分别桥接到 hub 的 RustSignal 发送与
    // oneshot 等待表。`sink` 同时供 `Engine::new` 与 `progress_reporter`
    // 使用(后者独立取走 progress_rx,不经由 manager 内部的 sink 字段)。
    // `live_speeds`:aria2 兼容层 `live_speeds()` 的实时速率表,`RinfEventSink`
    // 写、`HubApiHost` 读,构造后两侧共享同一个 `Arc`(见下方 `api_host` 构造点)。
    let live_speeds: LiveSpeedMap = Arc::new(Mutex::new(HashMap::new()));
    // aria2 兼容层 WS 通知源:`RinfEventSink` 在状态迁移判定后广播,
    // `HubApiHost::subscribe_task_events()`、本循环删除命令处理点与
    // `handle_api_command` 的 `DeleteTask` 分支(经 `rinf_sink.
    // broadcast_task_stop`)共用同一个 `Sender`。容量 256:无订阅者时
    // `send` 直接返回 `Err` 并被忽略,容量只影响「有订阅者但消费慢」时
    // 的积压上限,超限后旧订阅者下次 `recv()` 收到 `Lagged`。
    let (task_events_tx, _) = broadcast::channel::<TaskEvent>(256);
    // 保留具体类型的 `Arc`:除了作为 `Arc<dyn EventSink>` 注入引擎,删除
    // 命令处理点还需直接调用 `broadcast_task_stop`——它不在 `EventSink`
    // trait 上,因为那是 aria2 兼容层专属的收尾动作,不属于通用事件转发
    // 契约。
    let rinf_sink = Arc::new(RinfEventSink::new(
        live_speeds.clone(),
        task_events_tx.clone(),
    ));
    let sink: Arc<dyn EventSink> = rinf_sink.clone();
    let selector: Arc<dyn HostSelection> = Arc::new(RinfHostSelection::new());

    let mut engine = match Engine::new(
        EngineConfig {
            max_concurrent,
            speed_limit_bps,
            default_save_dir: save_dir,
            app_data_dir,
            bt_config,
            proxy_config,
            user_agent,
            // db_dir 已由 `actors::create_actors` 通过
            // `fluxdown_engine::data_dir::resolve_data_dir(None)` 解析,
            // 此处显式传入,使 `Engine::new` 内部的解析成为等价的直通,
            // 保持与 `Db::open(&db_dir)`(上面已单独执行一次)完全一致的路径。
            data_dir_override: Some(db_dir.clone()),
            database_url: None,
        },
        sink.clone(),
        selector.clone(),
    )
    .await
    {
        Ok(e) => e,
        Err(e) => {
            log_info!("Failed to create engine: {}", e);
            return;
        }
    };

    engine.manager.set_default_segments(default_segments);
    engine
        .manager
        .set_auto_max_connections(auto_max_connections);

    // Apply persisted log size cap (MB) to the global logger.
    if let Ok(Some(v)) = engine.db.get_config("log_max_size_mb").await
        && let Ok(mb) = v.parse::<u64>()
    {
        crate::logger::set_max_total_bytes(mb * 1024 * 1024);
    }

    // Apply persisted auto-retry config (key-value `config` table). Absent or
    // unparsable values fall back to the manager's built-in defaults.
    {
        let cfg = engine.db.get_all_config().await.unwrap_or_default();
        if let Some(v) = cfg
            .get("max_auto_retries")
            .and_then(|s| s.parse::<i32>().ok())
        {
            engine.manager.set_max_auto_retries(v);
        }
        if let Some(v) = cfg
            .get("auto_retry_delay_secs")
            .and_then(|s| s.parse::<u64>().ok())
        {
            engine.manager.set_auto_retry_delay_secs(v);
        }
        // 下载完成后是否采用服务器 Last-Modified 作为文件修改时间（默认关闭）。
        if let Some(v) = cfg.get("use_server_time") {
            engine.manager.set_use_server_time(v == "true");
        }
    }

    if let Some(rx) = engine.manager.take_progress_rx() {
        tokio::spawn(download_manager::progress_reporter(
            rx,
            engine.db.clone(),
            sink.clone(),
        ));
    }

    // Load named queue settings into the in-memory cache so that
    // per-queue speed limits and concurrency limits take effect immediately.
    engine.manager.load_queues().await;

    // Channel for spawned tasks to notify completion (for active_tokens cleanup)
    let mut done_rx: mpsc::Receiver<TaskDone> = match engine.manager.take_done_rx() {
        Some(rx) => rx,
        None => {
            // Should never happen — take_done_rx returns Some on first call
            let (_tx, rx) = mpsc::channel(1);
            rx
        }
    };

    // Channel for delayed auto-retry of failed tasks (stall / network errors).
    let mut retry_rx: mpsc::Receiver<String> = match engine.manager.take_retry_rx() {
        Some(rx) => rx,
        None => {
            let (_tx, rx) = mpsc::channel(1);
            rx
        }
    };

    // Off-actor plugin resolve 回流通道（见插件系统契约一，关键）：resolver
    // 平面在独立 tokio task 上异步执行，结果经 `resolve_rx` 回流本循环调用
    // `on_resolve_ready`；onError 重试意图经 `plugin_retry_rx` 回流调用
    // `plugin_request_retry`。不接线会导致该宿主下 off-actor resolve 永不
    // 完成（下载卡死）。
    let mut resolve_rx: mpsc::UnboundedReceiver<fluxdown_engine::download_manager::ResolveOutcome> =
        match engine.manager.take_resolve_rx() {
            Some(rx) => rx,
            None => {
                let (_tx, rx) = mpsc::unbounded_channel();
                rx
            }
        };
    let mut plugin_retry_rx: mpsc::UnboundedReceiver<(String, u64)> =
        match engine.manager.take_plugin_retry_rx() {
            Some(rx) => rx,
            None => {
                let (_tx, rx) = mpsc::unbounded_channel();
                rx
            }
        };

    let create_recv = CreateTask::get_dart_signal_receiver();
    let batch_create_recv = BatchCreateTask::get_dart_signal_receiver();
    let control_recv = ControlTask::get_dart_signal_receiver();
    let batch_control_recv = BatchControlTask::get_dart_signal_receiver();
    let all_recv = RequestAllTasks::get_dart_signal_receiver();
    let create_queue_recv = CreateQueue::get_dart_signal_receiver();
    let update_queue_recv = UpdateQueue::get_dart_signal_receiver();
    let delete_queue_recv = DeleteQueue::get_dart_signal_receiver();
    let move_task_queue_recv = MoveTaskToQueue::get_dart_signal_receiver();
    let all_queues_recv = RequestAllQueues::get_dart_signal_receiver();
    let config_save_recv = SaveConfig::get_dart_signal_receiver();
    let config_req_recv = RequestConfig::get_dart_signal_receiver();
    let confirm_ext_recv = ConfirmExternalDownload::get_dart_signal_receiver();
    let check_update_recv = CheckForUpdate::get_dart_signal_receiver();
    let download_update_recv = DownloadUpdate::get_dart_signal_receiver();
    let install_update_recv = InstallUpdate::get_dart_signal_receiver();
    let req_update_marker_recv = RequestUpdateFailureMarker::get_dart_signal_receiver();
    let set_file_assoc_recv = SetFileAssociation::get_dart_signal_receiver();
    let check_file_assoc_recv = CheckFileAssociation::get_dart_signal_receiver();
    let test_proxy_recv = TestProxyConnection::get_dart_signal_receiver();
    let detect_sys_proxy_recv = DetectSystemProxy::get_dart_signal_receiver();
    let select_hls_quality_recv = SelectHlsQuality::get_dart_signal_receiver();
    let select_resolve_variant_recv = SelectResolveVariant::get_dart_signal_receiver();
    let set_url_proto_recv = SetUrlProtocol::get_dart_signal_receiver();
    let check_url_proto_recv = CheckUrlProtocol::get_dart_signal_receiver();
    let set_priority_recv = SetPriorityTask::get_dart_signal_receiver();
    let select_bt_files_recv = SelectBtFiles::get_dart_signal_receiver();
    let probe_torrent_meta_recv = ProbeTorrentMeta::get_dart_signal_receiver();
    let reveal_file_recv = RevealFile::get_dart_signal_receiver();
    let open_file_recv = OpenFile::get_dart_signal_receiver();
    let update_tracker_sub_recv = UpdateTrackerSubscription::get_dart_signal_receiver();
    let rescan_recv = RescanFiles::get_dart_signal_receiver();
    let req_plugins_recv = RequestPlugins::get_dart_signal_receiver();
    let install_plugin_recv = InstallPlugin::get_dart_signal_receiver();
    let uninstall_plugin_recv = UninstallPlugin::get_dart_signal_receiver();
    let set_plugin_enabled_recv = SetPluginEnabled::get_dart_signal_receiver();
    let save_plugin_settings_recv = SavePluginSettings::get_dart_signal_receiver();
    let ignore_plugin_retry_recv = IgnorePluginRetry::get_dart_signal_receiver();
    let request_market_index_recv = RequestMarketIndex::get_dart_signal_receiver();
    let install_market_plugin_recv = InstallMarketPlugin::get_dart_signal_receiver();
    let req_ffmpeg_status_recv = RequestFfmpegStatus::get_dart_signal_receiver();
    let req_ffmpeg_versions_recv = RequestFfmpegVersions::get_dart_signal_receiver();
    let install_ffmpeg_recv = InstallFfmpeg::get_dart_signal_receiver();
    let uninstall_ffmpeg_recv = UninstallFfmpeg::get_dart_signal_receiver();
    let req_ytdlp_status_recv = RequestYtdlpStatus::get_dart_signal_receiver();
    let req_ytdlp_versions_recv = RequestYtdlpVersions::get_dart_signal_receiver();
    let install_ytdlp_recv = InstallYtdlp::get_dart_signal_receiver();
    let uninstall_ytdlp_recv = UninstallYtdlp::get_dart_signal_receiver();

    // Tracker 订阅刷新通道：后台 fetch 任务完成后把结果送回 actor 循环，
    // 由循环更新 BtConfig、失效 BT 会话并通知 Dart。
    let (tracker_sub_tx, mut tracker_sub_rx) =
        mpsc::channel::<fluxdown_engine::tracker_subscription::FetchOutcome>(4);

    // 启动时自动刷新：订阅启用且缓存超过 24 小时未更新。
    {
        let cfg = engine.db.get_all_config().await.unwrap_or_default();
        let sub_enabled = cfg
            .get("bt_tracker_sub_enabled")
            .map(|v| v == "true")
            .unwrap_or(true);
        let updated_at = cfg
            .get("bt_tracker_sub_updated_at")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let now = chrono::Utc::now().timestamp();
        if sub_enabled
            && now.saturating_sub(updated_at)
                > fluxdown_engine::tracker_subscription::REFRESH_INTERVAL_SECS
        {
            log_info!(
                "[actor] tracker subscription stale (updated_at={}), auto-refreshing",
                updated_at
            );
            spawn_tracker_sub_refresh(engine.db.clone(), tracker_sub_tx.clone());
        }
    }

    let update_ed2k_sub_recv = UpdateEd2kServerSubscription::get_dart_signal_receiver();

    // ED2K 服务器订阅刷新通道：后台 fetch 任务完成后把结果送回 actor 循环通知 Dart。
    let (ed2k_sub_tx, mut ed2k_sub_rx) =
        mpsc::channel::<fluxdown_engine::ed2k::server_subscription::ServerFetchOutcome>(4);

    // 启动时自动刷新：订阅启用且缓存超过 24 小时未更新。
    {
        let cfg = engine.db.get_all_config().await.unwrap_or_default();
        let sub_enabled = cfg
            .get("ed2k_server_sub_enabled")
            .map(|v| v == "true")
            .unwrap_or(true);
        let updated_at = cfg
            .get("ed2k_server_sub_updated_at")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        // 缓存格式版本：pre-fix（v1）写入的缓存 IP 字节序被反转，全为死主机。
        // 版本不符即清空缓存 + 归零时间戳，强制用修正后的解析器重取。
        let cache_version = cfg
            .get("ed2k_server_sub_cache_version")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let version_stale =
            cache_version < fluxdown_engine::ed2k::server_subscription::CACHE_FORMAT_VERSION;
        if version_stale {
            log_info!(
                "[actor] ed2k server sub cache version {} < {}, invalidating (byte-order fix)",
                cache_version,
                fluxdown_engine::ed2k::server_subscription::CACHE_FORMAT_VERSION
            );
            let _ = engine.db.set_config("ed2k_server_sub_cache", "").await;
        }
        let now = chrono::Utc::now().timestamp();
        if sub_enabled
            && (version_stale
                || now.saturating_sub(updated_at)
                    > fluxdown_engine::ed2k::server_subscription::REFRESH_INTERVAL_SECS)
        {
            log_info!(
                "[actor] ed2k server subscription stale (updated_at={}, version_stale={}), auto-refreshing",
                updated_at,
                version_stale
            );
            spawn_ed2k_server_sub_refresh(engine.db.clone(), ed2k_sub_tx.clone());
        }
    }

    // 启动时自动刷新 Kad nodes.dat：启用 Kad 且缓存超过 24 小时未更新（或为空）。
    {
        let cfg = engine.db.get_all_config().await.unwrap_or_default();
        let kad_enabled = cfg
            .get("ed2k_enable_kad")
            .map(|v| v == "true")
            .unwrap_or(true);
        let updated_at = cfg
            .get("ed2k_nodes_dat_updated_at")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let now = chrono::Utc::now().timestamp();
        if kad_enabled && now.saturating_sub(updated_at) > ED2K_NODES_DAT_REFRESH_SECS {
            log_info!(
                "[actor] ed2k nodes.dat stale (updated_at={}), auto-refreshing",
                updated_at
            );
            spawn_ed2k_nodes_dat_refresh(engine.db.clone());
        }
    }

    // Shared channel for external download requests. Both the Native Messaging
    // listener (browser extension via the NMH relay) and the local API server's
    // takeover / aria2-compat endpoints (Tampermonkey userscripts) push
    // `Vec<DownloadRequest>`s into this channel (NMH `batch_download` carries a
    // whole multi-select batch in one message; every single-item transport wraps
    // its request in a one-element Vec); the `native_msg_rx` select! branch
    // below handles both transports with identical logic.
    let (ext_dl_tx, mut native_msg_rx) =
        mpsc::channel::<Vec<fluxdown_api::types::DownloadRequest>>(64);

    // 本机 API 服务器（127.0.0.1）：探活 / 脚本接管 / aria2 兼容 / 管理 API。
    // 写操作经 api_cmd_rx 回到本事件循环串行执行；local_server_* 配置变更时
    // 热重启（见下方 SaveConfig 分支），无需重启应用。
    // 先于 Native Messaging listener 构造：listener 的 tasks/task_op/
    // open_file/reveal_file 分支需要同一个 `Arc<dyn ApiHost>` 查任务表 /
    // live_speeds / 下发写命令。
    // 插件管理器共享句柄：读/写方法均只碰 Db + 插件表(不碰 active_tasks)，
    // 可安全在 `HubApiHost`(HTTP 侧)与本循环两处并发持有同一个 `Arc` 直接
    // `.await` 调用，无需经 `ApiCommand` 往返(见插件系统契约 hub 节 5)。
    let plugin_manager = engine.manager.plugin_manager();
    let (api_cmd_tx, mut api_cmd_rx) = mpsc::channel::<ApiCommand>(32);
    let api_host: Arc<dyn fluxdown_api::service::ApiHost> = Arc::new(HubApiHost::new(
        engine.db.clone(),
        api_cmd_tx,
        ext_dl_tx.clone(),
        live_speeds,
        task_events_tx,
        plugin_manager.clone(),
        engine.data_dir.clone(),
    ));

    // Native Messaging listener (reads from the Named Pipe / Unix socket).
    native_messaging::spawn_native_messaging_listener_with(ext_dl_tx.clone(), api_host.clone());
    let mut api_server_handle = {
        let cfg = ApiServerConfig::from_config_map(
            &engine.db.get_all_config().await.unwrap_or_default(),
            env!("CARGO_PKG_VERSION"),
        );
        spawn_api_server(api_host.clone(), cfg)
    };

    // Auto-register fluxdown:// URL protocol on startup (idempotent).
    tokio::task::spawn_blocking(|| {
        if !protocol_registry::is_registered() {
            if let Err(e) = protocol_registry::register() {
                log_info!("[actor] auto-register fluxdown:// protocol failed: {}", e);
            }
        } else {
            log_info!("[actor] fluxdown:// protocol already registered");
        }
    });

    // Self-heal a spurious RUNASADMIN compatibility flag on our own exe (idempotent).
    // PCA/installer-detection may have flagged an older build lacking the asInvoker
    // manifest; that makes CreateProcess-based launches (e.g. the installer's [Run]
    // step) fail with error 740. Clearing it here fixes already-installed machines.
    tokio::task::spawn_blocking(crate::compat_flags::clear_runasadmin_self);

    // Auto-register NMH (Native Messaging Host) for browser extension communication.
    // Only re-registers when the registry is missing, incomplete, or stale (exe path changed).
    tokio::task::spawn_blocking(|| {
        if !crate::nmh_registry::needs_update() {
            log_info!("[actor] NMH already registered and up to date");
            return;
        }
        if let Err(e) = crate::nmh_registry::register() {
            log_info!("[actor] NMH registration failed: {}", e);
        }
    });

    // 缓存浏览器扩展捕获的请求事务上下文（headers/method/body + per-item
    // cookies/referrer/fileSize），以 URL 为 key，在用户确认下载时一并消耗——
    // 下游用此一比一重建浏览器请求。批量请求的 per-item 认证元数据只存在
    // 这里（确认信号是批级共享字段），按 URL 恢复精度。
    #[derive(Default, Clone)]
    struct ExtRequestCtx {
        headers: HashMap<String, String>,
        method: Option<String>,
        body: Option<fluxdown_api::types::RequestBody>,
        cookies: String,
        referrer: String,
        /// 文件大小提示：>0 已知大小、-1 已确认可下载但大小未知（跳过 probe）、
        /// 0 未知（正常 probe）。语义与 `DownloadRequest::file_size` 一致。
        file_size: i64,
    }
    let mut ext_request_cache: HashMap<String, ExtRequestCtx> = HashMap::new();
    // 缓存插入序（FIFO 淘汰用）：确认消费不回收队列条目（懒清理），
    // 淘汰/压实逻辑见 native_msg_rx 分支。
    let mut ext_cache_order: VecDeque<String> = VecDeque::new();

    loop {
        tokio::select! {
            Some(signal) = create_recv.recv() => {
                let msg = signal.message;
                engine.manager
                    .create_task(msg.url, msg.save_dir, msg.file_name, msg.segments, msg.cookies, String::new(), 0, msg.torrent_file_bytes, msg.proxy_url, msg.user_agent, msg.queue_id, msg.checksum, msg.extra_headers, msg.selected_file_indices, None, None, None)
                    .await;
                // 立即推送 AllTasks，确保 Dart 端在收到 TaskProgress 之前
                // 已通过 AllTasks 获得正确的 queue_id，防止新任务被错误归入默认队列。
                engine.manager.load_and_send_all_tasks().await;
            }
            Some(signal) = batch_create_recv.recv() => {
                let msg = signal.message;
                log_info!(
                    "[actor] batch create: {} entries, save_dir={}, segments={}",
                    msg.entries.len(), msg.save_dir, msg.segments,
                );
                for entry in msg.entries {
                    // 按 URL 消耗缓存的请求事务上下文，恢复 per-item 精度。
                    // 与单条确认分支的优先级方向【有意不同】：
                    //   • cookies：信号值优先（批量表单有共享 cookies 输入框，
                    //     用户手填 = 批级覆盖）、空则回退缓存的 per-item 值——
                    //     批量信号的 cookies 为空通常意味着"各条目 cookies 不一致
                    //     故未预填"，不是用户清空（对比单条路径：预填后清空 =
                    //     明确意图，故单条不回退）。
                    //   • referrer：缓存优先——批量表单【没有】referrer 输入框，
                    //     msg.referrer 只是首条请求的共享值，per-item 缓存更准。
                    //   • fileSize/method/body：仅存在于缓存。
                    let ctx = ext_request_cache.remove(&entry.url).unwrap_or_default();
                    let extra_headers = merge_ext_headers(ctx.headers, &msg.extra_headers);
                    let cookies = if msg.cookies.is_empty() { ctx.cookies } else { msg.cookies.clone() };
                    let referrer = if ctx.referrer.is_empty() { msg.referrer.clone() } else { ctx.referrer };
                    let body = ctx.body.map(nm_body_to_captured);
                    engine.manager
                        .create_task(entry.url, msg.save_dir.clone(), entry.file_name, msg.segments, cookies, referrer, ctx.file_size, Vec::new(), msg.proxy_url.clone(), msg.user_agent.clone(), msg.queue_id.clone(), entry.checksum, extra_headers, Vec::new(), ctx.method, body, if entry.audio_url.is_empty() { None } else { Some(entry.audio_url) })
                        .await;
                }
                // 批量创建完成后统一推送一次 AllTasks，同步 queue_id 到 Dart。
                engine.manager.load_and_send_all_tasks().await;
            }
            Some(signal) = control_recv.recv() => {
                let msg = signal.message;
                match msg.action {
                    0 => engine.manager.pause_task(&msg.task_id).await,
                    1 => engine.manager.resume_task(&msg.task_id).await,
                    2 => engine.manager.cancel_task(&msg.task_id).await,
                    3 => {
                        // delete record + files
                        engine.manager.delete_task(&msg.task_id, true).await;
                        rinf_sink.broadcast_task_stop(&msg.task_id);
                    }
                    4 => {
                        // delete record only
                        engine.manager.delete_task(&msg.task_id, false).await;
                        rinf_sink.broadcast_task_stop(&msg.task_id);
                    }
                    _ => {}
                }
            }
            Some(signal) = batch_control_recv.recv() => {
                let msg = signal.message;
                log_info!(
                    "[actor] batch control: {} tasks, action={}",
                    msg.task_ids.len(), msg.action,
                );
                match msg.action {
                    0 => engine.manager.batch_pause(&msg.task_ids).await,
                    1 => engine.manager.batch_resume(&msg.task_ids).await,
                    3 => {
                        engine.manager.delete_tasks_batch(&msg.task_ids, true).await;
                        for task_id in &msg.task_ids {
                            rinf_sink.broadcast_task_stop(task_id);
                        }
                    }
                    4 => {
                        engine.manager.delete_tasks_batch(&msg.task_ids, false).await;
                        for task_id in &msg.task_ids {
                            rinf_sink.broadcast_task_stop(task_id);
                        }
                    }
                    _ => {}
                }
            }
            Some(_) = all_recv.recv() => {
                engine.manager.load_and_send_all_tasks().await;
                // Also send queue list so Dart sidebar can show named queues.
                engine.manager.send_all_queues().await;
            }
            Some(_) = rescan_recv.recv() => {
                // 文件跟踪：桌面窗口聚焦 / 移动端回前台 → 重扫已完成任务文件是否仍在。
                engine.manager.spawn_file_scan();
            }
            Some(signal) = config_save_recv.recv() => {
                let msg = signal.message;
                // Persist to DB first.
                if let Err(e) = engine.db.set_config(&msg.key, &msg.value).await {
                    log_info!("Failed to save config: {}", e);
                }
                // Notify DownloadManager for runtime-effective settings.
                apply_config_key(
                    &mut engine,
                    &msg.key,
                    &msg.value,
                    &tracker_sub_tx,
                    &ed2k_sub_tx,
                    &api_host,
                    &mut api_server_handle,
                )
                .await;
            }
            Some(_) = config_req_recv.recv() => {
                match engine.db.get_all_config().await {
                    Ok(map) => {
                        let entries: Vec<ConfigEntry> = map
                            .into_iter()
                            .map(|(key, value)| ConfigEntry { key, value })
                            .collect();
                        ConfigLoaded { entries }.send_signal_to_dart();
                    }
                    Err(e) => {
                        log_info!("Failed to load config: {}", e);
                    }
                }
            }
            // --- 管理 API 写命令（本机 API 服务器 /api/v1，见 api_host.rs）---
            Some(cmd) = api_cmd_rx.recv() => {
                handle_api_command(
                    cmd,
                    &mut engine,
                    &tracker_sub_tx,
                    &ed2k_sub_tx,
                    &api_host,
                    &mut api_server_handle,
                    &rinf_sink,
                )
                .await;
            }
            // --- Native Messaging: browser extension download requests ---
            // 单条请求包成 1 元素 Vec；NMH `batch_download` 一条消息携带整批。
            Some(mut reqs) = native_msg_rx.recv() => {
                log_info!(
                    "[actor] external download request from browser: {} item(s), first_url={}",
                    reqs.len(),
                    reqs.first().map(|r| r.url.as_str()).unwrap_or(""),
                );
                // 缓存每条的请求事务上下文（headers/method/body/cookies/referrer/
                // fileSize）——任一字段有意义即缓存。长会话防累积：FIFO 淘汰
                // 最旧插入（缓存插入条件放宽后几乎每条请求都会入表，旧的
                // 「超阈值整表清空」会把并存的未确认弹窗全部降级；按插入序
                // 淘汰只影响最旧的少数条目）。
                const MAX_REQ_CTX_CACHE: usize = 200;
                for req in &reqs {
                    let has_headers =
                        req.headers.as_ref().is_some_and(|h| !h.is_empty());
                    let file_size = req.file_size.unwrap_or(0);
                    if has_headers || req.method.is_some() || req.body.is_some()
                        || !req.cookies.is_empty() || !req.referrer.is_empty()
                        || file_size != 0
                    {
                        ext_request_cache.insert(
                            req.url.clone(),
                            ExtRequestCtx {
                                headers: req.headers.clone().unwrap_or_default(),
                                method: req.method.clone(),
                                body: req.body.clone(),
                                cookies: req.cookies.clone(),
                                referrer: req.referrer.clone(),
                                file_size,
                            },
                        );
                        ext_cache_order.push_back(req.url.clone());
                        // 懒淘汰：队头 key 可能已被确认消费（remove），跳过即可。
                        while ext_request_cache.len() > MAX_REQ_CTX_CACHE {
                            match ext_cache_order.pop_front() {
                                Some(old) => {
                                    ext_request_cache.remove(&old);
                                }
                                None => break,
                            }
                        }
                    }
                }
                // 队列防膨胀：确认消费不回收队列条目，陈旧 key 堆积过多时压实。
                if ext_cache_order.len() > MAX_REQ_CTX_CACHE * 4 {
                    ext_cache_order.retain(|k| ext_request_cache.contains_key(k));
                }
                // Forward to Dart UI so it can pop the quick-download dialog.
                if reqs.len() == 1 {
                    if let Some(req) = reqs.pop() {
                        ExternalDownloadRequest {
                            url: req.url,
                            filename: req.filename,
                            save_dir: req.save_dir,
                            referrer: req.referrer,
                            file_size: req.file_size.unwrap_or(0),
                            mime_type: req.mime_type.unwrap_or_default(),
                            cookies: req.cookies,
                            audio_url: req.audio_url.unwrap_or_default(),
                        }
                        .send_signal_to_dart();
                    }
                } else if !reqs.is_empty() {
                    synthesize_batch_request(&reqs).send_signal_to_dart();
                }
            }
            // --- Dart confirmed an external download request ---
            Some(signal) = confirm_ext_recv.recv() => {
                let msg = signal.message;
                // 取出缓存的请求事务上下文（headers/method/body + per-item
                // cookies/referrer/fileSize）。命中即用、未命中按默认值（GET、
                // 无 body）继续——后者保留向后兼容旧版扩展的下载路径。
                let ctx = ext_request_cache.remove(&msg.url).unwrap_or_default();
                // 浏览器捕获的请求头打底，用户手填的同名（忽略大小写）覆盖。
                let extra_headers = merge_ext_headers(ctx.headers, &msg.extra_headers);
                // cookies/referrer 用信号值【直用、不回退缓存】：单条确认路径的
                // 信号本就携带 per-item 值（Dart round-trip 预填表单），字段为空
                // = 用户在表单里主动清空，必须尊重该意图（与改动前语义一致）。
                // hint_file_size 例外：表单不可编辑该值，0 只可能是"信号未携带"
                // （批量弹窗缩减为单条确认的路径），回退缓存恢复 per-item 精度。
                let cookies = msg.cookies;
                let referrer = msg.referrer;
                let hint_file_size = if msg.hint_file_size == 0 { ctx.file_size } else { msg.hint_file_size };
                let method = ctx.method;
                let body = ctx.body.map(nm_body_to_captured);
                log_info!(
                    "[actor] user confirmed external download: url={}, cookies_len={}, extra_headers={}, method={:?}, has_body={}",
                    msg.url,
                    cookies.len(),
                    extra_headers.len(),
                    method,
                    body.is_some(),
                );
                engine.manager
                    .create_task(msg.url, msg.save_dir, msg.file_name, msg.segments, cookies, referrer, hint_file_size, Vec::new(), msg.proxy_url, msg.user_agent, msg.queue_id, String::new(), extra_headers, Vec::new(), method, body, if msg.audio_url.is_empty() { None } else { Some(msg.audio_url) })
                    .await;
                // 推送 AllTasks 确保 Dart 端获得正确 queue_id。
                engine.manager.load_and_send_all_tasks().await;
            }
            // --- Named queue management ---
            Some(signal) = create_queue_recv.recv() => {
                let msg = signal.message;
                log_info!("[actor] CreateQueue: name={}", msg.name);
                engine.manager.create_queue(msg.name, msg.speed_limit_kbps, msg.max_concurrent, msg.default_save_dir, msg.default_segments, msg.default_user_agent).await;
            }
            Some(signal) = update_queue_recv.recv() => {
                let msg = signal.message;
                log_info!("[actor] UpdateQueue: id={}", msg.queue_id);
                engine.manager.update_queue(msg.queue_id, msg.name, msg.speed_limit_kbps, msg.max_concurrent, msg.default_save_dir, msg.default_segments, msg.default_user_agent).await;
            }
            Some(signal) = delete_queue_recv.recv() => {
                let msg = signal.message;
                log_info!("[actor] DeleteQueue: id={}", msg.queue_id);
                engine.manager.delete_queue(msg.queue_id).await;
            }
            Some(signal) = move_task_queue_recv.recv() => {
                let msg = signal.message;
                log_info!("[actor] MoveTaskToQueue: task={}, queue={}", msg.task_id, msg.queue_id);
                engine.manager.move_task_to_queue(msg.task_id, msg.queue_id).await;
            }
            Some(_) = all_queues_recv.recv() => {
                engine.manager.send_all_queues().await;
            }
            Some(done) = done_rx.recv() => {
                engine.manager.on_task_done(&done).await;
            }
            // --- Auto-retry for stalled/failed tasks ---
            Some(task_id) = retry_rx.recv() => {
                // 安全检查：仅在任务仍处于 error 状态时才自动恢复。
                // 如果用户已手动暂停、恢复或删除了该任务，跳过重试。
                if engine.manager.is_task_in_error(&task_id).await {
                    log_info!("[actor] auto-retry: resuming task {}", task_id);
                    // 使用 resume_task_auto 而非 resume_task：不重置自动重试计数，
                    // 使 on_task_done 中的累积计数能正确递增并最终触发重试上限。
                    engine.manager.resume_task_auto(&task_id).await;
                } else {
                    log_info!("[actor] auto-retry: skipping task {} (no longer in error state)", task_id);
                }
            }
            // --- Auto-update signals ---
            Some(signal) = check_update_recv.recv() => {
                let version = signal.message.current_version;
                tokio::spawn(async move {
                    let result = std::panic::AssertUnwindSafe(
                        updater::check(&version)
                    );
                    if futures_util::FutureExt::catch_unwind(result).await.is_err() {
                        log_info!("[updater] check panicked for version={}", version);
                        UpdateCheckResult {
                            has_update: false,
                            latest_version: String::new(),
                            current_version: version,
                            download_url: String::new(),
                            file_size: 0,
                            published_at: String::new(),
                            error_message: "internal error (panic)".to_string(),
                        }.send_signal_to_dart();
                    }
                });
            }
            Some(signal) = download_update_recv.recv() => {
                let url = signal.message.url;
                let version = signal.message.version;
                let file_size = signal.message.file_size;
                tokio::spawn(async move {
                    updater::download(&url, &version, file_size).await;
                });
            }
            Some(signal) = install_update_recv.recv() => {
                let path = signal.message.installer_path;
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = updater::install(&path) {
                        log_info!("[updater] install error: {}", e);
                        // Report the error back to the UI so the user can retry
                        // (e.g. they cancelled the pkexec password dialog).
                        crate::signals::UpdateDownloadProgress {
                            version: String::new(),
                            downloaded_bytes: 0,
                            total_bytes: 0,
                            speed: 0,
                            status: 2, // error
                            installer_path: path,
                            error_message: e.to_string(),
                            segments: 0,
                            active_segments: 0,
                        }
                        .send_signal_to_dart();
                    }
                });
            }
            Some(_signal) = req_update_marker_recv.recv() => {
                // Dart asks (once on startup) whether a previous portable update
                // failed. Reading the marker is quick file I/O; do it on a
                // blocking thread to keep the current-thread runtime responsive.
                let message = tokio::task::spawn_blocking(updater::check_failure_marker)
                    .await
                    .unwrap_or(None);
                if let Some(msg) = message {
                    log_info!("[updater] reporting pending failure marker to UI");
                    UpdateFailureMarker { message: msg }.send_signal_to_dart();
                } else {
                    UpdateFailureMarker { message: String::new() }.send_signal_to_dart();
                }
            }
            // --- File association signals ---
            Some(signal) = set_file_assoc_recv.recv() => {
                let enable = signal.message.enable;
                tokio::task::spawn_blocking(move || {
                    log_info!("[actor] set_file_association enable={}", enable);
                    let result = if enable {
                        file_association::associate()
                    } else {
                        file_association::disassociate()
                    };
                    if let Err(e) = result {
                        log_info!("[actor] file association error: {}", e);
                    }
                    // Report current status back to Dart
                    FileAssociationStatus {
                        is_associated: file_association::is_associated(),
                    }
                    .send_signal_to_dart();
                });
            }
            Some(_) = check_file_assoc_recv.recv() => {
                tokio::task::spawn_blocking(|| {
                    FileAssociationStatus {
                        is_associated: file_association::is_associated(),
                    }
                    .send_signal_to_dart();
                });
            }
            // --- URL protocol signals ---
            Some(signal) = set_url_proto_recv.recv() => {
                let enable = signal.message.enable;
                tokio::task::spawn_blocking(move || {
                    log_info!("[actor] set_url_protocol enable={}", enable);
                    let result = if enable {
                        protocol_registry::register()
                    } else {
                        protocol_registry::unregister()
                    };
                    if let Err(e) = result {
                        log_info!("[actor] url protocol error: {}", e);
                    }
                    UrlProtocolStatus {
                        is_registered: protocol_registry::is_registered(),
                    }
                    .send_signal_to_dart();
                });
            }
            Some(_) = check_url_proto_recv.recv() => {
                tokio::task::spawn_blocking(|| {
                    UrlProtocolStatus {
                        is_registered: protocol_registry::is_registered(),
                    }
                    .send_signal_to_dart();
                });
            }
            // --- System proxy detection ---
            Some(_) = detect_sys_proxy_recv.recv() => {
                tokio::task::spawn_blocking(|| {
                    match fluxdown_engine::proxy_config::detect_system_proxy() {
                        Ok(Some(cfg)) => {
                            SystemProxyInfo {
                                detected: true,
                                proxy_type: cfg.proxy_type.as_str().to_owned(),
                                host: cfg.host,
                                port: cfg.port.to_string(),
                                no_proxy_list: cfg.no_proxy_list,
                            }.send_signal_to_dart();
                        }
                        Ok(None) => {
                            SystemProxyInfo {
                                detected: false,
                                proxy_type: String::new(),
                                host: String::new(),
                                port: String::new(),
                                no_proxy_list: String::new(),
                            }.send_signal_to_dart();
                        }
                        Err(e) => {
                            log_info!("[actor] system proxy detection error: {}", e);
                            SystemProxyInfo {
                                detected: false,
                                proxy_type: String::new(),
                                host: String::new(),
                                port: String::new(),
                                no_proxy_list: String::new(),
                            }.send_signal_to_dart();
                        }
                    }
                });
            }
            // --- HLS quality selection ---
            Some(signal) = select_hls_quality_recv.recv() => {
                let msg = signal.message;
                log_info!(
                    "[actor] HLS quality selected: task={}, index={}",
                    msg.task_id,
                    msg.selected_index,
                );
                engine.selector.provide_hls_selection(&msg.task_id, msg.selected_index);
            }
            // --- Plugin resolve variant selection ---
            Some(signal) = select_resolve_variant_recv.recv() => {
                let msg = signal.message;
                log_info!(
                    "[actor] resolve variant selected: task={}, index={}",
                    msg.task_id,
                    msg.selected_index,
                );
                engine.selector.provide_variant_selection(&msg.task_id, msg.selected_index);
            }
            // --- Priority (Boost) download ---
            Some(signal) = set_priority_recv.recv() => {
                let task_id = signal.message.task_id;
                log_info!("[actor] SetPriorityTask: task_id={}", task_id);
                engine.manager.set_priority_task(task_id).await;
            }
            // --- Proxy connectivity test ---
            Some(signal) = test_proxy_recv.recv() => {
                let msg = signal.message;
                log_info!(
                    "[actor] proxy test: type={}, host={}, port={}",
                    msg.proxy_type, msg.proxy_host, msg.proxy_port,
                );
                // `Engine::test_proxy_connection` 内部就是纯 async I/O(reqwest),
                // 本身从不阻塞 current_thread runtime,无需外部 tokio::spawn 隔离。
                let result = engine.test_proxy_connection(
                    &msg.proxy_type,
                    &msg.proxy_host,
                    &msg.proxy_port,
                    &msg.proxy_username,
                    &msg.proxy_password,
                ).await;
                match result {
                    Ok(latency_ms) => {
                        ProxyTestResult {
                            success: true,
                            latency_ms,
                            error_message: String::new(),
                        }.send_signal_to_dart();
                    }
                    Err(e) => {
                        ProxyTestResult {
                            success: false,
                            latency_ms: 0,
                            error_message: e.to_string(),
                        }.send_signal_to_dart();
                    }
                }
            }
            // --- BT file selection ---
            Some(signal) = select_bt_files_recv.recv() => {
                let msg = signal.message;
                log_info!(
                    "[actor] SelectBtFiles: task={}, selected={:?}",
                    msg.task_id,
                    msg.selected_indices,
                );
                engine.selector.provide_bt_selection(&msg.task_id, msg.selected_indices);
            }
            // --- Reveal file in native file manager ---
            Some(signal) = reveal_file_recv.recv() => {
                let path = signal.message.path;
                // 从 DB 读用户自定义文件管理器命令模板（空 = 用平台默认）。
                // 在 spawn_blocking 之前异步读取，避免阻塞 actor 主循环；
                // get_config 失败按空模板处理，让平台默认兜底。
                let tpl = engine.db
                    .get_config("reveal_file_cmd")
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                tokio::task::spawn_blocking(move || {
                    crate::reveal_file::reveal(&path, &tpl);
                });
            }
            // --- Open file with default application ---
            Some(signal) = open_file_recv.recv() => {
                let path = signal.message.path;
                tokio::task::spawn_blocking(move || {
                    crate::reveal_file::open_file(&path);
                });
            }
            // --- Torrent meta probe (for new-download dialog preview) ---
            Some(signal) = probe_torrent_meta_recv.recv() => {
                let msg = signal.message;
                log_info!(
                    "[actor] ProbeTorrentMeta: probe_id={}, bytes={}",
                    msg.probe_id,
                    msg.torrent_bytes.len(),
                );
                // `Engine::probe_torrent_meta` 内部 spawn_blocking,不阻塞
                // current_thread runtime;纯本地解析(无网络),延迟可忽略。
                let result = engine.probe_torrent_meta(msg.probe_id, msg.torrent_bytes).await;
                crate::signals::TorrentMetaResult::from(result).send_signal_to_dart();
            }
            // --- Manual tracker subscription refresh (Settings page button) ---
            Some(_) = update_tracker_sub_recv.recv() => {
                log_info!("[actor] manual tracker subscription refresh requested");
                spawn_tracker_sub_refresh(engine.db.clone(), tracker_sub_tx.clone());
            }
            // --- Tracker subscription refresh finished ---
            Some(outcome) = tracker_sub_rx.recv() => {
                let all_cfg = engine.db.get_all_config().await.unwrap_or_default();
                if outcome.is_success() {
                    // 缓存已由后台任务写入 DB；重载 BtConfig 并失效会话，
                    // 使下一个 BT 任务用上最新的合并 tracker 列表。
                    engine.manager.set_bt_config(bt_config_from_map(&all_cfg));
                    engine.manager.invalidate_bt_session().await;
                }
                let updated_at = all_cfg
                    .get("bt_tracker_sub_updated_at")
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0);
                TrackerSubscriptionResult {
                    success: outcome.is_success(),
                    tracker_count: outcome.trackers.len() as i32,
                    ok_sources: outcome.ok_sources as i32,
                    total_sources: outcome.total_sources as i32,
                    updated_at,
                    error: outcome.error,
                }
                .send_signal_to_dart();
            }
            // --- Manual ED2K server subscription refresh (Settings page button) ---
            Some(_) = update_ed2k_sub_recv.recv() => {
                log_info!("[actor] manual ed2k server subscription refresh requested");
                spawn_ed2k_server_sub_refresh(engine.db.clone(), ed2k_sub_tx.clone());
            }
            // --- ED2K server subscription refresh finished ---
            Some(outcome) = ed2k_sub_rx.recv() => {
                let all_cfg = engine.db.get_all_config().await.unwrap_or_default();
                let updated_at = all_cfg
                    .get("ed2k_server_sub_updated_at")
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0);
                Ed2kServerSubscriptionResult {
                    success: outcome.is_success(),
                    server_count: outcome.servers.len() as i32,
                    ok_sources: outcome.ok_sources as i32,
                    total_sources: outcome.total_sources as i32,
                    updated_at,
                    error: outcome.error,
                }
                .send_signal_to_dart();
            }
            // --- Plugin system (see plugin-system contract §hub 3) ---
            Some(_) = req_plugins_recv.recv() => {
                if let Some(pm) = engine.manager.plugin_manager() {
                    send_plugin_list(&pm).await;
                } else {
                    PluginList { plugins: Vec::new() }.send_signal_to_dart();
                }
            }
            Some(signal) = install_plugin_recv.recv() => {
                let msg = signal.message;
                if let Some(pm) = engine.manager.plugin_manager() {
                    // 分发规则：dev_mode → install_dev；否则优先 zip 字节，
                    // 再退回目录路径；三者皆空 → 直接失败。
                    let result: Result<String, PluginError> = if msg.dev_mode {
                        pm.install_dev(Path::new(&msg.dir_path)).await
                    } else if !msg.zip_bytes.is_empty() {
                        pm.install_from_zip(msg.zip_bytes).await
                    } else if !msg.dir_path.is_empty() {
                        pm.install_from_dir(Path::new(&msg.dir_path)).await
                    } else {
                        Err(PluginError::ManifestInvalid(
                            "未提供插件 zip 字节或目录路径".to_string(),
                        ))
                    };
                    match result {
                        Ok(identity) => {
                            let missing = plugin_missing_components(&pm, &engine.db, &engine.data_dir, &identity).await;
                            finish_plugin_op(&pm, "install", &identity, Ok(()), missing).await;
                        }
                        Err(e) => finish_plugin_op(&pm, "install", "", Err(e), Vec::new()).await,
                    }
                } else {
                    notify_plugin_manager_unavailable("install", "").await;
                }
            }
            Some(signal) = uninstall_plugin_recv.recv() => {
                let msg = signal.message;
                if let Some(pm) = engine.manager.plugin_manager() {
                    let result = pm.uninstall(&msg.identity).await;
                    finish_plugin_op(&pm, "uninstall", &msg.identity, result, Vec::new()).await;
                } else {
                    notify_plugin_manager_unavailable("uninstall", &msg.identity).await;
                }
            }
            Some(signal) = set_plugin_enabled_recv.recv() => {
                let msg = signal.message;
                if let Some(pm) = engine.manager.plugin_manager() {
                    let result = pm.set_enabled(&msg.identity, msg.enabled).await;
                    finish_plugin_op(&pm, "set_enabled", &msg.identity, result, Vec::new()).await;
                } else {
                    notify_plugin_manager_unavailable("set_enabled", &msg.identity).await;
                }
            }
            Some(signal) = save_plugin_settings_recv.recv() => {
                let msg = signal.message;
                if let Some(pm) = engine.manager.plugin_manager() {
                    let entries: Vec<(String, String)> = msg
                        .entries
                        .into_iter()
                        .map(|e| (e.key, e.value))
                        .collect();
                    let result = pm.update_settings(&msg.identity, &entries).await;
                    finish_plugin_op(&pm, "save_settings", &msg.identity, result, Vec::new()).await;
                } else {
                    notify_plugin_manager_unavailable("save_settings", &msg.identity).await;
                }
            }
            // --- 逃生舱：清任务 resolver 绑定 + 按原始链接恢复(见插件系统契约一) ---
            Some(signal) = ignore_plugin_retry_recv.recv() => {
                let msg = signal.message;
                if let Some(pm) = engine.manager.plugin_manager() {
                    pm.clear_task_resolver(&msg.task_id).await;
                }
                engine.manager.resume_task(&msg.task_id).await;
            }
            // --- 去中心化插件市场（见市场契约）：fetch/install 是网络 I/O
            // （单源最长 20s），严禁在本 select! 分支内 await —— 会冻结整条
            // 命令面。分支内只做快速的 market_client() 构造（仅读 Db），真正
            // 的网络请求丢进 off-actor tokio::spawn，完成后直接在该任务里
            // send_signal_to_dart()（RustSignal 可从任意任务发送）。---
            Some(_) = request_market_index_recv.recv() => {
                match engine.manager.market_client().await {
                    Some(client) => {
                        tokio::spawn(async move {
                            match client.fetch_index().await {
                                Ok(idx) => {
                                    let entries = idx
                                        .entries
                                        .into_iter()
                                        .map(MarketEntrySignal::from)
                                        .collect();
                                    MarketIndexLoaded {
                                        ok: true,
                                        message: String::new(),
                                        entries,
                                    }
                                    .send_signal_to_dart();
                                }
                                Err(e) => {
                                    MarketIndexLoaded {
                                        ok: false,
                                        message: e.to_string(),
                                        entries: Vec::new(),
                                    }
                                    .send_signal_to_dart();
                                }
                            }
                        });
                    }
                    None => {
                        MarketIndexLoaded {
                            ok: false,
                            message: "插件系统未启用".to_string(),
                            entries: Vec::new(),
                        }
                        .send_signal_to_dart();
                    }
                }
            }
            Some(signal) = install_market_plugin_recv.recv() => {
                let plugin_id = signal.message.plugin_id;
                match engine.manager.market_client().await {
                    Some(client) => {
                        let plugin_manager = engine.manager.plugin_manager();
                        let db = engine.db.clone();
                        let data_dir = engine.data_dir.clone();
                        tokio::spawn(async move {
                            let result = client.install_latest(&plugin_id).await;
                            // identity 字段回填 plugin_id 供失败时 Dart 按市场条目
                            // 定位；成功时用引擎分配的真实本地 identity（供后续
                            // 启停/卸载/设置操作使用)。
                            let (ok, identity, message) = match result {
                                Ok(identity) => (true, identity, String::new()),
                                Err(e) => (false, plugin_id.clone(), e.to_string()),
                            };
                            // 安装成功后按声明权限探测缺失的基础组件（提醒式）。
                            let missing = match plugin_manager.as_ref() {
                                Some(pm) if ok => {
                                    plugin_missing_components(pm, &db, &data_dir, &identity).await
                                }
                                _ => Vec::new(),
                            };
                            PluginOpResult {
                                op: "market_install".to_string(),
                                identity,
                                ok,
                                message,
                                failed_key: String::new(),
                                missing_components: missing,
                            }
                            .send_signal_to_dart();
                            match plugin_manager {
                                Some(pm) => send_plugin_list(&pm).await,
                                None => PluginList { plugins: Vec::new() }.send_signal_to_dart(),
                            }
                        });
                    }
                    None => {
                        notify_plugin_manager_unavailable("market_install", &plugin_id).await;
                    }
                }
            }
            // --- ffmpeg 组件管理（v1，见组件契约）：状态探测是本地进程
            // 调用，快，直接分支内 await；版本列表/安装是网络 I/O（GitHub
            // Release API + 下载归档，单个可达数十 MB），严禁在本 select!
            // 分支内 await —— 会冻结整条命令面。分支内只做快速的 proxy
            // client 构造（读 Db config，无网络），真正的网络请求丢进
            // off-actor tokio::spawn，完成后直接在该任务里
            // send_signal_to_dart()（RustSignal 可从任意任务发送）。---
            Some(_) = req_ffmpeg_status_recv.recv() => {
                let status =
                    fluxdown_engine::components::ffmpeg_status(&engine.db, &engine.data_dir).await;
                ffmpeg_status_report(status).send_signal_to_dart();
            }
            Some(_) = req_ffmpeg_versions_recv.recv() => {
                let all_cfg = engine.db.get_all_config().await.unwrap_or_default();
                let proxy_cfg = ProxyConfig::from_config_map(&all_cfg);
                match fluxdown_engine::downloader::build_client(&proxy_cfg, "") {
                    Ok(client) => {
                        tokio::spawn(async move {
                            match fluxdown_engine::components::list_versions(&client).await {
                                Ok(v) => {
                                    FfmpegVersionList {
                                        ok: true,
                                        message: String::new(),
                                        versions: v.versions,
                                        latest_stable: v.latest_stable,
                                    }
                                    .send_signal_to_dart();
                                }
                                Err(e) => {
                                    FfmpegVersionList {
                                        ok: false,
                                        message: e.to_string(),
                                        versions: Vec::new(),
                                        latest_stable: String::new(),
                                    }
                                    .send_signal_to_dart();
                                }
                            }
                        });
                    }
                    Err(e) => {
                        FfmpegVersionList {
                            ok: false,
                            message: e.to_string(),
                            versions: Vec::new(),
                            latest_stable: String::new(),
                        }
                        .send_signal_to_dart();
                    }
                }
            }
            Some(signal) = install_ffmpeg_recv.recv() => {
                let version = signal.message.version;
                let all_cfg = engine.db.get_all_config().await.unwrap_or_default();
                let proxy_cfg = ProxyConfig::from_config_map(&all_cfg);
                match fluxdown_engine::downloader::build_client(&proxy_cfg, "") {
                    Ok(client) => {
                        let db = engine.db.clone();
                        let data_dir = engine.data_dir.clone();
                        tokio::spawn(async move {
                            let version_opt =
                                if version.is_empty() { None } else { Some(version.as_str()) };
                            // 引擎已按 ~256KB 步进节流回调，无需在此额外去抖。
                            let progress = |downloaded: u64, total: u64| {
                                FfmpegInstallProgress {
                                    downloaded_bytes: i64::try_from(downloaded).unwrap_or(i64::MAX),
                                    total_bytes: i64::try_from(total).unwrap_or(i64::MAX),
                                }
                                .send_signal_to_dart();
                            };
                            let result = fluxdown_engine::components::install_ffmpeg(
                                &db,
                                &data_dir,
                                &client,
                                version_opt,
                                &progress,
                            )
                            .await;
                            match result {
                                Ok(_) => {
                                    FfmpegInstallResult { ok: true, message: String::new() }
                                        .send_signal_to_dart();
                                }
                                Err(e) => {
                                    FfmpegInstallResult { ok: false, message: e.to_string() }
                                        .send_signal_to_dart();
                                }
                            }
                            // 无论成败都重新探测一次：安装失败时 UI 需要看到
                            // 回退状态（如此前已有的托管版本仍然生效）。
                            let status =
                                fluxdown_engine::components::ffmpeg_status(&db, &data_dir).await;
                            ffmpeg_status_report(status).send_signal_to_dart();
                        });
                    }
                    Err(e) => {
                        FfmpegInstallResult { ok: false, message: e.to_string() }
                            .send_signal_to_dart();
                    }
                }
            }
            Some(_) = uninstall_ffmpeg_recv.recv() => {
                let result =
                    fluxdown_engine::components::uninstall_ffmpeg(&engine.db, &engine.data_dir)
                        .await;
                match result {
                    Ok(()) => {
                        FfmpegInstallResult { ok: true, message: String::new() }
                            .send_signal_to_dart();
                    }
                    Err(e) => {
                        FfmpegInstallResult { ok: false, message: e.to_string() }
                            .send_signal_to_dart();
                    }
                }
                let status =
                    fluxdown_engine::components::ffmpeg_status(&engine.db, &engine.data_dir).await;
                ffmpeg_status_report(status).send_signal_to_dart();
            }
            // --- yt-dlp 组件管理：与 ffmpeg 同构（状态本地探测直 await；版本
            // 列表/安装是网络 I/O，丢 off-actor tokio::spawn，完成后从任务内
            // send_signal_to_dart）。---
            Some(_) = req_ytdlp_status_recv.recv() => {
                let status =
                    fluxdown_engine::components::ytdlp_status(&engine.db, &engine.data_dir).await;
                ytdlp_status_report(status).send_signal_to_dart();
            }
            Some(_) = req_ytdlp_versions_recv.recv() => {
                let all_cfg = engine.db.get_all_config().await.unwrap_or_default();
                let proxy_cfg = ProxyConfig::from_config_map(&all_cfg);
                match fluxdown_engine::downloader::build_client(&proxy_cfg, "") {
                    Ok(client) => {
                        tokio::spawn(async move {
                            match fluxdown_engine::components::list_ytdlp_versions(&client).await {
                                Ok(v) => {
                                    YtdlpVersionList {
                                        ok: true,
                                        message: String::new(),
                                        versions: v.versions,
                                        latest_stable: v.latest_stable,
                                    }
                                    .send_signal_to_dart();
                                }
                                Err(e) => {
                                    YtdlpVersionList {
                                        ok: false,
                                        message: e.to_string(),
                                        versions: Vec::new(),
                                        latest_stable: String::new(),
                                    }
                                    .send_signal_to_dart();
                                }
                            }
                        });
                    }
                    Err(e) => {
                        YtdlpVersionList {
                            ok: false,
                            message: e.to_string(),
                            versions: Vec::new(),
                            latest_stable: String::new(),
                        }
                        .send_signal_to_dart();
                    }
                }
            }
            Some(signal) = install_ytdlp_recv.recv() => {
                let version = signal.message.version;
                let all_cfg = engine.db.get_all_config().await.unwrap_or_default();
                let proxy_cfg = ProxyConfig::from_config_map(&all_cfg);
                match fluxdown_engine::downloader::build_client(&proxy_cfg, "") {
                    Ok(client) => {
                        let db = engine.db.clone();
                        let data_dir = engine.data_dir.clone();
                        tokio::spawn(async move {
                            let version_opt =
                                if version.is_empty() { None } else { Some(version.as_str()) };
                            let progress = |downloaded: u64, total: u64| {
                                YtdlpInstallProgress {
                                    downloaded_bytes: i64::try_from(downloaded).unwrap_or(i64::MAX),
                                    total_bytes: i64::try_from(total).unwrap_or(i64::MAX),
                                }
                                .send_signal_to_dart();
                            };
                            let result = fluxdown_engine::components::install_ytdlp(
                                &db,
                                &data_dir,
                                &client,
                                version_opt,
                                &progress,
                            )
                            .await;
                            match result {
                                Ok(_) => {
                                    YtdlpInstallResult { ok: true, message: String::new() }
                                        .send_signal_to_dart();
                                }
                                Err(e) => {
                                    YtdlpInstallResult { ok: false, message: e.to_string() }
                                        .send_signal_to_dart();
                                }
                            }
                            let status =
                                fluxdown_engine::components::ytdlp_status(&db, &data_dir).await;
                            ytdlp_status_report(status).send_signal_to_dart();
                        });
                    }
                    Err(e) => {
                        YtdlpInstallResult { ok: false, message: e.to_string() }
                            .send_signal_to_dart();
                    }
                }
            }
            Some(_) = uninstall_ytdlp_recv.recv() => {
                let result =
                    fluxdown_engine::components::uninstall_ytdlp(&engine.db, &engine.data_dir)
                        .await;
                match result {
                    Ok(()) => {
                        YtdlpInstallResult { ok: true, message: String::new() }
                            .send_signal_to_dart();
                    }
                    Err(e) => {
                        YtdlpInstallResult { ok: false, message: e.to_string() }
                            .send_signal_to_dart();
                    }
                }
                let status =
                    fluxdown_engine::components::ytdlp_status(&engine.db, &engine.data_dir).await;
                ytdlp_status_report(status).send_signal_to_dart();
            }
            // --- Off-actor plugin resolve 回流(见插件系统契约一，关键：不接线
            // 会导致该宿主下 off-actor resolve 永不完成，下载卡死) ---
            Some(out) = resolve_rx.recv() => {
                engine.manager.on_resolve_ready(out).await;
            }
            Some((tid, delay)) = plugin_retry_rx.recv() => {
                engine.manager.plugin_request_retry(&tid, delay).await;
            }
        }
    }
}

/// 刷新插件列表并回发 `PluginList`（安装/卸载/开关/存设置后统一调用）。
async fn send_plugin_list(plugin_manager: &PluginManager) {
    let plugins = plugin_manager
        .list()
        .await
        .into_iter()
        .map(Into::into)
        .collect();
    PluginList { plugins }.send_signal_to_dart();
}

/// 插件写操作统一收尾：回发 `PluginOpResult` + 刷新后的 `PluginList`
/// （见插件系统契约 hub 节 3：「每次操作后回发 PluginList + PluginOpResult」）。
/// `failed_key` 恒为空——`fluxdown_engine::plugin::PluginError` 未暴露结构化
/// 键名，仅 `message` 携带完整错误文本（含出错的设置项键名）。
async fn finish_plugin_op(
    plugin_manager: &PluginManager,
    op: &str,
    identity: &str,
    result: Result<(), PluginError>,
    missing_components: Vec<String>,
) {
    let (ok, message) = match result {
        Ok(()) => (true, String::new()),
        Err(e) => (false, e.to_string()),
    };
    PluginOpResult {
        op: op.to_string(),
        identity: identity.to_string(),
        ok,
        message,
        failed_key: String::new(),
        missing_components,
    }
    .send_signal_to_dart();
    send_plugin_list(plugin_manager).await;
}

/// 按插件声明权限探测缺失的基础组件（安装成功后调用，提醒式非阻断）。
/// 依赖表见 `fluxdown_engine::plugin::dependencies`。
async fn plugin_missing_components(
    plugin_manager: &PluginManager,
    db: &fluxdown_engine::db::Db,
    data_dir: &Path,
    identity: &str,
) -> Vec<String> {
    let perms = plugin_manager.permissions_of(identity).await;
    fluxdown_engine::plugin::dependencies::missing_components(db, data_dir, &perms).await
}

/// `plugin_manager()` 返回 `None`（理论上不应发生，`Engine::new` 恒注入）时
/// 的兜底回执：回发失败结果 + 空插件表，而非静默丢弃信号。
async fn notify_plugin_manager_unavailable(op: &str, identity: &str) {
    PluginOpResult {
        op: op.to_string(),
        identity: identity.to_string(),
        ok: false,
        message: "插件系统未启用".to_string(),
        failed_key: String::new(),
        missing_components: Vec::new(),
    }
    .send_signal_to_dart();
    PluginList {
        plugins: Vec::new(),
    }
    .send_signal_to_dart();
}

/// `fluxdown_engine::components::FfmpegStatus` → `FfmpegStatusReport` 信号。
/// `source` 走 `FfmpegSource::as_str()` 保持与 server/web 端一致的 wire 字符串。
fn ffmpeg_status_report(status: fluxdown_engine::components::FfmpegStatus) -> FfmpegStatusReport {
    FfmpegStatusReport {
        source: status.source.as_str().to_string(),
        path: status.path,
        version: status.version,
        managed_version: status.managed_version,
        system_path: status.system_path,
        managed_supported: status.managed_supported,
    }
}

/// `fluxdown_engine::components::YtdlpStatus` → `YtdlpStatusReport` 信号。
/// `source` 走 `ComponentSource::as_str()` 保持与 server/web 端一致的 wire 字符串。
fn ytdlp_status_report(status: fluxdown_engine::components::YtdlpStatus) -> YtdlpStatusReport {
    YtdlpStatusReport {
        source: status.source.as_str().to_string(),
        path: status.path,
        version: status.version,
        managed_version: status.managed_version,
        system_path: status.system_path,
        managed_supported: status.managed_supported,
    }
}

/// 处理管理 API 写命令：在 actor 事件循环内串行执行，完成后经 oneshot 回执。
/// 回执接收端掉线（HTTP 请求已中止）无影响，`send` 失败直接忽略。
///
/// `tracker_sub_tx`/`ed2k_sub_tx`/`api_host`/`api_server_handle`：仅
/// `ApplyConfig` 命令分支需要，透传给 [`apply_config_key`]（与 Dart
/// `SaveConfig` 信号分支共用同一套「键 → 引擎 setter」逻辑）。
/// `rinf_sink`:`DeleteTask` 分支需要,删除成功后广播 aria2 `Stop` 事件
/// 并从前态表移除条目(见 `RinfEventSink::broadcast_task_stop`)。
async fn handle_api_command(
    cmd: ApiCommand,
    engine: &mut Engine,
    tracker_sub_tx: &mpsc::Sender<fluxdown_engine::tracker_subscription::FetchOutcome>,
    ed2k_sub_tx: &mpsc::Sender<fluxdown_engine::ed2k::server_subscription::ServerFetchOutcome>,
    api_host: &Arc<dyn fluxdown_api::service::ApiHost>,
    api_server_handle: &mut ApiServerHandle,
    rinf_sink: &Arc<RinfEventSink>,
) {
    match cmd {
        ApiCommand::CreateTask { req, ack } => {
            let req = *req;
            // torrent_b64（aria2 addTorrent 兼容入口）非空时 base64 STANDARD
            // 解码为种子字节，优先于 url（参照 Dart 创建路径 :599，种子字节
            // 非空时 url 允许为空）。解码失败即请求参数非法；ack 类型仅
            // `Option<String>`，最接近 BadRequest 语义的表达是回 None 并记录日志。
            let torrent_file_bytes = match req.torrent_b64.as_deref() {
                Some(b64) => {
                    use base64::Engine as _;
                    match base64::engine::general_purpose::STANDARD.decode(b64) {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            log_info!("[actor] api create task: invalid torrent_b64: {}", e);
                            let _ = ack.send(None);
                            return;
                        }
                    }
                }
                None => Vec::new(),
            };
            // 空 save_dir → 全局默认目录（config 表）→ 平台默认下载目录。
            let mut save_dir = req.save_dir;
            if save_dir.trim().is_empty() {
                save_dir = engine
                    .db
                    .get_config("default_save_dir")
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
            }
            if save_dir.trim().is_empty() {
                save_dir = default_save_dir();
            }
            log_info!("[actor] api create task: url={}", req.url);
            let task_id = engine
                .manager
                .create_task(
                    req.url,
                    save_dir,
                    req.file_name,
                    req.segments,
                    req.cookies,
                    req.referrer,
                    0,
                    torrent_file_bytes,
                    req.proxy_url,
                    req.user_agent,
                    req.queue_id,
                    req.checksum,
                    req.headers.unwrap_or_default(),
                    Vec::new(),
                    req.method,
                    req.body.map(Into::into),
                    req.audio_url,
                )
                .await;
            // 与 Dart 创建路径一致：立即推送 AllTasks 同步 queue_id 到 UI。
            engine.manager.load_and_send_all_tasks().await;
            let _ = ack.send(task_id);
        }
        ApiCommand::PauseTask { task_id, ack } => {
            engine.manager.pause_task(&task_id).await;
            let _ = ack.send(());
        }
        ApiCommand::ContinueTask { task_id, ack } => {
            engine.manager.resume_task(&task_id).await;
            let _ = ack.send(());
        }
        ApiCommand::DeleteTask {
            task_id,
            delete_files,
            ack,
        } => {
            engine.manager.delete_task(&task_id, delete_files).await;
            rinf_sink.broadcast_task_stop(&task_id);
            let _ = ack.send(());
        }
        ApiCommand::PauseAll { ack } => {
            // pending(0) / downloading(1) / preparing(5) 均可暂停。
            let ids = task_ids_by_status(&engine.db, &[0, 1, 5]).await;
            engine.manager.batch_pause(&ids).await;
            let _ = ack.send(());
        }
        ApiCommand::ContinueAll { ack } => {
            // 仅恢复 paused(2)；error(4) 留给显式的单任务 continue。
            let ids = task_ids_by_status(&engine.db, &[2]).await;
            engine.manager.batch_resume(&ids).await;
            let _ = ack.send(());
        }
        ApiCommand::ApplyConfig { keys, ack } => {
            // 命令只带 keys：值已由 `HubApiHost::apply_config` 写入 DB，
            // 这里重新整表读取，与 server 侧 `ActorCmd::ApplyConfig` 语义一致。
            let all_cfg = engine.db.get_all_config().await.unwrap_or_default();
            for key in &keys {
                if let Some(value) = all_cfg.get(key) {
                    apply_config_key(
                        engine,
                        key,
                        value,
                        tracker_sub_tx,
                        ed2k_sub_tx,
                        api_host,
                        api_server_handle,
                    )
                    .await;
                }
            }
            let _ = ack.send(());
        }
    }
}

/// 把单个已持久化的配置键 live-apply 到运行中的引擎。是 Dart `SaveConfig`
/// 信号分支与管理 API `ApiCommand::ApplyConfig` 命令分支共用的核心逻辑
/// （单键粒度，行为与原内联 match 完全一致）；`local_server_*` 键触发本机
/// API 服务器优雅停机 + 用最新配置重新监听（热重启，不影响其余运行中任务）。
async fn apply_config_key(
    engine: &mut Engine,
    key: &str,
    value: &str,
    tracker_sub_tx: &mpsc::Sender<fluxdown_engine::tracker_subscription::FetchOutcome>,
    ed2k_sub_tx: &mpsc::Sender<fluxdown_engine::ed2k::server_subscription::ServerFetchOutcome>,
    api_host: &Arc<dyn fluxdown_api::service::ApiHost>,
    api_server_handle: &mut ApiServerHandle,
) {
    match key {
        "max_concurrent_tasks" => {
            if let Ok(v) = value.parse::<usize>() {
                log_info!("[actor] updating max_concurrent to {}", v);
                engine.manager.set_max_concurrent(v).await;
            }
        }
        "speed_limit_bytes" => {
            if let Ok(v) = value.parse::<u64>() {
                log_info!("[actor] updating speed_limit to {} B/s", v);
                engine.manager.set_speed_limit(v);
            }
        }
        "log_max_size_mb" => {
            if let Ok(mb) = value.parse::<u64>() {
                log_info!("[actor] updating log_max_size_mb to {}", mb);
                crate::logger::set_max_total_bytes(mb * 1024 * 1024);
            }
        }
        "default_save_dir" => {
            log_info!("[actor] updating default_save_dir to {}", value);
            engine.manager.set_default_save_dir(value.to_string());
        }
        // BT config keys — update in-memory BtConfig and invalidate
        // the current session so the next BT download picks up changes.
        "bt_enable_dht"
        | "bt_enable_upnp"
        | "bt_port_start"
        | "bt_port_end"
        | "bt_custom_trackers"
        | "bt_tracker_sub_enabled"
        | "bt_tracker_sub_urls" => {
            log_info!("[actor] BT config changed: {}={}", key, value);
            // Reload the full BT config from DB to stay consistent.
            let all_cfg = engine.db.get_all_config().await.unwrap_or_default();
            engine.manager.set_bt_config(bt_config_from_map(&all_cfg));
            // Invalidate (destroy) the current BT session so it is
            // re-created with the new config on next BT download.
            engine.manager.invalidate_bt_session().await;
            // 订阅地址变化 / 重新启用订阅 → 后台立即刷新一次。
            if key == "bt_tracker_sub_urls" || (key == "bt_tracker_sub_enabled" && value == "true")
            {
                spawn_tracker_sub_refresh(engine.db.clone(), tracker_sub_tx.clone());
            }
        }
        // ED2K 服务器订阅键：地址变化 / 重新启用 → 后台立即刷新一次。
        // 服务器列表在每次下载 find-sources 时新读，无需失效会话。
        "ed2k_server_sub_urls" | "ed2k_server_sub_enabled" => {
            log_info!("[actor] ED2K server sub config changed: {}={}", key, value);
            if key == "ed2k_server_sub_urls"
                || (key == "ed2k_server_sub_enabled" && value == "true")
            {
                spawn_ed2k_server_sub_refresh(engine.db.clone(), ed2k_sub_tx.clone());
            }
        }
        // Kad nodes.dat：URL 变化 / Kad 重新启用 → 后台立即刷新一次。
        "ed2k_nodes_dat_url" | "ed2k_enable_kad" => {
            log_info!("[actor] ED2K Kad config changed: {}={}", key, value);
            if key == "ed2k_nodes_dat_url" || (key == "ed2k_enable_kad" && value == "true") {
                spawn_ed2k_nodes_dat_refresh(engine.db.clone());
            }
        }
        // Proxy config keys — reload full proxy config from DB
        // and rebuild the HTTP client.
        "proxy_mode" | "proxy_type" | "proxy_host" | "proxy_port" | "proxy_username"
        | "proxy_password" | "proxy_no_list" => {
            log_info!("[actor] proxy config changed: {}={}", key, value);
            let all_cfg = engine.db.get_all_config().await.unwrap_or_default();
            let new_proxy = ProxyConfig::from_config_map(&all_cfg);
            if let Err(e) = engine.manager.set_proxy_config(new_proxy) {
                log_info!("[actor] failed to apply proxy config: {}", e);
            }
        }
        "global_user_agent" => {
            log_info!("[actor] user_agent changed: {}", value);
            if let Err(e) = engine.manager.set_user_agent(value.to_string()) {
                log_info!("[actor] failed to apply user_agent: {}", e);
            }
        }
        "default_segments" => {
            if let Ok(v) = value.parse::<i32>() {
                log_info!("[actor] updating default_segments to {}", v);
                engine.manager.set_default_segments(v);
            }
        }
        "auto_max_connections" => {
            if let Ok(v) = value.parse::<i32>() {
                log_info!("[actor] updating auto_max_connections to {}", v);
                engine.manager.set_auto_max_connections(v);
            }
        }
        "use_server_time" => {
            let v = value == "true";
            log_info!("[actor] updating use_server_time to {}", v);
            engine.manager.set_use_server_time(v);
        }
        "max_auto_retries" => {
            if let Ok(v) = value.parse::<i32>() {
                log_info!("[actor] updating max_auto_retries to {}", v);
                engine.manager.set_max_auto_retries(v);
            }
        }
        "auto_retry_delay_secs" => {
            if let Ok(v) = value.parse::<u64>() {
                log_info!("[actor] updating auto_retry_delay_secs to {}", v);
                engine.manager.set_auto_retry_delay_secs(v);
            }
        }
        // 值为空 = 用户在设置中点了「清除已学习的服务器策略」：清空内存缓存
        // 并重写持久化（非空值是引擎自己落盘的数据，不经此路径回流）。
        "domain_conn_caps" => {
            if value.is_empty() {
                log_info!("[actor] clearing learned domain connection caps");
                engine.manager.clear_domain_conn_caps();
            }
        }
        // 本机 API 服务器配置变更 → 热重启监听（优雅停机旧实例
        // 后按最新配置重启，含端口/token/子功能开关，无需重启应用）。
        k if k.starts_with("local_server_") => {
            log_info!(
                "[actor] api server config '{}' changed, restarting server",
                key
            );
            api_server_handle.shutdown();
            let cfg = ApiServerConfig::from_config_map(
                &engine.db.get_all_config().await.unwrap_or_default(),
                env!("CARGO_PKG_VERSION"),
            );
            *api_server_handle = spawn_api_server(api_host.clone(), cfg);
        }
        _ => {} // other config keys — no runtime action needed
    }
}

/// 按状态码过滤任务 ID（管理 API 的全局暂停/恢复用）。
async fn task_ids_by_status(db: &Db, statuses: &[i32]) -> Vec<String> {
    match db.load_all_tasks().await {
        Ok(tasks) => tasks
            .into_iter()
            .filter(|t| statuses.contains(&t.status))
            .map(|t| t.task_id)
            .collect(),
        Err(e) => {
            log_info!("[actor] load_all_tasks error: {}", e);
            Vec::new()
        }
    }
}

/// 合并请求头：浏览器捕获的头（`base`）打底，用户手填的同名头（忽略
/// 大小写）覆盖。外部下载确认与批量创建两条路径共用同一合并语义。
fn merge_ext_headers(
    base: HashMap<String, String>,
    overrides: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut merged = base;
    for (k, v) in overrides {
        merged.retain(|ek, _| !ek.eq_ignore_ascii_case(k));
        merged.insert(k.clone(), v.clone());
    }
    merged
}

/// 把一批（≥2 条）外部下载请求合成为一条多行文本的快速下载信号。
///
/// 文本为 aria2 风格：每条一行 URL，自定义文件名以缩进 `out=` 选项行跟随
/// （快速下载表单的 `parseQuickDownloadEntries` 原生支持该格式）。per-item
/// cookies/headers/fileSize 不进信号——调用方已按 URL 存入请求上下文缓存，
/// 用户确认后恢复；信号级 cookies 仅在全批一致时携带（作为表单预填值），
/// 不一致则留空、避免以偏概全。referrer/save_dir 取首个非空值。
fn synthesize_batch_request(
    reqs: &[fluxdown_api::types::DownloadRequest],
) -> ExternalDownloadRequest {
    // 控制字符防注入：filename 来自服务器 Content-Disposition（percent-decode
    // 后 %0A/%0D 会还原成字面 \n/\r），url/filename 若不剥离控制字符，恶意
    // 服务器可向合成的多行文本注入伪造下载条目（换行 = 新条目分隔符，
    // Dart 端 parseQuickDownloadEntries 按行解析）。控制字符替换为空格。
    fn strip_ctl(s: &str) -> std::borrow::Cow<'_, str> {
        if s.chars().any(char::is_control) {
            std::borrow::Cow::Owned(
                s.chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect(),
            )
        } else {
            std::borrow::Cow::Borrowed(s)
        }
    }
    let mut text = String::new();
    for req in reqs {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&strip_ctl(&req.url));
        if !req.filename.is_empty() {
            text.push_str("\n  out=");
            text.push_str(&strip_ctl(&req.filename));
        }
    }
    let cookies = if reqs.windows(2).all(|w| w[0].cookies == w[1].cookies) {
        reqs.first().map(|r| r.cookies.clone()).unwrap_or_default()
    } else {
        String::new()
    };
    let referrer = reqs
        .iter()
        .find(|r| !r.referrer.is_empty())
        .map(|r| r.referrer.clone())
        .unwrap_or_default();
    let save_dir = reqs
        .iter()
        .find(|r| !r.save_dir.is_empty())
        .map(|r| r.save_dir.clone())
        .unwrap_or_default();
    ExternalDownloadRequest {
        url: text,
        filename: String::new(),
        save_dir,
        referrer,
        file_size: 0,
        mime_type: String::new(),
        cookies,
        audio_url: String::new(),
    }
}

/// 把浏览器扩展/Native Messaging 的 wire-format `RequestBody` 转换为引擎侧
/// 传输无关的 `CapturedRequestBody`——两者字段形状一致，仅类型来源不同
/// (fluxdown_api 是对外 wire 契约，engine 侧不感知传输层)。
fn nm_body_to_captured(
    body: fluxdown_api::types::RequestBody,
) -> fluxdown_engine::downloader::CapturedRequestBody {
    use fluxdown_api::types::RequestBody;
    use fluxdown_engine::downloader::CapturedRequestBody as Captured;
    match body {
        RequestBody::FormData { fields } => Captured::FormData { fields },
        RequestBody::Urlencoded { raw } => Captured::Urlencoded { raw },
        RequestBody::Raw {
            bytes_b64,
            content_type,
        } => Captured::Raw {
            bytes_b64,
            content_type,
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn req(url: &str) -> fluxdown_api::types::DownloadRequest {
        serde_json::from_value(serde_json::json!({ "url": url })).unwrap()
    }

    #[test]
    fn synthesize_batch_joins_urls_with_out_lines() {
        let mut a = req("https://a.example/f1.zip");
        a.filename = "renamed.zip".to_string();
        let b = req("https://b.example/f2.zip");
        let signal = synthesize_batch_request(&[a, b]);
        assert_eq!(
            signal.url,
            "https://a.example/f1.zip\n  out=renamed.zip\nhttps://b.example/f2.zip"
        );
        assert!(signal.filename.is_empty());
        assert_eq!(signal.file_size, 0);
    }

    #[test]
    fn synthesize_batch_strips_control_chars_blocking_entry_injection() {
        // 恶意服务器经 Content-Disposition filename*=UTF-8''...%0A... 携带换行，
        // 企图向合成多行文本注入一条伪造下载条目。控制字符必须被扁平化，
        // 使 payload 保持在同一 out= 行内、不产生新条目行。
        let mut a = req("https://a.example/f1.zip");
        a.filename = "合法.pdf\nhttps://evil.example/payload.exe\n  out=无害.pdf".to_string();
        let b = req("https://b.example/f2.zip");
        let signal = synthesize_batch_request(&[a, b]);
        assert_eq!(
            signal.url,
            "https://a.example/f1.zip\n  out=合法.pdf https://evil.example/payload.exe   out=无害.pdf\nhttps://b.example/f2.zip",
            "控制字符应被替换为空格,注入内容保持在同一 out= 行内"
        );
        // 不变量:合成文本的行数 = 每条请求(URL 行 + 可选 out= 行),注入不增行。
        assert_eq!(signal.url.lines().count(), 3);
    }

    #[test]
    fn synthesize_batch_shares_cookies_only_on_consensus() {
        let mut a = req("https://a.example/1");
        a.cookies = "sid=1".to_string();
        let mut b = req("https://b.example/2");
        b.cookies = "sid=1".to_string();
        assert_eq!(synthesize_batch_request(&[a.clone(), b]).cookies, "sid=1");
        let mut c = req("https://c.example/3");
        c.cookies = "sid=2".to_string();
        assert!(synthesize_batch_request(&[a, c]).cookies.is_empty());
    }

    #[test]
    fn synthesize_batch_takes_first_nonempty_referrer_and_save_dir() {
        let a = req("https://a.example/1");
        let mut b = req("https://b.example/2");
        b.referrer = "https://page.example/".to_string();
        b.save_dir = "D:/dl".to_string();
        let signal = synthesize_batch_request(&[a, b]);
        assert_eq!(signal.referrer, "https://page.example/");
        assert_eq!(signal.save_dir, "D:/dl");
    }
}
