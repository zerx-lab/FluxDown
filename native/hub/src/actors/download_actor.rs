use std::path::PathBuf;

use rinf::{DartSignal, RustSignal};
use tokio::sync::mpsc;

use crate::db::Db;
use crate::download_manager::{self, DownloadManager, TaskDone};
use crate::native_messaging::{self};
use crate::signals::{
    CheckForUpdate, ConfigEntry, ConfigLoaded, ConfirmExternalDownload, ControlTask, CreateTask,
    DownloadUpdate, ExternalDownloadRequest, InstallUpdate, RequestAllTasks, RequestConfig,
    SaveConfig,
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
async fn load_initial_config(db: &Db) -> (usize, u64) {
    let config = db.get_all_config().await.unwrap_or_default();

    let max_concurrent = config
        .get("max_concurrent_tasks")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5);

    let speed_limit_bytes = config
        .get("speed_limit_bytes")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    (max_concurrent, speed_limit_bytes)
}

pub async fn run(db_dir: PathBuf) {
    let db = match Db::open(&db_dir) {
        Ok(db) => db,
        Err(e) => {
            rinf::debug_print!("Failed to open database: {}", e);
            return;
        }
    };

    // Initialize default config values in DB (no-op if already set)
    if let Err(e) = db.init_default_config(&default_save_dir()).await {
        rinf::debug_print!("Failed to init default config: {}", e);
    }

    // Load persisted config to initialize the manager with correct limits.
    let (max_concurrent, speed_limit_bps) = load_initial_config(&db).await;
    rinf::debug_print!(
        "[actor] init config: max_concurrent={}, speed_limit_bps={}",
        max_concurrent,
        speed_limit_bps
    );

    let mut manager = match DownloadManager::new(db.clone(), max_concurrent, speed_limit_bps) {
        Ok(m) => m,
        Err(e) => {
            rinf::debug_print!("Failed to create download manager: {}", e);
            return;
        }
    };

    if let Some(rx) = manager.take_progress_rx() {
        tokio::spawn(download_manager::progress_reporter(rx, db.clone()));
    }

    // Channel for spawned tasks to notify completion (for active_tokens cleanup)
    let mut done_rx: mpsc::Receiver<TaskDone> = match manager.take_done_rx() {
        Some(rx) => rx,
        None => {
            // Should never happen — take_done_rx returns Some on first call
            let (_tx, rx) = mpsc::channel(1);
            rx
        }
    };

    let create_recv = CreateTask::get_dart_signal_receiver();
    let control_recv = ControlTask::get_dart_signal_receiver();
    let all_recv = RequestAllTasks::get_dart_signal_receiver();
    let config_save_recv = SaveConfig::get_dart_signal_receiver();
    let config_req_recv = RequestConfig::get_dart_signal_receiver();
    let confirm_ext_recv = ConfirmExternalDownload::get_dart_signal_receiver();
    let check_update_recv = CheckForUpdate::get_dart_signal_receiver();
    let download_update_recv = DownloadUpdate::get_dart_signal_receiver();
    let install_update_recv = InstallUpdate::get_dart_signal_receiver();

    // Spawn the Native Messaging listener (reads from stdin in a blocking thread).
    // When the browser extension sends a download request, it arrives on this channel.
    let mut native_msg_rx = native_messaging::spawn_native_messaging_listener();

    loop {
        tokio::select! {
            Some(signal) = create_recv.recv() => {
                let msg = signal.message;
                manager
                    .create_task(msg.url, msg.save_dir, msg.file_name, msg.segments, msg.cookies)
                    .await;
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
            Some(_) = all_recv.recv() => {
                manager.load_and_send_all_tasks().await;
            }
            Some(signal) = config_save_recv.recv() => {
                let msg = signal.message;
                // Persist to DB first.
                if let Err(e) = db.set_config(&msg.key, &msg.value).await {
                    rinf::debug_print!("Failed to save config: {}", e);
                }
                // Notify DownloadManager for runtime-effective settings.
                match msg.key.as_str() {
                    "max_concurrent_tasks" => {
                        if let Ok(v) = msg.value.parse::<usize>() {
                            rinf::debug_print!("[actor] updating max_concurrent to {}", v);
                            manager.set_max_concurrent(v).await;
                        }
                    }
                    "speed_limit_bytes" => {
                        if let Ok(v) = msg.value.parse::<u64>() {
                            rinf::debug_print!("[actor] updating speed_limit to {} B/s", v);
                            manager.set_speed_limit(v);
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
                        rinf::debug_print!("Failed to load config: {}", e);
                    }
                }
            }
            // --- Native Messaging: browser extension download requests ---
            Some(req) = native_msg_rx.recv() => {
                rinf::debug_print!(
                    "[actor] external download request from browser: url={}, cookies_len={}",
                    req.url,
                    req.cookies.len()
                );
                // Forward to Dart UI so it can pop the quick-download dialog.
                ExternalDownloadRequest {
                    url: req.url,
                    filename: req.filename,
                    referrer: req.referrer,
                    file_size: req.file_size.map(|s| s as i64).unwrap_or(0),
                    mime_type: req.mime_type.unwrap_or_default(),
                    cookies: req.cookies,
                }
                .send_signal_to_dart();
            }
            // --- Dart confirmed an external download request ---
            Some(signal) = confirm_ext_recv.recv() => {
                let msg = signal.message;
                rinf::debug_print!(
                    "[actor] user confirmed external download: url={}, cookies_len={}",
                    msg.url,
                    msg.cookies.len()
                );
                manager
                    .create_task(msg.url, msg.save_dir, msg.file_name, msg.segments, msg.cookies)
                    .await;
            }
            Some(done) = done_rx.recv() => {
                manager.on_task_done(&done.task_id, done.generation).await;
            }
            // --- Auto-update signals ---
            Some(signal) = check_update_recv.recv() => {
                let version = signal.message.current_version;
                tokio::spawn(async move {
                    updater::check(&version).await;
                });
            }
            Some(signal) = download_update_recv.recv() => {
                let url = signal.message.url;
                let version = signal.message.version;
                tokio::spawn(async move {
                    updater::download(&url, &version).await;
                });
            }
            Some(signal) = install_update_recv.recv() => {
                let path = signal.message.installer_path;
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = updater::install(&path) {
                        rinf::debug_print!("[updater] install error: {}", e);
                    }
                });
            }
        }
    }
}
