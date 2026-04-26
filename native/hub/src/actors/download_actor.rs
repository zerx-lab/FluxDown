use std::collections::HashMap;
use std::path::PathBuf;

use rinf::{DartSignal, RustSignal};
use tokio::sync::mpsc;

use crate::bt_downloader::{self, BtConfig};
use crate::db::Db;
use crate::download_manager::{self, DownloadManager, DownloadManagerConfig, TaskDone};
use crate::file_association;
use crate::logger::log_info;
use crate::native_messaging::{self};
use crate::protocol_registry;
use crate::proxy_config::ProxyConfig;
use crate::signals::{
    BatchControlTask, BatchCreateTask, CheckFileAssociation, CheckForUpdate, CheckUrlProtocol,
    ConfigEntry, ConfigLoaded, ConfirmExternalDownload, ControlTask, CreateQueue, CreateTask,
    DeleteQueue, DetectSystemProxy, DownloadUpdate, ExternalDownloadRequest, FileAssociationStatus,
    InstallUpdate, MoveTaskToQueue, ProbeTorrentMeta, ProxyTestResult, RequestAllQueues,
    RequestAllTasks, RequestConfig, RevealFile, SaveConfig, SelectBtFiles, SelectHlsQuality,
    SetFileAssociation, SetPriorityTask, SetUrlProtocol, SystemProxyInfo, TestProxyConnection,
    UpdateCheckResult, UpdateQueue, UrlProtocolStatus,
};
use crate::updater;

/// Compute default save directory (platform-dependent).
fn default_save_dir() -> String {
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

/// Read initial config values from DB to pass to DownloadManager.
async fn load_initial_config(db: &Db) -> (usize, u64, String, BtConfig, ProxyConfig, String, i32) {
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

    let bt_config = BtConfig {
        enable_dht: config
            .get("bt_enable_dht")
            .map(|v| v == "true")
            .unwrap_or(true),
        enable_upnp: config
            .get("bt_enable_upnp")
            .map(|v| v == "true")
            .unwrap_or(true),
        port_start: config
            .get("bt_port_start")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(6881),
        port_end: config
            .get("bt_port_end")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(6891),
        custom_trackers: config
            .get("bt_custom_trackers")
            .cloned()
            .unwrap_or_default(),
    };

    let proxy_config = ProxyConfig::from_config_map(&config);
    let user_agent = config.get("global_user_agent").cloned().unwrap_or_default();
    let default_segments = config
        .get("default_segments")
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(0);

    (
        max_concurrent,
        speed_limit_bytes,
        save_dir,
        bt_config,
        proxy_config,
        user_agent,
        default_segments,
    )
}

pub async fn run(db_dir: PathBuf) {
    let db = match Db::open(&db_dir) {
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
        let defaults = bt_downloader::default_tracker_list();
        if let Err(e) = db.set_config("bt_custom_trackers", &defaults).await {
            log_info!("[actor] failed to save default trackers: {}", e);
        }
        bt_config.custom_trackers = defaults;
    }
    log_info!(
        "[actor] init config: max_concurrent={}, speed_limit_bps={}, save_dir={}, bt_config={:?}",
        max_concurrent,
        speed_limit_bps,
        save_dir,
        bt_config,
    );

    let app_data_dir = db_dir.to_string_lossy().into_owned();
    let mut manager = match DownloadManager::new(
        db.clone(),
        DownloadManagerConfig {
            max_concurrent,
            speed_limit_bps,
            default_save_dir: save_dir,
            app_data_dir,
            bt_config,
            proxy_config,
            user_agent,
        },
    ) {
        Ok(m) => m,
        Err(e) => {
            log_info!("Failed to create download manager: {}", e);
            return;
        }
    };

    manager.set_default_segments(default_segments);

    if let Some(rx) = manager.take_progress_rx() {
        tokio::spawn(download_manager::progress_reporter(rx, db.clone()));
    }

    // Load named queue settings into the in-memory cache so that
    // per-queue speed limits and concurrency limits take effect immediately.
    manager.load_queues().await;

    // Channel for spawned tasks to notify completion (for active_tokens cleanup)
    let mut done_rx: mpsc::Receiver<TaskDone> = match manager.take_done_rx() {
        Some(rx) => rx,
        None => {
            // Should never happen — take_done_rx returns Some on first call
            let (_tx, rx) = mpsc::channel(1);
            rx
        }
    };

    // Channel for delayed auto-retry of failed tasks (stall / network errors).
    let mut retry_rx: mpsc::Receiver<String> = match manager.take_retry_rx() {
        Some(rx) => rx,
        None => {
            let (_tx, rx) = mpsc::channel(1);
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
    let set_file_assoc_recv = SetFileAssociation::get_dart_signal_receiver();
    let check_file_assoc_recv = CheckFileAssociation::get_dart_signal_receiver();
    let test_proxy_recv = TestProxyConnection::get_dart_signal_receiver();
    let detect_sys_proxy_recv = DetectSystemProxy::get_dart_signal_receiver();
    let select_hls_quality_recv = SelectHlsQuality::get_dart_signal_receiver();
    let set_url_proto_recv = SetUrlProtocol::get_dart_signal_receiver();
    let check_url_proto_recv = CheckUrlProtocol::get_dart_signal_receiver();
    let set_priority_recv = SetPriorityTask::get_dart_signal_receiver();
    let select_bt_files_recv = SelectBtFiles::get_dart_signal_receiver();
    let probe_torrent_meta_recv = ProbeTorrentMeta::get_dart_signal_receiver();
    let reveal_file_recv = RevealFile::get_dart_signal_receiver();

    // Spawn the Native Messaging listener (reads from stdin in a blocking thread).
    // When the browser extension sends a download request, it arrives on this channel.
    let mut native_msg_rx = native_messaging::spawn_native_messaging_listener();

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

    // 缓存浏览器扩展传递的额外 HTTP 请求头（如 Authorization），
    // 以 URL 为 key，在用户确认下载时取出传递给下载引擎。
    let mut ext_headers_cache: HashMap<String, HashMap<String, String>> = HashMap::new();

    loop {
        tokio::select! {
            Some(signal) = create_recv.recv() => {
                let msg = signal.message;
                manager
                    .create_task(msg.url, msg.save_dir, msg.file_name, msg.segments, msg.cookies, String::new(), 0, msg.torrent_file_bytes, msg.proxy_url, msg.user_agent, msg.queue_id, msg.checksum, HashMap::new(), msg.selected_file_indices)
                    .await;
                // 立即推送 AllTasks，确保 Dart 端在收到 TaskProgress 之前
                // 已通过 AllTasks 获得正确的 queue_id，防止新任务被错误归入默认队列。
                manager.load_and_send_all_tasks().await;
            }
            Some(signal) = batch_create_recv.recv() => {
                let msg = signal.message;
                log_info!(
                    "[actor] batch create: {} entries, save_dir={}, segments={}",
                    msg.entries.len(), msg.save_dir, msg.segments,
                );
                for entry in msg.entries {
                    manager
                        .create_task(entry.url, msg.save_dir.clone(), entry.file_name, msg.segments, msg.cookies.clone(), msg.referrer.clone(), 0, Vec::new(), msg.proxy_url.clone(), msg.user_agent.clone(), msg.queue_id.clone(), entry.checksum, HashMap::new(), Vec::new())
                        .await;
                }
                // 批量创建完成后统一推送一次 AllTasks，同步 queue_id 到 Dart。
                manager.load_and_send_all_tasks().await;
            }
            Some(signal) = control_recv.recv() => {
                let msg = signal.message;
                match msg.action {
                    0 => manager.pause_task(&msg.task_id).await,
                    1 => manager.resume_task(&msg.task_id).await,
                    2 => manager.cancel_task(&msg.task_id).await,
                    3 => manager.delete_task(&msg.task_id, true).await,   // delete record + files
                    4 => manager.delete_task(&msg.task_id, false).await,  // delete record only
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
                    0 => manager.batch_pause(&msg.task_ids).await,
                    1 => manager.batch_resume(&msg.task_ids).await,
                    3 => manager.delete_tasks_batch(&msg.task_ids, true).await,
                    4 => manager.delete_tasks_batch(&msg.task_ids, false).await,
                    _ => {}
                }
            }
            Some(_) = all_recv.recv() => {
                manager.load_and_send_all_tasks().await;
                // Also send queue list so Dart sidebar can show named queues.
                manager.send_all_queues().await;
            }
            Some(signal) = config_save_recv.recv() => {
                let msg = signal.message;
                // Persist to DB first.
                if let Err(e) = db.set_config(&msg.key, &msg.value).await {
                    log_info!("Failed to save config: {}", e);
                }
                // Notify DownloadManager for runtime-effective settings.
                match msg.key.as_str() {
                    "max_concurrent_tasks" => {
                        if let Ok(v) = msg.value.parse::<usize>() {
                            log_info!("[actor] updating max_concurrent to {}", v);
                            manager.set_max_concurrent(v).await;
                        }
                    }
                    "speed_limit_bytes" => {
                        if let Ok(v) = msg.value.parse::<u64>() {
                            log_info!("[actor] updating speed_limit to {} B/s", v);
                            manager.set_speed_limit(v);
                        }
                    }
                    "default_save_dir" => {
                        log_info!("[actor] updating default_save_dir to {}", msg.value);
                        manager.set_default_save_dir(msg.value);
                    }
                    // BT config keys — update in-memory BtConfig and invalidate
                    // the current session so the next BT download picks up changes.
                    "bt_enable_dht" | "bt_enable_upnp" | "bt_port_start"
                    | "bt_port_end" | "bt_custom_trackers" => {
                        log_info!("[actor] BT config changed: {}={}", msg.key, msg.value);
                        // Reload the full BT config from DB to stay consistent.
                        let all_cfg = db.get_all_config().await.unwrap_or_default();
                        let new_bt = BtConfig {
                            enable_dht: all_cfg
                                .get("bt_enable_dht")
                                .map(|v| v == "true")
                                .unwrap_or(true),
                            enable_upnp: all_cfg
                                .get("bt_enable_upnp")
                                .map(|v| v == "true")
                                .unwrap_or(true),
                            port_start: all_cfg
                                .get("bt_port_start")
                                .and_then(|v| v.parse::<u16>().ok())
                                .unwrap_or(6881),
                            port_end: all_cfg
                                .get("bt_port_end")
                                .and_then(|v| v.parse::<u16>().ok())
                                .unwrap_or(6891),
                            custom_trackers: all_cfg
                                .get("bt_custom_trackers")
                                .cloned()
                                .unwrap_or_default(),
                        };
                        manager.set_bt_config(new_bt);
                        // Invalidate (destroy) the current BT session so it is
                        // re-created with the new config on next BT download.
                        manager.invalidate_bt_session().await;
                    }
                    // Proxy config keys — reload full proxy config from DB
                    // and rebuild the HTTP client.
                    "proxy_mode" | "proxy_type" | "proxy_host" | "proxy_port"
                    | "proxy_username" | "proxy_password" | "proxy_no_list" => {
                        log_info!("[actor] proxy config changed: {}={}", msg.key, msg.value);
                        let all_cfg = db.get_all_config().await.unwrap_or_default();
                        let new_proxy = ProxyConfig::from_config_map(&all_cfg);
                        if let Err(e) = manager.set_proxy_config(new_proxy) {
                            log_info!("[actor] failed to apply proxy config: {}", e);
                        }
                    }
                    "global_user_agent" => {
                        log_info!("[actor] user_agent changed: {}", msg.value);
                        if let Err(e) = manager.set_user_agent(msg.value) {
                            log_info!("[actor] failed to apply user_agent: {}", e);
                        }
                    }
                    "default_segments" => {
                        if let Ok(v) = msg.value.parse::<i32>() {
                            log_info!("[actor] updating default_segments to {}", v);
                            manager.set_default_segments(v);
                        }
                    }
                    _ => {} // other config keys — no runtime action needed
                }
            }
            Some(_) = config_req_recv.recv() => {
                match db.get_all_config().await {
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
            // --- Native Messaging: browser extension download requests ---
            Some(req) = native_msg_rx.recv() => {
                log_info!(
                    "[actor] external download request from browser: url={}, cookies_len={}, headers={:?}",
                    req.url,
                    req.cookies.len(),
                    req.headers.as_ref().map(|h| h.keys().collect::<Vec<_>>()),
                );
                // 缓存额外请求头，供用户确认下载时使用
                if let Some(ref headers) = req.headers
                    && !headers.is_empty()
                {
                    // Evict oldest entries when cache grows too large to
                    // prevent unbounded memory growth over long sessions.
                    const MAX_HEADER_CACHE: usize = 200;
                    if ext_headers_cache.len() >= MAX_HEADER_CACHE {
                        ext_headers_cache.clear();
                    }
                    ext_headers_cache.insert(req.url.clone(), headers.clone());
                }
                // Forward to Dart UI so it can pop the quick-download dialog.
                ExternalDownloadRequest {
                    url: req.url,
                    filename: req.filename,
                    referrer: req.referrer,
                    file_size: req.file_size.unwrap_or(0),
                    mime_type: req.mime_type.unwrap_or_default(),
                    cookies: req.cookies,
                }
                .send_signal_to_dart();
            }
            // --- Dart confirmed an external download request ---
            Some(signal) = confirm_ext_recv.recv() => {
                let msg = signal.message;
                // 取出缓存的额外请求头
                let extra_headers = ext_headers_cache.remove(&msg.url).unwrap_or_default();
                log_info!(
                    "[actor] user confirmed external download: url={}, cookies_len={}, extra_headers={}",
                    msg.url,
                    msg.cookies.len(),
                    extra_headers.len(),
                );
                manager
                    .create_task(msg.url, msg.save_dir, msg.file_name, msg.segments, msg.cookies, msg.referrer, msg.hint_file_size, Vec::new(), msg.proxy_url, msg.user_agent, msg.queue_id, String::new(), extra_headers, Vec::new())
                    .await;
                // 推送 AllTasks 确保 Dart 端获得正确 queue_id。
                manager.load_and_send_all_tasks().await;
            }
            // --- Named queue management ---
            Some(signal) = create_queue_recv.recv() => {
                let msg = signal.message;
                log_info!("[actor] CreateQueue: name={}", msg.name);
                manager.create_queue(msg.name, msg.speed_limit_kbps, msg.max_concurrent, msg.default_save_dir, msg.default_segments, msg.default_user_agent).await;
            }
            Some(signal) = update_queue_recv.recv() => {
                let msg = signal.message;
                log_info!("[actor] UpdateQueue: id={}", msg.queue_id);
                manager.update_queue(msg.queue_id, msg.name, msg.speed_limit_kbps, msg.max_concurrent, msg.default_save_dir, msg.default_segments, msg.default_user_agent).await;
            }
            Some(signal) = delete_queue_recv.recv() => {
                let msg = signal.message;
                log_info!("[actor] DeleteQueue: id={}", msg.queue_id);
                manager.delete_queue(msg.queue_id).await;
            }
            Some(signal) = move_task_queue_recv.recv() => {
                let msg = signal.message;
                log_info!("[actor] MoveTaskToQueue: task={}, queue={}", msg.task_id, msg.queue_id);
                manager.move_task_to_queue(msg.task_id, msg.queue_id).await;
            }
            Some(_) = all_queues_recv.recv() => {
                manager.send_all_queues().await;
            }
            Some(done) = done_rx.recv() => {
                manager.on_task_done(&done).await;
            }
            // --- Auto-retry for stalled/failed tasks ---
            Some(task_id) = retry_rx.recv() => {
                // 安全检查：仅在任务仍处于 error 状态时才自动恢复。
                // 如果用户已手动暂停、恢复或删除了该任务，跳过重试。
                if manager.is_task_in_error(&task_id).await {
                    log_info!("[actor] auto-retry: resuming task {}", task_id);
                    manager.resume_task(&task_id).await;
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
                    match crate::proxy_config::detect_system_proxy() {
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
                manager.send_hls_quality_selection(&msg.task_id, msg.selected_index);
            }
            // --- Priority (Boost) download ---
            Some(signal) = set_priority_recv.recv() => {
                let task_id = signal.message.task_id;
                log_info!("[actor] SetPriorityTask: task_id={}", task_id);
                manager.set_priority_task(task_id).await;
            }
            // --- Proxy connectivity test ---
            Some(signal) = test_proxy_recv.recv() => {
                let msg = signal.message;
                log_info!(
                    "[actor] proxy test: type={}, host={}, port={}",
                    msg.proxy_type, msg.proxy_host, msg.proxy_port,
                );
                tokio::spawn(async move {
                    let result = crate::proxy_config::test_proxy_connection(
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
                });
            }
            // --- BT file selection ---
            Some(signal) = select_bt_files_recv.recv() => {
                let msg = signal.message;
                log_info!(
                    "[actor] SelectBtFiles: task={}, selected={:?}",
                    msg.task_id,
                    msg.selected_indices,
                );
                manager.deliver_bt_file_selection(&msg.task_id, msg.selected_indices).await;
            }
            // --- Reveal file in native file manager ---
            Some(signal) = reveal_file_recv.recv() => {
                let path = signal.message.path;
                tokio::task::spawn_blocking(move || {
                    crate::reveal_file::reveal(&path);
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
                // Pure local parse — run in a blocking thread to avoid
                // blocking the current_thread runtime.
                let probe_id = msg.probe_id;
                let bytes = msg.torrent_bytes;
                tokio::task::spawn_blocking(move || {
                    bt_downloader::probe_torrent_meta(probe_id, bytes);
                });
            }
        }
    }
}
