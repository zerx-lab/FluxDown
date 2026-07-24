//! 引擎领域数据类型。
//!
//! 字段形状 1:1 复制自 `hub::signals` 中对应的 FFI DTO(参见每个类型上的
//! doc comment 溯源),但不带任何 `rinf` derive 宏——引擎不知道、也不依赖
//! Rinf/Dart 信号层。`hub` 侧通过 `signal_bridge` 模块的 `From`/`TryFrom`
//! 实现在这些类型与 `hub::signals::*` 之间转换。

/// 内置「主队列」的固定 ID。所有未显式指定队列的新任务归入此队列；
/// 不可删除、不可重命名（宿主 UI 按 ID 本地化显示名称）。
pub const MAIN_QUEUE_ID: &str = "main";

/// 内置「稍后下载」队列的固定 ID。默认停止；「稍后下载」入口在未选
/// 队列时落入此队列。不可删除、不可重命名。
pub const LATER_QUEUE_ID: &str = "later";

/// 判断队列 ID 是否为内置队列（`main`/`later`）。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::model::{is_builtin_queue, MAIN_QUEUE_ID};
/// assert!(is_builtin_queue(MAIN_QUEUE_ID));
/// assert!(!is_builtin_queue("some-uuid"));
/// ```
pub fn is_builtin_queue(queue_id: &str) -> bool {
    queue_id == MAIN_QUEUE_ID || queue_id == LATER_QUEUE_ID
}

/// 持久化任务信息。字段对应 `hub::signals::TaskInfo`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo {
    pub task_id: String,
    pub url: String,
    pub file_name: String,
    pub save_dir: String,
    /// 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
    pub status: i32,
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    pub error_message: String,
    /// Unix seconds 时间戳
    pub created_at: String,
    /// 单任务代理 URL(空 = 使用全局代理)。
    pub proxy_url: String,
    /// 命名队列 ID(空 = 默认队列)。
    pub queue_id: String,
    /// Checksum spec,格式 `algo=hexhash`(空 = 跳过校验)。
    pub checksum: String,
    /// 是否忽略 HTTPS 证书错误。默认 false；仅由用户为当前任务显式启用。
    pub ignore_tls_errors: bool,
    /// 文件跟踪：completed 任务的目标文件在磁盘上是否已丢失（被删除/移动）。
    /// 由引擎按需扫描计算并落库（见 `crate::download_manager::DownloadManager::spawn_file_scan`）；
    /// 仅对 status=3 语义有效，默认 false。
    pub file_missing: bool,
    /// 任务结束时间，Unix seconds 时间戳（空 = 尚未完成）。
    /// 记录下载真正完成（status→3）的时刻，不含插件 hook 后处理耗时。
    pub completed_at: String,
    /// 配置的分段（线程）数。0 = 自动（segment_advisor 动态计算）。
    /// 供 UI 展示与「创建后改线程数」编辑；与运行时实际分片数可能不同。
    pub segments: i32,
    /// 队列内启动顺序（越小越先启动）。0 = 未显式排序，按 `created_at`
    /// 先来先启动；>0 为显式顺序（`reorder_queue_tasks` 或建任务时追加）。
    pub queue_order: i32,
    /// 已上传字节数（BT 做种）。仅对 BT 任务有意义，默认 0。
    pub uploaded_bytes: i64,
    /// 下载完成时已上传字节数（BT 做种后分享率基准）。仅对 BT 任务有意义，默认 0。
    pub uploaded_at_completion: i64,
    /// Seeding status: 0=none, 1=active seeding, 2=ratio reached,
    /// 3=time reached, 4=user stopped, 5=task deleted, 6=session released,
    /// 7=inactive time reached.
    pub seeding_status: i32,
    /// BT 做种状态的辅助说明（如错误信息）。
    pub seeding_message: String,
    /// Source page URL captured by the browser extension (empty = none).
    pub referrer: String,
}

impl TaskInfo {
    /// 当前任务是否处于 BT 做种状态。
    ///
    /// 做种定义为：任务已完成（`status == 3`）且 `seeding_status == 1`。
    pub fn is_seeding(&self) -> bool {
        self.status == 3 && self.seeding_status == 1
    }

    /// 计算分享率（upload / download）。
    ///
    /// 当 `downloaded_bytes` 为 0 时返回 0.0，避免除零。
    pub fn seed_ratio(&self) -> f64 {
        if self.downloaded_bytes == 0 {
            0.0
        } else {
            self.uploaded_bytes as f64 / self.downloaded_bytes as f64
        }
    }
}

/// 命名队列元数据。字段对应 `hub::signals::QueueInfo`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueInfo {
    pub queue_id: String,
    pub name: String,
    /// 速度限制,KB/s。0 = 无限制。
    pub speed_limit_kbps: i64,
    /// 该队列内最大并发任务数。0 = 使用全局设置。
    pub max_concurrent: i32,
    /// 默认保存目录。空 = 使用全局默认值。
    pub default_save_dir: String,
    /// 显示顺序(越小越靠前)。
    pub position: i32,
    /// 新任务默认分段数。0 = 自动(全局 segment advisor)。
    pub default_segments: i32,
    /// 队列内任务默认 User-Agent。空 = 继承全局 UA。
    pub default_user_agent: String,
    /// 队列运行状态。停止的队列不自动启动其中任务（「稍后下载」与定时
    /// 调度的基础）；显式的单任务恢复/立即下载不受影响。
    pub is_running: bool,
    /// 定时计划是否启用。
    pub schedule_enabled: bool,
    /// 每日定时启动时间 `HH:MM`（空 = 不定时启动）。
    pub schedule_start: String,
    /// 每日定时停止时间 `HH:MM`（空 = 不定时停止）。
    pub schedule_stop: String,
    /// 定时生效的星期位掩码：bit0=周一 … bit6=周日；127 = 每天。
    pub schedule_days: i32,
}

/// 单个任务在队列中的位置。字段对应 `hub::signals::QueuePosition`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePosition {
    pub task_id: String,
    /// 1-based,0 = 不在队列中。
    pub position: i32,
}

/// 单个分段的字节范围与进度。字段对应 `hub::signals::SegmentDetail`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentDetail {
    pub index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
}

/// BT 种子内的单个文件条目。字段对应 `hub::signals::BtFileEntry`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtFileEntry {
    /// 种子内从 0 开始的文件索引。
    pub index: i32,
    /// 文件在种子内的相对路径(如 `"folder/sub/file.mp4"`)。
    pub path: String,
    /// 文件大小(字节)。
    pub size: i64,
}

/// HLS 可选码率变体。字段对应 `hub::signals::HlsQualityOption`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlsQualityOption {
    pub index: i32,
    pub bandwidth: i64,
    pub width: i64,
    pub height: i64,
}
/// 插件 resolve 返回的可选变体（画质/格式），供宿主弹框让用户选择。
/// 字段对应 `hub::signals::ResolveVariantOption`。含 `String`，非 `Copy`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveVariantOption {
    /// 变体在列表中的索引（从 0 开始）。
    pub index: i32,
    /// 展示标签（如 `"1080p MP4"`），由插件提供。
    pub label: String,
    /// 容器格式（如 `"mp4"`/`"webm"`），可为空。
    pub container: String,
    /// 码率（bps），未知为 0。
    pub bandwidth: i64,
    /// 视频宽度（像素），未知为 0。
    pub width: i64,
    /// 视频高度（像素），未知为 0。
    pub height: i64,
    /// 该变体的总字节数，未知为 0。
    pub total_bytes: i64,
}

/// 解析出的种子元数据(用于新建下载对话框预览)。
/// 字段对应 `hub::signals::TorrentMetaResult`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentMetaResult {
    /// 回显自请求方传入的 probe id,用于匹配响应。
    pub probe_id: String,
    /// 种子的展示名称(顶层 name 字段)。
    pub name: String,
    /// 种子内全部文件的总大小(字节)。
    pub total_bytes: i64,
    /// 解析出的文件列表,出错时为空。
    pub files: Vec<BtFileEntry>,
    /// 解析失败时非空。
    pub error: String,
}
