pub(crate) mod aria2c;
pub(crate) mod bt_files;
pub(crate) mod bt_guard;
pub(crate) mod bt_trackers;
pub(crate) mod categories;
pub(crate) mod counts;
pub(crate) mod devices;
pub(crate) mod directory_tree;
pub(crate) mod dispatch;
pub(crate) mod file_conflict;
pub(crate) mod file_hash;
pub(crate) mod file_rescan;
pub(crate) mod group_tree;
pub(crate) mod manifest;
pub(crate) mod new_download;
pub(crate) mod new_task_button;
pub(crate) mod preview;
pub(crate) mod progress_window;
pub(crate) mod refresh_gate;
pub(crate) mod row_order;
pub(crate) mod save_dir_category;
pub(crate) mod save_dir_history;
pub(crate) mod shutdown;
pub(crate) mod source_composition;
pub(crate) mod speed_limit_recent;
pub(crate) mod store;
pub(crate) mod task_route;
pub(crate) mod view_prefs;

pub(crate) use categories::CategoryIndex;
use std::rc::Rc;
pub(crate) use store::{RowChange, RowId, TaskStore};

use fluxdown_protocol::TaskRuntimeDto;
use fluxdown_ui_icon_pack::FileKind;
use gpui::SharedString;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DownloadStatusFilter {
    All,
    Completed,
    Seeding,
    Incomplete,
    Failed,
    Paused,
}

impl DownloadStatusFilter {
    /// 状态项数量（`slot` 取值范围）。
    pub(crate) const COUNT: usize = 6;

    /// 侧栏顺序：「做种中」紧随「已完成」。
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::All,
        Self::Incomplete,
        Self::Completed,
        Self::Seeding,
        Self::Failed,
        Self::Paused,
    ];

    pub(crate) fn exclusive_toggle(current: Option<Self>, clicked: Self) -> Option<Self> {
        if current == Some(clicked) {
            None
        } else {
            Some(clicked)
        }
    }

    fn slot(self) -> usize {
        match self {
            Self::All => 0,
            Self::Completed => 1,
            Self::Incomplete => 2,
            Self::Failed => 3,
            Self::Paused => 4,
            Self::Seeding => 5,
        }
    }

    pub(crate) fn matches(self, task: &DownloadTaskView) -> bool {
        let state = task.state;
        match self {
            Self::All => true,
            Self::Completed => state == TaskState::Completed,
            Self::Seeding => task.is_seeding(),
            // 下载中筛选包含下载中与等待中的任务。
            Self::Incomplete => matches!(state, TaskState::Downloading | TaskState::Pending),
            Self::Failed => state == TaskState::Failed,
            Self::Paused => state == TaskState::Paused,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StatusFolderMotion {
    from: [f32; DownloadStatusFilter::COUNT],
    to: [f32; DownloadStatusFilter::COUNT],
}

impl StatusFolderMotion {
    pub(crate) fn settled(open: Option<DownloadStatusFilter>) -> Self {
        let mut amounts = [0.; DownloadStatusFilter::COUNT];
        if let Some(status) = open {
            amounts[status.slot()] = 1.;
        }
        Self {
            from: amounts,
            to: amounts,
        }
    }

    pub(crate) fn retarget(self, progress: f32, next: Option<DownloadStatusFilter>) -> Self {
        let progress = progress.clamp(0., 1.);
        let mut from = [0.; DownloadStatusFilter::COUNT];
        for (slot, amount) in from.iter_mut().enumerate() {
            *amount = self.from[slot] + (self.to[slot] - self.from[slot]) * progress;
        }
        let mut to = [0.; DownloadStatusFilter::COUNT];
        if let Some(status) = next {
            to[status.slot()] = 1.;
        }
        Self { from, to }
    }

    pub(crate) fn amount(self, status: DownloadStatusFilter, progress: f32) -> f32 {
        let progress = progress.clamp(0., 1.);
        let slot = status.slot();
        self.from[slot] + (self.to[slot] - self.from[slot]) * progress
    }
}

/// 状态 + 分类（分类 id 来自 [`fluxdown_protocol::CustomCategoryDto::id`]）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DownloadFilter {
    pub(crate) status: DownloadStatusFilter,
    pub(crate) category: Option<String>,
}

impl DownloadFilter {
    pub(crate) const ALL: Self = Self {
        status: DownloadStatusFilter::All,
        category: None,
    };

    pub(crate) const fn status(status: DownloadStatusFilter) -> Self {
        Self {
            status,
            category: None,
        }
    }

    pub(crate) fn with_category(status: DownloadStatusFilter, category: &str) -> Self {
        Self {
            status,
            category: Some(category.to_owned()),
        }
    }

    pub(crate) fn matches(&self, task: &DownloadTaskView, categories: &CategoryIndex) -> bool {
        self.status.matches(task)
            && self
                .category
                .as_deref()
                .is_none_or(|category| categories.matches(category, task))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SidebarSection {
    Status,
    Queues,
    Devices,
}

impl SidebarSection {
    pub(crate) const ALL: [Self; 3] = [Self::Status, Self::Queues, Self::Devices];

    /// 分区可见性偏好键。设备区偏好只对本机有意义（每台设备各自决定是否显示），写设备本地
    /// 偏好；其余分区沿用云同步目录里的 `ui.show_sidebar_*`。
    pub(crate) fn visibility_pref(self) -> &'static str {
        match self {
            Self::Status => "ui.show_sidebar_status",
            Self::Queues => "ui.show_sidebar_queues",
            Self::Devices => "ui.show_sidebar_devices",
        }
    }

    /// 可见性偏好是否随云同步（设备区为设备本地）。
    pub(crate) fn visibility_syncs(self) -> bool {
        !matches!(self, Self::Devices)
    }
}

/// 侧栏当前选中项；决定表格的筛选来源。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SidebarSelection {
    Download(DownloadFilter),
    Queue(String),
    /// `"local"` = 本机，`"all"` = 全部设备，其余为云设备 id / 配对设备指纹。
    Device(String),
}

impl SidebarSelection {
    pub(crate) const LOCAL_DEVICE: &'static str = "local";
    pub(crate) const ALL_DEVICES: &'static str = "all";

    /// 设备项的行筛选（表格与侧栏计数共用，保证数字与表格行数一致）：
    /// 本机 = 全部本地任务；全部设备 = 本地 + 远程；其余 = 目标设备是它的远程任务
    /// （`to_device`，即任务实际下载的设备；`from_device` 只是发起者）。
    pub(crate) fn device_matches(device: &str, task: &DownloadTaskView) -> bool {
        match device {
            Self::LOCAL_DEVICE => task.source == TaskSource::Local,
            Self::ALL_DEVICES => true,
            id => task.source == TaskSource::Remote && task.to_device == id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TaskState {
    Pending,
    Downloading,
    Paused,
    Completed,
    Failed,
}

impl TaskState {
    /// 引擎原始 status → 展示态。5（准备中：探测 / 插件解析 / 分配）归入 `Pending`，
    /// 由 [`DownloadTaskView::preparing`] 区分；状态文案统一经 `DownloadStrings` 的行级规则。
    pub(crate) fn from_status(status: i32) -> Self {
        match status {
            0 | 5 => Self::Pending,
            1 => Self::Downloading,
            2 => Self::Paused,
            3 => Self::Completed,
            _ => Self::Failed,
        }
    }

    /// 状态优先级（「状态」列排序、按状态分组的组顺序）：
    /// 下载中 > 等待 > 失败 > 暂停 > 完成。失败排在暂停前：失败需要用户处理，暂停是主动操作。
    pub(crate) fn status_rank(self) -> u8 {
        match self {
            Self::Downloading => 0,
            Self::Pending => 1,
            Self::Failed => 2,
            Self::Paused => 3,
            Self::Completed => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TaskProtocol {
    Http,
    Bt,
    Ed2k,
    Ftp,
    Hls,
}

impl TaskProtocol {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::Bt => "BT",
            Self::Ed2k => "ED2K",
            Self::Ftp => "FTP",
            Self::Hls => "HLS",
        }
    }

    fn detect(url: &str, file_name: &str) -> Self {
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("magnet:") || lower.starts_with("torrent-file://") {
            Self::Bt
        } else if lower.starts_with("ed2k://") {
            Self::Ed2k
        } else if lower.starts_with("ftp://") || lower.starts_with("ftps://") {
            Self::Ftp
        } else if lower.contains(".m3u8") || lower.contains(".mpd") || file_name.ends_with(".m3u8")
        {
            Self::Hls
        } else {
            Self::Http
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub enum TaskSource {
    Local,
    Remote,
}

/// 稳定的行身份（跨快照重建仍有效）；选中集合与窗口 key 用它。
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub(crate) enum RowKey {
    Local(String),
    Remote(String),
}

impl RowKey {
    pub(crate) fn task_id(&self) -> &str {
        match self {
            Self::Local(id) | Self::Remote(id) => id,
        }
    }

    pub(crate) fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }
}

#[derive(Clone)]
pub(crate) struct DownloadTaskView {
    pub(crate) key: RowKey,
    pub(crate) source: TaskSource,
    pub(crate) queue_id: String,
    pub(crate) name: String,
    /// 大小写折叠后的名称：建行时算一次，名称排序 / 搜索不再逐次分配。
    pub(crate) name_fold: String,
    /// 小写扩展名（无点；空 = 无扩展名）：建行时算一次，分类匹配 / 系统图标键在渲染热路径
    /// 上不再逐次分配。
    pub(crate) file_extension: SharedString,
    pub(crate) size: String,
    pub(crate) size_bytes: u64,
    pub(crate) downloaded_bytes: u64,
    pub(crate) speed_bytes_per_second: Option<u64>,
    /// 本地任务实时上传速度（字节/秒）：仅下载中 / 做种中有值，其余为 0；远程任务恒为 0。
    pub(crate) upload_speed_bytes_per_second: u64,
    pub(crate) runtime: Option<Rc<TaskRuntimeDto>>,
    pub(crate) runtime_connected: bool,
    pub(crate) eta_seconds: Option<u64>,
    pub(crate) created_at_secs: i64,
    pub(crate) completed_at_secs: i64,
    pub(crate) kind: FileKind,
    pub(crate) protocol: TaskProtocol,
    pub(crate) progress: f32,
    pub(crate) progress_label: String,
    pub(crate) state: TaskState,
    /// 引擎原始 status 为 5（准备中：探测 / 解析 / 分配），已占并发名额，智能排序归入活跃档。
    pub(crate) preparing: bool,
    /// 队列内插入序（`TaskDto::queue_order`，每队列 MAX+1）；打破同一秒批量添加的平局。
    pub(crate) queue_order: i32,
    /// 引擎待启动队列里的排序键（`QueuePositionDto::order`；`None` = 不在队列中）。任务在队列
    /// 期间不变，只在进出队列时变化；展示序号 = 键在有序键集合中的名次
    /// （[`TaskStore::queue_rank`]），渲染可见行时二分得到，不存进行里。
    pub(crate) queue_key: Option<i64>,
    pub(crate) metadata_pending: bool,
    pub(crate) url: String,
    pub(crate) origin_url: String,
    pub(crate) site: String,
    /// 大小写折叠后的链接 / 站点：建行时算一次，搜索不再逐行分配。
    pub(crate) url_fold: String,
    pub(crate) site_fold: String,
    pub(crate) referrer: String,
    pub(crate) save_dir: String,
    pub(crate) group_id: String,
    pub(crate) error_message: String,
    /// 该任务有待确认的「文件已存在」请求（任务表「待确认」角标）。
    pub(crate) conflict_pending: bool,
    pub(crate) file_missing: bool,
    /// 落盘文件哈希，格式 `algo=hex`（空 = 尚未计算）。只对已完成的本地任务有意义。
    pub(crate) file_hash: String,
    pub(crate) seeding_status: i32,
    pub(crate) uploaded_bytes: i64,
    pub(crate) boosted: bool,
    /// BT 选片：按顺序下载（`TaskDto::bt_sequential`；缺省 false = 最稀有优先）。
    pub(crate) bt_sequential: bool,
    /// BT 选片：先下载首尾文件块（`TaskDto::bt_first_last_pieces_first`）。
    pub(crate) bt_first_last_pieces_first: bool,
    /// 任务级 BT 下载限速（字节/秒，`TaskDto::bt_download_limit_bps`；0 = 不限）。
    pub(crate) bt_download_limit_bps: i64,
    /// 任务级上传限速（字节/秒，`TaskDto::seed_upload_limit_bps`；0 = 不限，负值为跟随全局哨兵）。
    pub(crate) seed_upload_limit_bps: i64,
    /// 超级做种（`TaskDto::bt_super_seeding`）。
    pub(crate) bt_super_seeding: bool,
    /// 远程任务的目标设备 id（`CloudDevice::device_id`；本地任务为空）。
    pub(crate) to_device: String,
    /// 远程任务的云端原始状态（本地任务为 `None`）；`Canceled` / `Unknown` 无法用
    /// [`TaskState`] 精确表达，行文案与可控性据此判断。
    pub(crate) remote_status: Option<fluxdown_protocol::RemoteTaskStatus>,
}

impl DownloadTaskView {
    pub(crate) fn local(
        task: &fluxdown_protocol::TaskDto,
        speed: Option<i64>,
        boosted: bool,
    ) -> Self {
        let mut view = Self::new(
            RowKey::Local(task.task_id.clone()),
            TaskSource::Local,
            task.queue_id.clone(),
            task.file_name.clone(),
            task.total_bytes,
            task.downloaded_bytes,
            speed.map(|value| value.max(0) as u64),
            parse_timestamp_secs(&task.created_at),
            task.status,
            &task.url,
        );
        view.completed_at_secs = parse_timestamp_secs(&task.completed_at);
        view.queue_order = task.queue_order;
        view.origin_url.clone_from(&task.origin_url);
        view.referrer.clone_from(&task.referrer);
        view.save_dir.clone_from(&task.save_dir);
        view.group_id.clone_from(&task.group_id);
        view.error_message.clone_from(&task.error_message);
        view.file_missing = task.file_missing;
        view.file_hash.clone_from(&task.file_hash);
        view.seeding_status = task.seeding_status;
        view.uploaded_bytes = task.uploaded_bytes;
        view.boosted = boosted;
        view.bt_sequential = task.bt_sequential;
        view.bt_first_last_pieces_first = task.bt_first_last_pieces_first;
        view.bt_download_limit_bps = task.bt_download_limit_bps;
        view.seed_upload_limit_bps = task.seed_upload_limit_bps;
        view.bt_super_seeding = task.bt_super_seeding;
        view
    }

    pub(crate) fn remote(task: &fluxdown_protocol::RemoteTaskDto) -> Self {
        let mut view = Self::new(
            RowKey::Remote(task.id.clone()),
            TaskSource::Remote,
            String::new(),
            task.file_name.clone(),
            task.total_bytes.unwrap_or_default(),
            task.downloaded_bytes,
            Some(task.speed.max(0) as u64),
            parse_timestamp_secs(&task.created_at),
            match task.status {
                fluxdown_protocol::RemoteTaskStatus::Pending
                | fluxdown_protocol::RemoteTaskStatus::Accepted
                | fluxdown_protocol::RemoteTaskStatus::Unknown => 0,
                fluxdown_protocol::RemoteTaskStatus::Downloading => 1,
                fluxdown_protocol::RemoteTaskStatus::Paused => 2,
                fluxdown_protocol::RemoteTaskStatus::Completed => 3,
                fluxdown_protocol::RemoteTaskStatus::Failed
                | fluxdown_protocol::RemoteTaskStatus::Canceled => 4,
            },
            &task.url,
        );
        view.save_dir = task.save_dir.clone().unwrap_or_default();
        view.error_message = task.error.clone().unwrap_or_default();
        view.to_device.clone_from(&task.to_device);
        view.remote_status = Some(task.status);
        view
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "normalizes the same compact projection from local and remote wire tasks"
    )]
    fn new(
        key: RowKey,
        source: TaskSource,
        queue_id: String,
        name: String,
        total_bytes: i64,
        downloaded_bytes: i64,
        speed_bytes_per_second: Option<u64>,
        created_at_secs: i64,
        status: i32,
        url: &str,
    ) -> Self {
        let metadata_pending = name.trim().is_empty();
        let kind = FileKind::of_name(&name);
        let protocol = TaskProtocol::detect(url, &name);
        let size_bytes = total_bytes.max(0) as u64;
        let downloaded = downloaded_bytes.max(0) as u64;
        let progress = if size_bytes == 0 {
            0.0
        } else {
            (downloaded as f64 / size_bytes as f64).clamp(0.0, 1.0) as f32
        };
        let eta_seconds = speed_bytes_per_second
            .filter(|speed| *speed > 0 && downloaded < size_bytes)
            .map(|speed| (size_bytes - downloaded) / speed);
        Self {
            key,
            source,
            queue_id,
            kind,
            protocol,
            name_fold: name.to_lowercase(),
            file_extension: file_extension(&name),
            name,
            size: format_bytes(size_bytes),
            size_bytes,
            downloaded_bytes: downloaded,
            speed_bytes_per_second,
            upload_speed_bytes_per_second: 0,
            runtime: None,
            runtime_connected: false,
            eta_seconds,
            created_at_secs,
            completed_at_secs: 0,
            progress,
            progress_label: format!("{:.1}%", progress * 100.0),
            state: TaskState::from_status(status),
            preparing: status == 5,
            queue_order: 0,
            queue_key: None,
            metadata_pending,
            site: url_host(url).to_owned(),
            url: url.to_owned(),
            url_fold: url.to_lowercase(),
            site_fold: url_host(url).to_lowercase(),
            origin_url: String::new(),
            referrer: String::new(),
            save_dir: String::new(),
            group_id: String::new(),
            error_message: String::new(),
            conflict_pending: false,
            file_missing: false,
            file_hash: String::new(),
            seeding_status: 0,
            uploaded_bytes: 0,
            boosted: false,
            bt_sequential: false,
            bt_first_last_pieces_first: false,
            bt_download_limit_bps: 0,
            seed_upload_limit_bps: 0,
            bt_super_seeding: false,
            to_device: String::new(),
            remote_status: None,
        }
    }

    /// 远程任务能否执行该动作（本地任务恒为 `false`：走 daemon 命令）。
    pub(crate) fn remote_can(&self, action: fluxdown_protocol::RemoteCommandAction) -> bool {
        self.remote_status
            .is_some_and(|status| dispatch::remote_action_applies(status, action))
    }

    /// 已完成但文件跟踪扫描判定产物已不在下载目录（被删除或移走）。
    pub(crate) fn is_file_missing(&self) -> bool {
        self.state == TaskState::Completed && self.file_missing
    }

    /// 本地做种中：已完成且 seeding_status 为 1（做种）或 8（排队等待做种名额）。
    pub(crate) fn is_seeding(&self) -> bool {
        self.key.is_local()
            && self.state == TaskState::Completed
            && matches!(self.seeding_status, 1 | 8)
    }

    /// 可恢复做种：本地已完成、seeding_status 2..=7（比例 / 时长达标、用户停止等）且产物仍在。
    pub(crate) fn can_resume_seeding(&self) -> bool {
        self.key.is_local()
            && self.state == TaskState::Completed
            && (2..=7).contains(&self.seeding_status)
            && !self.file_missing
    }

    /// 用户停止做种且产物仍在：「全部开始」会重新启动它。
    pub(crate) fn is_resume_all_seed(&self) -> bool {
        self.key.is_local()
            && self.state == TaskState::Completed
            && self.seeding_status == 4
            && !self.file_missing
    }

    /// 本机最终产物可被打开 / 拖出：本地、已完成且文件仍在下载目录。
    pub(crate) fn has_local_file(&self) -> bool {
        self.key.is_local() && self.state == TaskState::Completed && !self.file_missing
    }

    /// 本机最终产物路径（`save_dir/name`）；远程任务或文件名未知时为 `None`。
    pub(crate) fn local_file_path(&self) -> Option<std::path::PathBuf> {
        (self.key.is_local() && !self.name.is_empty())
            .then(|| std::path::Path::new(&self.save_dir).join(&self.name))
    }

    /// 「复制链接」用：`origin_url` 优先，空则回退 `url`（torrent 任务的 `url` 是哨兵）。
    pub(crate) fn share_url(&self) -> &str {
        if self.origin_url.is_empty() {
            &self.url
        } else {
            &self.origin_url
        }
    }

    /// 来源站点：`referrer` host 为主，空则 url host。
    pub(crate) fn source_site(&self) -> &str {
        let host = url_host(&self.referrer);
        if host.is_empty() { &self.site } else { host }
    }

    /// 可在浏览器打开的来源页：仅 `http(s)://` 且有 host 的 `referrer`。
    /// `referrer` 来自浏览器捕获 / API，不可信；`file://`、`javascript:` 等交给系统打开
    /// 会触发本地文件或协议处理器，只能复制不能点开。
    pub(crate) fn source_page_url(&self) -> Option<&str> {
        let (scheme, _) = self.referrer.split_once("://")?;
        let web = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
        (web && !url_host(&self.referrer).is_empty()).then_some(self.referrer.as_str())
    }

    /// 文件扩展名（小写，无点）。
    pub(crate) fn extension(&self) -> Option<&str> {
        (!self.file_extension.is_empty()).then_some(self.file_extension.as_ref())
    }

    pub(crate) fn active_transfers(&self) -> Option<u32> {
        if !self.runtime_connected || self.source != TaskSource::Local {
            None
        } else if self.state != TaskState::Downloading {
            Some(0)
        } else {
            self.runtime
                .as_ref()
                .and_then(|runtime| runtime.active_transfers)
        }
    }
}

/// URL host（不含端口 / 凭据）；非 URL 返回空串。
pub(crate) fn url_host(url: &str) -> &str {
    let Some((_, rest)) = url.split_once("://") else {
        return "";
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host.starts_with('[') {
        host.split_once(']').map_or(host, |(ipv6, _)| &ipv6[1..])
    } else {
        host.split_once(':').map_or(host, |(host, _)| host)
    }
}

/// 任务时间戳 → Unix 秒。本地任务是秒级数字串，远程任务（FluxCloud）是 RFC3339；
/// 两者都解析失败时为 0（界面按「未知」处理）。
pub(crate) fn parse_timestamp_secs(value: &str) -> i64 {
    let value = value.trim();
    value.parse().unwrap_or_else(|_| {
        chrono::DateTime::parse_from_rfc3339(value).map_or(0, |time| time.timestamp())
    })
}

/// 小写扩展名：最后一个 `.` 之后的部分；为空或含 `/`（点在目录名里）时视为没有扩展名。
fn file_extension(name: &str) -> SharedString {
    name.rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .filter(|extension| !extension.is_empty() && !extension.contains('/'))
        .map(SharedString::from)
        .unwrap_or_default()
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        DownloadTaskView, RowKey, SidebarSelection, TaskProtocol, TaskSource, TaskState, url_host,
    };

    #[test]
    fn local_and_remote_wire_tasks_project_without_placeholders() {
        let local = serde_json::from_value::<fluxdown_protocol::TaskDto>(json!({
            "taskId":"local-1","url":"https://user@example.com:8443/a?x=1","fileName":"a.bin",
            "saveDir":"/tmp","status":1,"downloadedBytes":50,"totalBytes":100,
            "errorMessage":"","createdAt":"7","proxyUrl":"","queueId":"main","checksum":"",
            "originUrl":"https://origin.example/page"
        }))
        .expect("local task");
        let local = DownloadTaskView::local(&local, None, false);
        assert_eq!(local.key, RowKey::Local("local-1".to_owned()));
        assert_eq!(local.source, TaskSource::Local);
        assert_eq!(local.progress, 0.5);
        assert_eq!(local.state, TaskState::Downloading);
        assert_eq!(local.site, "example.com");
        assert_eq!(local.share_url(), "https://origin.example/page");
        assert_eq!(local.protocol, TaskProtocol::Http);
        assert!(!local.metadata_pending);

        let remote = serde_json::from_value::<fluxdown_protocol::RemoteTaskDto>(json!({
            "id":"remote-1","url":"https://example.com/b","fileName":"b.bin",
            "status":"paused","totalBytes":200,"downloadedBytes":20,"speed":0
        }))
        .expect("remote task");
        let remote = DownloadTaskView::remote(&remote);
        assert_eq!(remote.key, RowKey::Remote("remote-1".to_owned()));
        assert_eq!(remote.source, TaskSource::Remote);
        assert_eq!(remote.state, TaskState::Paused);

        let targeted = serde_json::from_value::<fluxdown_protocol::RemoteTaskDto>(json!({
            "id":"remote-2","fromDevice":"dev-a","toDevice":"dev-b","url":"https://example.com/c",
            "fileName":null,"status":"someFutureStatus","downloadedBytes":0
        }))
        .expect("remote task with unknown status");
        let targeted = DownloadTaskView::remote(&targeted);
        assert_eq!(targeted.to_device, "dev-b");
        assert!(targeted.name.is_empty());
        assert_eq!(targeted.state, TaskState::Pending);
        // 云端新状态：不可控制（包括删除）。
        assert!(!targeted.remote_can(fluxdown_protocol::RemoteCommandAction::Pause));
        assert!(!targeted.remote_can(fluxdown_protocol::RemoteCommandAction::Delete));
        assert!(remote.remote_can(fluxdown_protocol::RemoteCommandAction::Resume));

        let probing = serde_json::from_value::<fluxdown_protocol::TaskDto>(json!({
            "taskId":"local-2","url":"magnet:?xt=urn:btih:abc","fileName":"",
            "saveDir":"/tmp","status":0,"downloadedBytes":0,"totalBytes":0,
            "errorMessage":"","createdAt":"8","proxyUrl":"","queueId":"main","checksum":""
        }))
        .expect("probing task");
        let probing = DownloadTaskView::local(&probing, None, false);
        assert!(probing.metadata_pending);
        assert!(probing.name.is_empty());
        assert_eq!(probing.protocol, TaskProtocol::Bt);
    }

    #[test]
    fn device_filter_matches_target_device_not_sender() {
        let remote = |id: &str, from: &str, to: &str| {
            let dto = serde_json::from_value::<fluxdown_protocol::RemoteTaskDto>(json!({
                "id": id, "fromDevice": from, "toDevice": to, "url": "https://example.com/a",
                "status": "downloading"
            }))
            .expect("remote task");
            DownloadTaskView::remote(&dto)
        };
        let local = serde_json::from_value::<fluxdown_protocol::TaskDto>(json!({
            "taskId":"l1","url":"https://example.com/l","fileName":"l.bin","saveDir":"/tmp",
            "status":1,"downloadedBytes":0,"totalBytes":1,"errorMessage":"","createdAt":"1",
            "proxyUrl":"","queueId":"main","checksum":""
        }))
        .expect("local task");
        let local = DownloadTaskView::local(&local, None, false);
        // Windows(dev-a) 把任务下发给 mac(dev-b)：选 mac 应看到它，选发起者 Windows 不应看到。
        let sent = remote("r1", "dev-a", "dev-b");
        assert!(SidebarSelection::device_matches("dev-b", &sent));
        assert!(!SidebarSelection::device_matches("dev-a", &sent));
        assert!(!SidebarSelection::device_matches(
            SidebarSelection::LOCAL_DEVICE,
            &sent
        ));
        assert!(SidebarSelection::device_matches(
            SidebarSelection::ALL_DEVICES,
            &sent
        ));
        assert!(SidebarSelection::device_matches(
            SidebarSelection::LOCAL_DEVICE,
            &local
        ));
        assert!(SidebarSelection::device_matches(
            SidebarSelection::ALL_DEVICES,
            &local
        ));
        assert!(!SidebarSelection::device_matches("dev-b", &local));
    }

    fn local_task(status: i32, seeding_status: i32, file_missing: bool) -> DownloadTaskView {
        let dto = serde_json::from_value::<fluxdown_protocol::TaskDto>(json!({
            "taskId":"s1","url":"magnet:?xt=urn:btih:abc","fileName":"s.bin","saveDir":"/tmp",
            "status":status,"downloadedBytes":1,"totalBytes":1,"errorMessage":"","createdAt":"1",
            "proxyUrl":"","queueId":"main","checksum":""
        }))
        .expect("local task");
        let mut view = DownloadTaskView::local(&dto, None, false);
        view.seeding_status = seeding_status;
        view.file_missing = file_missing;
        view
    }

    #[test]
    fn seeding_folder_holds_only_seeding_and_queued_seeding_completed_tasks() {
        use super::DownloadStatusFilter;
        for seeding in [1, 8] {
            let view = local_task(3, seeding, false);
            assert!(view.is_seeding());
            assert!(DownloadStatusFilter::Seeding.matches(&view));
            // 做种中的任务仍属于「已完成」。
            assert!(DownloadStatusFilter::Completed.matches(&view));
        }
        for seeding in [0, 2, 3, 4, 5, 6, 7] {
            assert!(!DownloadStatusFilter::Seeding.matches(&local_task(3, seeding, false)));
        }
        // 非完成态即使带做种状态也不进「做种中」。
        for status in [0, 1, 2, 4] {
            assert!(!DownloadStatusFilter::Seeding.matches(&local_task(status, 1, false)));
        }
        // 远程任务没有做种语义。
        let remote = serde_json::from_value::<fluxdown_protocol::RemoteTaskDto>(json!({
            "id":"r1","url":"https://example.com/a","status":"completed","downloadedBytes":0
        }))
        .expect("remote task");
        let mut remote = DownloadTaskView::remote(&remote);
        remote.seeding_status = 1;
        assert!(!DownloadStatusFilter::Seeding.matches(&remote));
        assert!(!remote.can_resume_seeding());
        // 做种项紧随「已完成」。
        let order = DownloadStatusFilter::ALL;
        let completed = order
            .iter()
            .position(|s| *s == DownloadStatusFilter::Completed);
        let seeding = order
            .iter()
            .position(|s| *s == DownloadStatusFilter::Seeding);
        assert_eq!(seeding, completed.map(|ix| ix + 1));
    }

    #[test]
    fn resume_seeding_requires_stopped_seed_status_and_existing_files() {
        for seeding in 2..=7 {
            assert!(local_task(3, seeding, false).can_resume_seeding());
            assert!(!local_task(3, seeding, true).can_resume_seeding());
        }
        // 普通已完成（seeding_status 0）、做种中 / 排队、非完成态都不可恢复做种（#935）。
        for seeding in [0, 1, 8] {
            assert!(!local_task(3, seeding, false).can_resume_seeding());
        }
        for status in [0, 1, 2, 4] {
            assert!(!local_task(status, 4, false).can_resume_seeding());
        }
        // 「全部开始」只额外计入用户停止（4）且文件仍在的任务。
        assert!(local_task(3, 4, false).is_resume_all_seed());
        assert!(!local_task(3, 4, true).is_resume_all_seed());
        assert!(!local_task(3, 2, false).is_resume_all_seed());
    }

    #[test]
    fn url_host_strips_credentials_port_and_path() {
        assert_eq!(url_host("https://u:p@host.example:443/x"), "host.example");
        assert_eq!(url_host("http://[::1]:80/"), "::1");
        assert_eq!(url_host("magnet:?xt=abc"), "");
    }

    #[test]
    fn source_page_url_only_opens_http_pages_with_host() {
        let page = |referrer: &str| DownloadTaskView {
            referrer: referrer.to_owned(),
            ..local_task(1, 0, false)
        };
        for web in [
            "https://a.example/p?q=1",
            "HTTP://a.example",
            "http://[::1]:8080/",
        ] {
            assert_eq!(page(web).source_page_url(), Some(web));
        }
        for blocked in [
            "",
            "https://",
            "file:///etc/passwd",
            "javascript:alert(1)//https://x",
            "ftp://a.example/",
            "chrome-extension://abc/page.html",
            " https://a.example/",
        ] {
            assert_eq!(page(blocked).source_page_url(), None, "{blocked}");
        }
    }

    #[test]
    fn remote_rfc3339_and_local_unix_timestamps_share_one_timeline() {
        let remote = serde_json::from_value::<fluxdown_protocol::RemoteTaskDto>(json!({
            "id":"r1","url":"https://example.com/a","status":"completed",
            "downloadedBytes":0,"createdAt":"2026-09-28T13:40:00.123456Z"
        }))
        .expect("remote task");
        assert_eq!(
            DownloadTaskView::remote(&remote).created_at_secs,
            1_790_602_800
        );
        assert_eq!(super::parse_timestamp_secs("1790602800"), 1_790_602_800);
        assert_eq!(super::parse_timestamp_secs("not a time"), 0);
    }
}
