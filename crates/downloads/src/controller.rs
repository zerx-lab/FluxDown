use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
};

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, CloudDevice, DaemonEvent, DaemonRuntimeStatsDto, DaemonSnapshot,
    GroupDto, LinkDeviceInfo, LinkDispatchParams, QueueDto, RemoteCommandParams,
    RemoteDispatchParams, RemoteTaskDto, ServiceEvent, TaskActivityPage, TaskActivityQuery,
    TaskDto, TaskRuntimeDto, WsServerMsg,
};

use crate::model::{
    CategoryIndex, DownloadTaskView, TaskStore,
    devices::{DeviceEntry, local_device_id, other_devices},
    file_conflict::FileConflictSet,
    group_tree::{GroupAggregate, GroupMeta},
};

/// 本机偏好：新建下载对话框上次使用的保存目录（设备本地，不进云同步）。
pub const LAST_SAVE_DIR_PREF: &str = "download.last_save_dir";
/// 偏好：新建下载默认沿用上次保存目录。
pub const REMEMBER_LAST_SAVE_DIR_PREF: &str = "download.remember_last_save_dir";
/// 本机偏好：新建下载保存目录历史（字符串数组，最新在前；设备本地，不进云同步）。
pub const SAVE_DIR_HISTORY_PREF: &str = "download.save_dir_history";
/// 本机偏好：是否记录并在新建下载中列出保存目录历史（缺省开启；设备本地）。
pub const SAVE_DIR_HISTORY_ENABLED_PREF: &str = "download.save_dir_history_enabled";
/// 本机偏好：按分类记住的手动保存目录（`{分类 id: 字符串数组}`，最新在前；设备本地，不进云
/// 同步，与保存目录历史共用开关与清空）。
pub const CATEGORY_SAVE_DIR_HISTORY_PREF: &str = "download.category_save_dir_history";
/// 本机偏好：下载限速最近自定义值（KB/s 整数数组，最新在前；设备本地，不进云同步）。
pub const SPEED_LIMIT_RECENT_PREF: &str = "download.speed_limit_recent_kbps";
/// 本机偏好：上传限速最近自定义值（KB/s 整数数组，最新在前；设备本地，不进云同步）。
pub const UPLOAD_LIMIT_RECENT_PREF: &str = "download.upload_limit_recent_kbps";
/// 本机偏好：新建下载「下载到」上次选择的目标（`local` / `cloud:<id>` / `link:<fp>`），
/// 设备本地，不进云同步。
pub const LAST_DOWNLOAD_TARGET_PREF: &str = "download.last_target";

pub type PortFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, fluxdown_protocol::RpcErrorData>> + Send + 'static>>;

/// 队列创建 / 更新表单字段（与 `CreateQueueRequest` 一一对应）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueueFields {
    pub name: String,
    pub speed_limit_kbps: i64,
    pub upload_limit_kbps: i64,
    pub max_concurrent: i32,
    pub default_save_dir: String,
    pub default_segments: i32,
    pub default_user_agent: String,
}

/// BT 做种上限（`daemon.task.setSeedLimits`）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeedLimits {
    pub ratio_limit_milli: i64,
    pub post_ratio_limit_milli: i64,
    pub seed_time_limit_minutes: i64,
    pub inactive_time_limit_minutes: i64,
    pub upload_limit_bps: i64,
}

/// `SeedLimits` 各限制字段的「跟随全局」哨兵（与 `native/protocol` 一致）。
pub(crate) const SEED_LIMIT_FOLLOW_GLOBAL: i64 = -2;

/// 引擎单任务删除时广播的 `TaskProgress` 哨兵（`status == 4` 且 `error_message` 为此值）。
const DELETED_SENTINEL: &str = "deleted";

impl SeedLimits {
    /// 全部跟随全局（上传限速 0 = 未设置）。
    #[must_use]
    pub(crate) fn inherit_all() -> Self {
        Self {
            ratio_limit_milli: SEED_LIMIT_FOLLOW_GLOBAL,
            post_ratio_limit_milli: SEED_LIMIT_FOLLOW_GLOBAL,
            seed_time_limit_minutes: SEED_LIMIT_FOLLOW_GLOBAL,
            inactive_time_limit_minutes: SEED_LIMIT_FOLLOW_GLOBAL,
            upload_limit_bps: 0,
        }
    }

    /// 任务当前生效的单任务做种限制。
    #[must_use]
    pub(crate) fn from_dto(dto: &TaskDto) -> Self {
        Self {
            ratio_limit_milli: dto.seed_ratio_limit_milli,
            post_ratio_limit_milli: dto.seed_post_ratio_limit_milli,
            seed_time_limit_minutes: dto.seed_time_limit_minutes,
            inactive_time_limit_minutes: dto.seed_inactive_time_limit_minutes,
            upload_limit_bps: dto.seed_upload_limit_bps,
        }
    }
}

pub enum DownloadsCommand {
    /// 分页读取 daemon 持久活动历史。
    TaskActivity(TaskActivityQuery),
    /// 读取 daemon 持久化的任务全生命周期速度曲线与统计（`daemon.task.speedHistory`）。
    TaskSpeedHistory(fluxdown_protocol::TaskSpeedHistoryQuery),
    /// 读取 BT 任务的 peer / tracker 实况（`daemon.task.btStatus`），详情页轮询。
    BtStatus(fluxdown_protocol::DaemonBtStatusParams),
    /// 防吸血：给 IP 组加封禁 / 限速 / 加白条目（`daemon.bt.guard.entries.add`），连接页右键菜单用。
    BtGuardAddEntry(fluxdown_protocol::BtGuardEntriesAddParams),
    /// 防吸血「查看证据」：读取该 IP 组相关的名单条目与近期事件，结果为
    /// [`DownloadsResult::BtGuardEvidence`]。
    BtGuardEvidence {
        ip_group: String,
    },
    Create(Box<fluxdown_protocol::DaemonCreateTaskParams>),
    /// 批量建任务（`daemon.task.createMany`）：整块一个事务、按 `batch_id` 幂等，
    /// 结果不确定时可原样重发。
    CreateMany(Box<fluxdown_protocol::DaemonCreateTasksParams>),
    Pause {
        task_id: String,
    },
    Resume {
        task_id: String,
    },
    Rename {
        task_id: String,
        file_name: String,
    },
    /// 更换下载源地址（`daemon.task.changeUrl`）：仅非活跃、未完成、非 BT 任务；
    /// 失败为引擎稳定错误码（`invalid-url` / `task-active` / `protocol-mismatch` …）。
    ChangeUrl {
        task_id: String,
        url: String,
    },
    /// 修改线程数（`daemon.task.setSegments`）：`<= 0` = 自动，已下进度保留；
    /// 下载中任务由引擎静默暂停 → 改配置 → 恢复。
    SetSegments {
        task_id: String,
        segments: i32,
    },
    /// 已完成 BT 任务的完整性校验（`daemon.task.verify`，慢方法）：结果为
    /// [`DownloadsResult::TaskVerify`]；同一任务校验在途时为 `conflict`。
    VerifyTask {
        task_id: String,
    },
    /// BT 任务的分享材料（`daemon.task.torrentShare`）：结果为
    /// [`DownloadsResult::TorrentShare`]。
    TorrentShare {
        task_id: String,
        include_torrent: bool,
    },
    Delete {
        task_id: String,
        delete_files: bool,
    },
    /// 批量暂停（`daemon.task.pauseMany`）：整批一次 RPC、一次任务快照。
    PauseMany {
        task_ids: Vec<String>,
    },
    /// 批量继续（`daemon.task.resumeMany`）。
    ResumeMany {
        task_ids: Vec<String>,
    },
    /// 批量删除（`daemon.task.deleteMany`）：同一批共用 `delete_files`。
    DeleteMany {
        task_ids: Vec<String>,
        delete_files: bool,
    },
    PauseAll,
    ResumeAll,
    /// 插件重试挂起 → 忽略并按普通任务继续。
    IgnorePluginRetry {
        task_id: String,
    },
    /// `daemon.queue.boost`：同 id 再次调用即取消。
    ToggleBoost {
        task_id: String,
    },
    /// 删除（含文件）后按原参数重建。
    Redownload(Box<fluxdown_protocol::CreateTaskRequest>, String),
    MoveToQueue {
        task_id: String,
        queue_id: String,
    },
    SetSeedLimits {
        task_id: String,
        limits: SeedLimits,
    },
    /// daemon 公开配置增量写入（乐观并发：`expected_revision`）。
    PatchConfig {
        values: BTreeMap<String, String>,
        expected_revision: u64,
    },
    QueueCreate(QueueFields),
    QueueUpdate {
        queue_id: String,
        fields: QueueFields,
    },
    QueueSchedule {
        queue_id: String,
        enabled: bool,
        start_time: String,
        stop_time: String,
        days: i32,
    },
    QueueStart {
        queue_id: String,
    },
    QueueStop {
        queue_id: String,
    },
    QueueDelete {
        queue_id: String,
    },
    GroupPause {
        group_id: String,
    },
    GroupResume {
        group_id: String,
    },
    GroupDelete {
        group_id: String,
        delete_files: bool,
    },
    ResolveSelection(fluxdown_protocol::SelectionResolutionDto),
    /// 外部捕获确认 / 忽略；确认时携带表单产出的建任务参数，由 agent 与捕获原请求合并。
    CaptureResolve(Box<fluxdown_protocol::CaptureResolveParams>),
    /// 只读插件清单预解析；外部捕获上下文仍由 agent 持有。
    ResolvePreview {
        request: Box<fluxdown_protocol::CreateTaskRequest>,
        transaction_id: Option<String>,
    },
    /// 用户确认的插件清单建组，不再创建原始链接的普通任务。
    CreateGroup {
        request: Box<fluxdown_protocol::CreateGroupRequest>,
        context: Box<fluxdown_protocol::CreateTaskRequest>,
        transaction_id: Option<String>,
    },
    /// 按下载链接匹配已保存的站点 HTTP 凭据（新建下载表单自动回填）。
    SiteAuthMatch {
        url: String,
    },
    /// 新建下载表单的只读元信息预探测（`daemon.link.probe`，只发一次 HEAD，不消耗
    /// 一次性令牌）；结果为 [`fluxdown_protocol::LinkProbeResult`]。
    ProbeLink {
        url: String,
    },
    /// 经 FluxCloud 把链接下发到账号内另一台设备（`agent.remote.dispatch`）。
    RemoteDispatch(RemoteDispatchParams),
    /// 下发到局域网已配对设备（`agent.link.dispatch`）。
    LinkDispatch(LinkDispatchParams),
    /// 远程任务控制（`agent.remote.command`）。
    RemoteCommand(RemoteCommandParams),
    OpenTask {
        task_id: String,
    },
    RevealTask {
        task_id: String,
    },
    /// 把已完成任务的产物以文件形式写入系统剪贴板（`agent.platform.copyTaskFiles`，可在文件管理器
    /// 里粘贴）；任一产物已不在磁盘上即整体失败（`notFound`）。
    CopyTaskFiles {
        task_ids: Vec<String>,
    },
    /// 重扫已完成任务的产物是否仍在下载目录（`daemon.task.rescan`，结果经
    /// `fileMissingChanged` 事件回流）。
    RescanFiles,
    /// 本机 `.torrent` 文件：agent 读取、上传 blob 后按捕获路径建任务。
    /// `silent = false`（用户主动打开）走 BT 文件选择；`true` 仅用于扩展 / 文件关联
    /// 这类无人值守入口（全选文件）。保存目录 / 队列 / 开始暂停由表单入口携带，
    /// 缺省（`None`）沿用 agent 的默认行为。
    SubmitTorrentFile {
        path: String,
        silent: bool,
        save_dir: Option<String>,
        queue_id: Option<String>,
        start_paused: Option<bool>,
    },
    /// 本机做种（`daemon.task.seedLocal`，慢方法：无现成种子时 daemon 要为数据做哈希）：
    /// 把 daemon 主机上已有的文件 / 文件夹登记为已完成 BT 任务并开始做种。结果为
    /// `{taskId, duplicate?, existingName?}`（`DownloadsResult::Value`；`duplicate` 为真时未新建任务，
    /// `taskId` 是已持有同 info hash 的现有任务）；路径 / 种子不匹配或 BT 被禁用为 `invalidArgument`。
    /// 提交类命令，失败后不自动重试。`trackers` 仅在 `torrent_path` 为 `None` 时有意义。
    SeedLocal {
        path: String,
        torrent_path: Option<String>,
        trackers: Vec<String>,
        unlimited: bool,
    },
    /// 系统文件管理器为该文件显示的图标（`agent.platform.fileIcon`，结果为 PNG）。
    FileIcon(fluxdown_protocol::PlatformFileIconParams),
    /// 设备本地偏好写入（`sync:false`，不进云同步）。
    SetLocalPreference {
        key: &'static str,
        value: serde_json::Value,
    },
    /// 云同步偏好写入（`ui.*` 等）。
    SetSyncedPreference {
        key: &'static str,
        value: serde_json::Value,
    },
}

impl DownloadsCommand {
    /// 用户主动打开的 `.torrent`（菜单 / 拖入）：走 BT 文件选择，其余沿用 agent 默认。
    #[must_use]
    pub(crate) fn open_torrent_file(path: &std::path::Path) -> Self {
        Self::SubmitTorrentFile {
            path: path.display().to_string(),
            silent: false,
            save_dir: None,
            queue_id: None,
            start_paused: None,
        }
    }
}

pub enum DownloadsResult {
    Unit,
    Value(serde_json::Value),
    TaskActivity(TaskActivityPage),
    /// 无记录（从未下载或已重新下载重置）时为 `None`。
    TaskSpeedHistory(Option<fluxdown_protocol::TaskSpeedHistoryDto>),
    /// `BtStatus` 的 peer / tracker 实况。
    BtStatus(fluxdown_protocol::BtStatusDto),
    /// `BtGuardEvidence` 的名单条目与事件。
    BtGuardEvidence(BtGuardEvidence),
    Preview(fluxdown_protocol::ResolvePreviewResponse),
    /// `FileIcon` 的 PNG 字节。
    FileIcon(Vec<u8>),
    /// `CreateMany` 的逐条结果（与请求条目同序）。
    CreateMany(fluxdown_protocol::DaemonCreateTasksResult),
    /// `VerifyTask` 的校验报告。
    TaskVerify(fluxdown_protocol::TaskVerifyResult),
    /// `TorrentShare` 的磁力链接与（按需）已解码的 `.torrent` 字节。
    TorrentShare(TorrentShare),
}

/// [`DownloadsResult::TorrentShare`] 载荷。
pub struct TorrentShare {
    pub name: String,
    pub magnet: String,
    /// `.torrent` 文件内容；未请求或纯磁力任务（库里没有种子字节）为 `None`。
    pub torrent: Option<Vec<u8>>,
}

/// [`DownloadsResult::BtGuardEvidence`] 载荷：宿主按 IP 组精确查询得到的条目（覆盖该 IP 的
/// 全部名单层）与事件（该 IP 组的事件），UI 直接展示，不再客户端筛选。
#[derive(Clone, Debug, Default)]
pub struct BtGuardEvidence {
    pub entries: Vec<fluxdown_protocol::BtGuardEntryDto>,
    pub events: Vec<fluxdown_protocol::BtGuardEventDto>,
}

impl DownloadsResult {
    /// 建任务类命令（`daemon.task.create` → `{taskId}`、`agent.capture.resolve` /
    /// 种子上传 → `{taskIds}`）返回的新任务 ID；其它结果为空。
    #[must_use]
    pub fn created_task_ids(&self) -> Vec<String> {
        let Self::Value(value) = self else {
            return Vec::new();
        };
        if let Some(id) = value.get("taskId").and_then(serde_json::Value::as_str) {
            return vec![id.to_owned()];
        }
        value
            .get("taskIds")
            .and_then(serde_json::Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub trait DownloadsPort: Send + Sync {
    fn execute(&self, command: DownloadsCommand) -> PortFuture<DownloadsResult>;
}

/// 「全部开始」能恢复哪些暂停任务（见 [`DownloadsController::resume_targets`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResumeTargets {
    /// 有运行中队列里的暂停任务。
    Some,
    /// 暂停任务全在已停止的队列里：全部开始不会动它们，需先启动队列。
    OnlyInStoppedQueues,
    /// 没有暂停任务。
    None,
}

/// 任务组聚合（由任务行 + `snapshot.daemon.groups` 计算，覆盖组内全部本地成员）。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GroupSummary {
    pub(crate) id: String,
    pub(crate) meta: GroupMeta,
    pub(crate) aggregate: GroupAggregate,
}

pub struct DownloadsController {
    port: Arc<dyn DownloadsPort>,
    local: Vec<TaskDto>,
    remote: Vec<RemoteTaskDto>,
    store: Rc<TaskStore>,
    /// 任务实时速度 `(下载, 上传)`（字节/秒）。
    live_speeds: HashMap<String, (i64, i64)>,
    task_runtime: BTreeMap<String, Rc<TaskRuntimeDto>>,
    boosted: Option<String>,
    /// 引擎待启动队列排序键（task_id → `QueuePositionDto::order`）。行里存键，智能排序的排队档
    /// 按键排列；展示序号是键在 `store` 有序键集合里的名次，不随队首出队逐行改写。
    queue_keys: HashMap<String, i64>,
    queues: Vec<QueueDto>,
    groups: Vec<GroupDto>,
    group_summaries: Vec<GroupSummary>,
    group_summaries_generation: u64,
    cloud_devices: Vec<CloudDevice>,
    cloud_connected: bool,
    agent_stale: bool,
    linked_devices: Vec<LinkDeviceInfo>,
    config: BTreeMap<String, String>,
    config_revision: u64,
    runtime_stats: DaemonRuntimeStatsDto,
    preferences: BTreeMap<String, serde_json::Value>,
    categories: Rc<CategoryIndex>,
    /// 本机在云账号里的设备 id（会话优先，名册 `is_current` 兜底；未登录为 `None`）。
    session_device: Option<String>,
    /// 待确认的「文件已存在」请求；任务行角标由它派生，随快照 / 选择事件增量维护。
    conflicts: FileConflictSet,
    stale: bool,
}

impl DownloadsController {
    #[must_use]
    pub fn new(port: Arc<dyn DownloadsPort>) -> Self {
        Self {
            port,
            local: Vec::new(),
            remote: Vec::new(),
            store: Rc::new(TaskStore::default()),
            live_speeds: HashMap::new(),
            task_runtime: BTreeMap::new(),
            boosted: None,
            queue_keys: HashMap::new(),
            queues: Vec::new(),
            groups: Vec::new(),
            group_summaries: Vec::new(),
            group_summaries_generation: u64::MAX,
            cloud_devices: Vec::new(),
            cloud_connected: false,
            agent_stale: true,
            linked_devices: Vec::new(),
            config: BTreeMap::new(),
            config_revision: 0,
            runtime_stats: DaemonRuntimeStatsDto::default(),
            preferences: BTreeMap::new(),
            categories: Rc::new(CategoryIndex::from_preference(None)),
            session_device: None,
            conflicts: FileConflictSet::default(),
            stale: true,
        }
    }

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot) {
        self.live_speeds.clear();
        self.local.clone_from(&snapshot.daemon.tasks);
        self.note_loading(&snapshot.daemon);
        self.remote.clone_from(&snapshot.remote_tasks);
        self.cloud_devices.clone_from(&snapshot.cloud_devices);
        self.cloud_connected =
            snapshot.cloud_connection.state == fluxdown_protocol::CloudConnectionState::Connected;
        self.agent_stale = false;
        self.linked_devices.clone_from(&snapshot.linked_devices);
        self.session_device = snapshot
            .session
            .as_ref()
            .map(|session| session.device.device_id.clone());
        self.absorb_daemon_context(&snapshot.daemon);
        self.set_preferences(&snapshot.preferences.values);
        self.stale = !snapshot.daemon_connected;
        if self.stale {
            self.task_runtime.clear();
        }
        self.rebuild_all();
        self.rebuild_remote();
    }

    /// 应用事件；返回 `true` 表示任务行 / 队列 / 组等表格可见数据有变化。
    pub fn apply_event(&mut self, event: &ServiceEvent) -> bool {
        let ServiceEvent::Agent(event) = event else {
            return false;
        };
        match event {
            AgentEvent::DaemonSnapshotReplaced(snapshot) => {
                self.live_speeds.clear();
                self.local.clone_from(&snapshot.tasks);
                self.note_loading(snapshot);
                self.absorb_daemon_context(snapshot);
                self.stale = false;
                self.rebuild_all();
                true
            }
            AgentEvent::DaemonConnectionChanged(connected) => {
                self.stale = !connected;
                if !connected {
                    self.live_speeds.clear();
                    self.task_runtime.clear();
                }
                self.rebuild_all();
                true
            }
            AgentEvent::RemoteTasksChanged(tasks) => {
                self.remote.clone_from(tasks);
                self.rebuild_remote();
                true
            }
            AgentEvent::CloudDevicesChanged(devices) => {
                self.cloud_devices.clone_from(devices);
                // 名册里的 `is_current` 可能决定本机 id：镜像行过滤随之刷新。
                self.rebuild_remote();
                true
            }
            AgentEvent::CloudConnectionChanged(connection) => {
                self.cloud_connected =
                    connection.state == fluxdown_protocol::CloudConnectionState::Connected;
                true
            }
            // 会话结束（登出 / 被撤销）后不再展示旧账号的设备与远程任务。
            AgentEvent::SessionChanged(session) => {
                self.session_device = session
                    .as_ref()
                    .as_ref()
                    .map(|session| session.device.device_id.clone());
                if self.session_device.is_none() {
                    self.cloud_connected = false;
                    self.cloud_devices.clear();
                    self.remote.clear();
                }
                self.rebuild_remote();
                true
            }
            AgentEvent::LinkedDevicesChanged(devices) => {
                self.linked_devices.clone_from(devices);
                true
            }
            AgentEvent::PreferencesChanged(preferences) => {
                self.set_preferences(&preferences.values);
                true
            }
            AgentEvent::Daemon(event) => self.apply_daemon_event(event),
            _ => false,
        }
    }

    pub fn mark_stale(&mut self) {
        self.agent_stale = true;
        self.stale = true;
        self.task_runtime.clear();
        self.live_speeds.clear();
        self.rebuild_all();
    }

    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// 共享任务行存储（表格 / 详情按下标读取）。
    #[must_use]
    pub(crate) fn store(&self) -> &Rc<TaskStore> {
        &self.store
    }

    /// 本地任务的原始 DTO（重新下载等需要完整参数）。
    #[must_use]
    pub(crate) fn task_dto(&self, task_id: &str) -> Option<&TaskDto> {
        self.store
            .find_local(task_id)
            .and_then(|ix| self.local.get(ix))
    }

    #[must_use]
    pub(crate) fn task_runtime(&self, task_id: &str) -> Option<&TaskRuntimeDto> {
        self.task_runtime.get(task_id).map(Rc::as_ref)
    }
    #[must_use]
    pub(crate) fn categories(&self) -> &Rc<CategoryIndex> {
        &self.categories
    }

    /// daemon 队列清单（快照顺序）。
    #[must_use]
    pub(crate) fn queues(&self) -> &[QueueDto] {
        &self.queues
    }

    #[must_use]
    pub(crate) fn queue_name(&self, queue_id: &str) -> Option<&str> {
        self.queues
            .iter()
            .find(|queue| queue.queue_id == queue_id)
            .map(|queue| queue.name.as_str())
    }

    #[must_use]
    pub(crate) fn groups(&self) -> &[GroupDto] {
        &self.groups
    }

    /// 任务组聚合；按存储 generation 缓存。
    pub(crate) fn group_summaries(&mut self) -> &[GroupSummary] {
        let generation = self.store.generation();
        if self.group_summaries_generation != generation {
            self.group_summaries = compute_group_summaries(&self.groups, &self.store.local());
            self.group_summaries_generation = generation;
        }
        &self.group_summaries
    }

    /// 单个任务组的聚合（不走缓存，只扫一遍本地任务；停靠面板渲染用）。
    pub(crate) fn group_summary(&self, group_id: &str) -> Option<GroupSummary> {
        let group = self
            .groups
            .iter()
            .find(|group| group.group_id == group_id)?;
        let local = self.store.local();
        Some(GroupSummary {
            id: group.group_id.clone(),
            meta: GroupMeta::from_dto(group),
            aggregate: GroupAggregate::of(local.iter().filter(|task| task.group_id == group_id)),
        })
    }

    /// 本机的云设备 id（未登录 / 名册未就绪为 `None`）。
    #[must_use]
    pub(crate) fn local_device_id(&self) -> Option<String> {
        local_device_id(self.session_device.as_deref(), &self.cloud_devices)
    }

    /// 「其他设备」：云设备（去掉本机、去重）+ 已配对设备，同名已消歧。
    #[must_use]
    pub(crate) fn other_devices(&self) -> Vec<DeviceEntry> {
        other_devices(
            &self.cloud_devices,
            &self.linked_devices,
            !self.agent_stale && self.cloud_connected,
            !self.agent_stale,
        )
    }

    #[must_use]
    pub(crate) fn runtime_stats(&self) -> &DaemonRuntimeStatsDto {
        &self.runtime_stats
    }

    /// 任务表 / 队列位置表仍在分页载入：「任务全集」类判定（[`Self::has_pausable_tasks`]、
    /// 全选 / 全部暂停范围等）此刻都不成立，调用方应显示加载态而不是「没有任务」。
    #[must_use]
    pub(crate) fn tables_loading(&self) -> bool {
        self.store.loading().is_some()
    }

    /// 同步载入态到共享存储，读者（表格 / 详情）据此显示加载态、跳过删除推断。
    fn note_loading(&self, snapshot: &DaemonSnapshot) {
        self.store
            .set_loading(snapshot.tables_loading().then(|| snapshot.task_count()));
    }

    /// 状态栏「全部暂停」是否有作用对象：与 daemon `PauseAll` 同判定（等待 / 下载中 /
    /// 准备中，即 status 0 / 1 / 5 的本地任务），另含做种中（status 3 且 seeding_status 1 / 8）的任务。
    #[must_use]
    pub(crate) fn has_pausable_tasks(&self) -> bool {
        self.store.pausable_count() > 0
    }

    /// 状态栏「全部开始」的作用对象：与引擎 `eligible_resume_task_ids` 同判定——
    /// 已暂停（status 2）且所在队列运行中；队列不在快照里（孤儿）视作运行中；
    /// 另含用户停止做种（status 3、seeding_status 4）且产物仍在的任务（不受队列约束）。
    /// 停止队列内的暂停任务由「启动队列」恢复，不计入，但单独报告以便界面说明原因。
    /// 按存储里增量维护的「各队列暂停数」判定，不随任务数扫描。
    #[must_use]
    pub(crate) fn resume_targets(&self) -> ResumeTargets {
        if self.store.resumable_seed_count() > 0 {
            return ResumeTargets::Some;
        }
        let mut held = false;
        for queue_id in self.store.paused_by_queue().keys() {
            let running = self
                .queues
                .iter()
                .find(|queue| &queue.queue_id == queue_id)
                .is_none_or(|queue| queue.is_running);
            if running {
                return ResumeTargets::Some;
            }
            held = true;
        }
        if held {
            ResumeTargets::OnlyInStoppedQueues
        } else {
            ResumeTargets::None
        }
    }

    /// daemon 公开配置（字符串编码）。
    #[must_use]
    pub(crate) fn config(&self) -> &BTreeMap<String, String> {
        &self.config
    }

    #[must_use]
    pub(crate) fn config_revision(&self) -> u64 {
        self.config_revision
    }

    /// 配置值（缺省为空串），已去首尾空白。
    #[must_use]
    pub(crate) fn config_str(&self, key: &str) -> &str {
        self.config.get(key).map_or("", |value| value.trim())
    }

    /// agent 偏好值。
    #[must_use]
    pub(crate) fn preference(&self, key: &str) -> Option<&serde_json::Value> {
        self.preferences.get(key)
    }

    /// 全部 agent 偏好。
    #[must_use]
    pub(crate) fn preferences(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.preferences
    }

    #[must_use]
    pub(crate) fn preference_bool(&self, key: &str, default: bool) -> bool {
        self.preference(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default)
    }

    pub fn execute(&self, command: DownloadsCommand) -> PortFuture<DownloadsResult> {
        if self.stale {
            return Box::pin(async {
                Err(fluxdown_protocol::RpcErrorData::new(
                    fluxdown_protocol::ApplicationErrorCode::Unavailable,
                    true,
                ))
            });
        }
        self.port.execute(command)
    }

    fn set_preferences(&mut self, values: &BTreeMap<String, serde_json::Value>) {
        let categories_changed = self
            .preferences
            .get(fluxdown_protocol::CUSTOM_CATEGORIES_PREF_KEY)
            != values.get(fluxdown_protocol::CUSTOM_CATEGORIES_PREF_KEY);
        self.preferences.clone_from(values);
        if categories_changed || self.categories.rules().is_empty() {
            self.categories = Rc::new(CategoryIndex::from_preference(
                values.get(fluxdown_protocol::CUSTOM_CATEGORIES_PREF_KEY),
            ));
        }
    }

    fn absorb_daemon_context(&mut self, snapshot: &DaemonSnapshot) {
        self.queues.clone_from(&snapshot.queues);
        self.groups.clone_from(&snapshot.groups);
        self.config.clone_from(&snapshot.config.values);
        self.config_revision = snapshot.config.revision;
        self.runtime_stats.clone_from(&snapshot.runtime_stats);
        self.task_runtime = snapshot
            .task_runtime
            .iter()
            .map(|(id, runtime)| (id.clone(), Rc::new(runtime.clone())))
            .collect();
        for task in &snapshot.tasks {
            self.settle_runtime(&task.task_id, task.status);
        }
        self.boosted = snapshot.priority.first().cloned();
        self.queue_keys = queue_key_map(&snapshot.queue_positions);
        self.store
            .replace_queue_keys(self.queue_keys.values().copied().collect());
        self.conflicts.replace(&snapshot.pending_selections);
    }

    fn apply_daemon_event(&mut self, event: &DaemonEvent) -> bool {
        match event {
            DaemonEvent::SnapshotReplaced(snapshot) => {
                self.live_speeds.clear();
                self.local.clone_from(&snapshot.tasks);
                self.note_loading(snapshot);
                self.absorb_daemon_context(snapshot);
                self.rebuild_all();
                true
            }
            // 整表重载宣告：清空对应表并进入加载态；完整表由会话分页载入后以快照信号送达。
            DaemonEvent::TablesReloading { tasks, queue } => {
                if tasks.is_some() {
                    self.live_speeds.clear();
                    self.local.clear();
                    self.rebuild_all();
                }
                if queue.is_some() {
                    self.queue_keys.clear();
                    self.store.replace_queue_keys(Vec::new());
                    for ix in 0..self.local.len() {
                        self.store.set_queue_key(ix, None);
                    }
                }
                let total = tasks.map_or(self.local.len(), |total| {
                    usize::try_from(total).unwrap_or(usize::MAX)
                });
                self.store.set_loading(Some(total));
                tasks.is_some() || queue.is_some()
            }
            DaemonEvent::QueuesChanged(queues) => {
                self.queues.clone_from(queues);
                true
            }
            DaemonEvent::GroupsChanged(groups) => {
                self.groups.clone_from(groups);
                self.group_summaries_generation = u64::MAX;
                true
            }
            DaemonEvent::ConfigChanged(config) => {
                self.config.clone_from(&config.values);
                self.config_revision = config.revision;
                false
            }
            DaemonEvent::RuntimeStatsChanged(stats) => {
                self.runtime_stats.clone_from(stats);
                false
            }
            DaemonEvent::RssChanged { .. } => false,
            DaemonEvent::TaskChanged(task) => {
                if !matches!(task.status, 1 | 5) {
                    self.live_speeds.remove(&task.task_id);
                    self.settle_runtime(&task.task_id, task.status);
                }
                self.upsert(task.clone());
                true
            }
            DaemonEvent::TasksChanged(tasks) => {
                for task in tasks {
                    if !matches!(task.status, 1 | 5) {
                        self.live_speeds.remove(&task.task_id);
                        self.settle_runtime(&task.task_id, task.status);
                    }
                }
                self.upsert_many(tasks);
                !tasks.is_empty()
            }
            DaemonEvent::TaskRuntimeChanged(runtime) => {
                let task_id = &runtime.task_id;
                if self.task_runtime.get(task_id).is_some_and(|previous| {
                    previous.sample_sequence != 0
                        && runtime.sample_sequence <= previous.sample_sequence
                }) {
                    return false;
                }
                let row = self.store.find_local(task_id);
                // 已完成任务不再保留运行时：迟到的旧样本不得把条目加回来。
                if row.is_some_and(|ix| !fluxdown_protocol::runtime_retained(self.local[ix].status))
                {
                    return false;
                }
                let mut runtime = runtime.clone();
                if runtime.segments.is_empty()
                    && let Some(previous) = self.task_runtime.get(task_id)
                {
                    runtime.segments.clone_from(&previous.segments);
                }
                self.task_runtime.insert(task_id.clone(), Rc::new(runtime));
                if let Some(ix) = row {
                    if !matches!(self.local[ix].status, 1 | 5) {
                        self.stop_runtime(task_id);
                    }
                    self.rebuild_row(ix);
                    true
                } else {
                    // 任务元数据事件可能紧随其后；先保留样本供新行直接显示。
                    false
                }
            }
            DaemonEvent::TaskActivityAdded(_) => false,
            DaemonEvent::TaskDeleted { task_id } => {
                self.remove_tasks(std::slice::from_ref(task_id));
                true
            }
            DaemonEvent::TasksDeleted { task_ids } => {
                self.remove_tasks(task_ids);
                !task_ids.is_empty()
            }
            DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks }) => {
                let ids: HashSet<&str> = tasks.iter().map(|task| task.task_id.as_str()).collect();
                self.live_speeds
                    .retain(|task_id, _| ids.contains(task_id.as_str()));
                self.task_runtime
                    .retain(|task_id, _| ids.contains(task_id.as_str()));
                let mut changed = false;
                for ix in (0..self.local.len()).rev() {
                    if !ids.contains(self.local[ix].task_id.as_str()) {
                        self.local.swap_remove(ix);
                        self.store.swap_remove_local(ix);
                        changed = true;
                    }
                }
                // Apply every snapshot in order: deleting then reintroducing an ID
                // must not resurrect its runtime, even within one event batch.
                for (target, task) in tasks.iter().enumerate() {
                    let runtime_changed = if !matches!(task.status, 1 | 5) {
                        self.live_speeds.remove(&task.task_id).is_some()
                            | self.settle_runtime(&task.task_id, task.status)
                    } else {
                        false
                    };
                    match self.store.find_local(&task.task_id) {
                        Some(ix) => {
                            if self.local[ix] != *task || runtime_changed {
                                self.upsert(task.clone());
                                changed = true;
                            }
                            if ix != target {
                                self.local.swap(ix, target);
                                self.store.swap_local(ix, target);
                                changed = true;
                            }
                        }
                        None => {
                            self.upsert(task.clone());
                            let ix = self.local.len() - 1;
                            if ix != target {
                                self.local.swap(ix, target);
                                self.store.swap_local(ix, target);
                            }
                            changed = true;
                        }
                    }
                }
                changed
            }
            DaemonEvent::Engine(WsServerMsg::TaskQueueChanged { task_id, queue_id }) => {
                let Some(ix) = self.store.find_local(task_id) else {
                    return false;
                };
                self.local[ix].queue_id.clone_from(queue_id);
                self.rebuild_row(ix);
                true
            }
            DaemonEvent::Engine(WsServerMsg::TaskProgress {
                task_id,
                status,
                downloaded_bytes,
                total_bytes,
                file_name,
                speed,
                error_message,
                uploaded_bytes,
                upload_speed,
                seeding_status,
                ..
            }) => {
                // 单任务删除沿用 status=4 + "deleted" 哨兵：按删除处理，不能把行改成失败。
                if *status == 4 && error_message == DELETED_SENTINEL {
                    self.remove_tasks(std::slice::from_ref(task_id));
                    return true;
                }
                let upload = if matches!(*status, 1 | 3) {
                    *upload_speed
                } else {
                    0
                };
                self.live_speeds.insert(
                    task_id.clone(),
                    (if *status == 1 { *speed } else { 0 }, upload),
                );
                let Some(ix) = self.store.find_local(task_id) else {
                    return false;
                };
                if let Some(task) = self.local.get_mut(ix) {
                    task.status = *status;
                    task.downloaded_bytes = *downloaded_bytes;
                    task.total_bytes = *total_bytes;
                    task.uploaded_bytes = *uploaded_bytes;
                    task.seeding_status = *seeding_status;
                    if !file_name.is_empty() {
                        task.file_name.clone_from(file_name);
                    }
                    task.error_message.clone_from(error_message);
                }
                if !matches!(*status, 1 | 5) {
                    self.settle_runtime(task_id, *status);
                }
                self.rebuild_row(ix);
                true
            }
            DaemonEvent::Engine(WsServerMsg::TaskMetaProbed {
                task_id,
                file_name,
                total_bytes,
            }) => {
                let Some(ix) = self.store.find_local(task_id) else {
                    return false;
                };
                if let Some(task) = self.local.get_mut(ix) {
                    if !file_name.is_empty() {
                        task.file_name.clone_from(file_name);
                    }
                    if *total_bytes > 0 {
                        task.total_bytes = *total_bytes;
                    }
                }
                self.rebuild_row(ix);
                true
            }
            DaemonEvent::Engine(WsServerMsg::QueuePositionsChanged { positions }) => {
                let next = queue_key_map(positions);
                if next == self.queue_keys {
                    return false;
                }
                let changed: Vec<(usize, Option<i64>)> = self
                    .queue_keys
                    .keys()
                    .chain(
                        next.keys()
                            .filter(|task_id| !self.queue_keys.contains_key(*task_id)),
                    )
                    .filter(|task_id| self.queue_keys.get(*task_id) != next.get(*task_id))
                    .filter_map(|task_id| {
                        let ix = self.store.find_local(task_id)?;
                        Some((ix, next.get(task_id).copied()))
                    })
                    .collect();
                self.queue_keys = next;
                self.store
                    .replace_queue_keys(self.queue_keys.values().copied().collect());
                for (ix, key) in changed {
                    self.store.set_queue_key(ix, key);
                }
                true
            }
            DaemonEvent::Engine(WsServerMsg::QueueOrderChanged { removed, added }) => {
                self.apply_queue_order(removed, added)
            }
            DaemonEvent::Engine(WsServerMsg::PriorityTaskChanged {
                priority_task_id, ..
            }) => {
                let next = (!priority_task_id.is_empty()).then(|| priority_task_id.clone());
                if self.boosted == next {
                    return false;
                }
                let previous = std::mem::replace(&mut self.boosted, next.clone());
                let rows: Vec<usize> = [previous, next]
                    .iter()
                    .flatten()
                    .filter_map(|task_id| self.store.find_local(task_id))
                    .collect();
                for ix in rows {
                    self.rebuild_row(ix);
                }
                true
            }
            DaemonEvent::Engine(WsServerMsg::FileMissingChanged { updates }) => {
                let mut changed = false;
                for update in updates {
                    let Some(ix) = self.store.find_local(&update.task_id) else {
                        continue;
                    };
                    if self.local[ix].file_missing == update.missing {
                        continue;
                    }
                    self.local[ix].file_missing = update.missing;
                    self.rebuild_row(ix);
                    changed = true;
                }
                changed
            }
            DaemonEvent::SelectionPending(request) => {
                if !self.conflicts.upsert(request) {
                    return false;
                }
                self.rebuild_conflict_row(&request.task_id);
                true
            }
            DaemonEvent::SelectionResolved { request_id } => {
                let Some(item) = self.conflicts.remove(request_id) else {
                    return false;
                };
                self.rebuild_conflict_row(&item.task_id);
                true
            }
            _ => false,
        }
    }

    fn view_of(&self, task: &TaskDto) -> DownloadTaskView {
        let live = self.live_speeds.get(&task.task_id).copied();
        let mut view = DownloadTaskView::local(
            task,
            live.map(|(download, _)| download),
            self.boosted.as_deref() == Some(task.task_id.as_str()),
        );
        view.upload_speed_bytes_per_second = live.map_or(0, |(_, upload)| upload.max(0) as u64);
        view.runtime = self.task_runtime.get(&task.task_id).cloned();
        view.runtime_connected = !self.stale;
        view.conflict_pending = self.conflicts.contains_task(&task.task_id);
        view
    }

    /// 按序移除一批本地任务（语义等同逐条 `TaskDeleted`）：每行 `swap_remove` 为 O(1)，
    /// 整批只登记一次结构变化。
    fn remove_tasks(&mut self, task_ids: &[String]) {
        let mut removed = false;
        for task_id in task_ids {
            self.live_speeds.remove(task_id);
            self.task_runtime.remove(task_id);
            if let Some(ix) = self.store.find_local(task_id) {
                self.local.swap_remove(ix);
                removed |= self.store.swap_remove_local_quiet(ix);
            }
        }
        if removed {
            self.store.commit_removals();
        }
    }

    /// 任务状态变化后的运行时收尾：完成即删除条目，其余非活跃状态只清零传输读数。
    /// 返回条目是否有变化。
    fn settle_runtime(&mut self, task_id: &str, status: i32) -> bool {
        if !fluxdown_protocol::runtime_retained(status) {
            return self.task_runtime.remove(task_id).is_some();
        }
        if matches!(status, 1 | 5) {
            return false;
        }
        self.stop_runtime(task_id)
    }

    fn stop_runtime(&mut self, task_id: &str) -> bool {
        if let Some(runtime) = self.task_runtime.get_mut(task_id) {
            if runtime.active_transfers == Some(0)
                && runtime.connected_peers == Some(0)
                && runtime
                    .segments
                    .iter()
                    .all(|segment| segment.active == Some(false))
            {
                return false;
            }
            let runtime = Rc::make_mut(runtime);
            runtime.active_transfers = Some(0);
            runtime.connected_peers = Some(0);
            for segment in &mut runtime.segments {
                segment.active = Some(false);
            }
            return true;
        }
        false
    }

    fn upsert(&mut self, task: TaskDto) {
        if let Some(ix) = self.store.find_local(&task.task_id) {
            let view = self.view_of(&task);
            self.local[ix] = task;
            self.store.set_local(ix, view);
        } else {
            let view = self.view_of(&task);
            self.local.push(task);
            self.store.push_local(view);
        }
    }

    /// 按序批量 upsert（语义等同逐条 [`Self::upsert`]），新增行只登记一次结构变化。
    fn upsert_many(&mut self, tasks: &[TaskDto]) {
        let mut appended = false;
        for task in tasks {
            let view = self.view_of(task);
            if let Some(ix) = self.store.find_local(&task.task_id) {
                self.local[ix].clone_from(task);
                self.store.set_local(ix, view);
            } else {
                self.local.push(task.clone());
                self.store.append_local(view);
                appended = true;
            }
        }
        if appended {
            self.store.commit_appends();
        }
    }

    fn rebuild_all(&mut self) {
        let rows = self.local.iter().map(|task| self.view_of(task)).collect();
        self.store.replace_local(rows);
    }

    fn rebuild_remote(&mut self) {
        let local = self.local_device_id();
        let rows = self
            .remote
            .iter()
            // 目标是本机的任务已在本地落成真实任务；再显示云端镜像行会重复。
            .filter(|task| local.as_deref().is_none_or(|id| task.to_device != id))
            .map(DownloadTaskView::remote)
            .collect();
        self.store.replace_remote(rows);
    }

    /// 待确认请求增减后只重建该任务的行（任务不在列表里则无事可做）。
    fn rebuild_conflict_row(&mut self, task_id: &str) {
        if let Some(ix) = self.store.find_local(task_id) {
            self.rebuild_row(ix);
        }
    }

    fn rebuild_row(&mut self, ix: usize) {
        if let Some(task) = self.local.get(ix) {
            let view = self.view_of(task);
            self.store.set_local(ix, view);
        }
    }

    /// 队列顺序增量：只更新进出队列的任务（O(Δ·log P) 加有序数组移动）。其余排队行的键没有
    /// 变，名次在渲染时由 `store` 二分得到，所以队首出队不重建任何其它行。返回是否有净变化
    /// （调用方据此通知读者重绘：名次变了，行内容没变）。
    fn apply_queue_order(
        &mut self,
        removed: &[String],
        added: &[fluxdown_protocol::QueueOrderEntryDto],
    ) -> bool {
        // 记下每个被触及任务的原始键，批内重复 / 先删后加按净效果结算。
        let mut originals: HashMap<&str, Option<i64>> =
            HashMap::with_capacity(removed.len() + added.len());
        for task_id in removed {
            if !originals.contains_key(task_id.as_str()) {
                originals.insert(task_id, self.queue_keys.get(task_id).copied());
            }
            self.queue_keys.remove(task_id);
        }
        for entry in added {
            if !originals.contains_key(entry.task_id.as_str()) {
                originals.insert(&entry.task_id, self.queue_keys.get(&entry.task_id).copied());
            }
            match self.queue_keys.get_mut(&entry.task_id) {
                Some(key) => *key = entry.order,
                None => {
                    self.queue_keys.insert(entry.task_id.clone(), entry.order);
                }
            }
        }
        let mut removed_keys = Vec::new();
        let mut added_keys = Vec::new();
        let mut touched_rows = Vec::new();
        for (task_id, original) in originals {
            let current = self.queue_keys.get(task_id).copied();
            if current == original {
                continue;
            }
            removed_keys.extend(original);
            added_keys.extend(current);
            if let Some(ix) = self.store.find_local(task_id) {
                touched_rows.push((ix, current));
            }
        }
        if removed_keys.is_empty() && added_keys.is_empty() {
            return false;
        }
        self.store
            .apply_queue_keys(&mut removed_keys, &mut added_keys);
        for (ix, key) in touched_rows {
            self.store.set_queue_key(ix, key);
        }
        true
    }
}

fn queue_key_map(positions: &[fluxdown_protocol::QueuePositionDto]) -> HashMap<String, i64> {
    positions
        .iter()
        .filter(|entry| entry.position > 0)
        .map(|entry| (entry.task_id.clone(), entry.order))
        .collect()
}

fn compute_group_summaries(groups: &[GroupDto], rows: &[DownloadTaskView]) -> Vec<GroupSummary> {
    let index: HashMap<&str, usize> = groups
        .iter()
        .enumerate()
        .map(|(ix, group)| (group.group_id.as_str(), ix))
        .collect();
    let mut members: Vec<Vec<&DownloadTaskView>> = vec![Vec::new(); groups.len()];
    for row in rows.iter().filter(|row| !row.group_id.is_empty()) {
        if let Some(&ix) = index.get(row.group_id.as_str()) {
            members[ix].push(row);
        }
    }
    groups
        .iter()
        .zip(members)
        .map(|(group, members)| GroupSummary {
            id: group.group_id.clone(),
            meta: GroupMeta::from_dto(group),
            aggregate: GroupAggregate::of(members),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::{
        AgentEvent, DaemonEvent, DownloadsCommand, DownloadsController, DownloadsPort,
        DownloadsResult, PortFuture, ResumeTargets, ServiceEvent, WsServerMsg,
    };

    struct NullPort;

    impl DownloadsPort for NullPort {
        fn execute(&self, _command: DownloadsCommand) -> PortFuture<DownloadsResult> {
            Box::pin(async { Ok(DownloadsResult::Unit) })
        }
    }

    fn task(id: &str) -> fluxdown_protocol::TaskDto {
        serde_json::from_value(json!({
            "taskId":id,"url":"https://example.com/download","fileName":"",
            "saveDir":"/tmp","status":0,"downloadedBytes":0,"totalBytes":0,
            "errorMessage":"","createdAt":"1","proxyUrl":"","queueId":"main","checksum":""
        }))
        .expect("task")
    }

    #[test]
    fn snapshots_preserve_order_deletion_and_terminal_runtime_boundaries() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        let mut active = task("a");
        active.status = 1;
        let snapshot = |tasks| DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks });
        controller.apply_daemon_event(&snapshot(vec![active.clone(), task("b")]));
        controller.apply_daemon_event(&DaemonEvent::TaskRuntimeChanged(
            fluxdown_protocol::TaskRuntimeDto {
                task_id: "a".to_owned(),
                active_transfers: Some(2),
                connected_peers: Some(2),
                ..Default::default()
            },
        ));
        controller.live_speeds.insert("a".to_owned(), (100, 0));
        let generation = controller.store.generation();
        assert!(!controller.apply_daemon_event(&snapshot(vec![active.clone(), task("b")])));
        assert_eq!(controller.store.generation(), generation);
        let mut paused = active.clone();
        paused.status = 2;
        controller.apply_daemon_event(&snapshot(vec![task("b"), paused]));
        assert_eq!(
            controller
                .task_runtime("a")
                .expect("runtime")
                .active_transfers,
            Some(0)
        );
        assert!(!controller.live_speeds.contains_key("a"));
        assert_eq!(controller.store.find_local("a"), Some(1));
        controller.apply_daemon_event(&snapshot(vec![task("b")]));
        assert!(controller.task_runtime("a").is_none());
        controller.apply_daemon_event(&snapshot(vec![active, task("b")]));
        assert_eq!(controller.store.find_local("a"), Some(0));
        assert!(controller.store.local()[0].runtime.is_none());
        assert!(controller.task_runtime("a").is_none());
    }

    #[test]
    fn tasks_changed_batch_equals_sequential_task_changed_with_one_structure_bump() {
        let mut batched = DownloadsController::new(Arc::new(NullPort));
        let mut sequential = DownloadsController::new(Arc::new(NullPort));
        let mut running = task("a");
        running.status = 1;
        for controller in [&mut batched, &mut sequential] {
            controller.apply_daemon_event(&DaemonEvent::TaskChanged(running.clone()));
            controller.apply_daemon_event(&DaemonEvent::TaskRuntimeChanged(
                fluxdown_protocol::TaskRuntimeDto {
                    task_id: "a".to_owned(),
                    active_transfers: Some(2),
                    ..Default::default()
                },
            ));
        }
        // 已有任务进入终态 + 两个新任务 + 同批里对新任务的再次更新。
        let mut finished = running.clone();
        finished.status = 3;
        let mut updated = task("c");
        updated.downloaded_bytes = 7;
        let events = vec![finished, task("b"), task("c"), updated];

        let structure = batched.store.structure_generation();
        assert!(batched.apply_daemon_event(&DaemonEvent::TasksChanged(events.clone())));
        assert_eq!(
            batched.store.structure_generation(),
            structure.wrapping_add(1)
        );
        for event in events {
            sequential.apply_daemon_event(&DaemonEvent::TaskChanged(event));
        }
        assert_eq!(batched.local, sequential.local);
        assert_eq!(
            batched.store.find_local("c"),
            sequential.store.find_local("c")
        );
        let rows = |controller: &DownloadsController| {
            controller
                .store
                .local()
                .iter()
                .map(|row| {
                    (
                        row.key.clone(),
                        row.progress.to_bits(),
                        row.active_transfers(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(rows(&batched), rows(&sequential));
        // 完成任务的运行时在批内同样被收口：条目删除而不是清零。
        assert!(batched.task_runtime("a").is_none());
        assert!(!batched.apply_daemon_event(&DaemonEvent::TasksChanged(Vec::new())));
    }

    fn sampled(id: &str, sequence: u64) -> fluxdown_protocol::TaskRuntimeDto {
        fluxdown_protocol::TaskRuntimeDto {
            task_id: id.to_owned(),
            sample_sequence: sequence,
            total_bytes: 100,
            active_transfers: Some(2),
            segments: vec![fluxdown_protocol::TaskSegmentDto {
                start_byte: 0,
                end_byte: 99,
                downloaded_bytes: 10,
                active: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn completed_tasks_leave_no_runtime_and_late_samples_cannot_readd_it() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        const TASKS: u64 = 500;
        for index in 0..TASKS {
            let id = format!("t{index}");
            let mut running = task(&id);
            running.status = 1;
            controller.apply_daemon_event(&DaemonEvent::TaskChanged(running.clone()));
            controller
                .apply_daemon_event(&DaemonEvent::TaskRuntimeChanged(sampled(&id, index + 1)));
            assert!(controller.task_runtime(&id).is_some());
            // 三条完成路径各走一遍：TaskChanged / TaskProgress / TasksChanged。
            match index % 3 {
                0 => {
                    running.status = 3;
                    controller.apply_daemon_event(&DaemonEvent::TaskChanged(running));
                }
                1 => {
                    controller.apply_daemon_event(&DaemonEvent::Engine(
                        WsServerMsg::TaskProgress {
                            task_id: id.clone(),
                            status: 3,
                            downloaded_bytes: 100,
                            total_bytes: 100,
                            speed: 0,
                            upload_speed: 0,
                            file_name: String::new(),
                            save_dir: String::new(),
                            url: String::new(),
                            error_message: String::new(),
                            uploaded_bytes: 0,
                            seeding_status: 0,
                            seeding_message: String::new(),
                            seeding_time_secs: 0,
                        },
                    ));
                }
                _ => {
                    running.status = 3;
                    controller.apply_daemon_event(&DaemonEvent::TasksChanged(vec![running]));
                }
            }
            assert!(controller.task_runtime(&id).is_none(), "{id}");
            // 迟到的旧样本（序号更高也一样）不会把条目加回来。
            assert!(
                !controller.apply_daemon_event(&DaemonEvent::TaskRuntimeChanged(sampled(
                    &id,
                    index + 1_000
                )))
            );
            assert!(controller.task_runtime(&id).is_none(), "{id}");
        }
        assert!(controller.task_runtime.is_empty());
        assert!(
            controller
                .store
                .local()
                .iter()
                .all(|row| row.runtime.is_none())
        );
    }

    #[test]
    fn paused_and_failed_tasks_keep_their_segments() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        for (id, status) in [("paused", 2), ("failed", 4)] {
            let mut running = task(id);
            running.status = 1;
            controller.apply_daemon_event(&DaemonEvent::TaskChanged(running.clone()));
            controller.apply_daemon_event(&DaemonEvent::TaskRuntimeChanged(sampled(id, 1)));
            running.status = status;
            controller.apply_daemon_event(&DaemonEvent::TaskChanged(running));
            let runtime = controller.task_runtime(id).expect("runtime kept");
            assert_eq!(runtime.segments.len(), 1);
            assert_eq!(runtime.active_transfers, Some(0));
        }
    }

    #[test]
    fn bulk_pause_and_resume_availability_follows_daemon_targets() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        assert!(!controller.has_pausable_tasks());
        assert_eq!(controller.resume_targets(), ResumeTargets::None);
        let with_status = |id: &str, status: i32, queue: &str| {
            let mut dto = task(id);
            dto.status = status;
            queue.clone_into(&mut dto.queue_id);
            dto
        };
        let snapshot = |tasks| DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks });
        controller.queues = vec![
            serde_json::from_value(json!({
                "queueId":"stopped","name":"s","speedLimitKbps":0,"maxConcurrent":0,
                "defaultSaveDir":"","position":1,"defaultSegments":0,"defaultUserAgent":"",
                "isRunning":false
            }))
            .expect("queue"),
        ];
        // 完成 / 失败不是任何一侧的对象；停止队列里的暂停任务只由「启动队列」恢复。
        controller.apply_daemon_event(&snapshot(vec![
            with_status("done", 3, "main"),
            with_status("failed", 4, "main"),
            with_status("held", 2, "stopped"),
        ]));
        assert!(!controller.has_pausable_tasks());
        assert_eq!(
            controller.resume_targets(),
            ResumeTargets::OnlyInStoppedQueues
        );
        // 准备中（5）可暂停；快照里没有的队列（孤儿）视作运行中，可恢复。
        controller.apply_daemon_event(&snapshot(vec![
            with_status("held", 2, "stopped"),
            with_status("preparing", 5, "main"),
            with_status("orphan", 2, "gone"),
        ]));
        assert!(controller.has_pausable_tasks());
        assert_eq!(controller.resume_targets(), ResumeTargets::Some);
        // 做种中（status 3 + seeding_status 1 / 8）可暂停；用户停止做种（4）且文件在则「全部开始」
        // 有对象，即使其余暂停任务都在停止队列里；停止做种但文件丢失不计入。
        let with_seeding = |id: &str, seeding_status: i32, file_missing: bool| {
            let mut dto = with_status(id, 3, "main");
            dto.seeding_status = seeding_status;
            dto.file_missing = file_missing;
            dto
        };
        controller.apply_daemon_event(&snapshot(vec![
            with_status("held", 2, "stopped"),
            with_seeding("seed", 1, false),
        ]));
        assert!(controller.has_pausable_tasks());
        assert_eq!(
            controller.resume_targets(),
            ResumeTargets::OnlyInStoppedQueues
        );
        controller.apply_daemon_event(&snapshot(vec![
            with_status("held", 2, "stopped"),
            with_seeding("queued", 8, false),
            with_seeding("ratio", 2, false),
            with_seeding("missing", 4, true),
        ]));
        assert!(controller.has_pausable_tasks());
        assert_eq!(
            controller.resume_targets(),
            ResumeTargets::OnlyInStoppedQueues
        );
        controller.apply_daemon_event(&snapshot(vec![
            with_status("held", 2, "stopped"),
            with_seeding("stopped", 4, false),
        ]));
        assert!(!controller.has_pausable_tasks());
        assert_eq!(controller.resume_targets(), ResumeTargets::Some);
    }

    /// 分页载入期间（头快照 / 重载宣告）控制器只给出加载态，不把空表当完整表；完整快照到达才解除。
    #[test]
    fn paged_header_and_reload_announcement_put_the_controller_in_a_loading_state() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        let inline = |tasks| DaemonEvent::Engine(WsServerMsg::TasksSnapshot { tasks });
        controller.apply_daemon_event(&inline(vec![task("a"), task("b")]));
        assert!(!controller.tables_loading());

        assert!(
            controller.apply_daemon_event(&DaemonEvent::TablesReloading {
                tasks: Some(2_100),
                queue: None,
            })
        );
        assert!(controller.tables_loading());
        assert_eq!(controller.store.loading(), Some(2_100));
        assert!(controller.store.local().is_empty());
        assert!(!controller.has_pausable_tasks());

        let mut complete = fluxdown_protocol::AgentSnapshot::default();
        complete.daemon.tasks = vec![task("a"), task("b")];
        controller.replace_snapshot(&complete);
        assert!(!controller.tables_loading());
        assert_eq!(controller.store.local().len(), 2);

        let mut header = fluxdown_protocol::AgentSnapshot::default();
        header.daemon.task_total = Some(7);
        controller.replace_snapshot(&header);
        assert_eq!(controller.store.loading(), Some(7));
        assert!(controller.store.local().is_empty());

        // 头快照内联的替换事件同样进入 / 解除加载态。
        let replaced = fluxdown_protocol::DaemonSnapshot {
            tasks: vec![task("a")],
            ..fluxdown_protocol::DaemonSnapshot::default()
        };
        controller.apply_daemon_event(&DaemonEvent::SnapshotReplaced(replaced));
        assert!(!controller.tables_loading());
        assert_eq!(controller.store.local().len(), 1);
    }

    #[test]
    fn twenty_thousand_incremental_tasks_and_snapshots_keep_view_generations_local() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        let mut tasks: Vec<_> = (0..20_000).map(|ix| task(&format!("task-{ix}"))).collect();
        let start = std::time::Instant::now();
        for task in &tasks {
            controller.apply_daemon_event(&DaemonEvent::TaskChanged(task.clone()));
        }
        let create_time = start.elapsed();
        assert_eq!(controller.store.local().len(), 20_000);
        let generation = controller.store.generation();
        let snapshot = DaemonEvent::Engine(WsServerMsg::TasksSnapshot {
            tasks: tasks.clone(),
        });
        let start = std::time::Instant::now();
        for _ in 0..5 {
            assert!(!controller.apply_daemon_event(&snapshot));
        }
        let snapshot_time = start.elapsed();
        assert_eq!(controller.store.generation(), generation);
        let start = std::time::Instant::now();
        for task in &mut tasks {
            task.status = 3;
            task.downloaded_bytes = 128 * 1024;
            task.total_bytes = 128 * 1024;
            controller.apply_daemon_event(&DaemonEvent::TaskChanged(task.clone()));
        }
        assert_eq!(controller.store.generation(), generation + 20_000);
        assert!(
            controller
                .store
                .local()
                .iter()
                .all(|row| row.progress == 1.0)
        );
        eprintln!(
            "controller 20000 creates={create_time:?}, five snapshots={snapshot_time:?}, 20000 completions={:?}",
            start.elapsed()
        );
    }

    #[test]
    fn created_task_ids_read_single_and_batch_create_results() {
        let single = DownloadsResult::Value(json!({ "taskId": "a" }));
        assert_eq!(single.created_task_ids(), ["a"]);
        // 捕获确认里失败的条目为 null，不计入新任务。
        let batch = DownloadsResult::Value(json!({ "taskIds": ["a", null, "b"] }));
        assert_eq!(batch.created_task_ids(), ["a", "b"]);
        assert!(DownloadsResult::Unit.created_task_ids().is_empty());
    }

    #[test]
    fn task_queue_delta_moves_visible_row_without_reloading_tasks() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        controller.local.push(task("task-1"));
        controller.rebuild_all();

        assert!(controller.apply_daemon_event(&DaemonEvent::Engine(
            WsServerMsg::TaskQueueChanged {
                task_id: "task-1".to_owned(),
                queue_id: "work".to_owned(),
            },
        )));
        assert_eq!(controller.store().local()[0].queue_id, "work");
        assert_eq!(controller.task_dto("task-1").unwrap().queue_id, "work");
    }

    fn queue_controller(count: usize) -> DownloadsController {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        for ix in 0..count {
            controller.local.push(task(&format!("t{ix}")));
        }
        controller.rebuild_all();
        controller
    }

    fn full_queue_event(model: &[fluxdown_protocol::QueuePositionDto]) -> DaemonEvent {
        DaemonEvent::Engine(WsServerMsg::QueuePositionsChanged {
            positions: model.to_vec(),
        })
    }

    fn order_event(
        removed: &[&str],
        added: &[(&str, i64)],
    ) -> (Vec<String>, Vec<fluxdown_protocol::QueueOrderEntryDto>) {
        (
            removed.iter().map(|id| (*id).to_owned()).collect(),
            added
                .iter()
                .map(|(id, order)| fluxdown_protocol::QueueOrderEntryDto {
                    task_id: (*id).to_owned(),
                    order: *order,
                })
                .collect(),
        )
    }

    fn rank_by_task(controller: &DownloadsController) -> std::collections::BTreeMap<String, u32> {
        let store = controller.store();
        store
            .local()
            .iter()
            .map(|row| (row.key.task_id().to_owned(), store.queue_rank(row)))
            .collect()
    }

    /// 增量应用 `(removed, added)` 后，名次必须与对「共享 reducer 折叠出的全量表」整表替换完全一致。
    fn apply_delta_and_compare(
        incremental: &mut DownloadsController,
        model: &mut Vec<fluxdown_protocol::QueuePositionDto>,
        removed: &[&str],
        added: &[(&str, i64)],
    ) -> bool {
        let (removed, added) = order_event(removed, added);
        let changed =
            incremental.apply_daemon_event(&DaemonEvent::Engine(WsServerMsg::QueueOrderChanged {
                removed: removed.clone(),
                added: added.clone(),
            }));
        fluxdown_protocol::apply_queue_order(model, &removed, &added);
        let mut reference = queue_controller(incremental.local.len());
        reference.apply_daemon_event(&full_queue_event(model));
        assert_eq!(rank_by_task(incremental), rank_by_task(&reference));
        changed
    }

    #[test]
    fn queue_order_deltas_rank_like_full_tables_and_rebuild_only_changed_rows() {
        const COUNT: usize = 300;
        let mut incremental = queue_controller(COUNT);
        let mut model: Vec<_> = (0..COUNT)
            .map(|ix| fluxdown_protocol::QueuePositionDto {
                task_id: format!("t{ix}"),
                position: i32::try_from(ix + 1).unwrap(),
                order: i64::try_from(ix + 1).unwrap() * 10,
            })
            .collect();
        assert!(incremental.apply_daemon_event(&full_queue_event(&model)));
        let ranks = rank_by_task(&incremental);
        assert_eq!((ranks["t0"], ranks["t1"], ranks["t299"]), (1, 2, 300));
        // 内容相同的全量表不是变化。
        assert!(!incremental.apply_daemon_event(&full_queue_event(&model)));

        // 队首出队：其余 299 行一个都不重建（generation 只因被移出的那一行 +1），名次整体前移。
        let generation = incremental.store().generation();
        assert!(apply_delta_and_compare(
            &mut incremental,
            &mut model,
            &["t0"],
            &[]
        ));
        assert_eq!(incremental.store().generation(), generation + 1);
        let ranks = rank_by_task(&incremental);
        assert_eq!((ranks["t0"], ranks["t1"], ranks["t299"]), (0, 1, 299));
        assert_eq!(incremental.store().local()[0].queue_key, None);
        assert_eq!(incremental.store().local()[1].queue_key, Some(20));

        // 未排队任务的移除、内容相同的重复登记都没有净变化。
        let generation = incremental.store().generation();
        assert!(!apply_delta_and_compare(
            &mut incremental,
            &mut model,
            &["t0", "nope"],
            &[("t1", 20)]
        ));
        assert_eq!(incremental.store().generation(), generation);

        // 队尾追加 + 中间移除 + 改键（插到队首之前）+ 尚无行的任务（幽灵）。
        assert!(apply_delta_and_compare(
            &mut incremental,
            &mut model,
            &["t150"],
            &[("t5", 15), ("ghost", 5000), ("t0", 4000), ("ghost", 5001)],
        ));
        let ranks = rank_by_task(&incremental);
        assert_eq!((ranks["t5"], ranks["t1"], ranks["t150"]), (1, 2, 0));

        // 批量变化（走归并路径）：头部 120 个出队，同批里先删后加、重复登记以最后一项为准。
        let removed: Vec<String> = (1..=120).map(|ix| format!("t{ix}")).collect();
        let removed: Vec<&str> = removed.iter().map(String::as_str).collect();
        let mut added: Vec<(String, i64)> = (0..90)
            .map(|ix| (format!("new{ix}"), 1_000_000 + i64::from(ix) * 7 - 3000))
            .collect();
        added.push(("t7".to_owned(), 99));
        added.push(("t7".to_owned(), 101));
        added.push(("t3".to_owned(), 55));
        let added: Vec<(&str, i64)> = added
            .iter()
            .map(|(id, order)| (id.as_str(), *order))
            .collect();
        assert!(apply_delta_and_compare(
            &mut incremental,
            &mut model,
            &removed,
            &added
        ));
        let ranks = rank_by_task(&incremental);
        assert_eq!(
            (ranks["t5"], ranks["t3"], ranks["t7"], ranks["t1"]),
            (0, 1, 2, 0)
        );
    }

    #[test]
    fn progress_keeps_sidebar_counts_valid_until_a_counted_field_changes() {
        let mut controller = queue_controller(2);
        let counts = controller.store().counts_generation();
        let generation = controller.store().generation();

        // 字节 / 总大小 / 排队键变化只动行内容，不动侧栏计数。
        assert!(
            controller.apply_daemon_event(&DaemonEvent::Engine(WsServerMsg::TaskMetaProbed {
                task_id: "t0".to_owned(),
                file_name: String::new(),
                total_bytes: 4096,
            },))
        );
        assert!(controller.apply_daemon_event(&DaemonEvent::Engine(
            WsServerMsg::QueueOrderChanged {
                removed: Vec::new(),
                added: vec![fluxdown_protocol::QueueOrderEntryDto {
                    task_id: "t1".to_owned(),
                    order: 7,
                }],
            },
        )));
        assert!(controller.store().generation() > generation);
        assert_eq!(controller.store().counts_generation(), counts);

        // 状态变化（等待 → 完成）改变「已完成」桶，必须失效；行增删同理。
        assert!(controller.apply_daemon_event(&progress_event("t0", 3, "")));
        let after_status = controller.store().counts_generation();
        assert_ne!(after_status, counts);
        assert!(controller.apply_daemon_event(&DaemonEvent::TaskChanged(task("t2"))));
        assert_ne!(controller.store().counts_generation(), after_status);
    }

    #[test]
    fn metadata_probe_replaces_loading_row_name_and_size() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        controller.local.push(task("task-1"));
        controller.rebuild_all();
        assert!(controller.store().local()[0].metadata_pending);

        controller.apply_daemon_event(&DaemonEvent::Engine(WsServerMsg::TaskMetaProbed {
            task_id: "task-1".to_owned(),
            file_name: "resolved.bin".to_owned(),
            total_bytes: 4096,
        }));

        {
            let rows = controller.store().local();
            assert_eq!(rows[0].name, "resolved.bin");
            assert_eq!(rows[0].size_bytes, 4096);
            assert!(!rows[0].metadata_pending);
        }

        controller.apply_daemon_event(&DaemonEvent::Engine(WsServerMsg::TaskMetaProbed {
            task_id: "task-1".to_owned(),
            file_name: String::new(),
            total_bytes: 0,
        }));
        {
            let rows = controller.store().local();
            assert_eq!(rows[0].name, "resolved.bin");
            assert_eq!(rows[0].size_bytes, 4096);
        }

        controller.apply_daemon_event(&DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id: "task-1".to_owned(),
            status: 1,
            downloaded_bytes: 1024,
            total_bytes: 4096,
            speed: 1024,
            upload_speed: 0,
            file_name: "resolved.bin".to_owned(),
            save_dir: "/tmp".to_owned(),
            url: "https://example.com/download".to_owned(),
            error_message: String::new(),
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            seeding_time_secs: 0,
        }));

        let rows = controller.store().local();
        assert_eq!(rows[0].speed_bytes_per_second, Some(1024));
        assert_eq!(rows[0].eta_seconds, Some(3));
        assert_eq!(rows[0].progress, 0.25);
    }

    #[test]
    fn file_missing_changes_patch_completed_rows_and_self_heal() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        let mut done = task("done");
        done.status = 3;
        done.file_name = "done.bin".to_owned();
        controller.local.push(done);
        controller.rebuild_all();
        assert!(controller.store().local()[0].has_local_file());

        let missing = |missing: bool| {
            DaemonEvent::Engine(WsServerMsg::FileMissingChanged {
                updates: vec![
                    fluxdown_protocol::FileMissingUpdateDto {
                        task_id: "done".to_owned(),
                        missing,
                    },
                    // 已不在列表里的任务：忽略。
                    fluxdown_protocol::FileMissingUpdateDto {
                        task_id: "gone".to_owned(),
                        missing: true,
                    },
                ],
            })
        };
        assert!(controller.apply_daemon_event(&missing(true)));
        {
            let rows = controller.store().local();
            assert!(rows[0].is_file_missing());
            assert!(!rows[0].has_local_file());
        }
        // 重复上报同一状态不算变化，不触发重绘。
        assert!(!controller.apply_daemon_event(&missing(true)));

        // 文件移回原目录：标记翻回，行重新可打开 / 拖出。
        assert!(controller.apply_daemon_event(&missing(false)));
        assert!(controller.store().local()[0].has_local_file());
    }

    fn file_exists_request(
        request_id: &str,
        task_id: &str,
    ) -> fluxdown_protocol::SelectionRequestDto {
        serde_json::from_value(json!({
            "requestId": request_id,
            "taskId": task_id,
            "kind": {
                "type": "fileExists",
                "fileName": "a.bin",
                "saveDir": "/dl",
                "renamePreview": "a (1).bin",
                "actions": ["rename"],
            },
            "defaultChoice": {"kind": "fileExists", "action": "rename"},
            "deadlineUnixMs": 0,
        }))
        .expect("valid fileExists request")
    }

    #[test]
    fn pending_file_exists_selection_marks_only_its_task_row_until_resolved() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        for id in ["a", "b"] {
            controller.local.push(task(id));
        }
        controller.rebuild_all();
        let pending = |controller: &DownloadsController| -> Vec<bool> {
            controller
                .store()
                .local()
                .iter()
                .map(|row| row.conflict_pending)
                .collect()
        };
        assert_eq!(pending(&controller), [false, false]);

        let request = file_exists_request("r1", "b");
        assert!(controller.apply_daemon_event(&DaemonEvent::SelectionPending(request.clone())));
        assert_eq!(pending(&controller), [false, true]);
        // 重复到达同一请求不是变化。
        assert!(!controller.apply_daemon_event(&DaemonEvent::SelectionPending(request)));
        // 重连 / 晚到的整体快照自带的待确认请求同样立刻带角标，并替换掉旧集合。
        let mut snapshot = fluxdown_protocol::AgentSnapshot {
            daemon_connected: true,
            ..Default::default()
        };
        snapshot.daemon.tasks = vec![task("a"), task("b")];
        snapshot.daemon.pending_selections = vec![file_exists_request("r2", "a")];
        controller.replace_snapshot(&snapshot);
        assert_eq!(pending(&controller), [true, false]);
        snapshot.daemon.pending_selections.clear();
        controller.replace_snapshot(&snapshot);
        assert_eq!(pending(&controller), [false, false]);

        assert!(
            controller.apply_daemon_event(&DaemonEvent::SelectionPending(file_exists_request(
                "r1", "b"
            )))
        );
        assert_eq!(pending(&controller), [false, true]);

        // 未知请求 id 的解决事件不是变化；已知的才清除角标。
        assert!(
            !controller.apply_daemon_event(&DaemonEvent::SelectionResolved {
                request_id: "nope".to_owned()
            })
        );
        assert!(
            controller.apply_daemon_event(&DaemonEvent::SelectionResolved {
                request_id: "r1".to_owned()
            })
        );
        assert_eq!(pending(&controller), [false, false]);
    }

    #[test]
    fn progress_touches_only_its_row_and_delete_keeps_index_consistent() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        for id in ["a", "b", "c"] {
            controller.local.push(task(id));
        }
        controller.rebuild_all();
        let before = controller.store().generation();

        controller.apply_daemon_event(&DaemonEvent::Engine(WsServerMsg::TaskMetaProbed {
            task_id: "b".to_owned(),
            file_name: "b.bin".to_owned(),
            total_bytes: 10,
        }));
        assert_eq!(controller.store().generation(), before + 1);
        assert_eq!(controller.store().find_local("b"), Some(1));
        assert!(controller.store().local()[0].name.is_empty());
        assert_eq!(controller.store().local()[1].name, "b.bin");

        controller.apply_daemon_event(&DaemonEvent::TaskDeleted {
            task_id: "a".to_owned(),
        });
        assert_eq!(controller.local.len(), 2);
        assert_eq!(controller.store().find_local("a"), None);
        assert_eq!(controller.store().find_local("c"), Some(0));
        assert_eq!(controller.store().find_local("b"), Some(1));
        assert_eq!(controller.local[0].task_id, "c");
        assert_eq!(controller.store().local()[0].key.task_id(), "c");
    }

    fn progress_event(id: &str, status: i32, error_message: &str) -> DaemonEvent {
        DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id: id.to_owned(),
            status,
            downloaded_bytes: 0,
            total_bytes: 0,
            speed: 5,
            upload_speed: 0,
            file_name: String::new(),
            save_dir: "/tmp".to_owned(),
            url: "https://example.com/download".to_owned(),
            error_message: error_message.to_owned(),
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            seeding_time_secs: 0,
        })
    }

    fn controller_with_running(ids: &[&str]) -> DownloadsController {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        for id in ids {
            let mut running = task(id);
            running.status = 1;
            controller.apply_daemon_event(&DaemonEvent::TaskChanged(running));
            controller.apply_daemon_event(&progress_event(id, 1, ""));
            controller.apply_daemon_event(&DaemonEvent::TaskRuntimeChanged(
                fluxdown_protocol::TaskRuntimeDto {
                    task_id: (*id).to_owned(),
                    active_transfers: Some(1),
                    ..Default::default()
                },
            ));
        }
        controller
    }

    #[test]
    fn tasks_deleted_batch_equals_sequential_task_deleted_with_one_structure_bump() {
        let ids = ["a", "b", "c", "d", "e"];
        let mut batched = controller_with_running(&ids);
        let mut sequential = controller_with_running(&ids);
        // 未知 id 被忽略；顺序与重复 id 都按逐条语义处理。
        let deleted: Vec<String> = ["b", "missing", "d", "a", "b"]
            .into_iter()
            .map(str::to_owned)
            .collect();

        let structure = batched.store.structure_generation();
        assert!(batched.apply_daemon_event(&DaemonEvent::TasksDeleted {
            task_ids: deleted.clone(),
        }));
        assert_eq!(
            batched.store.structure_generation(),
            structure.wrapping_add(1)
        );
        for task_id in deleted {
            sequential.apply_daemon_event(&DaemonEvent::TaskDeleted { task_id });
        }

        assert_eq!(batched.local, sequential.local);
        let keys = |controller: &DownloadsController| {
            controller
                .store
                .local()
                .iter()
                .map(|row| row.key.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&batched), keys(&sequential));
        assert_eq!(batched.local.len(), 2);
        // 下标索引与行一致，被删任务的运行时与实时速度一并清掉。
        for (ix, row) in batched.local.iter().enumerate() {
            assert_eq!(batched.store.find_local(&row.task_id), Some(ix));
        }
        for gone in ["a", "b", "d"] {
            assert_eq!(batched.store.find_local(gone), None);
            assert!(batched.task_runtime(gone).is_none());
            assert!(!batched.live_speeds.contains_key(gone));
        }
        for kept in ["c", "e"] {
            assert!(batched.task_runtime(kept).is_some());
            assert!(batched.live_speeds.contains_key(kept));
        }
        // 空批次不报告变化；只含未知 id 的批次不动结构计数。
        let structure = batched.store.structure_generation();
        assert!(!batched.apply_daemon_event(&DaemonEvent::TasksDeleted {
            task_ids: Vec::new(),
        }));
        assert!(batched.apply_daemon_event(&DaemonEvent::TasksDeleted {
            task_ids: vec!["missing".to_owned()],
        }));
        assert_eq!(batched.store.structure_generation(), structure);
    }

    #[test]
    fn delete_sentinel_progress_removes_the_row_instead_of_marking_it_failed() {
        let mut controller = controller_with_running(&["a", "b", "c"]);

        // 真实的失败进度仍把行标成失败，保留错误信息。
        controller.apply_daemon_event(&progress_event("a", 4, "disk full"));
        assert_eq!(controller.store.local().len(), 3);
        assert_eq!(controller.task_dto("a").unwrap().error_message, "disk full");

        let structure = controller.store.structure_generation();
        assert!(controller.apply_daemon_event(&progress_event("b", 4, "deleted")));
        assert_eq!(
            controller.store.structure_generation(),
            structure.wrapping_add(1)
        );
        assert_eq!(controller.store.find_local("b"), None);
        assert!(controller.task_dto("b").is_none());
        assert!(controller.task_runtime("b").is_none());
        assert!(!controller.live_speeds.contains_key("b"));
        assert_eq!(controller.store.local().len(), 2);
        for (ix, row) in controller.local.iter().enumerate() {
            assert_eq!(controller.store.find_local(&row.task_id), Some(ix));
        }

        // 对未知任务的哨兵不留下实时速度残留。
        controller.apply_daemon_event(&progress_event("ghost", 4, "deleted"));
        assert!(!controller.live_speeds.contains_key("ghost"));
    }

    #[test]
    fn late_snapshot_contains_segments_and_pausing_clears_active_without_erasing_bytes() {
        let mut snapshot = fluxdown_protocol::AgentSnapshot {
            daemon_connected: true,
            ..Default::default()
        };
        let mut initial = task("t");
        initial.status = 1;
        initial.total_bytes = 100;
        snapshot.daemon.tasks.push(initial);
        snapshot.daemon.task_runtime.insert(
            "t".to_owned(),
            fluxdown_protocol::TaskRuntimeDto {
                task_id: "t".to_owned(),
                total_bytes: 100,
                active_transfers: Some(1),
                segments: vec![fluxdown_protocol::TaskSegmentDto {
                    start_byte: 0,
                    end_byte: 99,
                    downloaded_bytes: 23,
                    active: Some(true),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        controller.replace_snapshot(&snapshot);
        assert_eq!(controller.store().local()[0].active_transfers(), Some(1));
        assert_eq!(
            controller.store().local()[0]
                .runtime
                .as_ref()
                .unwrap()
                .segments[0]
                .downloaded_bytes,
            23
        );

        let mut paused = snapshot.daemon.tasks[0].clone();
        paused.status = 2;
        controller.apply_daemon_event(&DaemonEvent::TaskChanged(paused));
        {
            let row = &controller.store().local()[0];
            assert_eq!(row.active_transfers(), Some(0));
            assert_eq!(
                row.runtime.as_ref().unwrap().segments[0].downloaded_bytes,
                23
            );
            assert_eq!(
                row.runtime.as_ref().unwrap().segments[0].active,
                Some(false)
            );
        }

        controller.mark_stale();
        assert_eq!(controller.store().local()[0].active_transfers(), None);
        assert!(controller.store().local()[0].runtime.is_none());
    }
    #[test]
    fn runtime_source_sequence_prevents_regression_and_paused_transfer_revival() {
        let mut snapshot = fluxdown_protocol::AgentSnapshot {
            daemon_connected: true,
            ..Default::default()
        };
        let mut task = task("t");
        task.status = 1;
        snapshot.daemon.tasks.push(task.clone());
        let current = fluxdown_protocol::TaskRuntimeDto {
            task_id: "t".to_owned(),
            sampled_at_ms: 100,
            sample_sequence: 10,
            total_bytes: 100,
            active_transfers: Some(2),
            segments: vec![fluxdown_protocol::TaskSegmentDto {
                start_byte: 0,
                end_byte: 99,
                downloaded_bytes: 30,
                active: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };
        snapshot
            .daemon
            .task_runtime
            .insert("t".to_owned(), current.clone());
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        controller.replace_snapshot(&snapshot);

        let mut older = current.clone();
        older.sampled_at_ms = 10_000; // 墙上时间即使更新也不得决定顺序。
        older.sample_sequence = 9;
        older.segments[0].downloaded_bytes = 10;
        older.active_transfers = Some(7);
        assert!(!controller.apply_daemon_event(&DaemonEvent::TaskRuntimeChanged(older.clone())));
        older.sample_sequence = 10;
        assert!(!controller.apply_daemon_event(&DaemonEvent::TaskRuntimeChanged(older)));
        {
            let row = &controller.store().local()[0];
            assert_eq!(row.active_transfers(), Some(2));
            assert_eq!(
                row.runtime.as_ref().unwrap().segments[0].downloaded_bytes,
                30
            );
        }
        let mut count_only = current.clone();
        count_only.sample_sequence = 11;
        count_only.segments.clear();
        count_only.active_transfers = Some(3);
        assert!(controller.apply_daemon_event(&DaemonEvent::TaskRuntimeChanged(count_only)));
        {
            let row = &controller.store().local()[0];
            assert_eq!(row.active_transfers(), Some(3));
            assert_eq!(
                row.runtime.as_ref().unwrap().segments[0].downloaded_bytes,
                30
            );
        }

        task.status = 2;
        controller.apply_daemon_event(&DaemonEvent::TaskChanged(task));
        let mut later = current;
        later.sample_sequence = 12;
        later.sampled_at_ms = 1;
        later.segments[0].downloaded_bytes = 45;
        later.active_transfers = Some(3);
        assert!(controller.apply_daemon_event(&DaemonEvent::TaskRuntimeChanged(later)));
        let row = &controller.store().local()[0];
        assert_eq!(row.active_transfers(), Some(0));
        let runtime = row.runtime.as_ref().unwrap();
        assert_eq!(runtime.segments[0].downloaded_bytes, 45);
        assert_eq!(runtime.segments[0].active, Some(false));
    }

    #[test]
    fn empty_progress_error_clears_previous_failure() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        let mut initial = task("t");
        initial.status = 4;
        initial.error_message = "transient failure".to_owned();
        controller.local.push(initial);
        controller.rebuild_all();
        controller.apply_daemon_event(&DaemonEvent::Engine(WsServerMsg::TaskProgress {
            task_id: "t".to_owned(),
            status: 1,
            downloaded_bytes: 0,
            total_bytes: 100,
            speed: 0,
            upload_speed: 0,
            file_name: String::new(),
            save_dir: "/tmp".to_owned(),
            url: "https://example.com/download".to_owned(),
            error_message: String::new(),
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            seeding_time_secs: 0,
        }));
        assert!(controller.store().local()[0].error_message.is_empty());
        assert!(controller.task_dto("t").unwrap().error_message.is_empty());
    }

    fn agent(event: AgentEvent) -> ServiceEvent {
        ServiceEvent::Agent(event)
    }

    #[test]
    fn device_presence_tracks_cloud_and_agent_not_daemon_connectivity() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        let mut snapshot = fluxdown_protocol::AgentSnapshot {
            cloud_devices: serde_json::from_value(json!([
                {"id":"1","deviceId":"other","isOnline":true}
            ]))
            .expect("devices"),
            ..fluxdown_protocol::AgentSnapshot::default()
        };
        snapshot.cloud_connection.state = fluxdown_protocol::CloudConnectionState::Connected;
        controller.replace_snapshot(&snapshot);
        // 下载核心离线不代表云端或 agent 已断连。
        assert!(controller.is_stale());
        assert_eq!(controller.other_devices()[0].online, Some(true));
        controller.apply_event(&agent(AgentEvent::CloudConnectionChanged(
            fluxdown_protocol::CloudConnectionDto::default(),
        )));
        assert_eq!(controller.other_devices()[0].online, None);
        controller.apply_event(&agent(AgentEvent::CloudConnectionChanged(
            snapshot.cloud_connection.clone(),
        )));
        assert_eq!(controller.other_devices()[0].online, Some(true));
        controller.mark_stale();
        assert_eq!(controller.other_devices()[0].online, None);
        controller.apply_event(&agent(AgentEvent::DaemonConnectionChanged(true)));
        assert_eq!(controller.other_devices()[0].online, None);
        controller.replace_snapshot(&snapshot);
        assert_eq!(controller.other_devices()[0].online, Some(true));
    }

    fn remote_task(id: &str, from: &str, to: &str) -> fluxdown_protocol::RemoteTaskDto {
        serde_json::from_value(json!({
            "id": id, "fromDevice": from, "toDevice": to,
            "url": "https://example.com/a", "status": "downloading"
        }))
        .expect("remote task")
    }

    #[test]
    fn remote_rows_skip_tasks_targeting_this_device_and_clear_when_session_ends() {
        let mut controller = DownloadsController::new(Arc::new(NullPort));
        let devices = serde_json::from_value(json!([
            {"id":"1","deviceId":"dev-me","name":"Me","isCurrent":true},
            {"id":"2","deviceId":"dev-mac","name":"Mac"}
        ]))
        .expect("devices");
        controller.apply_event(&agent(AgentEvent::CloudDevicesChanged(devices)));
        controller.apply_event(&agent(AgentEvent::RemoteTasksChanged(vec![
            // 本机发出去的任务：显示。
            remote_task("sent", "dev-me", "dev-mac"),
            // 别的设备下发给本机的任务：本地已有真实任务，云端镜像行不显示。
            remote_task("received", "dev-mac", "dev-me"),
        ])));
        let store = controller.store();
        let rows = store.remote();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].to_device, "dev-mac");
        drop(rows);

        // 登出 / 会话被撤销：不留旧账号的设备与任务。
        controller.apply_event(&agent(AgentEvent::SessionChanged(Box::new(None))));
        assert!(controller.store().remote().is_empty());
        assert!(controller.other_devices().is_empty());
        assert_eq!(controller.local_device_id(), None);
    }
}
