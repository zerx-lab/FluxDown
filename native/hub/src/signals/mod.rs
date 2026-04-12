use rinf::{DartSignal, RustSignal, SignalPiece};
use serde::{Deserialize, Serialize};

// ========== Dart → Rust signals ==========

/// Create a new download task
#[derive(Deserialize, DartSignal)]
pub struct CreateTask {
    pub url: String,
    pub save_dir: String,
    pub file_name: String, // empty = auto detect from server
    pub segments: i32,     // 0 = auto (default 8)
    #[serde(default)]
    pub cookies: String, // browser cookies for authenticated downloads
    /// Raw .torrent file bytes (base64-decoded by Dart before sending).
    /// When non-empty, this takes priority over `url` for BT downloads.
    #[serde(default)]
    pub torrent_file_bytes: Vec<u8>,
    /// Per-task proxy URL override (e.g. "socks5://user:pass@host:port").
    /// Empty = use global proxy setting.
    #[serde(default)]
    pub proxy_url: String,
    /// Per-task user-agent override. Empty = use global UA setting.
    #[serde(default)]
    pub user_agent: String,
    /// Named queue ID to assign this task to. Empty = default queue.
    #[serde(default)]
    pub queue_id: String,
    /// Checksum spec for integrity verification after download.
    /// Format: "algo=hexhash", e.g. "sha-256=abc123..." or "md5=d41d8c...".
    /// Empty = skip verification.
    #[serde(default)]
    pub checksum: String,
    /// Pre-selected file indices for BT downloads (from the new-download dialog).
    /// When non-empty, Phase 3.5 will use these instead of waiting for a
    /// second file-selection dialog.
    /// Special value [-1] = user cancelled = task should abort immediately.
    /// Empty = no pre-selection (show the dialog after metadata resolves).
    #[serde(default)]
    pub selected_file_indices: Vec<i32>,
}

/// Single entry in a batch download (URL + optional filename + optional checksum)
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct UrlEntry {
    pub url: String,
    /// Custom file name. Empty = auto-detect from server response.
    pub file_name: String,
    /// Checksum spec for integrity verification after download.
    /// Format: "algo=hexhash", e.g. "sha-256=abc123..." or "md5=d41d8c...".
    /// Empty = skip verification.
    pub checksum: String,
}

/// Batch create multiple download tasks at once
#[derive(Deserialize, DartSignal)]
pub struct BatchCreateTask {
    pub entries: Vec<UrlEntry>, // list of download entries (URL + optional name/checksum)
    pub save_dir: String,
    pub segments: i32, // 0 = auto, shared across all tasks
    /// Per-task proxy URL override (shared for all tasks in batch).
    /// Empty = use global proxy setting.
    #[serde(default)]
    pub proxy_url: String,
    /// Per-task user-agent override (shared for all tasks in batch).
    /// Empty = use global UA setting.
    #[serde(default)]
    pub user_agent: String,
    /// Named queue ID to assign all tasks to. Empty = default queue.
    #[serde(default)]
    pub queue_id: String,
    /// 浏览器 cookies，用于需要认证的批量下载。
    /// 批次内所有任务共享。
    #[serde(default)]
    pub cookies: String,
    /// HTTP Referer 请求头，来自浏览器扩展。
    /// 批次内所有任务共享。
    #[serde(default)]
    pub referrer: String,
}

/// Control an existing task (pause/resume/cancel/delete)
#[derive(Deserialize, DartSignal)]
pub struct ControlTask {
    pub task_id: String,
    pub action: i32, // 0=pause, 1=resume, 2=cancel, 3=delete(+files), 4=delete(record only)
}

/// Batch control multiple tasks at once (pause/resume/delete).
/// Replaces N individual ControlTask IPC calls with a single signal.
#[derive(Deserialize, DartSignal)]
pub struct BatchControlTask {
    pub task_ids: Vec<String>,
    pub action: i32, // 0=pause, 1=resume, 3=delete(+files), 4=delete(record only)
}

/// Request all persisted tasks (sent on app startup)
#[derive(Deserialize, DartSignal)]
pub struct RequestAllTasks {}

/// Reveal a file in the native file manager and select it.
/// Windows: explorer.exe /select,"path" via raw_arg (bypasses argument escaping).
/// macOS/Linux: handled on the Dart side; this signal is Windows-only.
#[derive(Deserialize, DartSignal)]
pub struct RevealFile {
    pub path: String,
}

// ========== Rust → Dart signals ==========

/// Task progress update — sent periodically during download
#[derive(Serialize, RustSignal)]
pub struct TaskProgress {
    pub task_id: String,
    pub status: i32, // 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    pub speed: i64, // bytes per second
    pub file_name: String,
    pub save_dir: String,
    pub url: String,
    pub error_message: String, // empty if no error
}

/// Response to RequestAllTasks — all persisted tasks
#[derive(Serialize, RustSignal)]
pub struct AllTasks {
    pub tasks: Vec<TaskInfo>,
}

/// Segment-level progress for download visualization (IDM-style)
#[derive(Serialize, RustSignal)]
pub struct SegmentProgress {
    pub task_id: String,
    pub total_bytes: i64,
    /// Number of segments (1 = single-thread download)
    pub segment_count: i32,
    pub segments: Vec<SegmentDetail>,
}

/// Per-segment byte range and progress
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct SegmentDetail {
    pub index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
}

// ========== External download signals (browser extension → app) ==========

/// Notification to Dart that a download request arrived from the browser
/// extension via Native Messaging.  The Flutter UI should pop up a quick
/// confirmation dialog (independent download window).
#[derive(Serialize, RustSignal)]
pub struct ExternalDownloadRequest {
    pub url: String,
    pub filename: String,
    pub referrer: String,
    pub file_size: i64,    // 0 = unknown
    pub mime_type: String, // empty = unknown
    pub cookies: String,   // browser cookies for authenticated downloads
}

/// Dart → Rust: user confirmed the external download request.
#[derive(Deserialize, DartSignal)]
pub struct ConfirmExternalDownload {
    pub url: String,
    pub save_dir: String,
    pub file_name: String, // empty = auto detect
    pub segments: i32,     // 0 = auto
    #[serde(default)]
    pub cookies: String, // browser cookies for authenticated downloads
    /// HTTP Referer header value captured by the browser extension.
    /// Empty = do not send Referer (e.g. manually added downloads).
    #[serde(default)]
    pub referrer: String,
    /// File size hint from the browser extension (bytes). 0 = unknown.
    /// When > 0, the downloader skips the probe phase (HEAD + Range:0-0)
    /// and uses this value as total_bytes directly.  This is critical for
    /// one-time CDN URLs (e.g. Lanzou) where extra probe requests would
    /// consume the URL token before the actual download begins.
    #[serde(default)]
    pub hint_file_size: i64,
    /// Per-task proxy URL override.
    /// Empty = use global proxy setting.
    #[serde(default)]
    pub proxy_url: String,
    /// Per-task user-agent override. Empty = use global UA setting.
    #[serde(default)]
    pub user_agent: String,
    /// Named queue ID. Empty = default queue.
    #[serde(default)]
    pub queue_id: String,
}

// ========== Config signals ==========

/// Save a single config entry (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct SaveConfig {
    pub key: String,
    pub value: String,
}

/// Request all config entries (Dart → Rust, sent on app startup)
#[derive(Deserialize, DartSignal)]
pub struct RequestConfig {}

/// All config entries loaded from DB (Rust → Dart)
#[derive(Serialize, RustSignal)]
pub struct ConfigLoaded {
    pub entries: Vec<ConfigEntry>,
}

/// Single config key-value pair
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
}

/// Nested task info piece
#[derive(Clone, Serialize, Deserialize, SignalPiece)]
pub struct TaskInfo {
    pub task_id: String,
    pub url: String,
    pub file_name: String,
    pub save_dir: String,
    pub status: i32, // 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    pub error_message: String,
    pub created_at: String, // Unix seconds timestamp
    /// Per-task proxy URL (empty = global proxy).
    pub proxy_url: String,
    /// Named queue ID (empty = default queue).
    #[serde(default)]
    pub queue_id: String,
    /// Checksum spec for integrity verification (empty = skip).
    #[serde(default)]
    pub checksum: String,
}

/// Notification that a dynamic segment split occurred (IDM-style coordinator).
/// Sent in real-time so the Dart UI can animate the split transition.
#[derive(Serialize, RustSignal)]
pub struct SegmentSplitEvent {
    pub task_id: String,
    /// Index of the parent segment that was shrunk.
    pub parent_index: i32,
    /// New end_byte of the parent after the split.
    pub parent_new_end: i64,
    /// Index of the newly created child segment.
    pub child_index: i32,
    /// Start byte of the new child segment (= split point).
    pub child_start: i64,
    /// End byte of the new child segment (= parent's old end).
    pub child_end: i64,
    /// Whether this was a proactive split (true) or reactive/on-demand (false).
    pub is_proactive: bool,
    /// Current total number of segments after the split.
    pub total_segments: i32,
}

// ========== Auto-update signals ==========

/// Check for application updates (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct CheckForUpdate {
    pub current_version: String,
}

/// Update check result (Rust → Dart)
#[derive(Serialize, RustSignal)]
pub struct UpdateCheckResult {
    pub has_update: bool,
    pub latest_version: String,
    pub current_version: String,
    pub download_url: String,
    pub file_size: i64,
    pub published_at: String,
    pub error_message: String,
}

/// Start downloading an update (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct DownloadUpdate {
    pub url: String,
    pub version: String,
    /// Known file size from the check phase (bytes). Avoids relying on HEAD
    /// probes that may fail through API proxies / CDN redirects.
    pub file_size: i64,
}

/// Update download progress (Rust → Dart)
#[derive(Serialize, RustSignal)]
pub struct UpdateDownloadProgress {
    pub version: String,
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    pub speed: i64,
    /// 0=downloading, 1=completed, 2=error
    pub status: i32,
    pub installer_path: String,
    pub error_message: String,
    /// Number of concurrent download segments (0 = single-threaded fallback).
    pub segments: i32,
    /// Number of segments currently actively downloading.
    pub active_segments: i32,
}

/// Install a downloaded update (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct InstallUpdate {
    pub installer_path: String,
}

// ========== Proxy test signals ==========

/// Test proxy connectivity (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct TestProxyConnection {
    pub proxy_type: String, // "http" | "https" | "socks4" | "socks5"
    pub proxy_host: String,
    pub proxy_port: String,
    pub proxy_username: String,
    pub proxy_password: String,
}

/// Proxy test result (Rust → Dart)
#[derive(Serialize, RustSignal)]
pub struct ProxyTestResult {
    pub success: bool,
    pub latency_ms: i64,
    pub error_message: String,
}

// ========== System proxy detection signals ==========

/// Request system proxy detection (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct DetectSystemProxy {}

/// System proxy detection result (Rust → Dart)
#[derive(Serialize, RustSignal)]
pub struct SystemProxyInfo {
    /// Whether a system proxy was detected
    pub detected: bool,
    /// Proxy type: "http" | "https" | "socks4" | "socks5"
    pub proxy_type: String,
    /// Proxy host
    pub host: String,
    /// Proxy port
    pub port: String,
    /// Bypass / no-proxy list (comma-separated)
    pub no_proxy_list: String,
}

// ========== HLS quality selection signals ==========

/// HLS master playlist parsed — send available quality options to Dart (Rust → Dart).
/// Dart should display a selection dialog and respond with [SelectHlsQuality].
#[derive(Serialize, RustSignal)]
pub struct HlsQualityOptions {
    pub task_id: String,
    pub options: Vec<HlsQualityOption>,
}

#[derive(Serialize, Deserialize, SignalPiece)]
pub struct HlsQualityOption {
    pub index: i32,
    pub bandwidth: i64,
    pub width: i64,
    pub height: i64,
}

/// User selected an HLS quality variant (Dart → Rust).
#[derive(Deserialize, DartSignal)]
pub struct SelectHlsQuality {
    pub task_id: String,
    /// Index of the selected variant (from [HlsQualityOption.index]).
    pub selected_index: i32,
}

// ========== File association signals ==========

/// Set or remove .torrent file association (Dart → Rust).
/// `enable = true` → register, `enable = false` → unregister.
#[derive(Deserialize, DartSignal)]
pub struct SetFileAssociation {
    pub enable: bool,
}

/// Check current .torrent file association status (Dart → Rust).
#[derive(Deserialize, DartSignal)]
pub struct CheckFileAssociation {}

/// Report .torrent file association status back to Dart (Rust → Dart).
#[derive(Serialize, RustSignal)]
pub struct FileAssociationStatus {
    pub is_associated: bool,
}

// ========== URL protocol signals ==========

/// Set or remove `fluxdown://` URL protocol registration (Dart → Rust).
/// `enable = true` → register, `enable = false` → unregister.
#[derive(Deserialize, DartSignal)]
pub struct SetUrlProtocol {
    pub enable: bool,
}

/// Check current `fluxdown://` URL protocol registration status (Dart → Rust).
#[derive(Deserialize, DartSignal)]
pub struct CheckUrlProtocol {}

/// Report `fluxdown://` URL protocol registration status back to Dart (Rust → Dart).
#[derive(Serialize, RustSignal)]
pub struct UrlProtocolStatus {
    pub is_registered: bool,
}

// ========== Queue / meta-probe signals ==========

/// 队列任务探测到元数据 (Rust → Dart)
#[derive(Serialize, RustSignal)]
pub struct TaskMetaProbed {
    pub task_id: String,
    pub file_name: String, // 空 = 无法探测
    pub total_bytes: i64,  // 0 = 未知
}

/// 队列位置批量更新 (Rust → Dart) — 每次队列变化时广播
#[derive(Serialize, RustSignal)]
pub struct QueuePositionsUpdate {
    pub positions: Vec<QueuePosition>,
}

/// 单个任务的队列位置
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct QueuePosition {
    pub task_id: String,
    pub position: i32, // 1-based，0 = 不在队列
}

// ========== Named queue management signals ==========

/// Create a new named download queue (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct CreateQueue {
    pub name: String,
    /// Speed limit in KB/s for this queue. 0 = no limit.
    pub speed_limit_kbps: i64,
    /// Max simultaneous tasks for this queue. 0 = use global setting.
    pub max_concurrent: i32,
    /// Default save directory for tasks in this queue. Empty = use global default.
    pub default_save_dir: String,
    /// Default segment count for new tasks in this queue. 0 = auto (global advisor).
    #[serde(default)]
    pub default_segments: i32,
    /// Default user-agent for tasks in this queue. Empty = inherit global UA.
    #[serde(default)]
    pub default_user_agent: String,
}

/// Update an existing queue's settings (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct UpdateQueue {
    pub queue_id: String,
    pub name: String,
    pub speed_limit_kbps: i64,
    pub max_concurrent: i32,
    pub default_save_dir: String,
    /// Default segment count for new tasks in this queue. 0 = auto (global advisor).
    #[serde(default)]
    pub default_segments: i32,
    /// Default user-agent for tasks in this queue. Empty = inherit global UA.
    #[serde(default)]
    pub default_user_agent: String,
}

/// Delete a named queue (Dart → Rust). Tasks move to the default queue.
#[derive(Deserialize, DartSignal)]
pub struct DeleteQueue {
    pub queue_id: String,
}

/// Move a task to a different queue (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct MoveTaskToQueue {
    pub task_id: String,
    /// Target queue ID. Empty string = move to default queue.
    pub queue_id: String,
}

/// Request all named queues (Dart → Rust, sent on app startup)
#[derive(Deserialize, DartSignal)]
pub struct RequestAllQueues {}

/// All named queues — sent on startup and after any queue change (Rust → Dart)
#[derive(Serialize, RustSignal)]
pub struct AllQueues {
    pub queues: Vec<QueueInfo>,
}

// ========== Priority (Boost) download signals ==========

/// Set the priority download task — the selected task gets exclusive bandwidth
/// while all other active downloads are auto-paused (Dart → Rust).
/// Send `task_id = ""` to cancel boost mode.
#[derive(Deserialize, DartSignal)]
pub struct SetPriorityTask {
    /// ID of the task to boost. Empty string = cancel boost mode.
    pub task_id: String,
}

/// Notifies Dart that the boost-mode priority task has changed (Rust → Dart).
#[derive(Serialize, RustSignal)]
pub struct PriorityTaskChanged {
    /// ID of the current priority task. Empty string = boost mode inactive.
    pub priority_task_id: String,
    /// Number of tasks that were automatically paused to free bandwidth.
    pub auto_paused_count: i32,
}

/// Named queue metadata
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct QueueInfo {
    pub queue_id: String,
    pub name: String,
    /// Speed limit in KB/s. 0 = no limit.
    pub speed_limit_kbps: i64,
    /// Max simultaneous tasks in this queue. 0 = use global setting.
    pub max_concurrent: i32,
    /// Default save directory. Empty = use global default.
    pub default_save_dir: String,
    /// Display order (lower = higher up).
    pub position: i32,
    /// Default segment count for new tasks. 0 = auto (global segment advisor).
    pub default_segments: i32,
    /// Default user-agent for tasks in this queue. Empty = inherit global UA.
    #[serde(default)]
    pub default_user_agent: String,
}

// ========== BT file selection signals ==========

/// BT torrent metadata resolved — send file list to Dart for user selection (Rust → Dart).
/// Dart should display a file selection dialog and respond with [SelectBtFiles].
#[derive(Serialize, RustSignal)]
pub struct BtFilesInfo {
    pub task_id: String,
    /// Total size of all files in the torrent (bytes).
    pub total_bytes: i64,
    /// List of files in the torrent.
    pub files: Vec<BtFileEntry>,
}

/// A single file entry in a BT torrent.
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct BtFileEntry {
    /// Zero-based file index within the torrent.
    pub index: i32,
    /// Relative path of the file inside the torrent (e.g. "folder/sub/file.mp4").
    pub path: String,
    /// File size in bytes.
    pub size: i64,
}

/// User selected which BT files to download (Dart → Rust).
#[derive(Deserialize, DartSignal)]
pub struct SelectBtFiles {
    pub task_id: String,
    /// Indices of files the user wants to download (from [BtFileEntry.index]).
    /// Empty = download all files (should not happen in practice).
    pub selected_indices: Vec<i32>,
}

// ========== Torrent meta probe (for new-download dialog preview) ==========

/// Dart requests a preview of .torrent file contents before creating the task.
/// Rust will parse the torrent bytes locally (no network needed) and reply
/// immediately with [TorrentMetaResult].
#[derive(Deserialize, DartSignal)]
pub struct ProbeTorrentMeta {
    /// Unique probe ID chosen by Dart (e.g. a UUID or timestamp string).
    /// Echoed back in [TorrentMetaResult] so Dart can match the response.
    pub probe_id: String,
    /// Raw bytes of the .torrent file.
    pub torrent_bytes: Vec<u8>,
}

/// Rust replies to [ProbeTorrentMeta] with the parsed file list (Rust → Dart).
/// On parse error, `files` is empty and `error` is non-empty.
#[derive(Serialize, RustSignal)]
pub struct TorrentMetaResult {
    /// Echoed from [ProbeTorrentMeta.probe_id].
    pub probe_id: String,
    /// Display name of the torrent (the top-level name field).
    pub name: String,
    /// Total size of all files in the torrent (bytes).
    pub total_bytes: i64,
    /// Parsed file list. Empty on error.
    pub files: Vec<BtFileEntry>,
    /// Non-empty when parsing failed.
    pub error: String,
}
