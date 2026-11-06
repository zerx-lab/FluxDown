//! 引擎领域数据类型。
//!
//! 纯引擎数据模型，不依赖 UI 或 FFI。宿主负责转换为各自的 wire DTO；
//! 字段语义保持持久化数据与协议兼容。

/// 内置「主队列」的固定 ID。所有未显式指定队列的新任务归入此队列；
/// 不可删除、不可重命名（宿主 UI 按 ID 本地化显示名称）；始终运行，
/// 不可停止、不可定时（见 [`is_always_running_queue`]）。
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

/// 队列是否恒运行：主队列是新建下载的默认目标，停掉它会让新任务与
/// 「全部恢复」静默失效，所以不可停止、不可设每日定时（启动时由
/// [`crate::db::Db::seed_builtin_queues`] 迁移存量停止 / 定时状态）。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::model::{is_always_running_queue, LATER_QUEUE_ID, MAIN_QUEUE_ID};
/// assert!(is_always_running_queue(MAIN_QUEUE_ID));
/// assert!(!is_always_running_queue(LATER_QUEUE_ID));
/// ```
pub fn is_always_running_queue(queue_id: &str) -> bool {
    queue_id == MAIN_QUEUE_ID
}

/// Source-provided archive classification hints. These are descriptive metadata only:
/// never credentials, URLs, or filesystem paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArchiveMetadata {
    pub platform: String,
    pub media_kind: String,
    pub creator_id: String,
    pub creator_name: String,
    pub series_id: String,
    pub series_title: String,
    pub collection_id: String,
    pub collection_title: String,
}

/// Context passed to archive-folder plugins.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveContext {
    pub file_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ArchiveMetadata>,
    pub is_group: bool,
    /// Bounded hexadecimal file prefix for host-side signed rule matching.
    /// Never serialized to arbitrary plugin scripts.
    #[serde(skip)]
    pub header_hex: Option<String>,
}

/// Validated archive destination returned by a plugin, with legacy-shape provenance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveDirectories {
    pub components: Vec<String>,
    pub legacy: bool,
}

/// 持久化任务信息。
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
    /// 落盘文件的哈希记录，格式 `algo=hexhash`（空 = 尚未计算）。完成时由校验和
    /// 校验或「下载完成后计算哈希」写入，也可按需计算；重新下载时清空。
    pub file_hash: String,
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
    /// 7=inactive time reached, 8=queued for a seeding slot.
    pub seeding_status: i32,
    /// BT 做种状态的辅助说明（如错误信息）。
    pub seeding_message: String,
    /// 累计做种秒数（活跃做种期间累加；排队/暂停不计）。
    pub seeding_time_secs: i64,
    /// 任务级总分享率上限（千分比：1500 = 1.5）。-2 = 跟随全局，
    /// -1 = 不限制，>=0 = 自定义（0 视同不限制）。
    pub seed_ratio_limit_milli: i64,
    /// 任务级做种后分享率上限（千分比）。哨兵语义同上。
    pub seed_post_ratio_limit_milli: i64,
    /// 任务级做种时长上限（分钟）。哨兵语义同上。
    pub seed_time_limit_minutes: i64,
    /// 任务级不活跃做种时长上限（分钟）。哨兵语义同上。
    pub seed_inactive_time_limit_minutes: i64,
    /// 任务级做种上传限速（B/s）。0 = 无单任务限制，>0 = 自定义。
    /// 在 torrent 加入 librqbit 会话时烘焙生效（下载开始 / 重启续种 /
    /// 句柄丢失后重新挂载）；已 live 的句柄不热改。
    pub seed_upload_limit_bps: i64,
    /// BT 选片：按文件顺序下载（对齐 qBittorrent「按顺序下载」）。默认 false = 最稀有优先。
    pub bt_sequential: bool,
    /// BT 选片：先下载每个选中文件首尾各 1% 的块（对齐 qBittorrent「先下载首尾文件块」）。
    pub bt_first_last_pieces_first: bool,
    /// Source page URL captured by the browser extension (empty = none).
    pub referrer: String,
    /// 所属任务组 ID（空 = 不属于任何组）。多文件任务组裂变/建组时写入；
    /// `resolver_item`（二段解析标识）不进本结构，走专用 getter
    /// [`crate::db::Db::get_task_resolver_item`]（与 `resolver_plugin_id` 惯例一致）。
    pub group_id: String,
    /// 由哪条 RSS 订阅自动创建（空 = 非 RSS 来源）。任务详情「来源」行据此
    /// 反查订阅（设计文档 P5 / qB#19276）。
    pub rss_source_id: String,
    /// 展示用的原始来源链接（空 = 用 `url`)。`.torrent` 文件任务的 `url` 是
    /// `torrent-file://local` 哨兵(真正的内容在 `torrent_files` 表),对用户
    /// 毫无意义;RSS 自动建任务时把 enclosure 直链存在这里,右键「复制下载
    /// 链接」才有东西可复制。
    pub origin_url: String,
    /// `ProxyMode::Auto` 的任务级最终链路（可追溯性）。wire 标签见
    /// [`crate::auto_proxy::route`]：`direct` / `direct:sampled` /
    /// `direct:pinned` / `proxy:cached` / `proxy:sampled` / `proxy:failover`；
    /// 空 = 非 Auto 模式或任务从未启动。
    pub auto_route: String,
    /// 加速来源累计字节（跨运行累加，进度复位时随之清零）。源站字节 =
    /// `downloaded_bytes` − 三者之和。
    pub source_bytes: SourceBytes,
}

/// 按加速来源累计的已写入字节。
///
/// 只覆盖源站主链路之外的路径（多 CDN 钉定节点、Auto 代理候选链路、多网卡
/// 链路）；源站字节不单独计数，由 `downloaded_bytes` 减去三者之和得到。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceBytes {
    /// 多 CDN 钉定节点（额外源站 IP）。
    pub cdn: i64,
    /// `ProxyMode::Auto` 下走候选代理路径。
    pub proxy: i64,
    /// 多网卡聚合挂入的额外网卡链路。
    pub nic: i64,
}

impl SourceBytes {
    /// 三项均为 0。
    pub fn is_zero(&self) -> bool {
        self.cdn == 0 && self.proxy == 0 && self.nic == 0
    }

    /// 逐项饱和相加。
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            cdn: self.cdn.saturating_add(other.cdn),
            proxy: self.proxy.saturating_add(other.proxy),
            nic: self.nic.saturating_add(other.nic),
        }
    }

    /// 逐项饱和相减（可能为负；调用方以「累计值 − 已落库值」使用，结果按 `max(0)` 截断）。
    pub fn saturating_sub(self, other: Self) -> Self {
        Self {
            cdn: self.cdn.saturating_sub(other.cdn).max(0),
            proxy: self.proxy.saturating_sub(other.proxy).max(0),
            nic: self.nic.saturating_sub(other.nic).max(0),
        }
    }

    /// 逐项取较大值。
    pub fn max(self, other: Self) -> Self {
        Self {
            cdn: self.cdn.max(other.cdn),
            proxy: self.proxy.max(other.proxy),
            nic: self.nic.max(other.nic),
        }
    }
}

/// 命名队列元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueInfo {
    pub queue_id: String,
    pub name: String,
    /// 速度限制,KB/s。0 = 无限制。
    pub speed_limit_kbps: i64,
    /// 上传速度限制,KB/s。0 = 无限制（BT add/re-add 时折算生效）。
    pub upload_limit_kbps: i64,
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

/// 单个任务在队列中的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePosition {
    pub task_id: String,
    /// 1-based,0 = 不在队列中。
    pub position: i32,
    /// 引擎排序键：任务在队列期间保持不变，队首出队不改变其余任务的键。
    pub order: i64,
}

/// 队列顺序增量里新增 / 更新的一项：任务及其排序键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueOrderEntry {
    pub task_id: String,
    pub order: i64,
}

/// 单个分段的字节范围与进度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentDetail {
    pub index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
}

/// 多 CDN 并发的单节点描述（`EngineEvent::TaskCdnEvent` 载荷）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdnNodeInfo {
    /// 节点 IP；SYS 兜底节点（系统 DNS、无钉定）为 `"SYS"`。
    pub ip: String,
    /// 候选来源标记：`"sys"`（系统 DNS）/ `"doh:<端点IP>"` / `"ecs:<端点IP>"`；
    /// SYS 节点为空串。UI 侧据此本地化展示。
    pub origin: String,
    /// 本任务经该节点实际下载的字节数（`kind="summary"` 有效，其余事件为 0）。
    pub bytes: i64,
    /// EWMA 吞吐（字节/秒）：`kind="pool"` = 健康度先验；`kind="summary"` =
    /// 任务结束时实测。
    pub ewma_bps: i64,
    /// 当前未归还的段租约数（`kind="leases"` 快照有效，其余事件为 0）。
    pub active: i32,
}

/// BT 种子内的单个文件条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtFileEntry {
    /// 种子内从 0 开始的文件索引。
    pub index: i32,
    /// 文件在种子内的相对路径(如 `"folder/sub/file.mp4"`)。
    pub path: String,
    /// 文件大小(字节)。
    pub size: i64,
}

/// HLS 可选码率变体。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlsQualityOption {
    pub index: i32,
    pub bandwidth: i64,
    pub width: i64,
    pub height: i64,
}
/// 插件 resolve 返回的可选变体（画质/格式），供宿主弹框让用户选择。
/// 含 `String`，非 `Copy`。
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

/// 任务组元数据（多文件任务组的壳，纯逻辑聚合层——不参与调度/限速，见
/// `docs/multi-file-task-group-design.md` §4.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInfo {
    pub group_id: String,
    pub name: String,
    /// 原始分享/清单链接（展示/复制用）。
    pub source_url: String,
    /// 组根目录（`base_save_dir/sanitize(name)`），子任务落盘 = 本值 +
    /// 清单条目的相对路径。
    pub save_dir: String,
    /// Unix seconds 时间戳。
    pub created_at: String,
}

/// 预解析清单的单个条目（供 [`crate::events::EngineEvent::ResolvePreviewReady`]
/// 展示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestItemInfo {
    /// 插件自定义标识，回传 `resolver_item` 用（不透明字符串，引擎不解释语义）。
    pub id: String,
    pub name: String,
    /// 相对组根目录的子路径（空 = 根）。
    pub path: String,
    /// 已知大小（字节），未知为 0。
    pub size: i64,
    /// 可选规格（画质/格式），空 = 无规格选择。
    pub variants: Vec<ManifestVariantInfo>,
}

/// [`ManifestItemInfo::variants`] 的单个规格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestVariantInfo {
    pub id: String,
    pub label: String,
    /// 已知大小（字节），未知为 0。
    pub size: i64,
}
