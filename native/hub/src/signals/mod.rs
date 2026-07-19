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
    /// Ignore HTTPS certificate errors for this task. Secure default: false.
    #[serde(default)]
    pub ignore_tls_errors: bool,
    /// Custom HTTP request headers (key/value) for this task.
    /// Cookie is handled separately via `cookies`; do not mix the two.
    /// Empty = no extra headers.
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,
    /// Pre-selected file indices for BT downloads (from the new-download dialog).
    /// When non-empty, Phase 3.5 will use these instead of waiting for a
    /// second file-selection dialog.
    /// Special value [-1] = user cancelled = task should abort immediately.
    /// Empty = no pre-selection (show the dialog after metadata resolves).
    #[serde(default)]
    pub selected_file_indices: Vec<i32>,
    /// 稍后下载：true = 建任务后不启动（paused 落库），待「启动队列」
    /// 按序恢复或用户手动恢复。
    #[serde(default)]
    pub start_paused: bool,
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
    /// 音频轨 URL（通用「视频轨+音频轨」离散下载对语义）。
    /// 空 = 普通单 URL 下载；非空 = url 视作视频轨，本字段视作音频轨，
    /// 引擎分别下载两路后 mux 合并。
    #[serde(default)]
    pub audio_url: String,
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
    /// Ignore HTTPS certificate errors for every task in this batch.
    #[serde(default)]
    pub ignore_tls_errors: bool,
    /// 浏览器 cookies，用于需要认证的批量下载。
    /// 批次内所有任务共享。
    #[serde(default)]
    pub cookies: String,
    /// HTTP Referer 请求头，来自浏览器扩展。
    /// 批次内所有任务共享。
    #[serde(default)]
    pub referrer: String,
    /// 自定义 HTTP 请求头（批次内所有任务共享）。
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,
    /// 稍后下载：true = 批次内任务全部建为 paused，不启动。
    #[serde(default)]
    pub start_paused: bool,
}

/// Control an existing task (pause/resume/cancel/delete)
#[derive(Deserialize, DartSignal)]
pub struct ControlTask {
    pub task_id: String,
    pub action: i32, // 0=pause, 1=resume, 2=cancel, 3=delete(+files), 4=delete(record only)
}

/// 修改某个已存在任务的分段（线程）数（Dart → Rust）。
/// 仅在任务处于非活跃态（暂停/错误/等待）时生效；改动会清空已下进度，
/// 下次恢复时按新分段数重新下载。`segments <= 0` = 恢复为「自动」。
#[derive(Deserialize, DartSignal)]
pub struct UpdateTaskSegments {
    pub task_id: String,
    pub segments: i32,
}

/// 分段数修改结果（Rust → Dart）。`ok = false` 表示任务正在下载/准备中/
/// 已完成而被拒绝，Dart 侧据此提示用户先暂停。
#[derive(Serialize, RustSignal)]
pub struct TaskSegmentsUpdated {
    pub task_id: String,
    pub segments: i32,
    pub ok: bool,
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

/// Dart → Rust：用系统默认程序打开文件（裸路径经 ShellExecute，正确激活 UWP/
/// Store 关联；避免 file:// URL 打不开 .mp4 等 UWP 关联类型）。
#[derive(Deserialize, DartSignal)]
pub struct OpenFile {
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
    /// 请求指定的保存目录（aria2 `dir` 选项 / 接管请求 `saveDir`）。
    /// 空 = 由 Dart 端按分类匹配 / 默认目录决定。
    pub save_dir: String,
    pub referrer: String,
    pub file_size: i64,    // 0 = unknown
    pub mime_type: String, // empty = unknown
    pub cookies: String,   // browser cookies for authenticated downloads
    /// 音频轨 URL（通用「视频轨+音频轨」离散下载对语义）。
    /// 空 = 普通单 URL 下载；非空 = url 是视频轨，本字段是音频轨。
    pub audio_url: String,
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
    /// Ignore HTTPS certificate errors for this confirmed task.
    #[serde(default)]
    pub ignore_tls_errors: bool,
    /// 音频轨 URL（通用「视频轨+音频轨」离散下载对语义）。
    /// 空 = 普通单 URL 下载；非空 = url 是视频轨，本字段是音频轨，
    /// create_task 尾参按此非空/空转换为 Some/None。
    #[serde(default)]
    pub audio_url: String,
    /// 用户在快速下载表单里手填的自定义请求头。与 Rust 侧按 URL 缓存的
    /// 浏览器捕获请求头合并，同名以用户手填值覆盖。
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,
    /// 稍后下载：true = 建任务后不启动（paused 落库）。
    #[serde(default)]
    pub start_paused: bool,
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
    /// Whether this task explicitly accepts invalid HTTPS certificates.
    #[serde(default)]
    pub ignore_tls_errors: bool,
    /// 文件跟踪：completed 任务的目标文件是否已丢失（被删除/移动）。默认 false。
    #[serde(default)]
    pub file_missing: bool,
    /// 任务结束时间，Unix seconds 时间戳（空 = 尚未完成）。
    /// 记录下载真正完成（status→3）的时刻，不含插件 hook 后处理耗时。
    #[serde(default)]
    pub completed_at: String,
    /// 配置的分段（线程）数。0 = 自动。供 UI 展示与「改线程数」编辑。
    #[serde(default)]
    pub segments: i32,
    /// 队列内启动顺序（越小越先启动）。0 = 未显式排序（按创建时间）。
    #[serde(default)]
    pub queue_order: i32,
    /// Source page URL captured by the browser extension (empty = none).
    #[serde(default)]
    pub referrer: String,
    /// 所属任务组 ID（空 = 不属于任何组）。
    #[serde(default)]
    pub group_id: String,
}

/// 文件跟踪：一批已完成任务的「文件已丢失」标志变化（Rust → Dart）。
/// 只携带发生变化的任务，Dart 侧按 task_id 定向更新，避免整表重建导致活跃
/// 下载 UI 闪烁。
#[derive(Serialize, RustSignal)]
pub struct FileMissingChanged {
    pub updates: Vec<FileMissingUpdate>,
}

/// 文件跟踪：单个任务的「文件已丢失」标志更新（true=丢失，false=恢复存在）。
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct FileMissingUpdate {
    pub task_id: String,
    pub missing: bool,
}

/// 文件跟踪：请求引擎重扫所有已完成任务的文件是否仍在（Dart → Rust）。
/// 桌面窗口聚焦 / 移动端回前台时发送，触发一次即时扫描。
#[derive(Deserialize, DartSignal)]
pub struct RescanFiles {}

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
    /// Update channel: "stable" (default) or "frontier" (includes prereleases).
    pub channel: String,
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

/// Request any pending "update failed" marker left by the updater helper
/// after a failed portable update (Dart → Rust). Sent once on startup after
/// the Dart side has subscribed to the response signal (avoids a startup race).
#[derive(Deserialize, DartSignal)]
pub struct RequestUpdateFailureMarker {}

/// Pending "update failed" marker payload (Rust → Dart). `message` is empty
/// when there is no pending failure to report.
#[derive(Serialize, RustSignal)]
pub struct UpdateFailureMarker {
    /// Human-readable failure message; empty when there is nothing to report.
    pub message: String,
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

// ========== Plugin resolve variant selection signals ==========

/// 插件 resolve 返回多个可选变体（画质/格式）— 发送给 Dart 供用户选择
/// (Rust → Dart)。Dart 应展示一个选择对话框并回复 [SelectResolveVariant]。
#[derive(Serialize, RustSignal)]
pub struct ResolveVariantSelectionRequest {
    pub task_id: String,
    /// 插件按自身偏好排序的默认变体索引（用户未选择/超时时回退）。
    pub default_index: i32,
    pub options: Vec<ResolveVariantOption>,
}

/// 插件 resolve 返回的单个可选变体（画质/格式）。
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct ResolveVariantOption {
    pub index: i32,
    pub label: String,
    pub container: String,
    pub bandwidth: i64,
    pub width: i64,
    pub height: i64,
    pub total_bytes: i64,
}

/// User selected a plugin resolve variant (Dart → Rust).
#[derive(Deserialize, DartSignal)]
pub struct SelectResolveVariant {
    pub task_id: String,
    /// Index of the selected variant (from [ResolveVariantOption.index]).
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

/// Delete a named queue (Dart → Rust). Tasks move to the builtin main queue.
#[derive(Deserialize, DartSignal)]
pub struct DeleteQueue {
    pub queue_id: String,
}

/// Move a task to a different queue (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct MoveTaskToQueue {
    pub task_id: String,
    /// Target queue ID. Empty string = move to the builtin main queue.
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

/// Single task moved to another queue (Rust → Dart)
#[derive(Serialize, RustSignal)]
pub struct TaskQueueChanged {
    pub task_id: String,
    pub queue_id: String,
}

/// 启动队列：置运行态并按队列内顺序恢复其中所有待下载任务 (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct StartQueue {
    pub queue_id: String,
}

/// 停止队列：置停止态并暂停其中所有排队/活跃任务 (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct StopQueue {
    pub queue_id: String,
}

/// 更新队列的每日定时计划 (Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct SetQueueSchedule {
    pub queue_id: String,
    /// 定时计划是否启用。
    pub enabled: bool,
    /// 每日定时启动时间 "HH:MM"（空 = 不定时启动）。
    pub start_time: String,
    /// 每日定时停止时间 "HH:MM"（空 = 不定时停止）。
    pub stop_time: String,
    /// 生效星期位掩码：bit0=周一 … bit6=周日；0/127 = 每天。
    pub days: i32,
}

/// 持久化队列内任务顺序（完整新顺序，1..N）(Dart → Rust)
#[derive(Deserialize, DartSignal)]
pub struct ReorderQueueTasks {
    pub queue_id: String,
    /// 队列内任务的完整新顺序（未列出的任务保持原 queue_order）。
    pub task_ids: Vec<String>,
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
    /// 队列运行状态：停止的队列不自动启动其中任务。
    /// （QueueInfo 仅 Rust→Dart 序列化，字段恒由引擎填充。）
    #[serde(default)]
    pub is_running: bool,
    /// 定时计划是否启用。
    #[serde(default)]
    pub schedule_enabled: bool,
    /// 每日定时启动时间 "HH:MM"（空 = 不定时启动）。
    #[serde(default)]
    pub schedule_start: String,
    /// 每日定时停止时间 "HH:MM"（空 = 不定时停止）。
    #[serde(default)]
    pub schedule_stop: String,
    /// 定时生效星期位掩码：bit0=周一 … bit6=周日。
    #[serde(default)]
    pub schedule_days: i32,
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

// ========== BT tracker subscription signals ==========

/// Manually refresh the tracker subscription lists now (Dart → Rust).
/// Rust fetches all configured subscription URLs, dedupes the result,
/// caches it in the config table and replies with [TrackerSubscriptionResult].
#[derive(Deserialize, DartSignal)]
pub struct UpdateTrackerSubscription {}

/// Result of a tracker subscription refresh (Rust → Dart).
/// Sent after both manual refreshes and the automatic startup refresh.
#[derive(Serialize, RustSignal)]
pub struct TrackerSubscriptionResult {
    /// True when at least one subscription source was fetched successfully.
    pub success: bool,
    /// Number of unique trackers fetched across all sources (after dedup).
    pub tracker_count: i32,
    /// Number of sources fetched successfully.
    pub ok_sources: i32,
    /// Total number of subscription sources attempted.
    pub total_sources: i32,
    /// Unix seconds of this refresh. 0 when the refresh failed.
    pub updated_at: i64,
    /// Non-empty when all sources failed (error summary).
    pub error: String,
}

// ========== ED2K server subscription signals ==========

/// Manually refresh the ED2K server subscription (server.met) now (Dart → Rust).
/// Rust fetches all configured server.met URLs, parses + dedupes the result,
/// caches it in the config table and replies with [Ed2kServerSubscriptionResult].
#[derive(Deserialize, DartSignal)]
pub struct UpdateEd2kServerSubscription {}

/// Result of an ED2K server subscription refresh (Rust → Dart).
/// Sent after both manual refreshes and the automatic startup refresh.
#[derive(Serialize, RustSignal)]
pub struct Ed2kServerSubscriptionResult {
    /// True when at least one subscription source was fetched successfully.
    pub success: bool,
    /// Number of unique servers parsed across all sources (after dedup).
    pub server_count: i32,
    /// Number of sources fetched successfully.
    pub ok_sources: i32,
    /// Total number of subscription sources attempted.
    pub total_sources: i32,
    /// Unix seconds of this refresh. 0 when the refresh failed.
    pub updated_at: i64,
    /// Non-empty when all sources failed (error summary).
    pub error: String,
}

// ========== Plugin system signals ==========

/// Request the current plugin list (Dart → Rust, sent on plugin settings page open).
#[derive(Deserialize, DartSignal)]
pub struct RequestPlugins {}

/// Install a plugin — from a zip upload, a dev directory, or both fields
/// distinguish the mode (Dart → Rust). See `download_actor` dispatch rule.
#[derive(Deserialize, DartSignal)]
pub struct InstallPlugin {
    pub zip_bytes: Vec<u8>,
    pub dir_path: String,
    pub dev_mode: bool,
}

/// Uninstall a plugin by identity (Dart → Rust).
#[derive(Deserialize, DartSignal)]
pub struct UninstallPlugin {
    pub identity: String,
}

/// Enable/disable a plugin (Dart → Rust). Manual toggle — clears any
/// circuit-breaker auto-disable reason.
#[derive(Deserialize, DartSignal)]
pub struct SetPluginEnabled {
    pub identity: String,
    pub enabled: bool,
}

/// Save a plugin's settings values (Dart → Rust).
#[derive(Deserialize, DartSignal)]
pub struct SavePluginSettings {
    pub identity: String,
    pub entries: Vec<ConfigEntry>,
}

/// Escape hatch: clear a stuck task's resolver binding and resume it
/// (Dart → Rust). Used when a plugin resolver hangs or a task is stuck
/// waiting on plugin retry.
#[derive(Deserialize, DartSignal)]
pub struct IgnorePluginRetry {
    pub task_id: String,
}

/// Current plugin list, sent after [RequestPlugins] and after any
/// install/uninstall/enable/settings mutation (Rust → Dart).
#[derive(Serialize, RustSignal)]
pub struct PluginList {
    pub plugins: Vec<PluginInfoSignal>,
}

/// Result of a plugin write operation — install/uninstall/enable/settings
/// (Rust → Dart). `op` identifies the operation (e.g. "install", "uninstall",
/// "set_enabled", "save_settings"); `failed_key` names the offending setting
/// key on a settings validation failure (empty otherwise).
/// `missing_components` lists base components (e.g. "ffmpeg", "ytdlp") the
/// plugin's declared permissions require but that are not yet installed —
/// populated only on a successful install/market_install, so the UI can
/// remind the user to set up dependencies (advisory, install still succeeds).
#[derive(Serialize, RustSignal)]
pub struct PluginOpResult {
    pub op: String,
    pub identity: String,
    pub ok: bool,
    pub message: String,
    pub failed_key: String,
    pub missing_components: Vec<String>,
}

/// A plugin was auto-disabled by the circuit breaker (Rust → Dart).
#[derive(Serialize, RustSignal)]
pub struct PluginAutoDisabledNotice {
    pub identity: String,
    pub reason: String,
}

/// A plugin's onDone hook started/finished running for a task, purely
/// informational — does not affect task status (Rust → Dart). Same
/// `(task_id, plugin_id)` pair may fire multiple times; Dart should track
/// activity as a `(task_id, plugin_id)` set, not assume single-shot.
#[derive(Serialize, RustSignal)]
pub struct PluginHookActivityEvent {
    pub task_id: String,
    pub plugin_id: String,
    pub running: bool,
}

/// Nested plugin info piece — mirrors `fluxdown_engine::plugin::PluginInfo`.
/// Hub-local: not shared with `fluxdown_api`'s `PluginDto` (hub→api is a
/// one-way dependency).
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct PluginInfoSignal {
    pub identity: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub homepage: String,
    pub enabled: bool,
    pub dev_mode: bool,
    pub disabled_reason: String,
    pub settings: Vec<SettingFieldSignal>,
    pub settings_values: Vec<ConfigEntry>,
    /// manifest 声明的能力权限（如 `["ffmpeg"]`，供 UI 展示授权徽章）。
    pub permissions: Vec<String>,
}

#[cfg(hub_plugins)]
impl From<fluxdown_engine::plugin::PluginInfo> for PluginInfoSignal {
    fn from(info: fluxdown_engine::plugin::PluginInfo) -> Self {
        Self {
            identity: info.identity,
            name: info.name,
            version: info.version,
            description: info.description,
            homepage: info.homepage,
            enabled: info.enabled,
            dev_mode: info.dev_mode,
            disabled_reason: info.disabled_reason,
            settings: info.settings.into_iter().map(Into::into).collect(),
            settings_values: info
                .settings_values
                .into_iter()
                .map(|(key, value)| ConfigEntry { key, value })
                .collect(),
            permissions: info.permissions,
        }
    }
}

/// Nested plugin setting field piece — mirrors
/// `fluxdown_engine::plugin::SettingField`. `min`/`max` use `f64` +
/// `has_min`/`has_max` (rather than `f64::NAN`) to avoid NaN crossing the
/// Dart FFI boundary undetected.
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct SettingFieldSignal {
    pub key: String,
    pub title: String,
    pub description: String,
    pub setting_type: String,
    pub widget: String,
    pub options: Vec<SettingOptionSignal>,
    pub default_value: String,
    pub required: bool,
    pub min: f64,
    pub has_min: bool,
    pub max: f64,
    pub has_max: bool,
    pub pattern: String,
    /// 辅助脚本（空 = 无）。非空时 UI 在字段旁渲染复制按钮，仅复制文本、绝不执行。
    pub helper_script: String,
    /// 辅助脚本按钮文案（空 = 用默认文案）。
    pub helper_label: String,
}

#[cfg(hub_plugins)]
impl From<fluxdown_engine::plugin::SettingField> for SettingFieldSignal {
    fn from(field: fluxdown_engine::plugin::SettingField) -> Self {
        use fluxdown_engine::plugin::{SettingType, SettingWidget};

        let setting_type = match field.ty {
            SettingType::String => "string",
            SettingType::Number => "number",
            SettingType::Boolean => "boolean",
        }
        .to_string();
        let widget = match field.effective_widget() {
            SettingWidget::Text => "text",
            SettingWidget::Password => "password",
            SettingWidget::Textarea => "textarea",
            SettingWidget::Select => "select",
            SettingWidget::Toggle => "toggle",
            SettingWidget::Number => "number",
            SettingWidget::Folder => "folder",
        }
        .to_string();
        Self {
            key: field.key,
            title: field.title,
            description: field.description,
            setting_type,
            widget,
            options: field.options.into_iter().map(Into::into).collect(),
            default_value: field.default.unwrap_or_default(),
            required: field.required,
            has_min: field.min.is_some(),
            min: field.min.unwrap_or(0.0),
            has_max: field.max.is_some(),
            max: field.max.unwrap_or(0.0),
            pattern: field.pattern.unwrap_or_default(),
            helper_script: field.helper_script.unwrap_or_default(),
            helper_label: field.helper_label.unwrap_or_default(),
        }
    }
}

/// `select` widget option piece — mirrors `fluxdown_engine::plugin::SettingOption`.
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct SettingOptionSignal {
    pub value: String,
    pub label: String,
}

#[cfg(hub_plugins)]
impl From<fluxdown_engine::plugin::manifest::SettingOption> for SettingOptionSignal {
    fn from(opt: fluxdown_engine::plugin::manifest::SettingOption) -> Self {
        Self {
            value: opt.value,
            label: opt.label,
        }
    }
}

// ========== Decentralized plugin market signals ==========

/// Request the current market index (Dart → Rust, sent when the market page
/// opens or the user hits refresh). Fetching is network I/O (≤20s across all
/// mirrors) — `download_actor` MUST NOT `.await` it inside the `select!`
/// branch; it hands off to an off-actor `tokio::spawn` task instead.
#[derive(Deserialize, DartSignal)]
pub struct RequestMarketIndex {}

/// Install a plugin's latest non-yanked version from the market by its
/// market `plugin_id` (Dart → Rust). Same off-actor dispatch rule as
/// [`RequestMarketIndex`].
#[derive(Deserialize, DartSignal)]
pub struct InstallMarketPlugin {
    pub plugin_id: String,
}

/// Market index fetch result (Rust → Dart), sent after [`RequestMarketIndex`].
/// `entries` is empty and `message` carries the error text on failure
/// (network error / index parse error / sequence-rollback rejection).
#[derive(Serialize, RustSignal)]
pub struct MarketIndexLoaded {
    pub ok: bool,
    pub message: String,
    pub entries: Vec<MarketEntrySignal>,
}

/// Nested market entry piece — mirrors `fluxdown_engine::plugin::MarketEntry`.
/// `sequence` uses `i64` (not `u64`) to avoid rinf's u64 FFI limitation.
#[derive(Serialize, Deserialize, SignalPiece)]
pub struct MarketEntrySignal {
    pub plugin_id: String,
    pub version: String,
    pub sequence: i64,
    pub content_hash: String,
    pub min_app_version: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub homepage: String,
    pub mirrors: Vec<String>,
    pub publish_time: String,
    pub yanked: String,
    pub tags: Vec<String>,
    /// manifest 声明的能力权限（如 `["ffmpeg"]`，供安装前展示授权）。
    pub permissions: Vec<String>,
}

#[cfg(hub_plugins)]
impl From<fluxdown_engine::plugin::MarketEntry> for MarketEntrySignal {
    fn from(e: fluxdown_engine::plugin::MarketEntry) -> Self {
        Self {
            plugin_id: e.plugin_id,
            version: e.version,
            // 索引 sequence 是全局单调计数器,现实规模下远不会触及 i64::MAX;
            // 万一溢出也只影响这一条目的排序展示,钳到上限而非 panic。
            sequence: i64::try_from(e.sequence).unwrap_or(i64::MAX),
            content_hash: e.content_hash,
            min_app_version: e.min_app_version,
            name: e.name,
            description: e.description,
            author: e.author,
            homepage: e.homepage,
            mirrors: e.mirrors,
            publish_time: e.publish_time,
            yanked: e.yanked,
            tags: e.tags,
            permissions: e.permissions,
        }
    }
}

// ========== Component management signals (v1: ffmpeg only) ==========

/// Request the current ffmpeg component status (Dart → Rust, sent when the
/// components settings page opens). Local process probe — fast enough to
/// `.await` directly inside the `download_actor` `select!` branch.
#[derive(Deserialize, DartSignal)]
pub struct RequestFfmpegStatus {}

/// Request the list of installable ffmpeg versions (Dart → Rust). Network
/// I/O (GitHub release API) — `download_actor` MUST NOT `.await` it inside
/// the `select!` branch; it hands off to an off-actor `tokio::spawn` task.
#[derive(Deserialize, DartSignal)]
pub struct RequestFfmpegVersions {}

/// Install (or reinstall/update) the managed ffmpeg build (Dart → Rust).
/// `version` empty = latest stable. Same off-actor dispatch rule as
/// [`RequestFfmpegVersions`] (downloads a multi-MB archive).
#[derive(Deserialize, DartSignal)]
pub struct InstallFfmpeg {
    pub version: String,
}

/// Uninstall the managed ffmpeg build (Dart → Rust). No network I/O —
/// manual/system paths are unaffected.
#[derive(Deserialize, DartSignal)]
pub struct UninstallFfmpeg {}

/// ffmpeg component status snapshot (Rust → Dart), sent after
/// [`RequestFfmpegStatus`] and after every install/uninstall completes.
/// `source` mirrors `fluxdown_engine::components::FfmpegSource::as_str()`
/// ("manual"/"managed"/"system"/"none").
#[derive(Serialize, RustSignal)]
pub struct FfmpegStatusReport {
    pub source: String,
    pub path: String,
    pub version: String,
    pub managed_version: String,
    pub system_path: String,
    /// Whether managed install is available on this platform (mirrors
    /// `fluxdown_engine::components::FfmpegStatus::managed_supported`). `false`
    /// on macOS etc. — the settings page hides the managed-install section and
    /// only guides system PATH / manual path, avoiding repeated failure prompts.
    pub managed_supported: bool,
}

/// Installable ffmpeg version list result (Rust → Dart), sent after
/// [`RequestFfmpegVersions`]. `versions` is empty and `message` carries the
/// error text on failure (network error / unsupported platform).
#[derive(Serialize, RustSignal)]
pub struct FfmpegVersionList {
    pub ok: bool,
    pub message: String,
    pub versions: Vec<String>,
    pub latest_stable: String,
}

/// ffmpeg managed install download progress (Rust → Dart), sent while
/// [`InstallFfmpeg`] is in flight. `total_bytes == 0` means the total size
/// is unknown (indeterminate progress). Throttled by the engine (~256KB
/// steps) — safe to send on every event without extra debouncing.
#[derive(Serialize, RustSignal)]
pub struct FfmpegInstallProgress {
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
}

/// Result of an [`InstallFfmpeg`]/[`UninstallFfmpeg`] operation (Rust →
/// Dart). Always immediately followed by a fresh [`FfmpegStatusReport`].
#[derive(Serialize, RustSignal)]
pub struct FfmpegInstallResult {
    pub ok: bool,
    pub message: String,
}

// ========== Component management signals: yt-dlp ==========

/// Request the current yt-dlp component status (Dart → Rust, sent when the
/// components settings page opens). Local process probe — fast enough to
/// `.await` directly inside the `download_actor` `select!` branch.
#[derive(Deserialize, DartSignal)]
pub struct RequestYtdlpStatus {}

/// Request the list of installable yt-dlp versions (Dart → Rust). Network
/// I/O (GitHub release API) — `download_actor` MUST NOT `.await` it inside
/// the `select!` branch; it hands off to an off-actor `tokio::spawn` task.
#[derive(Deserialize, DartSignal)]
pub struct RequestYtdlpVersions {}

/// Install (or reinstall/update) the managed yt-dlp build (Dart → Rust).
/// `version` empty = latest stable. Same off-actor dispatch rule as
/// [`RequestYtdlpVersions`] (downloads a multi-MB binary).
#[derive(Deserialize, DartSignal)]
pub struct InstallYtdlp {
    pub version: String,
}

/// Uninstall the managed yt-dlp build (Dart → Rust). No network I/O —
/// manual/system paths are unaffected.
#[derive(Deserialize, DartSignal)]
pub struct UninstallYtdlp {}

/// yt-dlp component status snapshot (Rust → Dart), sent after
/// [`RequestYtdlpStatus`] and after every install/uninstall completes.
/// `source` mirrors `fluxdown_engine::components::ComponentSource::as_str()`
/// ("manual"/"managed"/"system"/"none").
#[derive(Serialize, RustSignal)]
pub struct YtdlpStatusReport {
    pub source: String,
    pub path: String,
    pub version: String,
    pub managed_version: String,
    pub system_path: String,
    /// Whether managed install is available on this platform (mirrors
    /// `fluxdown_engine::components::YtdlpStatus::managed_supported`).
    /// yt-dlp ships official builds for all desktop/server platforms, so this
    /// is normally `true` (unlike ffmpeg on macOS).
    pub managed_supported: bool,
}

/// Installable yt-dlp version list result (Rust → Dart), sent after
/// [`RequestYtdlpVersions`]. `versions` is empty and `message` carries the
/// error text on failure (network error / unsupported platform).
#[derive(Serialize, RustSignal)]
pub struct YtdlpVersionList {
    pub ok: bool,
    pub message: String,
    pub versions: Vec<String>,
    pub latest_stable: String,
}

/// yt-dlp managed install download progress (Rust → Dart), sent while
/// [`InstallYtdlp`] is in flight. `total_bytes == 0` means the total size
/// is unknown (indeterminate progress). Throttled by the engine (~256KB
/// steps) — safe to send on every event without extra debouncing.
#[derive(Serialize, RustSignal)]
pub struct YtdlpInstallProgress {
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
}

/// Result of an [`InstallYtdlp`]/[`UninstallYtdlp`] operation (Rust →
/// Dart). Always immediately followed by a fresh [`YtdlpStatusReport`].
#[derive(Serialize, RustSignal)]
pub struct YtdlpInstallResult {
    pub ok: bool,
    pub message: String,
}

// ========== 任务组 / 清单预解析（多文件任务组） ==========

/// Dart 请求前置预解析清单（多文件分享/合集链接，建组对话框展示用）。
/// 只读、不建任务、不写库；结果经 [`ResolvePreviewResult`] 回传。
#[derive(Deserialize, DartSignal)]
pub struct ResolvePreviewRequest {
    /// 由 Dart 生成，[`ResolvePreviewResult.preview_id`] 原样回传用于匹配。
    pub preview_id: String,
    pub url: String,
    pub cookies: String,
    pub referrer: String,
    pub user_agent: String,
    pub extra_headers: std::collections::HashMap<String, String>,
}

/// [`ResolvePreviewRequest`] 的结果（Rust → Dart）。`items` 为空且 `error`
/// 为空 = 插件未返回清单（Dart 应回退普通单任务创建对话框）；`error` 非空 =
/// 预解析失败（同样回退，`error` 供 UI 提示）。
#[derive(Serialize, RustSignal)]
pub struct ResolvePreviewResult {
    pub preview_id: String,
    pub name: String,
    pub source_url: String,
    /// 无错误时为空。
    pub error: String,
    pub items: Vec<ManifestItemDto>,
}

/// [`ResolvePreviewResult.items`] 的单个清单条目。
#[derive(Serialize, SignalPiece)]
pub struct ManifestItemDto {
    /// 插件自定义标识，建组时按 `<id>` 或 `<id>@<variantId>` 拼进
    /// [`GroupItemEntry.resolver_item`]。
    pub id: String,
    pub name: String,
    /// 相对组根目录的子路径（空 = 根）。
    pub path: String,
    /// 已知大小（字节），未知为 0。
    pub size: i64,
    pub variants: Vec<ManifestVariantDto>,
}

/// [`ManifestItemDto.variants`] 的单个规格（画质/格式）。
#[derive(Serialize, SignalPiece)]
pub struct ManifestVariantDto {
    pub id: String,
    pub label: String,
    /// 已知大小（字节），未知为 0。
    pub size: i64,
}

/// Dart 请求建立多文件任务组（用户在预览对话框确认条目选择后发送）。
#[derive(Deserialize, DartSignal)]
pub struct CreateTaskGroup {
    /// 原始分享/清单链接（组行 `source_url`，展示/复制用）。
    pub source_url: String,
    /// 组名（空 = 组根目录直接用 `save_dir`）。
    pub group_name: String,
    /// 基础保存目录（组根目录 = `save_dir/sanitize(group_name)`）。
    pub save_dir: String,
    pub queue_id: String,
    pub segments: i32,
    pub cookies: String,
    pub referrer: String,
    pub user_agent: String,
    pub proxy_url: String,
    pub extra_headers: std::collections::HashMap<String, String>,
    pub ignore_tls_errors: bool,
    /// 稍后下载：true = 建组后不启动，待「启动队列」或用户手动恢复。
    pub start_paused: bool,
    pub items: Vec<GroupItemEntry>,
}

/// [`CreateTaskGroup.items`] 的单个组成员条目（用户在预览对话框勾选后的
/// 清单条目/规格投影）。
#[derive(Deserialize, SignalPiece)]
pub struct GroupItemEntry {
    /// 二段解析标识，按 `<itemId>` 或 `<itemId>@<variantId>` 拼接（见
    /// [`ManifestItemDto.id`]/[`ManifestVariantDto.id`]）。
    pub resolver_item: String,
    pub file_name: String,
    /// 相对组根目录的子路径（空 = 组根）。
    pub rel_path: String,
    /// 已知大小（字节，0 = 未知）。
    pub size: i64,
}

/// Dart 请求对一个任务组执行操作（暂停/恢复/重试失败/删除）。
#[derive(Deserialize, DartSignal)]
pub struct GroupControl {
    pub group_id: String,
    /// 0=pause, 1=resume, 2=retry_failed, 3=delete(记录+文件),
    /// 4=delete(仅记录)。
    pub action: i32,
}

/// Dart 请求重命名任务组。`name` trim 后为空则忽略。
#[derive(Deserialize, DartSignal)]
pub struct RenameGroup {
    pub group_id: String,
    pub name: String,
}

/// Dart 请求全量任务组快照（结果经 [`AllGroups`] 回传）。
#[derive(Deserialize, DartSignal)]
pub struct RequestAllGroups {}

/// 全量任务组快照（Rust → Dart）。组建/删除/改名/回收(GC)后发送。
#[derive(Serialize, RustSignal)]
pub struct AllGroups {
    pub groups: Vec<GroupInfo>,
}

/// 单个任务组元数据（[`AllGroups.groups`] 的成员）。
#[derive(Serialize, SignalPiece)]
pub struct GroupInfo {
    pub group_id: String,
    pub name: String,
    /// 原始分享/清单链接（展示/复制用）。
    pub source_url: String,
    /// 组根目录（子任务落盘 = 本值 + 清单条目的相对路径）。
    pub save_dir: String,
    /// Unix seconds 时间戳。
    pub created_at: String,
}
