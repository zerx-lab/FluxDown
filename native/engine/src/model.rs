//! 引擎领域数据类型。
//!
//! 字段形状 1:1 复制自 `hub::signals` 中对应的 FFI DTO(参见每个类型上的
//! doc comment 溯源),但不带任何 `rinf` derive 宏——引擎不知道、也不依赖
//! Rinf/Dart 信号层。`hub` 侧通过 `signal_bridge` 模块的 `From`/`TryFrom`
//! 实现在这些类型与 `hub::signals::*` 之间转换。

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
    /// 文件跟踪：completed 任务的目标文件在磁盘上是否已丢失（被删除/移动）。
    /// 由引擎按需扫描计算并落库（见 `crate::download_manager::DownloadManager::spawn_file_scan`）；
    /// 仅对 status=3 语义有效，默认 false。
    pub file_missing: bool,
    /// 任务结束时间，Unix seconds 时间戳（空 = 尚未完成）。
    /// 记录下载真正完成（status→3）的时刻，不含插件 hook 后处理耗时。
    pub completed_at: String,
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
