//! 纯下载 daemon 的传输无关 DTO。

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
/// 内置主队列的稳定 wire ID。
pub const MAIN_QUEUE_ID: &str = "main";
/// 内置“稍后下载”队列的稳定 wire ID。
pub const LATER_QUEUE_ID: &str = "later";

/// 交互选择的种类。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SelectionKind {
    Hls {
        options: Vec<HlsQualityOptionDto>,
    },
    Bt {
        files: Vec<BtFileDto>,
    },
    Variant {
        options: Vec<ResolveVariantOptionDto>,
    },
    /// 保存目录里已有同名最终文件（`file_exists_behavior = ask`）。字段全部由主机算好：
    /// 远程客户端看不到主机文件系统。
    #[serde(rename_all = "camelCase")]
    FileExists {
        /// 发生冲突的目标文件名。
        file_name: String,
        /// 主机上的保存目录（只用于展示）。
        save_dir: String,
        /// 已有文件大小（字节）；读取失败时缺省。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        existing_size: Option<u64>,
        /// 已有文件修改时间（Unix 毫秒）；读取失败时缺省。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        existing_modified_unix_ms: Option<i64>,
        /// 新下载的大小（字节）；未知时缺省。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        incoming_size: Option<i64>,
        /// 选「重命名」时的预览名（询问时刻计算，最终名可能不同）。
        rename_preview: String,
        /// 该任务允许的动作；恒含 `rename`。
        actions: Vec<FileExistsAction>,
    },
    /// 多个已启用插件的 resolver 同时匹配该任务链接，且该站点没有可用的默认插件：
    /// 由用户选择本次使用的插件，可选记为站点默认插件。
    #[serde(rename_all = "camelCase")]
    Plugin {
        /// 站点键（链接主机名，小写、去掉 `www.` 前缀）；也是站点默认插件的键。
        site: String,
        /// 任务原始链接（只用于展示）。
        url: String,
        /// 候选插件，按 identity 升序；默认项见 `default_choice`。
        candidates: Vec<PluginCandidateDto>,
    },
}

/// 多插件选择的候选项（取自插件 manifest，主机侧算好，远程客户端无需再查插件列表）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginCandidateDto {
    pub identity: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
}

/// 站点默认插件（`daemon.plugin.siteDefaults` 列表项）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginSiteDefaultDto {
    /// 站点键（主机名，小写、去掉 `www.` 前缀）。
    pub site: String,
    /// 该站点使用的插件。
    pub identity: String,
    /// 记录时的冲突插件集合（升序）；出现集合外的新插件时会重新询问。
    pub candidates: Vec<String>,
    /// `true` = 无人值守下载（RSS / 远程派发 / 免打扰跳过二次选择）自动选用、尚未经用户
    /// 确认；有界面的下载仍会询问。`false` = 用户确认过的站点默认插件。
    pub automatic: bool,
}

/// `daemon.plugin.setSiteDefault`：`identity` 为 `None` 时删除该站点记录；否则把该站点
/// 确认为使用 `identity`（必须是记录里的候选插件之一）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginSetSiteDefaultParams {
    pub site: String,
    #[serde(default)]
    pub identity: Option<String>,
}

/// 文件已存在时可选的处理动作。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum FileExistsAction {
    /// 自动追加序号另存。
    Rename,
    /// 下载完成后替换旧文件（失败或取消不会动旧文件）。
    Overwrite,
    /// 不下载，任务直接记为完成。
    Skip,
}

/// 引擎可接受的类型化选择结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SelectionOutcome {
    Hls {
        index: i32,
    },
    Bt {
        indices: Vec<i32>,
    },
    Variant {
        index: i32,
    },
    FileExists {
        action: FileExistsAction,
    },
    /// 应答 [`SelectionKind::Plugin`]：`remember` = 记为该站点的默认插件，以后不再询问。
    Plugin {
        identity: String,
        remember: bool,
    },
    Cancelled,
}

/// daemon 向已订阅连接发布的待选择请求。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SelectionRequestDto {
    pub request_id: String,
    pub task_id: String,
    pub kind: SelectionKind,
    pub default_choice: SelectionOutcome,
    pub deadline_unix_ms: i64,
}

/// 客户端对待选择请求的终局答复。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SelectionResolutionDto {
    pub request_id: String,
    pub outcome: SelectionOutcome,
}

/// 原子 daemon 配置投影。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonConfigSnapshot {
    pub revision: u64,
    pub values: BTreeMap<String, String>,
}

/// 带乐观并发版本的 daemon 配置补丁。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonConfigPatch {
    pub expected_revision: u64,
    pub values: BTreeMap<String, String>,
}

/// daemon 运行状态投影。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonRuntimeStatsDto {
    pub active_tasks: u32,
    pub pending_tasks: u32,
    pub total_download_bps: i64,
    pub total_upload_bps: i64,
    pub disk_free_bytes: Option<u64>,
    pub save_dir: String,
    /// 「等待自动重试」的任务数：自动重试 / 备用链路 / 插件重试已排程、尚未重新派发。
    /// 这些任务在任务表里仍是失败状态，`activeTasks` / `pendingTasks` 看不到它们；
    /// 「完成后关机」「空闲退出」等联动必须把它们算作仍有工作。
    #[serde(default)]
    pub retry_pending_tasks: u32,
}

/// daemon 托管组件标识。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum ComponentKind {
    Ffmpeg,
    Ytdlp,
}

/// 查询或卸载一个托管组件。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ComponentParams {
    pub component: ComponentKind,
}

/// 安装或更新一个托管组件。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ComponentInstallParams {
    pub component: ComponentKind,
    #[serde(default)]
    pub version: Option<String>,
}

/// daemon 受管组件的类型化状态。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "component", content = "status", rename_all = "camelCase")]
pub enum ComponentStatusDto {
    Ffmpeg(ComponentFfmpegStatus),
    Ytdlp(ComponentYtdlpStatus),
}

/// `daemon.task.create` 参数。大种子文件通过一次性 blob 引用传递。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonCreateTaskParams {
    pub request: CreateTaskRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_blob_id: Option<String>,
    #[serde(default)]
    pub unattended: bool,
    /// 调用方已知的文件大小（字节，`>0` 才生效；缺省 / 非正数 = 未知）。
    /// 浏览器扩展对一次性 URL 已拿到 Content-Length 时透传，引擎据此创建任务。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint_file_size: Option<i64>,
    /// 参照文件名：调用方已知的原始文件名（浏览器捕获时浏览器定下的名字）。仅当
    /// `request.fileName` 是缺扩展名的显式名（用户改名）时，引擎借它的扩展名补全，
    /// 主名不变；缺省 / 空串 = 无参照。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hint: Option<String>,
    /// 分类目录路由：调用方未替用户选定保存目录时（外部捕获）携带，引擎据此按初始名与
    /// 下载完成时的最终文件名把产物放进命中分类的目录；`request.saveDir` 为路由未命中时
    /// 的基准目录（空 = daemon 默认）。缺省 = 保存目录即最终目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_route: Option<crate::agent::CategoryRouteDto>,
}

/// `daemon.task.seedLocal` 参数：把 daemon 主机磁盘上已有的文件 / 文件夹登记为已完成并开始做种。
/// 路径都指 daemon 所在主机上的绝对路径。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonSeedLocalParams {
    /// 要做种的文件或文件夹（绝对路径）。
    pub path: String,
    /// 现成的 `.torrent` 文件路径；缺省 / 空 = 由 daemon 为 `path` 现场生成种子。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_path: Option<String>,
    /// 生成种子时写入的 tracker（每项一条 announce URL）；给了 `torrentPath` 时忽略。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trackers: Vec<String>,
    /// `true` = 该任务永不因分享率 / 时长 / 不活跃限制停止做种（任务级限制全部「不限制」）。
    #[serde(default)]
    pub unlimited: bool,
}

/// `daemon.task.seedLocal` 结果。`duplicate = true` 表示同一个种子（info hash）已被另一任务
/// 持有，**未**创建新任务：`taskId` 是已有任务，`existingName` 是它的显示名（可能为空）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SeedLocalResultDto {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub duplicate: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub existing_name: String,
}

/// `daemon.task.btStatus` 参数。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonBtStatusParams {
    pub task_id: String,
}

/// tracker 汇报状态（对齐 qBittorrent：工作 / 更新中 / 未工作 / 未联系）。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum BtTrackerStatus {
    /// 最近一次 announce 成功。
    Working,
    /// 监控已启动，尚无结果。
    Updating,
    /// 最近一次 announce 失败，原因见 `message`。
    NotWorking,
    /// 任务当前没有在向它汇报（暂停 / 未运行）。
    #[default]
    NotContacted,
    /// 宿主未提供 tracker 状态采集（旧宿主 / 嵌入式宿主）。
    Unknown,
}

/// tracker 来源。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum BtTrackerSource {
    /// 种子 / 磁力链接自带。
    #[default]
    Torrent,
    /// 会话按设置追加（自定义 / 内置公共 / 订阅）。
    Session,
}

/// 一个 tracker 行。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtTrackerDto {
    pub url: String,
    pub source: BtTrackerSource,
    pub status: BtTrackerStatus,
    /// 最近一次成功 announce 返回的 peer 数；未知缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peers: Option<u64>,
    /// 失败原因的简短归类（qBittorrent 风格，如 `Timed out`）；非失败为空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// 失败的原始错误文本（供「复制错误」）；非失败为空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// 最近一次成功的 unix 秒；0 = 从未成功。
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub last_ok_at: i64,
    /// 预计下次 announce 的 unix 秒；0 = 未知。
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub next_announce_at: i64,
}

/// 一个已连接 peer。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtPeerDto {
    /// `ip:port`。
    pub addr: String,
    /// 对端客户端名（握手上报，可能为空）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client: String,
    /// `tcp` / `utp` / `socks`。
    pub transport: String,
    /// 从该 peer 下载的字节。
    pub downloaded: u64,
    /// 上传给该 peer 的字节。
    pub uploaded: u64,
    /// 下载速度（字节/秒），按相邻两次采样折算。
    pub down_bps: u64,
    /// 上传速度（字节/秒）。
    pub up_bps: u64,
    /// peer 所在国家（ISO 3166-1 两位大写码）；国家库未就绪 / 内网 / 未收录缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// 防吸血 IP 组文本（IPv4 `1.2.3.4`，IPv6 `2001:db8::/64`）；防吸血未启用 / 旧 daemon 缺省为空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ip_group: String,
    /// 该 IP 组在本任务累计送达给我方的字节。
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub group_delivered_bytes: u64,
    /// 嫌疑等级：`none` / `weak` / `medium` / `strong`；未评估缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspicion: Option<String>,
    /// 命中的规则 id（`excessive` / `rerequest` / `rewind` / `lag` / `fingerprint` / `cloud`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    /// 当前处置：`allow` / `throttle` / `choke` / `banned`。
    #[serde(default = "default_peer_action")]
    pub action: String,
    /// `true` = 命中规则但处于「仅观察」，`action` 为本应执行的动作而未真正执行。
    #[serde(default, skip_serializing_if = "is_false")]
    pub observe_only: bool,
}

impl Default for BtPeerDto {
    fn default() -> Self {
        Self {
            addr: String::new(),
            client: String::new(),
            transport: String::new(),
            downloaded: 0,
            uploaded: 0,
            down_bps: 0,
            up_bps: 0,
            country: None,
            ip_group: String::new(),
            group_delivered_bytes: 0,
            suspicion: None,
            rules: Vec::new(),
            action: default_peer_action(),
            observe_only: false,
        }
    }
}

fn default_peer_action() -> String {
    BT_GUARD_PEER_ACTION_ALLOW.to_owned()
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// [`BtPeerDto::action`] 取值：放行。
pub const BT_GUARD_PEER_ACTION_ALLOW: &str = "allow";
/// 过滤条目所在层（优先级 allow > manual > auto / cloud / file）。
pub const BT_GUARD_LAYERS: &[&str] = &["allow", "manual", "auto", "cloud", "file"];
/// 过滤条目动作。
pub const BT_GUARD_ENTRY_ACTIONS: &[&str] = &["allow", "ban", "choke", "throttle"];
/// 规则 id（事件 / 条目 / 证据共用）。
pub const BT_GUARD_RULES: &[&str] = &[
    "excessive",
    "rerequest",
    "rewind",
    "lag",
    "fingerprint",
    "cloud",
];
/// 条目 / 事件列表单页上限。
pub const BT_GUARD_LIST_MAX_LIMIT: u32 = 500;
/// 条目 / 事件列表默认页大小。
pub const BT_GUARD_LIST_DEFAULT_LIMIT: u32 = 100;
/// `daemon.bt.guard.entries.remove` 单次 id 数上限。
pub const BT_GUARD_REMOVE_MAX_IDS: usize = 1000;

fn default_guard_limit() -> u32 {
    BT_GUARD_LIST_DEFAULT_LIMIT
}

/// 今日（本地日）统计。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardStatsDto {
    /// 仅观察命中数。
    pub today_observed: u64,
    /// 已执行处置数。
    pub today_enforced: u64,
    /// 因封禁被拒绝的连接数。
    pub today_blocked_connections: u64,
    /// choke / throttle 丢弃或受限的上传请求字节之和。
    pub today_refused_bytes: u64,
}

/// 云名单状态。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardCloudStatusDto {
    /// 当前已应用文档序号；0 = 无。
    pub sequence: u64,
    /// 文档应用时间（unix 秒）；0 = 无。
    pub updated_at: i64,
    /// 文档过期时间（unix 秒）；0 = 无。
    pub expires_at: i64,
    /// 云层 IP 条目数。
    pub entries: u32,
    /// 客户端指纹规则数。
    pub client_rules: u32,
    /// 文档已过期，云层停用。
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// 最近一份被拒绝文档的原因；成功应用后清空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_reason: Option<String>,
    /// 文档要求展示的署名行（CC BY 4.0 等）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attribution: Vec<String>,
}

/// 过滤文件订阅状态。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardFilterSourceDto {
    pub url: String,
    pub ok: bool,
    pub entries: u32,
    /// 最近一次成功时间（unix 秒）；0 = 从未成功。
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 各层未过期条目数。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardCountsDto {
    pub allow: u32,
    pub manual: u32,
    pub auto: u32,
    pub cloud: u32,
    pub file: u32,
}

/// `daemon.bt.guard.state` 结果。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardStateDto {
    /// `bt_guard_enabled`。
    pub enabled: bool,
    /// BT 会话存活（防吸血仅在会话内生效）。
    pub session_active: bool,
    pub stats: BtGuardStatsDto,
    pub cloud: BtGuardCloudStatusDto,
    pub filter_sources: Vec<BtGuardFilterSourceDto>,
    pub counts: BtGuardCountsDto,
}

/// 一条过滤条目。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEntryDto {
    pub id: i64,
    /// 见 [`BT_GUARD_LAYERS`]。
    pub layer: String,
    /// `1.2.3.4` / `1.2.3.0/24` / `2001:db8::/64`。
    pub cidr: String,
    /// 见 [`BT_GUARD_ENTRY_ACTIONS`]。
    pub action: String,
    /// 触发规则 id（见 [`BT_GUARD_RULES`]）；手动条目为 `manual`、订阅文件条目为 `file`；其余可为空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rule: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// 证据摘要（JSON）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub evidence: Option<Value>,
    /// 来源（`auto` / `user` / 订阅 URL / `cloud:community` 等）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    pub created_at: i64,
    /// 到期 unix 秒；缺省 = 永久。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// 距到期的剩余秒数（服务端按当前时间折算，≥ 0）；永久缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_secs: Option<i64>,
}

/// `daemon.bt.guard.entries.list` 参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEntriesListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// 对 cidr / reason 的子串搜索。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// 只返回 CIDR 覆盖该 IP 的条目（含云 / 订阅层的宽网段）；与其他过滤叠加。非法 IP → `invalidArgument`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains_ip: Option<String>,
    #[serde(default)]
    pub offset: u32,
    /// 1..=[`BT_GUARD_LIST_MAX_LIMIT`]，缺省 [`BT_GUARD_LIST_DEFAULT_LIMIT`]。
    #[serde(default = "default_guard_limit")]
    pub limit: u32,
}

impl Default for BtGuardEntriesListParams {
    fn default() -> Self {
        Self {
            layer: None,
            rule: None,
            query: None,
            contains_ip: None,
            offset: 0,
            limit: BT_GUARD_LIST_DEFAULT_LIMIT,
        }
    }
}

/// `daemon.bt.guard.entries.list` 结果。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEntriesListDto {
    /// 满足过滤条件的总数（不受分页影响）。
    pub total: u64,
    pub items: Vec<BtGuardEntryDto>,
}

/// `daemon.bt.guard.entries.add` 参数。`action == allow` 进白名单层，其余进手动层。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEntriesAddParams {
    pub cidr: String,
    /// 见 [`BT_GUARD_ENTRY_ACTIONS`]。
    pub action: String,
    /// 有效期秒数；缺省 = 永久。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<i64>,
    #[serde(default)]
    pub reason: String,
}

/// `daemon.bt.guard.entries.remove` 参数。云层条目拒绝删除（`invalidArgument`），应改为加白。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEntriesRemoveParams {
    pub ids: Vec<i64>,
}

/// `daemon.bt.guard.entries.remove` 结果。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEntriesRemoveDto {
    pub removed: u32,
}

/// 事件日志一行。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEventDto {
    pub id: i64,
    pub at: i64,
    /// 事件类型：`observed` / `enforced` / `probe` / `probe_cleared` / `entry_added` /
    /// `entry_removed` / `cloud_applied` / `cloud_rejected` / `filter_applied`。
    pub kind: String,
    pub ip_group: String,
    /// 种子 infohash（40 hex）；无关种子的事件为空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub info_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rule: String,
    /// 规则当时的模式（`observe` / `enforce`）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// `daemon.bt.guard.events.list` 参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEventsListParams {
    #[serde(default)]
    pub offset: u32,
    #[serde(default = "default_guard_limit")]
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// 只返回该对端组的事件。可以是 IP（`1.2.3.4`、`2001:db8::1`）或组文本（`2001:db8::/64`）；
    /// daemon 按当前 IPv6 前缀规范化后精确匹配。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_group: Option<String>,
}

impl Default for BtGuardEventsListParams {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: BT_GUARD_LIST_DEFAULT_LIMIT,
            rule: None,
            kind: None,
            ip_group: None,
        }
    }
}

/// `daemon.bt.guard.events.list` 结果（按时间降序）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEventsListDto {
    pub total: u64,
    pub items: Vec<BtGuardEventDto>,
}

/// `daemon.bt.guard.export` 参数。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardExportParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
}

/// `daemon.bt.guard.export` 结果：每行 `cidr # layer rule reason expires`。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardExportDto {
    pub text: String,
    pub lines: u32,
}

/// `daemon.bt.guard.filterSources.refresh` 结果。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardFilterSourcesRefreshDto {
    pub sources: Vec<BtGuardFilterSourceDto>,
}

/// `daemon.bt.guard.cloud.apply` 参数（仅 agent 调用，agent 已验签）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardCloudApplyParams {
    /// 原始签名信封 JSON 文本（仅校验长度；daemon 不保存，agent 自己缓存并在重启后重放）。
    pub envelope: String,
    pub sequence: u64,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub verified_payload: Value,
}

/// `daemon.bt.guard.cloud.apply` 结果。拒绝（序号回滚 / 差异防护等）不是 RPC 错误：保留上一份并
/// 以 `rejectedReason` 说明。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardCloudApplyDto {
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_reason: Option<String>,
    pub status: BtGuardCloudStatusDto,
}

/// 待上报的一条证据（字段与 FluxCloud `POST /api/v1/bt-guard/evidence` 的 item 一致）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEvidenceItemDto {
    pub ip_group: String,
    /// `excessive` / `rerequest` / `rewind` / `lag` / `fingerprint`。
    pub rule: String,
    /// `strong` / `medium` / `weak`。
    pub strength: String,
    /// 40 位十六进制 infohash（FluxCloud 收到后立即 HMAC）。
    pub info_hash: String,
    pub torrent_size: u64,
    pub delivered_bytes: u64,
    pub dup_bytes: u64,
    pub ratio_milli: u64,
    pub rerequests: u32,
    pub rewind_permille: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub peer_id_prefix: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client: String,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// `daemon.bt.guard.evidence.drain` 结果。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtGuardEvidenceDrainDto {
    pub items: Vec<BtGuardEvidenceItemDto>,
}

/// peer 数量汇总。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtPeerCountsDto {
    pub live: u32,
    pub connecting: u32,
    pub queued: u32,
    pub seen: u32,
    pub dead: u32,
}

/// `daemon.task.btStatus` 结果：任务的 peer 与 tracker 实况。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtStatusDto {
    /// DHT 是否启用（影响无 tracker 时的发现能力）。
    pub dht_enabled: bool,
    pub peer_counts: BtPeerCountsDto,
    pub trackers: Vec<BtTrackerDto>,
    pub peers: Vec<BtPeerDto>,
}
/// `daemon.task.torrentShare` 参数。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonTorrentShareParams {
    pub task_id: String,
    /// `true` 时结果带 `.torrent` 文件内容（base64）；只要磁力链接时留 `false`。
    #[serde(default)]
    pub include_torrent: bool,
}

/// `daemon.task.torrentShare` 结果：把 BT 任务分享给别人所需的材料。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TorrentShareDto {
    /// 小写十六进制 info hash。
    pub info_hash: String,
    /// 种子名（磁力任务尚无元数据时可能为空）。
    pub name: String,
    /// 磁力链接：含种子自带 tracker 与本机会话追加的 tracker。
    pub magnet: String,
    /// `.torrent` 内容（base64）；未请求或库里没有种子字节（纯磁力任务）时缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_b64: Option<String>,
}

/// `daemon.task.createMany` 单次调用的条目上限；客户端须按块拆分更大的导入。
pub const CREATE_TASKS_MAX_ITEMS: usize = 512;
/// `daemon.task.createMany` 幂等键的最大长度（字节）。
pub const CREATE_TASKS_BATCH_ID_MAX_LEN: usize = 128;

/// `daemon.task.createMany` 参数：一块链接共享同一份建任务选项，整块一个事务落库。
///
/// 幂等：同一 `batchId` 已成功落库时，重放直接返回首次结果、不再建任务；连接中断等
/// 结果不确定的调用可以原样重发。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonCreateTasksParams {
    /// 调用方为每块生成的唯一键（非空，不超过 [`CREATE_TASKS_BATCH_ID_MAX_LEN`] 字节）。
    pub batch_id: String,
    /// 整块共享的选项。`url` / `fileName` / `checksum` 由条目决定，这里必须为空；
    /// 不支持种子、音轨与自定义 method / body（这些只走 `daemon.task.create`）。
    pub common: CreateTaskRequest,
    /// 1..=[`CREATE_TASKS_MAX_ITEMS`] 条。
    pub items: Vec<CreateTaskItem>,
    /// 分类目录路由，对整块每一条生效，语义同 [`DaemonCreateTaskParams::category_route`]；
    /// 缺省 = 保存目录即最终目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_route: Option<crate::agent::CategoryRouteDto>,
}

/// `daemon.task.createMany` 的单条链接。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskItem {
    pub url: String,
    /// 空 = 从 URL / Content-Disposition 推断；允许 `子目录/文件名`。
    #[serde(default)]
    pub file_name: String,
    /// Checksum spec，格式 `algo=hexhash`（空 = 跳过校验）。
    #[serde(default)]
    pub checksum: String,
}

/// `daemon.task.createMany` 结果：`results` 与请求 `items` 逐条同序对应。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonCreateTasksResult {
    pub results: Vec<CreateTaskOutcome>,
}

/// 单条建任务结果：成功带 `taskId`（开启 URL 去重时可能是已有任务），失败带 `error`。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskOutcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::RpcErrorData>,
}

/// 批量任务操作参数（`daemon.task.pauseMany` / `daemon.task.resumeMany`）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonTaskIdsParams {
    #[serde(default)]
    pub task_ids: Vec<String>,
}

/// `daemon.task.deleteMany` 参数。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonDeleteTasksParams {
    #[serde(default)]
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub delete_files: bool,
}

/// CDN 样本的持久化租约。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CdnReportLeaseDto {
    pub batch_id: String,
    pub samples: Vec<Value>,
}

/// 确认一个 CDN 样本租约。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CdnReportAckParams {
    pub batch_id: String,
}

/// `daemon.egress.plan` 参数。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct EgressPlanParams {
    /// 要请求的原始地址。
    pub url: String,
    /// 调用方会按独立来源的哈希 / 签名校验响应内容：只有这类载荷才允许插件改写地址。
    /// 校验和、索引、版本清单等完整性锚必须为 `false`，否则插件可同时替换载荷与其校验和。
    #[serde(default)]
    pub verified_payload: bool,
}

/// 出口代理。`url` 可能含认证信息，只在本机服务间传递，不得写日志。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct EgressProxyDto {
    /// 完整代理地址（`http://`、`https://`、`socks4://`、`socks5h://` …）。
    pub url: String,
    /// 已规范化的直连名单（逗号分隔，reqwest `NoProxy` 语法）；空 = 无例外。
    #[serde(default)]
    pub no_proxy: String,
}

/// `daemon.egress.plan` 结果：下载任务同一套代理与插件改写规则得出的请求方案。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct EgressPlanDto {
    /// 首选地址：插件改写后的直链；未改写时等于原地址。
    pub url: String,
    /// 改写插件要求附带的请求头（只对改写后的地址生效）。
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 改写地址的插件 identity；`None` = 未改写。改写地址失败或内容校验不符时调用方必须回退原地址。
    #[serde(default)]
    pub plugin: Option<String>,
    /// 出口代理；`None` = 直连，调用方不得再套用环境 / 系统代理。
    #[serde(default)]
    pub proxy: Option<EgressProxyDto>,
}

/// 云端 CDN 先验配置经 agent 专用边界下发的可写键。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CdnConfigApplyParams {
    pub values: BTreeMap<String, String>,
}

/// 旧宿主设备链接数据的一次性迁移结果。敏感内容只允许在鉴权 RPC 内传输。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkMigrationExport {
    pub revision: u64,
    pub identity: Value,
    pub roster: Vec<Value>,
}

/// 旧宿主 UI Gateway 设置的一次性迁移结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct GatewayMigrationExport {
    pub revision: u64,
    pub takeover_enabled: bool,
    pub jsonrpc_enabled: bool,
    pub api_enabled: bool,
    pub mcp_enabled: bool,
    pub cors_enabled: bool,
    pub user_token_configured: bool,
    /// 仅在鉴权迁移 RPC 中出现；不进入任何 snapshot/event。
    pub user_token: String,
}

/// 确认已持久化的一次性迁移版本。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct MigrationAckParams {
    pub revision: u64,
}

// ---------------------------------------------------------------------------
// 外部下载请求（浏览器扩展 NMH / 油猴脚本接管 / aria2 兼容层共用）
// ---------------------------------------------------------------------------

/// 浏览器原始请求体（form POST / XHR raw body 等）。
///
/// 当用户在 form-submit 触发的下载中点击下载按钮时，浏览器实际发起的是
/// POST 请求并携带表单数据；扩展通过 `webRequest.onBeforeRequest` 抓到 method
/// 与 body 后透传到此字段。宿主端按 `kind` 重建请求体。
///
/// 协议字段：
/// - `formData`：来自 `requestBody.formData`，宿主用 `reqwest::form()` 编码为
///   `application/x-www-form-urlencoded`
/// - `urlencoded`：扩展端已序列化好的 url-encoded 字符串（直接作为 body 发送）
/// - `raw`：base64 编码的二进制 body（XHR / fetch 直接发送 ArrayBuffer 的场景）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RequestBody {
    FormData {
        fields: HashMap<String, Vec<String>>,
    },
    Urlencoded {
        raw: String,
    },
    Raw {
        #[serde(rename = "bytesB64")]
        bytes_b64: String,
        #[serde(rename = "contentType", default)]
        content_type: Option<String>,
    },
}

/// 外部下载请求载荷（浏览器扩展 / 油猴脚本 / aria2 兼容层）。
///
/// 由宿主的「外部下载」通道消费：缓存请求事务 → 弹出快速下载确认框 →
/// 用户确认后创建任务。与管理 API 的 [`CreateTaskRequest`]（直接建任务、
/// 无确认框）语义不同。
///
/// # Examples
///
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DownloadRequest {
    pub url: String,
    #[serde(default)]
    pub filename: String,
    /// 保存目录（aria2 `dir` 选项 / 接管请求 `saveDir` 字段）。
    /// 空 = 由宿主按分类匹配 / 默认目录决定。
    #[serde(rename = "saveDir")]
    #[serde(default)]
    pub save_dir: String,
    #[serde(default)]
    pub referrer: String,
    #[serde(default)]
    pub cookies: String,
    /// 浏览器请求中捕获的额外 HTTP 头（如 Authorization）。
    /// 由下载引擎在发起请求时附加到请求头中。
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    /// 文件大小提示（字节）。
    ///   - `>0` = 已知大小，跳过 probe
    ///   - `-1` = 大小未知但确认是下载资源（webRequest 嗅探），跳过 probe
    ///   - `0` / `None` = 正常 probe
    #[serde(rename = "fileSize")]
    #[serde(default)]
    pub file_size: Option<i64>,
    #[serde(rename = "mimeType")]
    #[serde(default)]
    pub mime_type: Option<String>,
    /// 浏览器原始请求方法（"GET" / "POST" / ...）。
    /// 缺省 = "GET"。POST/PUT/PATCH 类请求由 `body` 携带请求体。
    #[serde(default)]
    pub method: Option<String>,
    /// 浏览器原始请求体（仅在非 GET 时有意义）。
    #[serde(default)]
    pub body: Option<RequestBody>,
    /// 音频轨 URL（可选，通用「视频轨+音频轨」离散下载对语义，按 MIME
    /// video/* vs audio/* 分轨判定，非站点专用协议字段）。
    /// 非空 = 这是一对轨道，引擎分别下载两路后用 ffmpeg mux 合并；
    /// 空/缺省 = 普通单 URL 下载。
    #[serde(rename = "audioUrl", default)]
    pub audio_url: Option<String>,
}

// ---------------------------------------------------------------------------
// 管理 API（/api/v1）资源类型
// ---------------------------------------------------------------------------

/// 任务信息（`GET /api/v1/tasks`、`GET /api/v1/tasks/{id}` 响应）。
///
/// # Examples
///
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskDto {
    pub task_id: String,
    pub url: String,
    pub file_name: String,
    pub save_dir: String,
    /// 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
    pub status: i32,
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error_message: String,
    /// Unix 秒级时间戳（字符串）。
    pub created_at: String,
    /// 单任务代理 URL（空 = 使用全局代理）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proxy_url: String,
    /// 命名队列 ID（空 = 默认队列）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub queue_id: String,
    /// Checksum spec，格式 `algo=hexhash`（空 = 跳过校验）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub checksum: String,
    /// 是否显式忽略 HTTPS 证书错误。默认 false（严格验证）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub ignore_tls_errors: bool,
    /// 文件跟踪：completed 任务的目标文件是否已丢失（被删除/移动）。默认 false。
    #[serde(default, skip_serializing_if = "is_false")]
    pub file_missing: bool,
    /// 任务结束时间，Unix 秒级时间戳（空 = 尚未完成）。
    /// 记录下载真正完成（status→3）的时刻，不含插件 hook 后处理耗时。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub completed_at: String,
    /// Source page URL captured by the browser extension (empty = none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub referrer: String,
    /// 所属任务组 ID（空 = 不属于任何组）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group_id: String,
    /// 由哪条 RSS 订阅自动创建（空 = 非 RSS 来源）。P5 任务溯源。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rss_source_id: String,
    /// 展示用原始来源链接（空 = 用 `url`）。`.torrent` 任务的 `url` 是
    /// `torrent-file://local` 哨兵，客户端「复制链接」应优先取本字段。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin_url: String,
    /// Auto 路由或站点规则标签（可追溯性）：Auto 为 `direct` /
    /// `direct:sampled` / `direct:pinned` / `direct:failover` /
    /// `proxy:cached` / `proxy:sampled` / `proxy:failover`（代理类标签带
    /// 候选来源后缀 `:system`/`:manual`）；站点规则为 `rule:direct` /
    /// `rule:system` / `rule:manual` / `rule:custom`；空串 = 非 Auto 且未命中规则。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auto_route: String,
    /// 加速来源累计字节（多 CDN / 智能代理 / 多网卡）；源站 = 已下载 − 三者之和。
    #[serde(default, skip_serializing_if = "is_default_source_bytes")]
    pub source_bytes: crate::TaskSourceBytesDto,
    /// 队列内启动顺序（0 = 未显式排序，按创建时间；>0 = 显式顺序）。
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub queue_order: i32,
    /// BT 已上传字节数（做种累计，非 BT 任务恒 0）。
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub uploaded_bytes: i64,
    /// 下载完成时刻的已上传字节数（做种后分享率基准，非 BT 任务恒 0）。
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub uploaded_at_completion: i64,
    /// BT 做种状态：0=无, 1=做种中, 2=达分享率, 3=达时长, 4=用户停止,
    /// 5=任务删除, 6=会话释放, 7=不活跃停止, 8=排队等待做种槽。
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub seeding_status: i32,
    /// 做种状态辅助说明（如停止原因，空 = 无）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub seeding_message: String,
    /// 累计做种秒数（活跃做种期间累加；排队/暂停不计，非 BT 任务恒 0）。
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub seeding_time_secs: i64,
    /// 任务级总分享率上限（千分比，1500 = 1.5）。哨兵：-2 = 跟随全局，
    /// -1 = 不限制，>=0 = 自定义（0 视同不限制）。
    #[serde(
        default = "default_seed_limit_inherit",
        skip_serializing_if = "is_seed_limit_inherit"
    )]
    pub seed_ratio_limit_milli: i64,
    /// 任务级做种后分享率上限（千分比）。哨兵语义同上。
    #[serde(
        default = "default_seed_limit_inherit",
        skip_serializing_if = "is_seed_limit_inherit"
    )]
    pub seed_post_ratio_limit_milli: i64,
    /// 任务级做种时长上限（分钟）。哨兵语义同上。
    #[serde(
        default = "default_seed_limit_inherit",
        skip_serializing_if = "is_seed_limit_inherit"
    )]
    pub seed_time_limit_minutes: i64,
    /// 任务级不活跃做种时长上限（分钟）。哨兵语义同上。
    #[serde(
        default = "default_seed_limit_inherit",
        skip_serializing_if = "is_seed_limit_inherit"
    )]
    pub seed_inactive_time_limit_minutes: i64,
    /// 任务级做种上传限速（字节/秒），0 = 未设置（跟随全局）。
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub seed_upload_limit_bps: i64,
}

// `TaskDto` 默认值字段不上线：二十万任务的全表传输里它们占了约三分之一字节。反序列化端
// 对应字段都带 `#[serde(default)]`，缺省值与这里判定的「默认」一致。
pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

fn is_seed_limit_inherit(value: &i64) -> bool {
    *value == default_seed_limit_inherit()
}

fn is_default_source_bytes(value: &crate::TaskSourceBytesDto) -> bool {
    *value == crate::TaskSourceBytesDto::default()
}

/// 命名队列信息（`GET /api/v1/queues` 响应）。
///
/// # Examples
///
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct QueueDto {
    pub queue_id: String,
    pub name: String,
    /// 队列限速（KB/s），0 = 不限速。
    pub speed_limit_kbps: i64,
    /// 队列上传限速（KB/s），0 = 不限速。
    #[serde(default)]
    pub upload_limit_kbps: i64,
    /// 队列并发上限，0 = 跟随全局。
    pub max_concurrent: i32,
    pub default_save_dir: String,
    pub position: i32,
    pub default_segments: i32,
    pub default_user_agent: String,
    /// 队列运行状态：停止的队列不自动启动其中任务。
    #[serde(default = "default_true")]
    pub is_running: bool,
    /// 每日定时计划是否启用。
    #[serde(default)]
    pub schedule_enabled: bool,
    /// 每日定时启动时间 `HH:MM`（空 = 不定时启动）。
    #[serde(default)]
    pub schedule_start: String,
    /// 每日定时停止时间 `HH:MM`（空 = 不定时停止）。
    #[serde(default)]
    pub schedule_stop: String,
    /// 定时生效星期位掩码：bit0=周一 … bit6=周日；127 = 每天。
    #[serde(default = "default_schedule_days")]
    pub schedule_days: i32,
}

/// 创建任务请求（`POST /api/v1/tasks`）。
///
/// 与外部下载请求 [`DownloadRequest`] 不同：本请求**直接创建任务**，
/// 不经过快速下载确认弹框（管理 API 的调用方是受信任的自动化客户端）。
///
/// # Examples
///
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskRequest {
    pub url: String,
    /// 空 = 从 URL / Content-Disposition 推断。
    #[serde(default)]
    pub file_name: String,
    /// 空 = 使用全局默认保存目录。
    #[serde(default)]
    pub save_dir: String,
    /// 0 = 由 segment_advisor 按文件大小动态决定。
    #[serde(default)]
    pub segments: i32,
    #[serde(default)]
    pub cookies: String,
    #[serde(default)]
    pub referrer: String,
    /// 单任务代理 URL（空 = 使用全局代理）。
    #[serde(default)]
    pub proxy_url: String,
    /// 空 = 使用全局 User-Agent。
    #[serde(default)]
    pub user_agent: String,
    /// 命名队列 ID（空 = 默认队列）。
    #[serde(default)]
    pub queue_id: String,
    /// Checksum spec，格式 `algo=hexhash`（空 = 跳过校验）。
    #[serde(default)]
    pub checksum: String,
    /// 忽略 HTTPS 证书错误。缺省 false（严格验证）。
    #[serde(default)]
    pub ignore_tls_errors: bool,
    /// 附加 HTTP 请求头。
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    /// BT 种子文件字节（base64 编码，aria2 `addTorrent` 兼容入口）。
    /// 非空时按种子任务创建，`url` 允许为空占位。
    #[serde(default)]
    pub torrent_b64: Option<String>,
    /// 浏览器原始 HTTP method（`"GET"`/`"POST"`/…）。缺省 = GET。
    /// form-POST 触发的下载必须携带，否则引擎用 GET 重发会拿到错误内容。
    #[serde(default)]
    pub method: Option<String>,
    /// 浏览器原始请求体（仅非 GET 时有意义）。
    #[serde(default)]
    pub body: Option<RequestBody>,
    /// 音频轨 URL（「视频轨+音频轨」离散下载对语义）。
    /// 非空 = 引擎分别下载两路后 mux 合并；空/缺省 = 普通单 URL 下载。
    #[serde(default)]
    pub audio_url: Option<String>,
    /// 稍后下载：true = 建任务后不启动（paused 落库），待「启动队列」
    /// 按序恢复或用户手动恢复。缺省 false = 立即开始。
    #[serde(default)]
    pub start_paused: bool,
    /// HTTP Basic 认证用户名。非空时引擎生成 `Authorization: Basic` 头
    /// 注入请求（覆盖 `headers` 中的同名头）。空 = 未提供，若该站点有
    /// 已保存凭据则自动套用。
    #[serde(default)]
    pub http_user: String,
    /// HTTP Basic 认证密码（仅 `httpUser` 非空时有意义，允许为空串）。
    #[serde(default)]
    pub http_password: String,
    /// 为此网站保存凭据：true 且 `httpUser` 非空时按站点（host[:port]）
    /// 持久化，供后续同站点任务自动套用。
    #[serde(default)]
    pub save_site_auth: bool,
}

fn default_true() -> bool {
    true
}

fn default_schedule_days() -> i32 {
    127
}

/// 任务级做种限制的反序列化默认值：-2 = 跟随全局（缺字段不得落到 0=自定义）。
fn default_seed_limit_inherit() -> i64 {
    -2
}

/// 创建任务响应（`POST /api/v1/tasks`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CreatedTask {
    pub task_id: String,
}

/// 应用信息（`GET /api/v1/info` 响应）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ApiInfo {
    pub name: String,
    pub version: String,
}

/// 通用结果响应（接管端点应答 / 各端点错误响应统一格式）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ResultMessage {
    pub success: bool,
    pub message: String,
}

// ---------------------------------------------------------------------------
// 插件系统 DTO（camelCase；双向 serde + ToSchema）
// ---------------------------------------------------------------------------

/// select 控件选项。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SettingOptionDto {
    pub value: String,
    pub label: String,
}

/// 声明式设置项（镜像 `engine::plugin::SettingField`，api 本地定义）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SettingFieldDto {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// `string` / `number` / `boolean`。
    #[serde(rename = "type")]
    pub setting_type: String,
    /// `text`/`password`/`textarea`/`select`/`toggle`/`number`/`folder`。
    pub widget: String,
    #[serde(default)]
    pub options: Vec<SettingOptionDto>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub pattern: Option<String>,
    /// 辅助脚本（非空时 UI 在字段旁渲染复制按钮，仅复制文本、绝不执行）。
    #[serde(default)]
    pub helper_script: Option<String>,
    /// 辅助脚本按钮文案（空则用默认文案）。
    #[serde(default)]
    pub helper_label: Option<String>,
}

/// plugin 层嗅探规则包（`daemon.plugin.sniffRules`）：`pack` 为 `null` 表示从未有过插件规则
/// （此时 `sequence == 0`）。`pack` 的形状即浏览器扩展的 `RulePack` JSON。
/// `epoch` 是随序号持久化的实例标识：数据库重置后序号重新计数，`epoch` 必变，
/// 浏览器扩展据此丢弃旧序号而不是把倒退当作回滚。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginSniffRulesDto {
    pub sequence: u64,
    #[serde(default)]
    pub epoch: String,
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub pack: Option<Value>,
}

/// 插件页面脚本索引（`daemon.plugin.pageScripts`）。只列「启用、已加载、已授权」插件声明的
/// `contributes.pageScripts`；`sequence` / `epoch` 语义同 [`PluginSniffRulesDto`]：索引内容
/// （含每条脚本的 `sha256`）变化才递增，数据库重置后 `epoch` 必变。源码不随索引下发，
/// 浏览器扩展按需经 `daemon.plugin.pageScriptSource` 逐条拉取（NMH 单帧 1 MiB）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginPageScriptsDto {
    pub sequence: u64,
    #[serde(default)]
    pub epoch: String,
    #[serde(default)]
    pub scripts: Vec<PluginPageScriptDto>,
}

/// 页面脚本索引条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginPageScriptDto {
    /// 插件 identity（`author@name`）。
    pub identity: String,
    pub plugin_name: String,
    pub plugin_version: String,
    /// manifest 内的脚本 id（插件内唯一）。
    pub id: String,
    pub title: String,
    /// 浏览器 match pattern。
    pub matches: Vec<String>,
    #[serde(default)]
    pub exclude_matches: Vec<String>,
    /// `document_start` / `document_end` / `document_idle`。
    pub run_at: String,
    /// `isolated`（扩展 USER_SCRIPT world，可用 `fluxdown` 桥）/ `main`（页面 world，无桥）。
    pub world: String,
    #[serde(default)]
    pub all_frames: bool,
    /// 拼接后源码（UTF-8）的 SHA-256 小写十六进制。
    pub sha256: String,
    /// 拼接后源码字节数。
    pub bytes: u64,
}

/// `daemon.plugin.pageScriptSource` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginPageScriptSourceParams {
    pub identity: String,
    pub id: String,
}

/// 单条页面脚本源码：manifest `js` 各文件按声明顺序以 `"\n;\n"` 拼接。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginPageScriptSourceDto {
    pub identity: String,
    pub id: String,
    pub sha256: String,
    pub code: String,
}

/// 已安装插件视图（列表/设置表单）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginDto {
    pub identity: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    pub enabled: bool,
    pub dev_mode: bool,
    /// `None` / `Manual` / `CircuitBreaker` / `Revoked`。
    pub disabled_reason: String,
    pub settings: Vec<SettingFieldDto>,
    /// 当前设置值（key → value 字符串）。
    pub settings_values: HashMap<String, String>,
    /// manifest 声明的能力权限（如 `["ffmpeg"]`，供 UI 展示授权徽章）。
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 是否声明平台登录入口。
    #[serde(default)]
    pub auth_supported: bool,
    /// manifest 声明的订阅 provider ID，供订阅创建界面生成可选来源。
    #[serde(default)]
    pub subscription_provider_ids: Vec<String>,
    /// manifest resolvers[0].match.urls（glob，语义见 url_glob_match）。
    #[serde(default)]
    pub resolver_urls: Vec<String>,
    /// `Loaded` / `Failed`；与 `enabled` 独立，手动禁用的插件仍可能已加载。
    #[serde(default = "default_plugin_load_status")]
    pub load_status: String,
    /// 加载失败的可读原因；成功时为空。
    #[serde(default)]
    pub load_error: String,
    /// manifest `contributes`：视图 / 命令 / 嗅探规则贡献点。
    #[serde(default)]
    pub contributions: PluginContributionsDto,
    /// 风险等级（由权限推导）：`standard` / `elevated` / `full`。
    #[serde(default = "default_plugin_risk")]
    pub risk: String,
    /// 尚未授权的权限（授权流程启用前恒为空）。
    #[serde(default)]
    pub consent_pending: Vec<String>,
    /// 尚未授权的网络主机（授权流程启用前恒为空）。
    #[serde(default)]
    pub consent_pending_hosts: Vec<String>,
    /// 全部桌面视图声明的网络主机（排序去重）。
    #[serde(default)]
    pub network_hosts: Vec<String>,
    /// 插件包在 daemon 主机上的绝对目录（仅已加载插件有值）；本机桌面端据此读取 `desktop.entry`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_dir: Option<String>,
    /// 插件包修订号：包代码被替换（安装 / 升级 / dev 安装 / dev 重载）后变化，只做相等比较；
    /// 桌面端据此重新挂载桌面脚本视图。旧 daemon 不下发时为 0。
    #[serde(default)]
    pub package_revision: u64,
}

/// 插件贡献点（镜像 `engine::plugin::ContributesDecl`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginContributionsDto {
    #[serde(default)]
    pub views: Vec<PluginViewDeclDto>,
    #[serde(default)]
    pub commands: Vec<PluginCommandDeclDto>,
    /// 是否贡献浏览器扩展嗅探规则。
    #[serde(default)]
    pub sniff_rules: bool,
    /// 浏览器页面脚本声明（供授权 / 详情界面展示将在哪些网站运行脚本）。
    #[serde(default)]
    pub page_scripts: Vec<PluginPageScriptDeclDto>,
}

/// 页面脚本贡献声明（镜像 manifest `contributes.pageScripts[]` 的展示字段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginPageScriptDeclDto {
    pub id: String,
    pub title: String,
    pub matches: Vec<String>,
    /// `isolated` / `main`。
    pub world: String,
}

/// 视图贡献声明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginViewDeclDto {
    pub id: String,
    pub title: String,
    /// `activity` / `settings` / `taskDetail` / `window`。
    pub location: String,
    /// 是否有可移植实现（由 main 内的 `flux.ui.view` 提供）：无 `desktop` 的视图恒为 true，
    /// 带 `desktop` 的视图仅在 manifest 显式 `portable: true` 时为 true。
    #[serde(default)]
    pub portable: bool,
    /// 桌面增强实现（仅 GPUI 桌面端经 gpui-shell 运行，daemon 不执行）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<PluginDesktopEntryDto>,
    /// 视图图标 SVG 文本（manifest `icon` 指向的包内文件，加载期读入并校验）；客户端
    /// 只把它当单色蒙版按主题着色渲染。未声明时为 `None`，客户端用默认图标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_svg: Option<String>,
}

/// 视图的桌面增强入口。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginDesktopEntryDto {
    /// 包内相对路径（`.js` / `.mjs`），默认导出继承 `View` 的类。
    pub entry: String,
    /// 初始窗口宽度（逻辑像素，仅 `window` 视图）。
    #[serde(default)]
    pub width: Option<f32>,
    /// 初始窗口高度（逻辑像素，仅 `window` 视图）。
    #[serde(default)]
    pub height: Option<f32>,
    /// 界面脚本可直连的网络主机（gpui-shell 网络授权只接受逐个主机名）。
    #[serde(default)]
    pub network_hosts: Vec<String>,
}

/// 命令贡献声明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginCommandDeclDto {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub palette: bool,
    #[serde(default)]
    pub task_menu: bool,
}

fn default_plugin_risk() -> String {
    "standard".to_string()
}

fn default_plugin_load_status() -> String {
    "Loaded".to_string()
}

/// 驱动插件平台登录（二维码/账号登录）的请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginAuthRequest {
    #[serde(default)]
    pub identity: String,
    /// `begin` / `poll` / `cancel` / `logout` / `status`。
    pub action: String,
    #[serde(default)]
    pub site: String,
    #[serde(default)]
    pub auth_ref: String,
    #[serde(default)]
    pub session_id: String,
    /// 账号、验证码或平台登录流程需要的额外输入。
    #[serde(default)]
    pub input: String,
}

/// 插件平台登录交互状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginAuthResponse {
    /// `pending` / `success` / `error`。
    pub status: String,
    pub session_id: String,
    /// 二维码文本、data URL 或其他挑战内容。
    #[serde(default)]
    pub challenge: Option<String>,
    #[serde(default)]
    pub challenge_type: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub auth_ref: Option<String>,
}

/// 安装 dev 插件请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct InstallPluginDevRequest {
    pub dir_path: String,
}

/// 重命名任务文件请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RenameTaskRequest {
    /// 新文件名（不含路径分隔符；引擎侧校验非法字符与状态）。
    pub file_name: String,
}

/// 更换任务下载源地址请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ChangeTaskUrlRequest {
    /// 新下载地址（http(s)/ftp，或待解封装的 `thunder://` 链接）。
    pub url: String,
}

/// `daemon.task.changeUrl` 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ChangeTaskUrlParams {
    pub task_id: String,
    /// 新下载地址（http(s)/ftp，或待解封装的 `thunder://` 链接）。
    pub url: String,
}

/// `daemon.task.setSegments` 参数：修改未完成任务的线程（分段）数，已下进度保留。
///
/// `segments <= 0` = 自动；上限 [`crate::MAX_TASK_SEGMENTS`]。下载中任务由引擎
/// 静默暂停 → 改配置 → 恢复，其余状态下次开始时生效。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SetTaskSegmentsParams {
    pub task_id: String,
    pub segments: i32,
}

/// `daemon.task.verify` 结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum TaskVerifyStatus {
    /// 全部需要的 piece 哈希一致。
    Intact,
    /// 至少一个需要的 piece 哈希不符或数据缺失 / 截断。
    Damaged,
    /// 本地没有种子元数据（磁力任务未在做种且无缓存），或没有可比对的 piece。
    Unverifiable,
}

/// `daemon.task.verify` 结果：已完成 BT 任务的完整性校验报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskVerifyResult {
    pub status: TaskVerifyStatus,
    /// 种子 piece 总数（未取得元数据时为 0）。
    pub total_pieces: u64,
    /// 哈希一致的 piece 数。
    pub verified_pieces: u64,
    /// 损坏的 piece 数。
    pub bad_pieces: u64,
    /// 未校验的 piece 数：不属于所选文件，或缺少可比对的字节。
    pub skipped_pieces: u64,
    /// 被损坏 piece 覆盖的选中文件（种子内相对路径，`/` 分隔，种子顺序）。
    #[serde(default)]
    pub damaged_files: Vec<String>,
}

/// 设置插件启用状态请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SetPluginEnabledRequest {
    pub enabled: bool,
}

/// 安装成功返回体。
///
/// `missing_components` 列出插件声明权限所需、但尚未安装的基础组件
/// （如 `"ffmpeg"`/`"ytdlp"`，依赖表见引擎 `plugin::dependencies`）——
/// 提醒式而非阻断式：安装本身已成功，客户端应提示用户前往组件设置安装依赖。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub identity: String,
    #[serde(default)]
    pub missing_components: Vec<String>,
}

/// 市场索引条目视图（去中心化插件市场浏览/安装）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct MarketEntryDto {
    pub plugin_id: String,
    pub version: String,
    pub sequence: u64,
    pub content_hash: String,
    #[serde(default)]
    pub min_app_version: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub mirrors: Vec<String>,
    #[serde(default)]
    pub publish_time: String,
    #[serde(default)]
    pub yanked: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// manifest 声明的能力权限（如 `["ffmpeg"]`，供安装前展示授权）。
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 插件桌面视图声明的网络主机（授权界面展示）。
    #[serde(default)]
    pub network_hosts: Vec<String>,
}

/// 从市场安装请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct MarketInstallRequest {
    pub plugin_id: String,
    /// 用户确认权限时看到的版本；最新可装版本与之不一致时拒绝安装（`Conflict`），
    /// 由客户端刷新目录后重新确认。缺省 = 不钉版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// `daemon.plugin.consent` 参数：`permissions` / `networkHosts` 必须与插件当前请求的集合完全相等。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginConsentParams {
    pub identity: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub network_hosts: Vec<String>,
}

/// `daemon.plugin.audit.list` 参数。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginAuditListParams {
    /// 只看某插件；缺省 = 全部。
    #[serde(default)]
    pub identity: Option<String>,
    /// 翻页游标：只返回 id 小于它的记录。
    #[serde(default)]
    pub before_id: Option<i64>,
    /// 每页条数，缺省 200，上限 1000。
    #[serde(default)]
    pub limit: Option<u32>,
}

/// 一条插件审计记录。`origin` ∈ `daemon|desktop`，`outcome` ∈ `ok|denied|error`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginAuditEntryDto {
    pub id: i64,
    pub at_ms: i64,
    pub identity: String,
    pub origin: String,
    pub action: String,
    pub target: String,
    pub outcome: String,
    pub detail: String,
}

/// `daemon.plugin.audit.list` 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginAuditListDto {
    pub entries: Vec<PluginAuditEntryDto>,
    /// 还有更早的记录时给出下一页游标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before_id: Option<i64>,
}

/// `daemon.plugin.audit.clear` 参数。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginAuditClearParams {
    #[serde(default)]
    pub identity: Option<String>,
}

/// 客户端上报的一条桌面脚本审计记录（`origin` 由 daemon 强制为 `desktop`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginAuditRecordEntry {
    pub identity: String,
    pub action: String,
    #[serde(default)]
    pub target: String,
    pub outcome: String,
    #[serde(default)]
    pub detail: String,
}

/// `daemon.plugin.audit.record` 参数（每次 ≤100 条）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PluginAuditRecordParams {
    pub entries: Vec<PluginAuditRecordEntry>,
}

// ---------------------------------------------------------------------------
// 任务组与预解析（多文件任务组，Phase D；`docs/multi-file-task-group-design.md`）
// ---------------------------------------------------------------------------

/// 任务组信息（`GET /api/v1/groups` 响应元素）。
///
/// # Examples
///
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct GroupDto {
    pub group_id: String,
    pub name: String,
    /// 原始分享/清单链接（展示/复制用）。
    pub source_url: String,
    /// 组根目录（子任务落盘 = 本值 + 清单条目的相对路径）。
    pub save_dir: String,
    /// Unix 秒级时间戳（字符串）。
    pub created_at: String,
}

/// [`CreateGroupRequest::items`] 的单个组成员条目（客户端在预览响应上
/// 勾选后的清单条目/规格投影）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct GroupItemRequest {
    /// 二段解析标识，按 `<itemId>` 或 `<itemId>@<variantId>` 拼接（见
    /// [`PreviewItemDto::id`]/[`PreviewVariantDto::id`]）。
    pub resolver_item: String,
    pub file_name: String,
    /// 相对组根目录的子路径（空 = 组根）。
    #[serde(default)]
    pub rel_path: String,
    /// 已知大小（字节，0 = 未知）。
    #[serde(default)]
    pub size: i64,
}

/// 创建多文件任务组请求（`POST /api/v1/groups`）。`items` 不可为空
/// （空数组 → 400）。
///
/// # Examples
///
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CreateGroupRequest {
    /// 原始分享/清单链接（组行 `source_url`，展示/复制用）。
    #[serde(default)]
    pub source_url: String,
    /// 组名（空 = 组根目录直接用 `save_dir`）。
    #[serde(default)]
    pub group_name: String,
    /// 基础保存目录（组根目录 = `save_dir/sanitize(group_name)`）；
    /// 空 = 使用全局默认保存目录。
    #[serde(default)]
    pub save_dir: String,
    /// 命名队列 ID（空 = 默认队列）。
    #[serde(default)]
    pub queue_id: String,
    /// 0 = 由 segment_advisor 按文件大小动态决定。
    #[serde(default)]
    pub segments: i32,
    #[serde(default)]
    pub cookies: String,
    #[serde(default)]
    pub referrer: String,
    #[serde(default)]
    pub user_agent: String,
    /// 单任务代理 URL（空 = 使用全局代理）。
    #[serde(default)]
    pub proxy_url: String,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    /// 忽略 HTTPS 证书错误。缺省 false（严格验证）。
    #[serde(default)]
    pub ignore_tls_errors: bool,
    /// 稍后下载：true = 建组后不启动，待「启动队列」或用户手动恢复。
    #[serde(default)]
    pub start_paused: bool,
    /// 预解析命中的插件（[`ResolvePreviewResponse::plugin_id`] 原样回传）。非空 = 成员任务
    /// 固定绑定该插件，建组时它已不可用（禁用 / 卸载 / 无 resolver）则成员以「解析插件不可用」
    /// fail-closed 落库；空 = 旧客户端，引擎按预解析同一规则（仅 `multi` resolver）重新选择。
    #[serde(default)]
    pub resolver_plugin_id: String,
    /// 组成员清单（不可为空，见本类型文档）。
    #[serde(default)]
    pub items: Vec<GroupItemRequest>,
}

/// 创建任务组响应（`POST /api/v1/groups`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CreateGroupResponse {
    pub group_id: String,
}

/// 前置预解析请求（`POST /api/v1/resolve/preview`）。只读、不建任务、
/// 不写库；结果见 [`ResolvePreviewResponse`]。
///
/// # Examples
///
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ResolvePreviewRequest {
    pub url: String,
    #[serde(default)]
    pub cookies: String,
    #[serde(default)]
    pub referrer: String,
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
}

/// [`ResolvePreviewRequest`] 的结果。`items` 为空且 `error` 为空 = 插件未
/// 返回清单（客户端应回退普通单任务创建对话框）；`error` 非空 = 预解析
/// 失败（同样回退，`error` 供 UI 提示）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ResolvePreviewResponse {
    pub name: String,
    pub source_url: String,
    /// 无错误时为空。
    #[serde(default)]
    pub error: String,
    /// 产出本清单的插件 identity（无清单时为空）；建组时原样填入
    /// [`CreateGroupRequest::resolver_plugin_id`]，保证成员任务交给同一插件二段解析。
    #[serde(default)]
    pub plugin_id: String,
    #[serde(default)]
    pub items: Vec<PreviewItemDto>,
}

/// [`ResolvePreviewResponse::items`] 的单个清单条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PreviewItemDto {
    /// 插件自定义标识，建组时按 `<id>` 或 `<id>@<variantId>` 拼进
    /// [`GroupItemRequest::resolver_item`]。
    pub id: String,
    pub name: String,
    /// 相对组根目录的子路径（空 = 根）。
    pub path: String,
    /// 已知大小（字节），未知为 0。
    pub size: i64,
    pub variants: Vec<PreviewVariantDto>,
}

/// [`PreviewItemDto::variants`] 的单个规格（画质/格式）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PreviewVariantDto {
    pub id: String,
    pub label: String,
    /// 已知大小（字节），未知为 0。
    pub size: i64,
}

// ---------------------------------------------------------------------------
// P2P 设备互联（device link）—— 配对握手 + 数据面下发的 wire 契约
// ---------------------------------------------------------------------------

/// `/ping` 透出的本机设备互联身份（无鉴权），供发起方 TOFU 固定 + 展示。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPingInfo {
    /// 本机 Ed25519 身份指纹（设备 ID）。
    pub fingerprint: String,
    /// 展示名。
    pub name: String,
    /// 平台标识。
    #[serde(default)]
    pub platform: String,
    /// 数据面链路协议版本（`X-FluxLink-Enc` 方案号）。旧版对端没有此字段（按 0 处理）：
    /// 发起方在下发 / 交换信息前据此判定对端过旧并返回 `PeerOutdated`，不发送请求。
    #[serde(default)]
    pub link_protocol: u32,
    /// 设备控制配对能力版本：`>= 1` 表示本机接受带 `requestControl` 的配对 hello。旧版对端没有
    /// 此字段（按 0 处理），发起方据此在发出 hello 之前判定对端不支持并放弃，避免对端静默忽略
    /// 控制请求后只配成一台普通设备。
    #[serde(default)]
    pub link_control: u32,
}

/// 配对 `hello` 请求（发起方 → 响应方）。全部密钥/签名字段为 base64。
///
/// 配对协议版本 2：发起方在这一步只交出临时公钥与随机数的**承诺**，临时公钥本身
/// 要等响应方回出自己的临时值之后才在 `pair/reveal` 里揭示。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairHelloRequest {
    /// 发起方的配对协议版本。旧版发起方没有这个字段（按 0 处理），与响应方版本不一致
    /// 一律以版本不兼容拒绝。
    #[serde(default)]
    pub protocol_version: u32,
    /// 一次性配对码（响应方 UI 展示、用户手输）。
    pub code: String,
    /// 发起方 Ed25519 身份公钥（base64）。
    pub initiator_id_pub: String,
    /// 发起方临时 X25519 公钥与随机数的承诺 `sha256(eph_pub || nonce)`（base64）。
    #[serde(default)]
    pub initiator_commit: String,
    /// 发起方对 hello（码、身份公钥、承诺、自报信息）的 Ed25519 签名（base64）。
    pub initiator_sig: String,
    /// 发起方展示名。
    pub name: String,
    /// 发起方平台。
    #[serde(default)]
    pub platform: String,
    /// 发起方客户端版本。
    #[serde(default)]
    pub app_version: String,
    /// 发起方自报可达候选地址（`ip:port`），供响应方存为回连候选。
    #[serde(default)]
    pub initiator_addrs: Vec<String>,
    /// 发起方请求「完全控制」本机（`/rpc`）。受签名保护（`true` 时进入 hello 签名字节与转录）；
    /// `false` 时省略，与旧版 hello 逐字节兼容。
    #[serde(default, skip_serializing_if = "is_false")]
    pub request_control: bool,
}

/// 配对 `hello` 回复（响应方 → 发起方）：本次会话全新的临时公钥与随机数。不含 SAS
/// 与签名——响应方此刻还看不到发起方的临时公钥，两者都在 `pair/reveal` 之后才产生。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairHelloResponse {
    /// 响应方的配对协议版本。
    pub protocol_version: u32,
    pub session_id: String,
    /// 响应方本次会话的临时 X25519 公钥（base64）。
    pub responder_eph_pub: String,
    /// 响应方本次会话的随机数（base64）。
    pub responder_nonce: String,
    pub responder_id_pub: String,
    pub name: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub app_version: String,
}

/// 配对 `reveal` 请求（发起方 → 响应方）：揭示 `hello` 里承诺过的临时公钥与随机数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairRevealRequest {
    /// `pair/hello` 回复里的会话 id。
    pub session_id: String,
    /// 发起方临时 X25519 公钥（base64）。
    pub initiator_eph_pub: String,
    /// 发起方随机数（base64）。
    pub initiator_nonce: String,
}

/// 配对 `reveal` 回复（响应方 → 发起方）：响应方对完整握手转录的 Ed25519 签名。SAS
/// 不上线，两端各自在本机屏幕上展示。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairRevealResponse {
    /// 响应方对完整转录的 Ed25519 签名（base64）。
    pub responder_sig: String,
}

/// 配对 `confirm` 请求：SAS 核对后确认/拒绝。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairConfirmRequest {
    pub session_id: String,
    pub confirm: bool,
}

/// 响应方处理一次入站配对 `confirm` 的终局（HTTP 层用于渲染 `paired` + `reason`）。
///
/// 与引擎 `link::manager::PairConfirmOutcome` 一一对应，但独立定义：`fluxdown_api`
/// 不依赖引擎的 link 模块（后者是可选 feature，移动端整块不编译）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkPairConfirmOutcome {
    /// 本机用户批准，发起方已入册。
    Paired,
    /// 发起方自己传了 `confirm=false`。
    Declined,
    /// 本机用户核对 SAS 后拒绝。
    Rejected,
    /// 等待本机用户核验超时（60s 决策窗口耗尽）。
    TimedOut,
}

impl LinkPairConfirmOutcome {
    /// 是否真的完成了配对。
    #[must_use]
    pub fn paired(self) -> bool {
        matches!(self, Self::Paired)
    }

    /// 透出给发起方的稳定判别串；`Paired`/`Declined` 无需额外理由。
    #[must_use]
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Rejected => Some("rejected"),
            Self::TimedOut => Some("timeout"),
            Self::Paired | Self::Declined => None,
        }
    }
}

/// 批准/拒绝一次入站配对核验（管理面版本；`session_id` 对应
/// `LinkEvent{kind:"incomingPairing"}` / WS `linkIncomingPairing` 携带的会话 id）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairApproveRequest {
    pub session_id: String,
    pub accept: bool,
}

/// 已配对设备下发下载任务的请求体（鉴权走 `X-FluxLink-*` 头，非 body）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkTaskRequest {
    pub url: String,
    #[serde(default)]
    pub save_dir: String,
    #[serde(default)]
    pub file_name: String,
}

/// 数据面链路 HMAC 鉴权凭据（从 `X-FluxLink-*` 请求头提取，非 wire body）。
#[derive(Debug, Clone)]
pub struct LinkAuth {
    /// 发起方设备指纹（响应方据此查每对独立链路密钥）。
    pub device: String,
    /// Unix 秒时间戳（防重放）。
    pub ts: i64,
    /// 一次性随机串。
    pub nonce: String,
    /// HMAC-SHA256 标签（hex，覆盖密文摘要与收发双方指纹，encrypt-then-MAC）。
    pub tag: String,
    /// `X-FluxLink-Enc` 头原始值：数据面方案版本号。当前唯一合法值是 `"v2"`
    /// （MAC 绑定收发双方、请求 / 响应都经 ChaCha20-Poly1305 封装并以 AAD 绑定方向与请求
    /// nonce）；缺失或其他值一律鉴权失败——双端同版本发布，不留降级回退路径。
    pub enc: String,
}

/// 生成配对码的响应（`POST /api/v1/link/code`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkCodeResponse {
    /// 6 位一次性配对码。
    pub code: String,
    /// 有效秒数。
    pub ttl_seconds: i64,
}

// ---------------------------------------------------------------------------
// P2P 设备互联管理面（发现/配对/名册；均需 management token；
// docs 见 local://link_mgmt_contract.md）
// ---------------------------------------------------------------------------

/// 一台被发现、尚未配对的设备（`GET /api/v1/link/discovered` 元素 /
/// `POST /api/v1/link/probe` 响应）。对应引擎 `link::DiscoveredPeer`（宿主层
/// 转换，`kind` → `source` 小写字符串 `"mdns"`/`"manual"`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkDiscoveredPeer {
    /// 对端指纹（经 `/ping` TOFU 获得；mDNS 未探测到时为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    /// 发现途径：`"mdns"` | `"manual"`。
    pub source: String,
}

/// 本地设备发现开关请求体（`POST /api/v1/link/discovery`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkDiscoveryRequest {
    /// `"start"` | `"stop"`。
    pub action: String,
}

/// 手动地址探测请求体（`POST /api/v1/link/probe`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkProbeRequest {
    pub host: String,
    pub port: u16,
}

/// `daemon.link.probe` 参数：待探测的下载链接。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkProbeParams {
    pub url: String,
}

/// 链接探测判定：`"resource"` / `"notResource"` / `"unknown"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum LinkProbeVerdict {
    /// 确认是可下载资源。
    Resource,
    /// 确认是网页/文本/图片预览等非下载资源。
    NotResource,
    /// 网络错误、超时、非 2xx 或无 Content-Type，无法判定。
    Unknown,
}

/// `daemon.link.probe` 结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkProbeResult {
    pub verdict: LinkProbeVerdict,
    /// 跟随重定向后的地址；未拿到响应时等于原 url。
    pub final_url: String,
    /// 可推断时的文件名，否则空。
    pub file_name: String,
    /// 小写 MIME essence（去参数），无则空。
    pub mime: String,
    /// 0 = 未知。
    pub total_bytes: i64,
}

/// 发起配对请求体（`POST /api/v1/link/pair/begin`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairBeginRequest {
    pub host: String,
    pub port: u16,
    pub code: String,
}

/// 发起配对成功后的待确认结果（`POST /api/v1/link/pair/begin` 响应）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairBeginResponse {
    pub token: String,
    /// 供双方肉眼核对的短认证串。
    pub sas: String,
    pub peer_name: String,
    pub peer_fingerprint: String,
}

/// SAS 核对后确认/拒绝配对请求体（`POST /api/v1/link/pair/finish`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairFinishRequest {
    pub token: String,
    pub accept: bool,
}

/// 配对完成结果（`POST /api/v1/link/pair/finish` 响应）。`accept=false` 或
/// 对端拒绝时 `paired=false`，`device` 省略。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkPairFinishResponse {
    pub paired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<LinkDeviceInfo>,
}

/// 一台**已配对**设备的对外视图（`GET /api/v1/link/devices` 元素）。严禁透出
/// `link_secret`/`identity_pub` 等敏感字段（对应引擎 `link::PeerRecord`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkDeviceInfo {
    pub fingerprint: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// 并发探测得到的在线状态（见 `ApiHost::link_devices` 实现）。
    pub online: bool,
    pub paired_at: i64,
    pub last_seen_at: i64,
    /// 对端自报的默认下载目录（经已认证链路获取；旧版对端为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_save_dir: Option<String>,
    /// 对端自报的路径风格（旧版对端为 `None`，可按 `platform` 推断）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_style: Option<crate::agent::PathStyle>,
    /// 本机给该设备设置的备注名（`agent.link.rename`，仅本机可见）；`name` 已是生效名
    /// （备注名优先，否则对端自报名）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    /// 对端自报的客户端版本（经已认证链路获取）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    /// 对端已登录云账号时经已认证链路自报的云设备 id。客户端仅当本机云设备名册里存在同 id
    /// 设备时把两者合并为同一台设备展示；直连设备本身永不注册到云端、不占云端设备名额。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_device_id: Option<String>,
    /// 对端已解除与本机的配对（对端经已认证链路通知）：不可再下发，只能删除或重新配对。
    #[serde(default)]
    pub revoked: bool,
    /// 该设备被本机授权可经 `/rpc` 完全控制本机（本机用户批准了其 `requestControl` 配对）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub grants_control: bool,
    /// 本机可经 `/rpc` 控制该设备（本机以 `requestControl` 向它发起并完成了配对）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub controllable: bool,
}

/// 发现快照响应（`GET /api/v1/link/discovered`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LinkDiscoveredResponse {
    pub peers: Vec<LinkDiscoveredPeer>,
}

/// 已配对设备列表响应（`GET /api/v1/link/devices`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LinkDevicesResponse {
    pub devices: Vec<LinkDeviceInfo>,
}

/// 已配对设备下发下载任务请求体（管理面，token 鉴权；区别于数据面链路 HMAC
/// 鉴权的 [`LinkTaskRequest`]，字段语义一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LinkDeviceTaskRequest {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

/// 通用 `{"ok":true}` 应答（发现开关 / 解除配对）。与 [`ResultMessage`] 的
/// `{"success","message"}` 形态不同——契约就此路由指定的字面 wire 形状。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LinkOkResponse {
    pub ok: bool,
}

/// 一个 RSS 订阅（`GET/POST /api/v1/rss`、`PUT /api/v1/rss/{id}`）。
///
/// 写请求复用同一结构：全字段 `#[serde(default)]`，客户端只需给关心的字段；
/// 运行态字段（`lastFetchAt`/`lastError`/`failCount`/`seeded`/`unreadCount`）
/// 只读，写入时被引擎忽略。
///
/// # Examples
///
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RssSourceDto {
    #[serde(default)]
    pub source_id: String,
    /// 来源 provider 的稳定 ID；缺省为内置 RSS provider。
    #[serde(default = "default_rss_provider_id")]
    pub provider_id: String,
    /// provider 专属配置 JSON；缺省为空。
    #[serde(default)]
    pub provider_config: String,
    pub url: String,
    /// 空 = 用 feed 标题回填。
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// false = 收集模式（只收集条目供手动挑选）。
    #[serde(default = "default_true")]
    pub auto_download: bool,
    /// 自动创建的任务以 paused 落库。
    #[serde(default)]
    pub start_paused: bool,
    /// 空 = 内置主队列。
    #[serde(default)]
    pub queue_id: String,
    /// 空 = 队列目录 → 全局目录。
    #[serde(default)]
    pub save_dir: String,
    /// 抓取间隔（分钟）；0 = 引擎默认 30。
    #[serde(default)]
    pub interval_minutes: i32,
    /// 包含关键词（`|` = 或，空格 = 且；空 = 不过滤）。
    #[serde(default)]
    pub include_pattern: String,
    #[serde(default)]
    pub exclude_pattern: String,
    #[serde(default)]
    pub use_regex: bool,
    #[serde(default)]
    pub smart_episode: bool,
    /// 体积下限（字节，0 = 不限）。
    #[serde(default)]
    pub size_min_bytes: i64,
    /// 体积上限（字节，0 = 不限）。
    #[serde(default)]
    pub size_max_bytes: i64,
    #[serde(default = "default_true")]
    pub send_referer: bool,
    #[serde(default = "default_true")]
    pub notify_on_download: bool,
    /// 每轮最多新建任务数（1..=100）；0 = 引擎默认 20。
    #[serde(default)]
    pub max_per_fetch: i32,
    #[serde(default)]
    pub cookies: String,
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub proxy_url: String,
    /// 只读：上次发起抓取的 Unix 秒（0 = 从未）。
    #[serde(default)]
    pub last_fetch_at: i64,
    /// 只读：上次成功抓取的 Unix 秒（0 = 从未）。
    #[serde(default)]
    pub last_success_at: i64,
    /// 只读：上次失败原因（空 = 健康）。
    #[serde(default)]
    pub last_error: String,
    /// 只读：连续失败次数（驱动指数退避）。
    #[serde(default)]
    pub fail_count: i32,
    /// 只读：首轮抓取是否已完成。
    #[serde(default)]
    pub seeded: bool,
    #[serde(default)]
    pub position: i32,
    /// 只读：未处理条目数（侧边栏 badge）。
    #[serde(default)]
    pub unread_count: i32,
}

fn default_rss_provider_id() -> String {
    "rss".to_string()
}

/// 订阅流中的一个条目（`GET /api/v1/rss/{id}/items`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RssItemDto {
    pub source_id: String,
    /// 去重主键。
    pub guid: String,
    pub title: String,
    pub link: String,
    /// enclosure 直链（空 = 回退 `link`）。
    pub enclosure_url: String,
    /// enclosure 声明大小（字节，0 = 未知）。
    pub enclosure_length: i64,
    /// 发布时间（Unix 秒，0 = 未知）。
    pub pub_date: i64,
    pub fetched_at: i64,
    /// 0=新 1=已下载 2=已忽略 3=规则未命中 4=重复剧集 5=首轮历史条目。
    pub status: i32,
    /// `status == 1` 时回链的任务 ID。
    pub task_id: String,
    /// 智能剧集归一键（空 = 未识别）。
    pub episode_key: String,
    /// 稳定原因码（`excluded`/`too_large`/`dup_episode`/`seed_skipped`/…；
    /// 空 = 无）。**客户端负责本地化**。
    pub reason: String,
}

/// 对条目执行手动操作（`POST /api/v1/rss/{id}/items/action`）。
///
/// guid 走请求体而不是路径段：真实 feed 的 guid 常常就是一整条 URL，
/// 塞进路径要双重编码，且被反向代理规范化后会静默改写。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RssItemActionRequest {
    /// `action = "readAll"` 时忽略。
    #[serde(default)]
    pub guid: String,
    /// `download`（绕过规则强制下载）/ `ignore` / `readAll`（全部标记已读）。
    pub action: String,
}

/// 验证一个订阅地址（`POST /api/v1/rss/validate`，只读、不落库）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RssValidateRequest {
    /// 订阅来源 provider；缺省/空为内置 RSS provider。
    #[serde(default = "default_rss_provider_id")]
    pub provider_id: String,
    /// provider 专属配置 JSON；缺省为空。
    #[serde(default)]
    pub provider_config: String,
    pub url: String,
    #[serde(default)]
    pub cookies: String,
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub proxy_url: String,
}

/// feed 验证结果。`error` 非空即验证失败（HTTP 状态仍是 200——这是一次
/// **诊断**调用，失败原因本身就是有效载荷）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RssValidateResponse {
    pub url: String,
    /// feed 标题（供回填订阅名）。
    pub feed_title: String,
    /// 最近条目预览。
    pub items: Vec<RssItemDto>,
    /// 无错误时为空。
    pub error: String,
}

// ---------------------------------------------------------------------------
// WS 服务端 → 客户端
// ---------------------------------------------------------------------------

/// 分段字节范围与进度（`segmentProgress` 载荷）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SegmentDetailDto {
    pub index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
}

/// 多 CDN 单节点描述（`taskCdnEvent` 载荷）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CdnNodeDto {
    /// 节点 IP；SYS 兜底节点为 "SYS"。
    pub ip: String,
    /// 候选来源："sys" / "doh:<端点IP>" / "ecs:<端点IP>"；SYS 为空串。
    pub origin: String,
    /// 本任务经该节点下载的字节数（summary 有效，其余 0）。
    pub bytes: i64,
    /// EWMA 吞吐（B/s）：pool = 健康度先验（0 = 无先验）；summary = 实测。
    pub ewma_bps: i64,
    /// 当前未归还的段租约数（leases 快照有效，其余 0）。
    pub active: i32,
}

/// 任务在等待队列中的位置（`queuePositionsChanged` 载荷 / 快照 `queuePositions`）。
///
/// 全量列表按 `order` 升序排列，`position` = 下标 + 1。增量见
/// [`WsServerMsg::QueueOrderChanged`]。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct QueuePositionDto {
    pub task_id: String,
    /// 1-based，0 = 不在队列中。
    pub position: i32,
    /// 引擎分配的排序键：升序即启动顺序；任务留在队列期间不变（队首出队不改其余任务的键），
    /// 只有用户调整顺序时整表重排。客户端排序用它，展示序号按键的名次计算。
    #[serde(default)]
    pub order: i64,
}

/// 队列顺序增量的单项（[`WsServerMsg::QueueOrderChanged`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct QueueOrderEntryDto {
    pub task_id: String,
    pub order: i64,
}

/// 文件跟踪扫描判定的产物文件存在性变化（`fileMissingChanged` 载荷）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct FileMissingUpdateDto {
    pub task_id: String,
    /// true = 已完成任务的目标文件从磁盘上消失；false = 重新探测到存在（自愈）。
    pub missing: bool,
}

/// HLS 可选码率变体（`hlsSelectionRequest` 载荷）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct HlsQualityOptionDto {
    pub index: i32,
    pub bandwidth: i64,
    pub width: i64,
    pub height: i64,
}

/// 种子内单个文件条目（`btSelectionRequest` 载荷）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BtFileDto {
    pub index: i32,
    pub path: String,
    pub size: i64,
}

/// 插件 resolve 返回的可选变体（`resolveVariantRequest` 载荷）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ResolveVariantOptionDto {
    pub index: i32,
    pub label: String,
    pub container: String,
    pub bandwidth: i64,
    pub width: i64,
    pub height: i64,
    pub total_bytes: i64,
}

/// 服务端经 `/api/v1/ws` 推送的实时消息。
///
/// JSON 形态：`{"type":"taskProgress","taskId":"…",…}`（`type` 判别 + 扁平
/// camelCase 字段）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WsServerMsg {
    /// 任务进度（下载中周期推送；含 live speed —— REST `TaskDto` 无此字段）。
    TaskProgress {
        task_id: String,
        /// 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
        status: i32,
        downloaded_bytes: i64,
        total_bytes: i64,
        /// 字节/秒。
        speed: i64,
        /// BT 上传速率（字节/秒；非 BT 任务恒 0）。
        upload_speed: i64,
        file_name: String,
        save_dir: String,
        url: String,
        error_message: String,
        /// BT 做种累计上传字节数（非 BT 任务恒 0）。
        uploaded_bytes: i64,
        /// 0=none, 1=做种中, 2-7=停止原因, 8=排队做种（等待槽位）
        seeding_status: i32,
        /// 做种停止原因的人类可读描述（无则为空）。
        seeding_message: String,
        /// 累计做种秒数（发帧时刻；排队/暂停不计，非 BT 任务恒 0）。
        seeding_time_secs: i64,
    },
    /// 全部任务快照（连接建立时 + 引擎主动广播）。
    TasksSnapshot { tasks: Vec<TaskDto> },
    /// 分段级进度（详情面板分段可视化）。
    SegmentProgress {
        task_id: String,
        total_bytes: i64,
        segment_count: i32,
        segments: Vec<SegmentDetailDto>,
    },
    /// 动态分段拆分事件（驱动拆分动画）。
    SegmentSplit {
        task_id: String,
        parent_index: i32,
        parent_new_end: i64,
        child_index: i32,
        child_start: i64,
        child_end: i64,
        is_proactive: bool,
        total_segments: i32,
    },
    /// 多 CDN 并发下载的节点级活动事件（任务详情日志）。语义与字段约定见
    /// `fluxdown_engine::events::EngineEvent::TaskCdnEvent`。
    TaskCdnEvent {
        task_id: String,
        /// "pool" | "kick" | "breaker" | "fallback" | "summary"
        kind: String,
        host: String,
        nodes: Vec<CdnNodeDto>,
        ip: String,
        reason: String,
        candidates: i32,
        alive: i32,
        cap: i32,
        auto_cap: bool,
    },
    /// 任务元数据探测完成（文件名/大小确定）。
    TaskMetaProbed {
        task_id: String,
        file_name: String,
        total_bytes: i64,
    },
    /// 命名队列列表变化。
    QueuesChanged { queues: Vec<QueueDto> },
    /// 单任务队列归属变化（move_task_to_queue 定向广播）。
    TaskQueueChanged { task_id: String, queue_id: String },
    /// Auto 路由或站点规则标签的落定/变更（详情面板「链路」行）。
    /// `route` wire 标签同 `TaskDto.autoRoute`；恒非空（空串 = 非 Auto 且未命中规则，不广播）。
    TaskRouteChanged { task_id: String, route: String },
    /// 队列位置全量替换（首次发布 / 用户调整顺序导致整表重排）；按 `order` 升序。
    QueuePositionsChanged { positions: Vec<QueuePositionDto> },
    /// 队列顺序增量：先移除 `removed`，再按 `order` 插入 / 更新 `added`；结果按键升序，
    /// 位置为名次（见 [`crate::event::apply_queue_order`]）。队首出队只发被移除的任务，
    /// 其余任务的键不变，客户端不必逐行重算。
    QueueOrderChanged {
        removed: Vec<String>,
        added: Vec<QueueOrderEntryDto>,
    },
    /// 文件跟踪扫描结果增量：已完成任务的产物文件在磁盘上消失/回归。
    /// 只带本轮变化项，客户端按 taskId patch `fileMissing`，不重发全量快照。
    FileMissingChanged { updates: Vec<FileMissingUpdateDto> },
    /// Boost 优先任务变化。
    PriorityTaskChanged {
        priority_task_id: String,
        auto_paused_count: i32,
    },
    /// 请求客户端选择 HLS 画质（超时自动选最高带宽）。
    HlsSelectionRequest {
        task_id: String,
        options: Vec<HlsQualityOptionDto>,
    },
    /// 请求客户端选择 BT 文件（超时默认全部下载）。
    BtSelectionRequest {
        task_id: String,
        files: Vec<BtFileDto>,
    },
    /// 请求客户端选择插件 resolve 变体（画质/格式）；超时用插件提供的默认索引。
    ResolveVariantRequest {
        task_id: String,
        default_index: i32,
        options: Vec<ResolveVariantOptionDto>,
    },
    /// `ping` 应答（RTT 测量）。
    Pong {},
    /// 插件被自动禁用：`reason` 为 `"CircuitBreaker"`（连续超时/过载熔断）或
    /// `"Revoked"`（被插件市场撤回）。
    PluginAutoDisabled { identity: String, reason: String },
    /// BT 重复添加：新任务的 info-hash 已被 `existingTaskId` 持有，占位任务
    /// （`taskId`）已被引擎删除。客户端据此提示用户；`existingName` 可能为空。
    DuplicateTorrent {
        task_id: String,
        existing_task_id: String,
        existing_name: String,
    },
    /// 插件 onDone 钩子执行中（`running=true` 开始/`false` 结束）；同一任务可
    /// 有多个插件并发钩子，客户端按 `(taskId, pluginId)` 集合跟踪，用于在
    /// 已完成任务旁显示“插件处理中…”指示器。事件可能因 fire-and-forget 丢失
    /// （尤其是 `running=false`），客户端需自带看门狗超时兜底清除。
    PluginHookActivity {
        task_id: String,
        plugin_id: String,
        running: bool,
    },
    /// 插件表发生增删改（安装/卸载/启停/设置变更）；空载荷 ping，客户端收到后
    /// 全量 invalidate 插件列表查询。
    PluginsChanged {},
    /// 任务组列表变化（组建/删除/改名/回收后）；组进度仍由前端按
    /// `groupId` 对 `taskProgress` SUM 聚合，本消息不含进度字段。
    GroupsChanged { groups: Vec<GroupDto> },
    /// RSS 订阅列表变化（增删改 / 抓取状态 / 未读计数）；订阅数量少，直接
    /// 全量推，客户端整表替换。
    RssSourcesChanged { sources: Vec<RssSourceDto> },
    /// 某订阅的条目流快照（新→旧）。只在该源确有变化时下发——定时轮询抓到
    /// 0 条新条目时引擎静默，不打扰 UI。`notifyTitles` 是本轮自动建任务的
    /// 条目标题，客户端据此弹**一条**合批通知（无自动下载时为空数组）。
    RssItemsChanged {
        source_id: String,
        items: Vec<RssItemDto>,
        notify_titles: Vec<String>,
    },
    /// 新建订阅向导的 feed 验证结果。`error` 非空即验证失败——这是诊断
    /// 信息而非传输错误，客户端照常展示。
    RssFeedValidated {
        request_id: String,
        url: String,
        feed_title: String,
        items: Vec<RssItemDto>,
        error: String,
    },
    /// 组件安装/下载进度（`component` 固定 `"ffmpeg"`；`totalBytes=0` 表示未知）。
    ComponentProgress {
        component: String,
        downloaded_bytes: i64,
        total_bytes: i64,
    },
    /// 组件安装/卸载操作结果（成功/失败 + 说明）。
    ComponentResult {
        component: String,
        ok: bool,
        message: String,
    },
    /// 入站配对请求待本机用户核验（本机作为响应方收到远端 hello+配对码后，
    /// 等待管理员核对 SAS 并调用管理面 `POST /api/v1/link/pair/approve`）。
    /// 字段名保持历史 `incomingPairing` 事件兼容。
    LinkIncomingPairing {
        session_id: String,
        sas: String,
        name: String,
        platform: String,
    },
    /// 已配对设备名册发生变化（新增/移除），前端据此 invalidate 名册查询。
    /// 空载荷：名册本身走 REST 拉取，这里只做「该刷新了」的通知——配对落库
    /// （`PairingResponder::handle_confirm` 里的 `store.upsert`）发生在被
    /// 唤醒的后台任务中，早于 `pair/approve` 请求的 HTTP 响应返回；Web 若
    /// 只在 approve 的 onSuccess 里 refetch 名册会读到还没写入新设备的
    /// 陈旧快照，且没有其它机制能纠正它，靠这条消息触发前端重新拉取。
    LinkDevicesChanged {},
    /// 投递日志增量（新→旧，单次最多 100 条，按 `deliveryId` 合并）。任务真完成时的投递、以及
    /// 「模拟一次下载完成」都发生在前端拉过快照之后——没有这条推送，打开着
    /// 的日志面板就停在打开时的样子。引擎侧已按 500ms 节流。
    WebhookDeliveriesChanged { deliveries: Vec<WebhookDeliveryDto> },
}

// ---------------------------------------------------------------------------
// WS 客户端 → 服务端
// ---------------------------------------------------------------------------

/// 客户端经 `/api/v1/ws` 发来的入站消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WsClientMsg {
    /// 应答 `hlsSelectionRequest`。
    HlsSelection {
        task_id: String,
        selected_index: i32,
    },
    /// 应答 `btSelectionRequest`（空数组 = 全部文件）。
    BtSelection {
        task_id: String,
        selected_indices: Vec<i32>,
    },
    /// 应答 `resolveVariantRequest`。
    SelectVariant {
        task_id: String,
        selected_index: i32,
    },
    /// 设置单任务做种限制覆盖（qBittorrent 三态语义：-2 = 跟随全局，
    /// -1 = 不限制，>=0 = 自定义，0 视同不限制；分享率为千分比）。
    /// `upload_limit_bps` 为任务级做种上传限速（B/s，0 = 不限），在
    /// 下一次 torrent add 时烘焙生效。
    SetTaskSeedLimits {
        task_id: String,
        ratio_limit_milli: i64,
        post_ratio_limit_milli: i64,
        seed_time_limit_minutes: i64,
        inactive_time_limit_minutes: i64,
        #[serde(default)]
        upload_limit_bps: i64,
    },
    /// RTT 测量，服务端回 `pong`。
    Ping {},
}

// ---------------------------------------------------------------------------
// 扩展 REST 端点请求/响应体
// ---------------------------------------------------------------------------

/// 代理连通性测试请求（`POST /api/v1/proxy/test`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestRequest {
    /// `http` / `https` / `socks4` / `socks5`。
    pub proxy_type: String,
    pub host: String,
    pub port: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

/// 代理连通性测试响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestResponse {
    pub latency_ms: i64,
}

/// `daemon.config.proxyRoute` 参数：`rules` 为 `Some` 时以该草稿替代已保存的
/// `proxy_rules` 做校验与解释；`url` 为空时只校验。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProxyRouteRequest {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub rules: Option<String>,
}

/// 站点代理规则的一条严格校验问题；`index` 为数组下标（从 0 起），`field` 为空表示整表问题。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProxyRuleIssueDto {
    pub index: u32,
    pub field: String,
    pub code: String,
    #[serde(default)]
    pub detail: String,
}

/// 「测试网址」命中的规则；`index` 从 0 起。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProxyRouteMatchDto {
    pub index: u32,
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// `daemon.config.proxyRoute` 响应：`reason == ""` 表示请求没有给出 url；
/// `endpoint` 为 `type://host:port`，绝不带凭据。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProxyRouteResponse {
    #[serde(default)]
    pub issues: Vec<ProxyRuleIssueDto>,
    #[serde(default)]
    pub matched: Option<ProxyRouteMatchDto>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub endpoint: String,
}

/// 已保存的站点 HTTP Basic 凭据（`daemon.siteAuth.list`）；不携带密码。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteAuthEntryDto {
    /// `host` 或 `host:port`。
    pub site: String,
    pub user: String,
}

/// 单站点 HTTP Basic 凭据详情；仅由受保护的定向查询返回（`GET
/// /api/v1/site-auth/{site}`，须管理 token；本机 RPC `daemon.siteAuth.match`）。
/// `pass` 是明文密码，不脱敏——保留是为了表单可以回填原值；只应用于编辑对话框 /
/// 新建下载认证框这类需要原文的场景，不要在列表/日志里回显（L-1；列表接口
/// [`SiteAuthEntryDto`] 本就不含 `pass`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteAuthCredentialDto {
    pub site: String,
    pub user: String,
    pub pass: String,
}

/// 保存单站点 HTTP Basic 凭据的请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteAuthSaveRequest {
    pub site: String,
    pub user: String,
    pub pass: String,
}

/// `daemon.siteAuth.delete` 参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteAuthDeleteParams {
    pub site: String,
}

/// `daemon.siteAuth.get` 参数：`site` 可写 `host` / `host:port` 或完整 URL，服务端归一化后查询；
/// 结果为 [`SiteAuthCredentialDto`] 或 `null`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteAuthGetParams {
    pub site: String,
}

/// `daemon.siteAuth.match` 参数：按下载链接的站点键（`host` / `host:port`，与引擎
/// 自动套用规则同一实现）查已保存凭据；结果为 [`SiteAuthCredentialDto`] 或 `null`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteAuthMatchParams {
    pub url: String,
}

/// 引擎学习到的按域连接上限摘要（`daemon.config.connPolicy`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ConnPolicySummaryDto {
    pub domain_count: u64,
}

/// 系统代理检测结果（`daemon.config.systemProxy`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SystemProxyDto {
    /// 是否检测到系统代理。
    pub detected: bool,
    /// `http` / `https` / `socks4` / `socks5`；未检测到时为空串。
    pub proxy_type: String,
    pub host: String,
    pub port: u16,
    /// 逗号分隔的排除列表；未检测到时为空串。
    pub no_list: String,
}

/// Tracker 订阅刷新结果（`POST /api/v1/bt/tracker-sub/refresh`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TrackerSubRefreshResponse {
    /// 至少一个订阅源拉取成功。
    pub success: bool,
    /// 去重合并后的唯一 Tracker 数。
    pub tracker_count: i64,
    /// 成功拉取的源数。
    pub ok_sources: i64,
    /// 尝试的订阅源总数。
    pub total_sources: i64,
    /// 缓存更新时间（Unix 秒；本次未成功时沿用旧值）。
    pub updated_at: i64,
    /// 全部源失败时的错误摘要（成功时为空）。
    pub error: String,
}

/// ED2K 服务器订阅刷新结果（`POST /api/v1/ed2k/server-sub/refresh`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct Ed2kServerSubRefreshResponse {
    /// 至少一个订阅源拉取成功。
    pub success: bool,
    /// 去重合并后的唯一服务器（`ip:port`）数。
    pub server_count: i64,
    /// 成功拉取的源数。
    pub ok_sources: i64,
    /// 尝试的订阅源总数。
    pub total_sources: i64,
    /// 缓存更新时间（Unix 秒；本次未成功时沿用旧值）。
    pub updated_at: i64,
    /// 全部源失败时的错误摘要（成功时为空）。
    pub error: String,
}

/// 创建命名队列请求（`POST /api/v1/queues`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct CreateQueueRequest {
    pub name: String,
    #[serde(default)]
    pub speed_limit_kbps: i64,
    #[serde(default)]
    pub upload_limit_kbps: i64,
    #[serde(default)]
    pub max_concurrent: i32,
    #[serde(default)]
    pub default_save_dir: String,
    #[serde(default)]
    pub default_segments: i32,
    #[serde(default)]
    pub default_user_agent: String,
}

/// 更新命名队列请求（`PUT /api/v1/queues/{id}`），字段同创建。
pub type UpdateQueueRequest = CreateQueueRequest;

/// 移动任务到队列请求（`PUT /api/v1/tasks/{id}/queue`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct MoveQueueRequest {
    /// 空 = 默认队列。
    #[serde(default)]
    pub queue_id: String,
}

/// 队列每日定时计划请求（`PUT /api/v1/queues/{id}/schedule`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct QueueScheduleRequest {
    /// 定时计划是否启用。
    pub enabled: bool,
    /// 每日定时启动时间 `HH:MM`（空 = 不定时启动）。
    #[serde(default)]
    pub start_time: String,
    /// 每日定时停止时间 `HH:MM`（空 = 不定时停止）。
    #[serde(default)]
    pub stop_time: String,
    /// 生效星期位掩码：bit0=周一 … bit6=周日；0/缺省 = 每天。
    #[serde(default)]
    pub days: i32,
}

/// 队列内任务排序请求（`PUT /api/v1/queues/{id}/order`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ReorderQueueRequest {
    /// 队列内任务的完整新顺序（依次写入 1..N 的 queueOrder）。
    pub task_ids: Vec<String>,
}

/// 目录项（`FsListResponse.dirs` 元素）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub path: String,
}

/// 目录列举响应（`GET /api/v1/fs/list`，服务器端保存目录选择器用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct FsListResponse {
    /// 实际列举的目录（绝对路径）。
    pub path: String,
    /// 上级目录（根目录时为 None）。
    pub parent: Option<String>,
    /// 子目录列表（不含文件）。
    pub dirs: Vec<FsEntry>,
    /// 服务进程对该目录无读取权限（EACCES）。此时 `dirs` 必为空，但语义是
    /// 「看不到」而非「没有」——NAS 套件以受限用户运行、未给共享文件夹授权时
    /// 就是这种情况，前端据此提示授权而不是显示「空目录」。
    #[serde(default)]
    pub denied: bool,
}

/// 服务器运行状态（`GET /api/v1/stats`，前端状态栏用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct StatsResponse {
    /// 默认保存目录所在磁盘的剩余字节；探测失败为 None。
    pub disk_free_bytes: Option<u64>,
    pub save_dir: String,
    pub server_version: String,
    /// 当前 WS 连接数。
    pub ws_clients: usize,
    /// 演示模式开关（服务器以 `FLUXDOWN_DEMO_URL` 启动时为 true）。
    pub demo_mode: bool,
    /// 演示模式下唯一允许下载的 URL；非演示模式为空串。
    pub demo_url: String,
}

/// 单个日志文件（`GET /api/v1/logs` 列表项）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LogFileDto {
    pub name: String,
    pub size: i64,
}

/// 日志目录与文件清单（`GET /api/v1/logs`，前端「关于」页展示 + 导出入口）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LogsResponse {
    /// 日志目录绝对路径（NAS 用户据此在服务器文件系统定位日志）。
    pub dir: String,
    /// 全部日志文件（按日期 + 分卷序升序）。
    pub files: Vec<LogFileDto>,
    /// 日志 writer 是否已成功初始化。
    pub initialized: bool,
    /// 本次进程生命周期内是否发生过日志写入/轮转/清理失败。
    pub degraded: bool,
    /// 本次进程生命周期内累计日志基础设施失败次数。
    pub failure_count: u64,
    /// 最近一次日志基础设施失败；无失败时为 `null`。
    pub last_error: Option<String>,
}

/// token 重新生成响应（`POST /api/v1/token/regenerate`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    pub token: String,
    /// 生效说明（新 token 立即生效，旧 token 同时失效）。
    pub note: String,
}

/// 首次运行状态（`GET /api/v1/setup/status`，**无鉴权**）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SetupStatusResponse {
    /// `true` = 尚未设置访问密钥，Web 端应展示首次运行向导。
    pub setup_required: bool,
    /// 访问密钥最短长度（前端提示与校验用，避免两端各写一份常量）。
    pub min_length: i64,
}

/// 首次运行设置访问密钥请求（`POST /api/v1/setup`，**无鉴权**）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SetupRequest {
    /// 用户设定的访问密钥；规则见 `config::validate_access_key`。
    pub token: String,
}

// ---------------------------------------------------------------------------
// 组件（v1 仅 ffmpeg）
// ---------------------------------------------------------------------------

/// ffmpeg 组件状态（`GET /api/v1/components/ffmpeg`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ComponentFfmpegStatus {
    /// 生效路径来源：`manual` / `managed` / `system` / `none`。
    pub source: String,
    /// 生效的可执行文件路径（`source == "none"` 时为空）。
    pub path: String,
    /// `ffmpeg -version` 探测到的版本串（探测失败/未找到时为空）。
    pub version: String,
    /// 托管安装记录的版本号（空 = 未托管安装）。
    pub managed_version: String,
    /// 系统 PATH 中探测到的 ffmpeg 路径（无论是否生效；空 = 无）。
    pub system_path: String,
    /// 当前平台是否提供托管安装（BtbN 构建）。`false` = macOS 等——Web UI 隐藏
    /// 托管安装区块，只引导系统 PATH / 手动指定，避免反复弹「不支持安装」。
    pub managed_supported: bool,
}

/// ffmpeg 可安装版本列表（`GET /api/v1/components/ffmpeg/versions`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ComponentVersions {
    /// 降序排列的稳定版本号。
    pub versions: Vec<String>,
    /// 最新稳定版（= `versions` 首个；空 = 解析失败）。
    pub latest_stable: String,
}

/// yt-dlp 组件状态（`GET /api/v1/components/ytdlp`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ComponentYtdlpStatus {
    /// 生效路径来源：`manual` / `managed` / `system` / `none`。
    pub source: String,
    /// 生效的可执行文件路径（`source == "none"` 时为空）。
    pub path: String,
    /// `yt-dlp --version` 探测到的版本串（探测失败/未找到时为空）。
    pub version: String,
    /// 托管安装记录的版本号（空 = 未托管安装）。
    pub managed_version: String,
    /// 系统 PATH 中探测到的 yt-dlp 路径（无论是否生效；空 = 无）。
    pub system_path: String,
    /// 当前平台是否提供托管安装（GitHub Release 构建）。
    pub managed_supported: bool,
}

/// 安装/更新 ffmpeg 请求（`POST /api/v1/components/ffmpeg/install`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct InstallFfmpegRequest {
    /// 钉住的版本号；`None` = 安装/更新到最新稳定版。
    #[serde(default)]
    pub version: Option<String>,
}

// ---------------------------------------------------------------------------
// Doctor 动态探测（`daemon.diagnostics.probe`）
// ---------------------------------------------------------------------------

/// 存储探测目录的用途。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageProbeRole {
    /// 全局默认保存目录（`default_save_dir`；未配置时为系统下载目录）。
    DefaultSaveDir,
    /// 队列的默认保存目录。
    Queue,
    /// RSS 源的保存目录。
    Rss,
    /// 分类保存目录（agent 偏好，经 [`DiagnosticsProbeParams::extra_dirs`] 传入）。
    Category,
    /// daemon 数据目录（下载数据库、BT 会话、托管组件、日志）。
    DataDir,
}

/// 待探测的目录。daemon 自己持有的目录（默认 / 队列 / RSS / 数据目录）由 daemon 收集，
/// 调用方只追加 daemon 不知道的目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageProbeTarget {
    pub role: StorageProbeRole,
    /// 展示用名称（队列名 / RSS 源名 / 分类名）；默认目录与数据目录为空。
    #[serde(default)]
    pub label: String,
    pub path: String,
}

/// `daemon.diagnostics.probe` 参数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsProbeParams {
    #[serde(default)]
    pub extra_dirs: Vec<StorageProbeTarget>,
}

/// 存储探测失败的分类（由 IO 错误种类归并，决定 UI 给出的处理建议）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageProbeFailure {
    /// 无权限（含 macOS 隐私授权 / TCC 拒绝、ACL、属主不符）。
    PermissionDenied,
    /// 只读文件系统 / 只读挂载。
    ReadOnly,
    /// 磁盘或配额已满。
    NoSpace,
    /// 路径及其所有上级目录都不存在（如未挂载的移动硬盘 / 网络盘）。
    Missing,
    /// 路径（或最近的已存在上级）不是目录。
    NotDirectory,
    /// 探测在时限内未完成（常见于失联的网络挂载）。
    Timeout,
    /// 其它 IO 错误或读回内容不一致。
    Other,
}

/// 单个目录的动态写入探测结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageProbeDto {
    pub role: StorageProbeRole,
    #[serde(default)]
    pub label: String,
    pub path: String,
    /// 目标目录已存在；不存在时在最近的已存在上级目录上探测（首次下载时会自动创建）。
    pub exists: bool,
    /// 实际执行写入探测的目录；找不到可用上级时为空。
    #[serde(default)]
    pub probed_dir: String,
    /// 失败的步骤（`create` / `write` / `sync` / `read` / `rename` / `remove` / `stat`）；通过时为空。
    #[serde(default)]
    pub failed_step: String,
    /// 通过时为 `None`。
    #[serde(default)]
    pub failure: Option<StorageProbeFailure>,
    /// 原始错误描述；通过时为空。
    #[serde(default)]
    pub error: String,
    /// 探测目录所在卷的可用空间；无法获取时为 `None`。
    #[serde(default)]
    pub available_bytes: Option<u64>,
    /// 失败时的系统错误码（`EACCES`=13 / `EPERM`=1 / Windows `ERROR_ACCESS_DENIED`=5 …）；
    /// 同为「无权限」时据此区分属主 / ACL 问题与 macOS 隐私授权（TCC）拒绝。
    #[serde(default)]
    pub os_error: Option<i32>,
}

/// 外部组件（ffmpeg / ffprobe / yt-dlp）的实际运行探测结果。只报告已解析到路径的组件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentProbeDto {
    /// `ffmpeg` / `ffprobe` / `yt-dlp`。
    pub name: String,
    pub path: String,
    /// 版本输出首行；失败时为空。
    #[serde(default)]
    pub output: String,
    /// 启动被拒（`permissionDenied`）等失败的根因描述；可运行时为空。
    #[serde(default)]
    pub error: String,
    /// 启动被系统拒绝（无执行权限、noexec 挂载、隔离属性 / 安全软件拦截）。
    #[serde(default)]
    pub permission_denied: bool,
    /// 生效路径位于 daemon 数据目录内（托管安装），daemon 可就地修复其执行权限。
    #[serde(default)]
    pub managed: bool,
}

/// `daemon.diagnostics.fixComponent` 参数：把托管组件的执行权限还给当前用户。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentRepairParams {
    /// [`ComponentProbeDto::name`]（`ffmpeg` / `ffprobe` / `yt-dlp`）。
    pub name: String,
}

/// `daemon.diagnostics.probe` 结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsProbeResult {
    pub storage: Vec<StorageProbeDto>,
    pub components: Vec<ComponentProbeDto>,
}

// ---------------------------------------------------------------------------
// Webhook 任务事件推送（免费自托管）
// ---------------------------------------------------------------------------

/// 一条投递记录（`GET /api/v1/webhooks/deliveries` 列表项）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct WebhookDeliveryDto {
    pub delivery_id: String,
    /// Unix 毫秒。
    pub timestamp_ms: i64,
    /// 事件 wire 名（`task.completed` 等）。
    pub event: String,
    pub endpoint_id: String,
    pub endpoint_name: String,
    pub url: String,
    /// 请求头摘录，每行 `K: V`；鉴权类值已掩码。
    pub request_headers: String,
    pub request_body: String,
    /// HTTP 状态码；0 = 未拿到响应。
    pub status_code: i32,
    pub response_body: String,
    pub latency_ms: i64,
    pub attempts: i32,
    pub success: bool,
    pub error: String,
}

/// 服务预设元数据（`GET /api/v1/webhooks/deliveries` 一并返回）——Web 端
/// 「实时载荷预览」的模板来源，客户端只做占位符替换，不复制模板内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct WebhookPresetDto {
    pub id: String,
    pub label: String,
    pub url_placeholder: String,
    pub default_template: String,
    pub content_type: String,
}

/// 投递日志 + 预设目录（`GET /api/v1/webhooks/deliveries`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct WebhookDeliveriesResponse {
    /// 新的在前，最多 [`crate::WEBHOOK_DELIVERY_LIMIT`] 条（引擎落盘环形缓冲，重启回灌）。
    pub deliveries: Vec<WebhookDeliveryDto>,
    pub presets: Vec<WebhookPresetDto>,
    /// 可用占位符清单（`{task.fileName}` 等）。
    pub variables: Vec<String>,
}

/// 测试投递请求（`POST /api/v1/webhooks/test`）。
///
/// 直接内嵌端点草稿（**无需先保存**），schema 同 `webhook.endpoints` 数组元素。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct WebhookTestRequest {
    #[serde(flatten)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub endpoint: serde_json::Value,
}

/// 测试投递结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct WebhookTestResponse {
    pub success: bool,
    /// HTTP 状态码；0 = 未拿到响应。
    pub status_code: i32,
    pub latency_ms: i64,
    /// 成功时为空。
    pub error: String,
}

/// `POST /api/v1/webhooks/simulate` 的回执。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct WebhookSimulateResponse {
    /// 按订阅规则投出去的端点数。0 = 没有端点订阅 `task.completed`，
    /// 前端该直说而不是干等投递记录。
    pub dispatched: i32,
}
