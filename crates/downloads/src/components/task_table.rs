use std::{
    cell::{Cell, Ref, RefCell},
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    time::{Duration, Instant},
};

use fluxdown_protocol::{RemoteCommandAction, RemoteCommandParams, RemoteTaskStatus};
use fluxdown_ui_components::{CheckState, FluxIcon, check_mark, tabular_numbers, tooltip_view};
use fluxdown_ui_icon_pack::FileKind;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Edges, Entity, FocusHandle, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, ScrollEvent, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, Task, WeakEntity, Window, div, linear_color_stop,
    linear_gradient, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ElementExt as _, Icon, IconName, Sizable as _, Size, h_flex,
    menu::{PopupMenu, PopupMenuItem},
    scroll::Scrollbar,
    spinner::Spinner,
    table::{Column, DataTable, TableDelegate, TableState},
    v_flex,
};

use crate::{
    UserStartIntent,
    batch::{MAX_IN_FLIGHT, coalesce_commands, retry_copy, retry_delay},
    components::{
        conflict_badge::conflict_badge, file_icon::task_file_icon, task_drag::DraggedTasks,
    },
    controller::DownloadsCommand,
    deletion::{MIN_BATCH_DELETE, split_local_deletes},
    model::{
        CategoryIndex, DownloadFilter, DownloadTaskView, RowChange, RowId, RowKey,
        SidebarSelection, TaskProtocol, TaskSource, TaskState, TaskStore,
        counts::SidebarCounts,
        dispatch::DispatchSummary,
        format_bytes,
        group_tree::GroupMeta,
        row_order::{InteractionHold, RowOrder},
        view_prefs::{
            DateBucket, HeaderAlign, SortDir, ViewDensity, ViewGroupBy, ViewPrefs, ViewSortKey,
            state_group_key,
        },
    },
    pages::downloads::{DownloadView, PluginTaskCommand},
    strings::{DownloadStrings, error_text},
};

#[path = "task_table_groups.rs"]
mod groups;
use groups::GroupRow;

/// 固定左侧选择列的最小宽（含表格左侧留白）：平时显示文件类型图标，行悬停 / 已选中 /
/// 存在任意选中时换成复选框。实际宽度随密度下的图标尺寸放宽，见
/// [`DownloadTableDelegate::selection_column_width`]。
const MIN_SELECTION_COLUMN_WIDTH: f32 = 36.;
/// 表头拖拽调宽的上限（下限按列取 [`DownloadColumnKind::min_width`]）。
const MAX_COLUMN_WIDTH: f32 = 480.;
/// 文件名列拖宽上限（长文件名需要比其他列更宽的空间）。
const FILE_NAME_MAX_WIDTH: f32 = 1600.;
/// 表头高：取 `density.control`（默认 28），随文字增大；DataTable 的 `Size` 同时决定
/// 表头行高，任务行高由 `render_tr` 按密度覆盖。
pub(crate) fn table_header_height(cx: &App) -> Pixels {
    active_theme(cx).density().control
}
/// 表格右侧留白（`render_last_empty_col`），与选择列内含的左侧留白对称（≈ spacing.sm）。
const TABLE_TRAILING_GUTTER: f32 = 8.;
/// 表头列分隔线高（常驻的拖宽提示，悬停时由拖宽柄全高高亮接管）。
const COLUMN_DIVIDER_HEIGHT: f32 = 14.;
/// 数据列左右内边距：显式设给每列，进度条宽度据此推算。
const CELL_PADDING_X: f32 = 8.;
/// 进度列百分比区宽：容纳 12px 等宽数字「99%」。
const PROGRESS_LABEL_WIDTH: f32 = 32.;
/// 进度条与百分比的间距。
const PROGRESS_GAP: f32 = 8.;
/// 暂停态进度条：`statusPaused` 的 40%。
const PAUSED_BAR_ALPHA: f32 = 0.4;
/// 正在删除（已提交、尚未从列表消失）的行的不透明度。
const PENDING_DELETE_OPACITY: f32 = 0.4;
/// 行悬停操作按钮边长。
const ROW_ACTION_SIZE: f32 = 24.;
/// 行悬停操作浮层左侧渐隐宽：盖住末列文字时软过渡，避免硬切。
const ROW_ACTION_FADE: f32 = 16.;
/// 舒适密度下文件图标相对 `icon.lg`（16）的倍数：24px，与双行文字块（约 34px）协调。
const FILE_ICON_COMFORTABLE_SCALE: f32 = 1.5;
/// 宽松密度下文件图标相对 `icon.lg` 的倍数：32px，与三行主列（名称 / 进度条 / 元信息）协调。
const FILE_ICON_RELAXED_SCALE: f32 = 2.;
/// 选中底色左右内缩（inset 样式）。
const SELECTED_INSET_X: f32 = 4.;
/// 选择列图标与选中底色左右边缘之间的最小留白：图标（含满画布的图标包字形）不贴边、不越出圆角底色。
const FILE_ICON_GAP_X: f32 = 4.;

/// 各密度下选择列文件图标边长（`lg` = 主题 `icon.lg`）。
fn file_icon_size_for(density: ViewDensity, lg: f32) -> f32 {
    match density {
        ViewDensity::Relaxed => lg * FILE_ICON_RELAXED_SCALE,
        ViewDensity::Comfortable => lg * FILE_ICON_COMFORTABLE_SCALE,
        ViewDensity::Compact => lg,
    }
}

/// 选择列宽：图标居中后两侧仍留出 选中内缩 + 留白，且不小于 [`MIN_SELECTION_COLUMN_WIDTH`]。
fn selection_column_width_for(density: ViewDensity, lg: f32) -> f32 {
    let icon = file_icon_size_for(density, lg);
    (icon + 2. * (SELECTED_INSET_X + FILE_ICON_GAP_X)).max(MIN_SELECTION_COLUMN_WIDTH)
}
/// 任务行 group 名：选择列复选框与行操作按钮随行悬停显隐。
const ROW_GROUP: &str = "download-task-row";
/// 表头全选格 group 名：悬停该格时显示全选框。
const SELECT_ALL_GROUP: &str = "download-select-all";
/// 任务行首图标格 group 名：只在悬停该格本身时把文件图标换成复选框。
const SELECT_CELL_GROUP: &str = "download-task-select-cell";
/// 超过一天的剩余时间估算不可信，不显示。
const MAX_ETA_SECS: u64 = 86_400;
/// 表头按下与抬起的位移超过它视为拖动列（不触发排序）。
const HEADER_CLICK_SLOP: f32 = 4.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DownloadColumnKind {
    FileName,
    Progress,
    Size,
    Speed,
    Eta,
    Status,
    Created,
    Protocol,
    Source,
    Queue,
}

impl DownloadColumnKind {
    /// 默认列顺序（`columns` 偏好为空或缺列时按此补齐）。
    pub(crate) const ALL: [Self; 10] = [
        Self::FileName,
        Self::Size,
        Self::Progress,
        Self::Status,
        Self::Created,
        Self::Speed,
        Self::Eta,
        Self::Protocol,
        Self::Source,
        Self::Queue,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::FileName => "file_name",
            Self::Progress => "progress",
            Self::Size => "size",
            Self::Speed => "speed",
            Self::Eta => "eta",
            Self::Status => "status",
            Self::Created => "created",
            Self::Protocol => "protocol",
            Self::Source => "source",
            Self::Queue => "queue",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == key)
    }

    /// 速度 / 剩余时间已并入「状态」列的活动文案，默认隐藏（仍可在列设置打开）。
    fn default_visible(self) -> bool {
        matches!(
            self,
            Self::FileName | Self::Size | Self::Progress | Self::Status | Self::Created
        )
    }

    /// 文件名列的值只在容器宽度未知时使用；已知后由 [`DownloadTableDelegate`]
    /// 按容器宽回算（吸收剩余宽度），用户拖过后改用 `ViewPrefs::file_name_width`。
    fn default_width(self) -> f32 {
        match self {
            Self::FileName => 240.,
            Self::Progress => 150.,
            Self::Size => 84.,
            Self::Speed => 90.,
            Self::Eta => 84.,
            Self::Status => 140.,
            // `YYYY-MM-DD HH:MM:SS` + 左右内边距；过窄时秒位贴边，悬停操作条一盖就看不清。
            Self::Created => 164.,
            Self::Protocol => 64.,
            Self::Source => 148.,
            Self::Queue => 88.,
        }
    }

    fn min_width(self) -> f32 {
        match self {
            Self::FileName => 160.,
            Self::Progress => 110.,
            Self::Size => 64.,
            Self::Speed => 72.,
            Self::Eta => 64.,
            Self::Status => 84.,
            Self::Created => 152.,
            Self::Protocol => 56.,
            Self::Source => 96.,
            Self::Queue => 72.,
        }
    }

    /// 文字缩放 `scale` 下列的显示宽：等于默认宽的列（用户没拖过）整体随文字放大，
    /// 其余保持用户宽度，但不低于放大后的最小宽。`scale` 为 1 时恒等于 `width`
    /// （不小于 [`Self::min_width`]）。
    fn display_width(self, width: f32, scale: f32) -> f32 {
        let min_width = self.min_width() * scale;
        if (width - self.default_width()).abs() < 0.5 {
            (width * scale).max(min_width)
        } else {
            width.max(min_width)
        }
    }

    /// 点击表头切换到的排序键。
    fn sort_key(self) -> Option<ViewSortKey> {
        match self {
            Self::FileName => Some(ViewSortKey::Name),
            Self::Progress => Some(ViewSortKey::Progress),
            Self::Size => Some(ViewSortKey::Size),
            Self::Speed => Some(ViewSortKey::Speed),
            Self::Created => Some(ViewSortKey::Created),
            Self::Status => Some(ViewSortKey::Status),
            _ => None,
        }
    }

    /// 首次切到该列排序时的方向：名称 A→Z，数值类从大到新。
    fn default_sort_dir(self) -> SortDir {
        match self {
            Self::FileName => SortDir::Asc,
            _ => SortDir::Desc,
        }
    }

    pub(crate) fn label(self, strings: &DownloadStrings) -> SharedString {
        match self {
            Self::FileName => strings.col_file_name.clone(),
            Self::Progress => strings.col_progress.clone(),
            Self::Size => strings.col_size.clone(),
            Self::Speed => strings.col_speed.clone(),
            Self::Eta => strings.col_eta.clone(),
            Self::Status => strings.col_status.clone(),
            Self::Created => strings.col_created.clone(),
            Self::Protocol => strings.col_protocol.clone(),
            Self::Source => strings.col_source.clone(),
            Self::Queue => strings.col_queue.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct DownloadColumn {
    pub(crate) kind: DownloadColumnKind,
    pub(crate) width: f32,
    /// 用户开关（右上角列设置）；是否显示只由它决定，超宽走横向滚动。
    pub(crate) visible: bool,
}

impl DownloadColumn {
    fn new(kind: DownloadColumnKind) -> Self {
        Self {
            kind,
            width: kind.default_width(),
            visible: kind.default_visible(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct DraggedColumnMenuItem {
    pub(crate) kind: DownloadColumnKind,
    pub(crate) label: SharedString,
}

impl Render for DraggedColumnMenuItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        h_flex()
            .px(tokens.spacing.md)
            .py(tokens.spacing.xs)
            .gap(tokens.spacing.sm)
            .rounded(tokens.radius.sm)
            .border(theme.extended().stroke.thin)
            .border_color(tokens.colors.border)
            .bg(tokens.colors.surface)
            .shadow(tokens.shadow.sm.clone())
            .text_size(tokens.typography.sm.size)
            .line_height(tokens.typography.sm.line_height)
            .text_color(tokens.colors.surface_foreground)
            .child(Icon::new(FluxIcon::GripVertical).size(theme.extended().icon.md))
            .child(self.label.clone())
    }
}

/// 表格可见行：任务行、任务组文件夹行（及其展开的成员行 / 目录行）或分组方式的分桶头。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum VisibleRow {
    Task(RowId),
    /// 任务组文件夹行（`DownloadTableDelegate::group_rows` 下标）。
    Group(usize),
    /// 展开的组内成员；`depth` 1 = 组根目录，2 = 位于某个目录行之下。
    Member {
        id: RowId,
        depth: u8,
    },
    /// 展开的组内目录分段行（`group` 为所属组行下标）。
    GroupDir {
        group: usize,
        path: String,
        count: usize,
        size_bytes: u64,
        collapsed: bool,
    },
    GroupHeader {
        key: String,
        label: SharedString,
        count: usize,
        collapsed: bool,
    },
}

/// 分组计算结果。
struct GroupBucket {
    key: String,
    label: SharedString,
    order: i64,
}

/// 自上次重算以来，影响可见行的变化范围。
enum ViewChange {
    /// 没有影响排序 / 筛选 / 分组的变化：可见行与选中集都无需重算。
    Unchanged,
    /// 只有这些本地行变了（下标为当前布局）、没有行被移除；排序已增量并入。选中集只需复核它们。
    Rows(Vec<usize>),
    /// 视图参数变了 / 有行被移除 / 整表重排：筛选结果与选中集都要整体复核。
    Everything,
}

fn row_at<'a>(
    local: &'a [DownloadTaskView],
    remote: &'a [DownloadTaskView],
    id: RowId,
) -> Option<&'a DownloadTaskView> {
    match id {
        RowId::Local(ix) => local.get(ix),
        RowId::Remote(ix) => remote.get(ix),
    }
}

/// 行的稳定平局序：本地行在前、各自按下标。
fn row_slot(id: RowId) -> (bool, usize) {
    match id {
        RowId::Local(ix) => (false, ix),
        RowId::Remote(ix) => (true, ix),
    }
}

/// 排序比较（全序：比较器以任务 key 收尾，key 相同时按行下标定序，与稳定排序结果一致）。
fn compare_rows(
    prefs: &ViewPrefs,
    local: &[DownloadTaskView],
    remote: &[DownloadTaskView],
    left: RowId,
    right: RowId,
) -> Ordering {
    match (row_at(local, remote, left), row_at(local, remote, right)) {
        (Some(left_row), Some(right_row)) => prefs
            .compare(left_row, right_row)
            .then_with(|| row_slot(left).cmp(&row_slot(right))),
        _ => row_slot(left).cmp(&row_slot(right)),
    }
}

/// 按 key 分桶：桶顺序为 key 首次出现的顺序，桶元数据取首个成员的；用 key → 下标的
/// 哈希表定位，分桶数很多时（如按站点分组）仍是线性。
fn group_by_key<B, V>(
    items: impl IntoIterator<Item = (B, V)>,
    key_of: impl Fn(&B) -> &str,
) -> Vec<(B, Vec<V>)> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut groups: Vec<(B, Vec<V>)> = Vec::new();
    for (bucket, value) in items {
        match index.get(key_of(&bucket)) {
            Some(&ix) => groups[ix].1.push(value),
            None => {
                index.insert(key_of(&bucket).to_owned(), groups.len());
                groups.push((bucket, vec![value]));
            }
        }
    }
    groups
}

/// 侧栏选中项到表格筛选的投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TableFilter {
    Download(DownloadFilter),
    Queue(String),
    Device(String),
    Group(String),
}

impl From<&SidebarSelection> for TableFilter {
    fn from(selection: &SidebarSelection) -> Self {
        match selection {
            SidebarSelection::Download(filter) => Self::Download(filter.clone()),
            SidebarSelection::Queue(id) => Self::Queue(id.clone()),
            SidebarSelection::Device(id) => Self::Device(id.clone()),
        }
    }
}

/// 右键菜单单任务事实：足以判定菜单项可见性的最小状态快照，与
/// [`DownloadTaskView`] 解耦以便纯函数测试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TaskMenuFacts {
    pub(crate) is_local: bool,
    /// 任务可被控制（暂停 / 继续 / 删除）。远程任务云端状态未知时为 `false`。
    pub(crate) controllable: bool,
    pub(crate) state: TaskState,
    /// 引擎原始 status 为 5（准备中，展示态归入 `Pending`）：引擎视同下载中。
    pub(crate) preparing: bool,
    pub(crate) boosted: bool,
    /// `url` 是否为 BT 哨兵值 `torrent-file://…`（不可重新下载）。
    pub(crate) is_torrent_sentinel: bool,
    /// BT 任务（`magnet:` 链接或 `torrent-file://` 哨兵）：不支持更换链接 / 修改线程数。
    pub(crate) is_bt: bool,
    /// BT 选片：按顺序下载。仅本地 BT 行有意义；远程 / 非 BT 为 false。
    pub(crate) bt_sequential: bool,
    /// BT 选片：先下载每个选中文件的首尾块。
    pub(crate) bt_first_last_pieces_first: bool,
    /// `url` 是 http(s) / ftp 链接（引擎 `change_task_url` 仅接受这些源）。
    pub(crate) url_replaceable: bool,
    /// 失败态、错误消息带插件重试前缀且非任务组成员（组成员的原始链接是分享页，daemon 拒绝忽略重试）。
    pub(crate) is_plugin_retry_error: bool,
    /// 已完成但产物已不在下载目录（文件跟踪扫描结果）。
    pub(crate) file_missing: bool,
    /// 本地做种中（已完成且 seeding_status 1 / 8）：可暂停（停止做种）。
    pub(crate) is_seeding: bool,
    /// 已完成且做种已停止 / 达标（seeding_status 2..=7）、产物仍在：可继续（恢复做种）。
    pub(crate) can_resume_seeding: bool,
}

/// 可「忽略插件重试」的插件失败错误消息前缀（引擎 `handle_resolve_ready`；二段解析任务
/// 失败用 `[解析插件]`，不匹配本前缀，daemon 也拒绝对其忽略插件）。
pub(crate) const PLUGIN_ERROR_PREFIX: &str = "[插件]";

/// `url` 以任一 scheme 前缀开头（ASCII 大小写不敏感，不分配）。
pub(crate) fn url_has_prefix(url: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| {
        url.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    })
}

/// BT 任务源：`magnet:` 链接或 `torrent-file://` 本地种子哨兵。
fn is_bt_url(url: &str) -> bool {
    url_has_prefix(url, &["magnet:", "torrent-file://"])
}

/// 引擎 `change_task_url` 接受的源：http(s) / ftp。
fn is_replaceable_url(url: &str) -> bool {
    url_has_prefix(url, &["http://", "https://", "ftp://"])
}

impl TaskMenuFacts {
    /// 「更换链接」可用：本地任务，状态为暂停 / 失败 / 等待（引擎拒绝下载中与已完成），
    /// 且源为 http(s) / ftp。
    pub(crate) fn can_change_url(&self) -> bool {
        self.is_local
            && self.url_replaceable
            && matches!(
                self.state,
                TaskState::Paused | TaskState::Failed | TaskState::Pending
            )
    }

    /// 「重命名」可用：本地非 BT、不在下载 / 准备中的任务（对齐引擎 `rename_task`：
    /// 活跃任务返回 `task-active`；BT 落盘名由完成布局决定且做种期间持有句柄，返回
    /// `bt-unsupported`）。eD2K 与 HTTP 同样以 `<名>.fdownloading` 续传，可改名。
    pub(crate) fn can_rename(&self) -> bool {
        self.is_local && !self.is_bt && self.state != TaskState::Downloading && !self.preparing
    }

    /// 「修改线程数」可用：本地非 BT、未完成的任务（下载中也可，引擎静默暂停后恢复）。
    pub(crate) fn can_change_segments(&self) -> bool {
        self.is_local && !self.is_bt && self.state != TaskState::Completed
    }

    /// 「完整性校验」可用：本地已完成、产物仍在的 BT 任务（按种子 piece 表重哈希）。
    pub(crate) fn can_verify_integrity(&self) -> bool {
        self.is_local && self.is_bt && self.state == TaskState::Completed && !self.file_missing
    }

    /// 「复制磁力链接 / 导出 .torrent」可用：本机 BT 任务（任意状态）。
    pub(crate) fn can_share_torrent(&self) -> bool {
        self.is_local && self.is_bt
    }

    /// 本地已完成且产物仍在下载目录：「打开文件」「复制文件」的共同前提。
    pub(crate) fn has_product_on_disk(&self) -> bool {
        self.is_local && self.state == TaskState::Completed && !self.file_missing
    }
}

/// 由任务行计算菜单事实：右键菜单、应用菜单与详情面板按钮共用同一事实源。
pub(crate) fn task_menu_facts_for(store: &TaskStore, key: &RowKey) -> Option<TaskMenuFacts> {
    let row = store.get(key)?;
    Some(TaskMenuFacts {
        is_local: key.is_local(),
        controllable: row.remote_status != Some(RemoteTaskStatus::Unknown),
        state: row.state,
        preparing: row.preparing,
        boosted: row.boosted,
        is_torrent_sentinel: row.url.starts_with("torrent-file://"),
        is_bt: is_bt_url(&row.url),
        url_replaceable: is_replaceable_url(&row.url),
        is_plugin_retry_error: row.state == TaskState::Failed
            && row.group_id.is_empty()
            && row.error_message.starts_with(PLUGIN_ERROR_PREFIX),
        file_missing: row.is_file_missing(),
        is_seeding: row.is_seeding(),
        can_resume_seeding: row.can_resume_seeding(),
        bt_sequential: row.bt_sequential,
        bt_first_last_pieces_first: row.bt_first_last_pieces_first,
    })
}

/// 任务右键菜单条目；数组顺序即渲染顺序。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuEntry {
    Resume,
    Pause,
    IgnorePluginRetry,
    Boost,
    CancelBoost,
    OpenFile,
    OpenFolder,
    /// 把产物以文件写入系统剪贴板（条件同 [`MenuEntry::OpenFile`]，可多选）。
    CopyFile,
    Rename,
    /// 更换下载链接（单选；条件见 [`TaskMenuFacts::can_change_url`]）。
    ChangeUrl,
    /// 修改线程数（单选；条件见 [`TaskMenuFacts::can_change_segments`]）。
    ChangeSegments,
    /// 完整性校验（单选；条件见 [`TaskMenuFacts::can_verify_integrity`]）。
    VerifyIntegrity,
    /// 最稀有优先（本地未完成 BT；可多选。选中态见 [`piece_picking_order`]）。
    RarestFirst,
    /// 按顺序下载（条件同 [`MenuEntry::RarestFirst`]；与最稀有优先互斥）。
    SequentialDownload,
    /// 先下载首尾文件块（条件同 [`MenuEntry::RarestFirst`]；可与任一顺序组合）。
    FirstLastPiecesFirst,
    /// 复制磁力链接（单选；条件见 [`TaskMenuFacts::can_share_torrent`]）。
    CopyMagnet,
    /// 导出 .torrent 文件（单选；条件同 [`MenuEntry::CopyMagnet`]）。
    ExportTorrent,
    Redownload,
    CopyUrl,
    MoveToQueue,
    /// 以当前选区进入显式多选模式（行首复选框常显、表头换选择条）。
    Select,
    SelectAll,
    Separator,
    Delete,
    DeleteWithFiles,
    /// 在主窗口停靠详情面板中查看（独立窗口经面板头部「在独立窗口打开」）。
    ShowDetail,
    /// 插件贡献的命令（下标对应 [`DownloadView`] 上的插件命令列表）。
    Plugin(u16),
}

/// 纯函数：按选中任务集合的事实计算应显示的右键菜单项。
///
/// 多选时每一项都要求对**全部**选中任务成立（交集语义），resume/pause 例外
/// ——只要选中集合中**存在**一个可恢复/可暂停的任务就显示（与工具栏按钮行为
/// 一致：命令按 `task_command` 逐项生成，不适用状态的任务（如已完成任务的「继续」）被跳过）。
pub(crate) fn context_menu_items(selection: &[TaskMenuFacts]) -> Vec<MenuEntry> {
    if selection.is_empty() {
        return Vec::new();
    }
    let mut items = Vec::new();
    if selection.iter().any(|task| {
        task.controllable
            && (task.can_resume_seeding
                || !matches!(task.state, TaskState::Completed | TaskState::Downloading))
    }) {
        items.push(MenuEntry::Resume);
    }
    // 失败的本机任务也可暂停：撤销已排程的自动重试，任务落为已暂停（#947）。
    if selection.iter().any(|task| {
        task.controllable
            && (task.is_seeding
                || matches!(task.state, TaskState::Downloading | TaskState::Pending)
                || (task.is_local && task.state == TaskState::Failed))
    }) {
        items.push(MenuEntry::Pause);
    }
    if let [only] = selection {
        if only.state == TaskState::Failed && only.is_plugin_retry_error {
            items.push(MenuEntry::IgnorePluginRetry);
        }
        if only.is_local {
            // 已完成 / 失败的任务不会处于 Boost；残留标记也只显示「优先下载」，点击给出提示。
            let ended = matches!(only.state, TaskState::Completed | TaskState::Failed);
            items.push(if only.boosted && !ended {
                MenuEntry::CancelBoost
            } else {
                MenuEntry::Boost
            });
        }
    }
    if selection.iter().all(TaskMenuFacts::has_product_on_disk) {
        items.push(MenuEntry::OpenFile);
    }
    if selection.iter().all(|task| task.is_local) {
        items.push(MenuEntry::OpenFolder);
    }
    // 产物都在磁盘上才能整体放进剪贴板（agent 遇到任一缺失即整体拒绝）。
    if selection.iter().all(TaskMenuFacts::has_product_on_disk) {
        items.push(MenuEntry::CopyFile);
    }
    if let [only] = selection
        && only.can_rename()
    {
        items.push(MenuEntry::Rename);
    }
    if let [only] = selection {
        if only.can_change_url() {
            items.push(MenuEntry::ChangeUrl);
        }
        if only.can_change_segments() {
            items.push(MenuEntry::ChangeSegments);
        }
        if only.can_verify_integrity() {
            items.push(MenuEntry::VerifyIntegrity);
        }
    }
    if piece_picking_visible(selection) {
        items.push(MenuEntry::RarestFirst);
        items.push(MenuEntry::SequentialDownload);
        items.push(MenuEntry::FirstLastPiecesFirst);
    }
    if selection.iter().all(|task| {
        task.is_local
            && matches!(task.state, TaskState::Completed | TaskState::Failed)
            && !task.is_torrent_sentinel
    }) {
        items.push(MenuEntry::Redownload);
    }
    items.push(MenuEntry::CopyUrl);
    if let [only] = selection
        && only.can_share_torrent()
    {
        items.push(MenuEntry::CopyMagnet);
        items.push(MenuEntry::ExportTorrent);
    }
    if selection.iter().all(|task| task.is_local) {
        items.push(MenuEntry::MoveToQueue);
    }
    items.push(MenuEntry::Separator);
    if selection.iter().any(|task| task.controllable) {
        items.push(MenuEntry::Delete);
        items.push(MenuEntry::DeleteWithFiles);
    }
    if selection.iter().any(|task| task.is_local) {
        items.push(MenuEntry::ShowDetail);
    }
    items
}

/// 右键菜单完整条目：未处于显式多选模式时，在删除组前插入「选择 / 全选」作为
/// 多选模式的鼠标入口（已在多选模式时表头选择条与复选框已可见，不再重复）。
pub(crate) fn task_menu_entries(
    selection: &[TaskMenuFacts],
    in_multi_select: bool,
    plugin_commands: usize,
) -> Vec<MenuEntry> {
    let mut items = context_menu_items(selection);
    if items.is_empty() {
        return items;
    }
    // 插件命令自成一组，位于多选分组与删除分组之间。
    if plugin_commands > 0 {
        let at = separator_index(&items);
        let count = u16::try_from(plugin_commands).unwrap_or(u16::MAX);
        items.splice(
            at..at,
            std::iter::once(MenuEntry::Separator).chain((0..count).map(MenuEntry::Plugin)),
        );
    }
    if in_multi_select {
        return items;
    }
    let at = separator_index(&items);
    items.splice(
        at..at,
        [
            MenuEntry::Separator,
            MenuEntry::Select,
            MenuEntry::SelectAll,
        ],
    );
    items
}

fn separator_index(items: &[MenuEntry]) -> usize {
    items
        .iter()
        .position(|entry| *entry == MenuEntry::Separator)
        .unwrap_or(items.len())
}

/// 选区全体都是本机、未完成的 BT 任务时才显示选片项（交集语义，可多选）。
pub(crate) fn piece_picking_visible(selection: &[TaskMenuFacts]) -> bool {
    !selection.is_empty()
        && selection
            .iter()
            .all(|task| task.is_local && task.is_bt && task.state != TaskState::Completed)
}

/// 选片顺序：全体一致才点亮对应单选；混合或不可见选区为 [`PiecePickingOrder::Mixed`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PiecePickingOrder {
    /// 全体 `bt_sequential == false`（缺省：最稀有优先）。
    Rarest,
    /// 全体 `bt_sequential == true`。
    Sequential,
    /// 选区有的开、有的关，或选区不满足显示条件（两个单选都不选）。
    Mixed,
}

/// 选区的下载顺序。空选区与不可见选区视为混合（不下发、不点亮）。
pub(crate) fn piece_picking_order(selection: &[TaskMenuFacts]) -> PiecePickingOrder {
    if !piece_picking_visible(selection) {
        return PiecePickingOrder::Mixed;
    }
    if selection.iter().all(|task| task.bt_sequential) {
        PiecePickingOrder::Sequential
    } else if selection.iter().all(|task| !task.bt_sequential) {
        PiecePickingOrder::Rarest
    } else {
        PiecePickingOrder::Mixed
    }
}

/// 把顺序设为 `sequential` 时需要下发的任务下标。已是该状态的跳过；
/// 选区不可见或全体已是目标值时为空（不下发）。只改 `sequential` 字段。
pub(crate) fn piece_picking_order_targets(
    selection: &[TaskMenuFacts],
    sequential: bool,
) -> Vec<usize> {
    if !piece_picking_visible(selection) {
        return Vec::new();
    }
    selection
        .iter()
        .enumerate()
        .filter(|(_, task)| task.bt_sequential != sequential)
        .map(|(index, _)| index)
        .collect()
}

/// 「先下载首尾文件块」勾选态：选区里该项全部为开才勾上（混合视为未勾）。
pub(crate) fn piece_picking_checked(selection: &[TaskMenuFacts]) -> bool {
    !selection.is_empty() && selection.iter().all(|task| task.bt_first_last_pieces_first)
}

/// 点击首尾勾选后应写入的值：`!all_on`。选区不满足显示条件时为 `None`（不下发）。
pub(crate) fn piece_picking_toggle_target(selection: &[TaskMenuFacts]) -> Option<bool> {
    piece_picking_visible(selection).then(|| !piece_picking_checked(selection))
}

/// 分组头菜单项的外观：普通图标项，或破坏性项（图标与文字取主题 `destructive` 色）。
fn group_menu_base(label: SharedString, icon: FluxIcon, tint: Option<Hsla>) -> PopupMenuItem {
    match tint {
        Some(color) => {
            PopupMenuItem::element(move |_, _| div().text_color(color).child(label.clone()))
                .icon(Icon::from(icon).text_color(color))
        }
        None => PopupMenuItem::new(label).icon(icon),
    }
}

/// 构造一个分组头右键菜单项：点击时把 `group_id` 回调进 [`DownloadView`] 的方法
/// （无需 `Window`，用于批量命令类操作）。`tint` 为 `Some` 时按破坏性项着色。
fn group_menu_item(
    label: SharedString,
    icon: FluxIcon,
    tint: Option<Hsla>,
    host: &WeakEntity<DownloadView>,
    group_id: &str,
    action: fn(&mut DownloadView, String, &mut Context<DownloadView>),
) -> PopupMenuItem {
    let host = host.clone();
    let group_id = group_id.to_owned();
    group_menu_base(label, icon, tint).on_click(move |_, _, cx| {
        let group_id = group_id.clone();

        let Ok(()) = host.update(cx, |view, cx| action(view, group_id, cx)) else {
            // 视图已释放，结束这次回调而不再更新状态。
            return;
        };
    })
}

/// 同 [`group_menu_item`]，但回调需要 `Window`（打开确认对话框 / 独立窗口）。
fn group_menu_item_windowed(
    label: SharedString,
    icon: FluxIcon,
    tint: Option<Hsla>,
    host: &WeakEntity<DownloadView>,
    group_id: &str,
    action: fn(&mut DownloadView, String, &mut Window, &mut Context<DownloadView>),
) -> PopupMenuItem {
    let host = host.clone();
    let group_id = group_id.to_owned();
    group_menu_base(label, icon, tint).on_click(move |_, window, cx| {
        let group_id = group_id.clone();

        let Ok(()) = host.update(cx, |view, cx| action(view, group_id, window, cx)) else {
            // 视图已释放，结束这次回调而不再更新状态。
            return;
        };
    })
}

pub(crate) struct DownloadTableDelegate {
    pub(crate) strings: DownloadStrings,
    pub(crate) columns: Vec<DownloadColumn>,
    store: Rc<TaskStore>,
    categories: Rc<CategoryIndex>,
    visible: Vec<VisibleRow>,
    seen_generation: u64,
    /// 上次重算时存储的结构计数；未变说明只有行内容变化，可沿用上次的行顺序。
    seen_structure: u64,
    /// 全部行（本地 + 远程）按 `sorted_for` 排好的顺序，筛选 / 搜索之前，不是 RowOrder 保持期内
    /// 显示的顺序。排序键没变时随存储变更日志增量维护，不随事件整表重排。
    sorted_rows: Vec<RowId>,
    sorted_for: Option<(ViewSortKey, SortDir)>,
    /// `sorted_rows` 已追平到的存储变更日志位置，及当时的本地行数（重放日志需要变更前的布局）。
    sorted_log_pos: u64,
    sorted_local_len: usize,
    sorted_group_date: Option<chrono::NaiveDate>,
    #[cfg(test)]
    force_full_sort: bool,
    #[cfg(test)]
    full_sort_count: usize,
    /// 整表重排（而非增量重放）的次数。
    #[cfg(test)]
    sort_all_count: usize,
    view_dirty: bool,
    /// 行顺序稳定器：指针活动期间 / 动态排序键限频内推迟内容变化引起的重排。
    row_order: RowOrder,
    /// 被推迟的重排的补偿定时器（到期后强制按最新排序重排）。
    reorder_timer: Option<Task<()>>,
    selected_tasks: HashSet<RowKey>,
    /// 正在构建的任务 / 组行：`render_tr` 记下，同一行随后的 `render_last_empty_col` 取走并
    /// 在右侧留白上挂行悬停操作（留白始终贴可视区右缘，横向滚动时按钮不随末列移出视口）。
    trailing_actions: Option<groups::Entity>,
    selection_anchor: Option<RowKey>,
    /// 显式多选模式：Cmd/Ctrl+点击、全选或划选多行进入，普通单击 / 清空选择退出；
    /// 行首复选框只在该模式且有选中项时显示（与 Web `multiSelect` 对齐）。
    multi_select: bool,
    /// 左键按住拖过任务行时的划选；`None` 表示没有进行中的手势。
    row_sweep: Option<RowSweep>,
    /// 这次左键是否按在文件拖出把手上。行在捕获阶段清掉，把手在捕获阶段置位，
    /// 行冒泡阶段的 [`Self::begin_row_sweep`] 读完即清。父捕获早于子捕获，所以不会
    /// 把同一次按下的置位清掉；没被读走的残留由下一次按下的行捕获清掉。
    press_on_file_handle: bool,
    /// 划选已经改过选区，或文件拖放仍在进行：随后落到同一行的单击不再覆盖成单选。
    suppress_sweep_click: bool,
    /// 已提交删除、尚未从列表消失的本地任务 id：行渲染时变淡，且不可再被选中 / 批量操作。
    pending_delete: HashSet<String>,
    filter: TableFilter,
    query: String,
    prefs: ViewPrefs,
    /// 设置「隐藏任务来源」：文件名列副标题不再附来源站点（设备本地偏好，
    /// 见 [`crate::model::view_prefs::HIDE_TASK_SOURCE_PREF`]）。
    hide_source: bool,
    /// queue_id → 名称（列 / 分组标签）。
    queue_names: Vec<(String, String)>,
    /// group_id → 组元信息；为空时不折叠任务组（如组详情窗口的成员表）。
    groups: HashMap<String, GroupMeta>,
    /// 本次可见行里的任务组文件夹行（`VisibleRow::Group` 的下标对象）。
    group_rows: Vec<GroupRow>,
    /// 运行期展开的任务组（默认折叠，不持久化）。
    expanded_groups: HashSet<String>,
    /// 运行期折叠的组内目录：group_id → 相对目录集合（默认展开）。
    collapsed_dirs: HashMap<String, HashSet<String>>,
    /// 经组行单击选中的组：选区恰为该组成员时停靠面板显示组概览。
    focused_group: Option<String>,
    /// 远程任务目标设备显示名：设备 id / 指纹 → 已消歧的名称。
    device_names: HashMap<String, String>,
    /// 表格容器宽（`render_download_table` 在 prepaint 测得、帧外写入）；0 = 尚未测得。
    viewport_width: f32,
    /// 最近一次交给表格的文件名列宽（`column()` 写入）；与期望值不同才需 refresh。
    applied_file_width: Cell<f32>,
    /// 最近一次交给表格的文字缩放（界面缩放 × 字号缩放，`column()` 写入）：
    /// 列的最小宽 / 默认宽按它放大，与当前主题不同即需 refresh 重建列。
    text_scale: Cell<f32>,
    /// 最近一次交给表格的主题 `icon.lg`（`column()` 写入）：选择列宽按它与密度推算，
    /// 与当前主题不同即需 refresh 重建列。
    icon_base: Cell<f32>,
    /// 列配置（宽度 / 可见性 / 顺序 / 排序指示）需要重建表头 `col_groups`。
    /// 纯行数据变化不置位：`TableState::refresh` 会用代理宽度覆盖表格内部
    /// 实时宽度，进度节拍若每次都刷新会把用户正在拖拽的列宽弹回去。
    columns_dirty: bool,
    /// 上级页面弱引用：右键菜单需要参数的命令（移动到队列 / 忽略插件重试 /
    /// 任务组操作）经它回调 `DownloadView` 的方法执行；`None` 时这些项不渲染。
    host: Option<WeakEntity<DownloadView>>,
    /// 插件贡献的任务右键命令。
    plugin_commands: Vec<PluginTaskCommand>,
    /// 右键菜单动作分派目标；`None` 时菜单项不带 action_context（仍可点击，
    /// 只是动作从菜单自身的焦点路径冒泡）。
    action_context: Option<FocusHandle>,
}

impl DownloadTableDelegate {
    pub(crate) fn new(strings: DownloadStrings, store: Rc<TaskStore>) -> Self {
        Self {
            strings,
            columns: DownloadColumnKind::ALL
                .into_iter()
                .map(DownloadColumn::new)
                .collect(),
            store,
            categories: Rc::new(CategoryIndex::default()),
            visible: Vec::new(),
            seen_generation: u64::MAX,
            seen_structure: u64::MAX,
            sorted_rows: Vec::new(),
            sorted_for: None,
            sorted_log_pos: u64::MAX,
            sorted_local_len: 0,
            sorted_group_date: None,
            #[cfg(test)]
            force_full_sort: false,
            #[cfg(test)]
            full_sort_count: 0,
            #[cfg(test)]
            sort_all_count: 0,
            view_dirty: true,
            row_order: RowOrder::default(),
            reorder_timer: None,
            selected_tasks: HashSet::new(),
            trailing_actions: None,
            selection_anchor: None,
            multi_select: false,
            row_sweep: None,
            press_on_file_handle: false,
            suppress_sweep_click: false,
            pending_delete: HashSet::new(),
            filter: TableFilter::Download(DownloadFilter::ALL),
            query: String::new(),
            prefs: ViewPrefs::default(),
            hide_source: false,
            queue_names: Vec::new(),
            groups: HashMap::new(),
            group_rows: Vec::new(),
            expanded_groups: HashSet::new(),
            collapsed_dirs: HashMap::new(),
            focused_group: None,
            device_names: HashMap::new(),
            viewport_width: 0.,
            applied_file_width: Cell::new(0.),
            text_scale: Cell::new(1.),
            icon_base: Cell::new(16.),
            columns_dirty: false,
            host: None,
            plugin_commands: Vec::new(),
            action_context: None,
        }
    }

    pub(crate) fn set_strings(&mut self, strings: DownloadStrings) {
        self.strings = strings;
        self.view_dirty = true;
    }

    /// 注入宿主弱引用；右键菜单中需要参数的命令经它回调
    /// [`DownloadView`] 的方法执行。
    pub(crate) fn set_host(&mut self, host: WeakEntity<DownloadView>) {
        self.host = Some(host);
    }

    /// 插件任务右键命令（下标即 `MenuEntry::Plugin`）。
    pub(crate) fn set_plugin_commands(&mut self, commands: Vec<PluginTaskCommand>) {
        self.plugin_commands = commands;
    }

    /// 注入右键菜单动作分派目标（根元素的 focus handle）。
    pub(crate) fn set_action_context(&mut self, handle: FocusHandle) {
        self.action_context = Some(handle);
    }

    pub(crate) fn set_categories(&mut self, categories: Rc<CategoryIndex>) {
        if !Rc::ptr_eq(&self.categories, &categories) {
            self.categories = categories;
            self.view_dirty = true;
        }
    }

    pub(crate) fn set_queue_names(&mut self, queues: Vec<(String, String)>) {
        if self.queue_names != queues {
            self.queue_names = queues;
            self.view_dirty = true;
        }
    }

    pub(crate) fn set_groups(&mut self, groups: HashMap<String, GroupMeta>) {
        if self.groups != groups {
            self.expanded_groups.retain(|id| groups.contains_key(id));
            self.collapsed_dirs.retain(|id, _| groups.contains_key(id));
            self.groups = groups;
            self.view_dirty = true;
        }
    }

    pub(crate) fn set_device_names(&mut self, names: HashMap<String, String>) {
        if self.device_names != names {
            self.device_names = names;
            self.view_dirty = true;
        }
    }

    pub(crate) fn set_filter(&mut self, filter: TableFilter) {
        if self.filter != filter {
            self.filter = filter;
            self.view_dirty = true;
        }
    }

    pub(crate) fn set_query(&mut self, query: &str) {
        let query = query.trim().to_lowercase();
        if self.query != query {
            self.query = query;
            self.view_dirty = true;
        }
    }

    pub(crate) fn prefs(&self) -> &ViewPrefs {
        &self.prefs
    }

    /// 替换视图偏好（密度 / 分组 / 排序 / 列 / 折叠）。
    pub(crate) fn set_prefs(&mut self, prefs: ViewPrefs) {
        if self.prefs == prefs {
            return;
        }
        self.apply_column_prefs(&prefs.columns);
        self.columns_dirty = true;
        self.prefs = prefs;
        self.view_dirty = true;
    }

    /// 同步「隐藏任务来源」偏好；返回是否变化（变化后行副标题要重画）。
    pub(crate) fn set_hide_source(&mut self, hide: bool) -> bool {
        std::mem::replace(&mut self.hide_source, hide) != hide
    }

    pub(crate) fn prefs_mut(&mut self) -> &mut ViewPrefs {
        self.view_dirty = true;
        &mut self.prefs
    }

    /// 列配置是否需要 `TableState::refresh`（读取后清零）。
    pub(crate) fn take_columns_dirty(&mut self) -> bool {
        std::mem::take(&mut self.columns_dirty)
    }

    /// 表头拖拽调宽结束后，把表格内部列宽（含首列勾选列）写回代理列配置，
    /// 作为后续刷新与持久化的事实源。返回是否有变化。
    ///
    /// 文件名列默认由容器宽回算（吸收剩余宽度）；表格给出的宽度与最近一次应用值
    /// 不同即说明用户拖过它，此后固定为 `prefs.file_name_width`，不再随容器伸缩。
    /// 其他列变宽后，下一帧 `render_download_table` 的 prepaint 发现未固定的
    /// 文件名列宽过期，帧外 refresh 补足。拖拽进行中代理宽度不变，因此不会触发
    /// refresh 把拖拽中的列宽弹回。
    pub(crate) fn sync_column_widths(&mut self, widths: &[Pixels]) -> bool {
        let applied_file_width = self.applied_file_width.get();
        let text_scale = self.text_scale.get();
        let mut file_name_width = None;
        let mut changed = false;
        let shown = self.columns.iter_mut().filter(|column| column.visible);
        for (column, width) in shown.zip(widths.iter().skip(1)) {
            let width = f32::from(*width);
            if column.kind == DownloadColumnKind::FileName {
                if (width - applied_file_width).abs() >= 0.5 {
                    file_name_width = Some(width.clamp(
                        DownloadColumnKind::FileName.min_width() * text_scale,
                        FILE_NAME_MAX_WIDTH,
                    ));
                }
                continue;
            }
            // 表格给出的是放大后的显示宽；与显示宽一致说明用户没拖过，不把放大值写成偏好。
            if (width - column.kind.display_width(column.width, text_scale)).abs() < 0.5 {
                continue;
            }
            let width = width.clamp(
                column.kind.min_width() * text_scale,
                MAX_COLUMN_WIDTH * text_scale,
            );
            if (column.width - width).abs() > f32::EPSILON {
                column.width = width;
                changed = true;
            }
        }
        if let Some(width) = file_name_width {
            self.prefs.file_name_width = Some(width);
            self.applied_file_width.set(width);
            changed = true;
        }
        changed
    }

    /// 容器宽为 `viewport` 时文件名列应得的宽度。用户拖过则取固定宽；否则
    /// `max(min_width, 容器宽 − 选择列 − 其他可见列 − 右侧留白 − 竖滚动条预留)`，
    /// 容器宽未知（≤ 0）时退回列配置宽度。
    fn file_name_width_for(&self, viewport: f32) -> f32 {
        let text_scale = self.text_scale.get();
        let min_width = DownloadColumnKind::FileName.min_width() * text_scale;
        if let Some(width) = self.prefs.file_name_width {
            return width.clamp(min_width, FILE_NAME_MAX_WIDTH);
        }
        if viewport <= 0. {
            return self
                .columns
                .iter()
                .find(|column| column.kind == DownloadColumnKind::FileName)
                .map_or(min_width, |column| column.width.max(min_width));
        }
        let others: f32 = self
            .columns
            .iter()
            .filter(|column| column.kind != DownloadColumnKind::FileName && column.visible)
            .map(|column| column.kind.display_width(column.width, text_scale))
            .sum();
        let reserved = self.selection_column_width()
            + others
            + TABLE_TRAILING_GUTTER
            + f32::from(Scrollbar::width());
        (viewport - reserved).max(min_width)
    }

    fn file_name_visible(&self) -> bool {
        self.columns
            .iter()
            .any(|column| column.kind == DownloadColumnKind::FileName && column.visible)
    }

    /// 文字缩放（主题的界面缩放 × 字号缩放）与上次交给表格的列不一致，需重建列。
    fn text_scale_stale(&self, text_scale: f32) -> bool {
        (self.text_scale.get() - text_scale).abs() > f32::EPSILON
    }

    /// 主题 `icon.lg` 与上次交给表格的列不一致（选择列宽随之变化），需重建列。
    fn icon_base_stale(&self, lg: f32) -> bool {
        (self.icon_base.get() - lg).abs() > f32::EPSILON
    }

    /// 当前密度下的选择列宽（px）；选择条等覆盖层据此对齐选择列右缘。
    pub(crate) fn selection_column_width(&self) -> f32 {
        selection_column_width_for(self.prefs.density, self.icon_base.get())
    }

    /// prepaint 判定：容器宽变化，或文件名列宽与期望不符（例如刚同步了其他列宽）。
    fn viewport_needs_sync(&self, viewport: f32) -> bool {
        (self.viewport_width - viewport).abs() >= 0.5
            || (self.file_name_visible()
                && (self.file_name_width_for(viewport) - self.applied_file_width.get()).abs()
                    >= 0.5)
    }

    /// 按当前容器宽计算文件名列宽并记为「已应用」（`column()` 构建表格列时调用）。
    fn apply_file_name_width(&self) -> f32 {
        let width = self.file_name_width_for(self.viewport_width);
        self.applied_file_width.set(width);
        width
    }

    /// 写入容器宽；返回表格是否需要 `refresh` 以应用新的文件名列宽。
    pub(crate) fn set_viewport_width(&mut self, viewport: f32) -> bool {
        self.viewport_width = viewport;
        self.file_name_visible()
            && (self.file_name_width_for(viewport) - self.applied_file_width.get()).abs() >= 0.5
    }

    /// 表头点击排序，三档循环：列默认方向 → 反方向 → 恢复默认排序（智能：添加顺序）。
    /// 返回偏好是否改变。
    fn toggle_sort(&mut self, kind: DownloadColumnKind) -> bool {
        let Some(key) = kind.sort_key() else {
            return false;
        };
        let default_dir = kind.default_sort_dir();
        if self.prefs.sort_key != key {
            self.prefs.sort_key = key;
            self.prefs.sort_dir = default_dir;
        } else if self.prefs.sort_dir == default_dir {
            self.prefs.sort_dir = match default_dir {
                SortDir::Asc => SortDir::Desc,
                SortDir::Desc => SortDir::Asc,
            };
        } else {
            self.prefs.sort_key = ViewSortKey::Smart;
            self.prefs.sort_dir = SortDir::default();
        }
        self.view_dirty = true;
        true
    }

    fn apply_column_prefs(&mut self, prefs: &[crate::model::view_prefs::ColumnPref]) {
        if prefs.is_empty() {
            return;
        }
        let mut columns: Vec<DownloadColumn> = prefs
            .iter()
            .filter_map(|pref| {
                let kind = DownloadColumnKind::from_key(&pref.key)?;
                let mut column = DownloadColumn::new(kind);
                column.visible = pref.visible;
                if pref.width >= kind.min_width() {
                    column.width = pref.width;
                }
                Some(column)
            })
            .collect();
        for kind in DownloadColumnKind::ALL {
            if !columns.iter().any(|column| column.kind == kind) {
                columns.push(DownloadColumn::new(kind));
            }
        }
        if !columns.iter().any(|column| column.visible) {
            columns[0].visible = true;
        }
        self.columns = columns;
    }

    /// 当前列配置 → 偏好条目。
    pub(crate) fn column_prefs(&self) -> Vec<crate::model::view_prefs::ColumnPref> {
        self.columns
            .iter()
            .map(|column| crate::model::view_prefs::ColumnPref {
                key: column.kind.key().to_owned(),
                visible: column.visible,
                width: column.width,
            })
            .collect()
    }

    /// 存储变化或视图参数变化时重算可见行。返回是否重算。
    pub(crate) fn refresh_view(&mut self) -> bool {
        self.refresh_view_at(Instant::now())
    }

    fn refresh_view_at(&mut self, now: Instant) -> bool {
        let generation = self.store.generation();
        if !self.view_dirty && generation == self.seen_generation {
            return false;
        }
        let structure = self.store.structure_generation();
        let dirty = self.view_dirty;
        let content_only = !dirty && structure == self.seen_structure;
        self.seen_generation = generation;
        self.seen_structure = structure;
        self.view_dirty = false;
        let store = Rc::clone(&self.store);
        #[cfg(test)]
        let force_full = self.force_full_sort;
        #[cfg(not(test))]
        let force_full = false;
        let change = self.sync_sorted(&store, dirty, force_full);
        let group_date =
            (self.prefs.group_by == ViewGroupBy::Date).then(|| chrono::Local::now().date_naive());
        let reuse = content_only
            && !force_full
            && matches!(change, ViewChange::Unchanged)
            && group_date == self.sorted_group_date;
        self.sorted_group_date = group_date;
        #[cfg(test)]
        {
            if !reuse {
                self.full_sort_count += 1;
            }
        }
        if reuse && !self.row_order.is_stale() {
            // 没有任何变化影响排序 / 筛选 / 分组（进度、速度等纯内容更新）：可见行与选中集都不变，
            // 只需维持保持期时钟，不再逐帧遍历全部可见行；组行的聚合值随成员内容刷新。
            self.row_order
                .settle_unchanged(self.prefs.sort_key.is_live(), now);
            self.refresh_group_rows();
            return true;
        }
        self.visible = self.compute_visible(content_only, now);
        self.revalidate_selection(&change);
        true
    }

    /// 选中集只保留当前筛选 + 搜索下仍在视图内的任务（已删除、被侧栏切换 / 搜索 /
    /// 状态变化筛掉的一律移出），选择条计数与快捷键批量动作都只作用于看得见的任务。
    /// 折叠分组内的任务仍属于当前视图，不因折叠而取消选中。只有部分行变了时只复核这些行。
    fn revalidate_selection(&mut self, change: &ViewChange) {
        let still_visible = |this: &Self, key: &RowKey| {
            !this.is_pending_delete(key)
                && this
                    .store
                    .get(key)
                    .is_some_and(|task| this.matches_filter(&task) && this.matches_query(&task))
        };
        match change {
            ViewChange::Unchanged => {}
            ViewChange::Rows(rows) => {
                if !self.selected_tasks.is_empty() {
                    let store = Rc::clone(&self.store);
                    let local = store.local();
                    for row in rows.iter().filter_map(|&ix| local.get(ix)) {
                        if self.selected_tasks.contains(&row.key) && !still_visible(self, &row.key)
                        {
                            self.selected_tasks.remove(&row.key);
                        }
                    }
                }
            }
            ViewChange::Everything => {
                let mut selected = std::mem::take(&mut self.selected_tasks);
                selected.retain(|key| still_visible(self, key));
                self.selected_tasks = selected;
            }
        }
        self.selection_anchor = self
            .selection_anchor
            .take()
            .filter(|key| self.selected_tasks.contains(key));
    }

    /// 把 `sorted_rows` 追平到存储当前状态：排序键没变且存储变更日志仍连续时只重放日志
    /// （变更行各做一次二分定位，加一趟线性搬移）；否则整表重排。
    fn sync_sorted(&mut self, store: &TaskStore, dirty: bool, force_full: bool) -> ViewChange {
        let local = store.local();
        let remote = store.remote();
        let end = store.change_log_end();
        let sort = (self.prefs.sort_key, self.prefs.sort_dir);
        let replayed = if force_full || self.sorted_for != Some(sort) {
            None
        } else {
            store
                .changes_since(self.sorted_log_pos)
                .and_then(|entries| self.replay_changes(&entries, &local, &remote))
        };
        let change = replayed.unwrap_or_else(|| {
            self.sort_all(&local, &remote);
            ViewChange::Everything
        });
        self.sorted_for = Some(sort);
        self.sorted_log_pos = end;
        self.sorted_local_len = local.len();
        if dirty {
            ViewChange::Everything
        } else {
            change
        }
    }

    fn sort_all(&mut self, local: &[DownloadTaskView], remote: &[DownloadTaskView]) {
        #[cfg(test)]
        {
            self.sort_all_count += 1;
        }
        self.sorted_rows.clear();
        self.sorted_rows.extend(
            (0..local.len())
                .map(RowId::Local)
                .chain((0..remote.len()).map(RowId::Remote)),
        );
        let prefs = &self.prefs;
        self.sorted_rows
            .sort_unstable_by(|&left, &right| compare_rows(prefs, local, remote, left, right));
    }

    /// 把存储变更日志重放到 `sorted_rows`；结果与整表重排相同。日志与布局对不上时返回
    /// `None`，由调用方整表重排。
    fn replay_changes(
        &mut self,
        entries: &[RowChange],
        local: &[DownloadTaskView],
        remote: &[DownloadTaskView],
    ) -> Option<ViewChange> {
        /// 重放里新追加、尚未进入 `sorted_rows` 的行。
        const FRESH: usize = usize::MAX;
        const KEEP: u8 = 0;
        const DROP: u8 = 1;
        const RENAME: u8 = 2;
        let old_len = self.sorted_local_len;
        let sort_bit = self.prefs.sort_key.change_bit();
        // 当前下标处的行在上次排序时的下标（`FRESH` = 其后新追加）；没记录的下标沿用原位。
        let mut origin: HashMap<usize, usize> = HashMap::new();
        let origin_of = |origin: &HashMap<usize, usize>, ix: usize| {
            origin
                .get(&ix)
                .copied()
                .unwrap_or(if ix < old_len { ix } else { FRESH })
        };
        let mut dropped = Vec::new();
        let mut resort = Vec::new();
        let mut touched = Vec::new();
        let (mut changed, mut removed) = (false, false);
        for &entry in entries {
            match entry {
                RowChange::Updated { ix, view, sort } => {
                    let moves = sort & sort_bit != 0;
                    if !view && !moves {
                        continue;
                    }
                    changed = true;
                    touched.push(ix);
                    if moves {
                        let was = origin_of(&origin, ix);
                        if was != FRESH {
                            resort.push(was);
                        }
                    }
                }
                RowChange::Appended(ix) => {
                    changed = true;
                    origin.insert(ix, FRESH);
                    touched.push(ix);
                }
                RowChange::Removed { ix, last } => {
                    changed = true;
                    removed = true;
                    let gone = origin_of(&origin, ix);
                    if gone != FRESH {
                        dropped.push(gone);
                    }
                    if ix == last {
                        origin.remove(&ix);
                    } else {
                        let moved = origin_of(&origin, last);
                        origin.insert(ix, moved);
                        origin.remove(&last);
                    }
                }
            }
        }
        if !changed {
            return Some(ViewChange::Unchanged);
        }
        let mut flags = vec![KEEP; old_len];
        let mut renames: HashMap<usize, usize> = HashMap::new();
        let mut fresh: Vec<RowId> = Vec::new();
        for (&now_at, &was) in &origin {
            if was == FRESH {
                fresh.push(RowId::Local(now_at));
            } else if was != now_at {
                renames.insert(was, now_at);
                if let Some(flag) = flags.get_mut(was) {
                    *flag = RENAME;
                }
            }
        }
        for &was in &dropped {
            if let Some(flag) = flags.get_mut(was) {
                *flag = DROP;
            }
        }
        // 排序值变了的行：从旧位置摘下，按新值（及搬迁后的新下标）重新插入。
        for &was in &resort {
            if let Some(flag) = flags.get_mut(was)
                && *flag != DROP
            {
                *flag = DROP;
                fresh.push(RowId::Local(renames.get(&was).copied().unwrap_or(was)));
            }
        }
        if flags.iter().any(|flag| *flag != KEEP) {
            self.sorted_rows.retain_mut(|id| {
                let RowId::Local(ix) = id else {
                    return true;
                };
                match flags.get(*ix).copied() {
                    Some(DROP) => false,
                    Some(RENAME) => {
                        if let Some(&moved_to) = renames.get(&*ix) {
                            *ix = moved_to;
                        }
                        true
                    }
                    _ => true,
                }
            });
        }
        if !fresh.is_empty() {
            self.merge_fresh(fresh, local, remote);
        }
        if self.sorted_rows.len() != local.len() + remote.len() {
            return None;
        }
        Some(if removed {
            ViewChange::Everything
        } else {
            ViewChange::Rows(touched)
        })
    }

    /// 把一批行按排序并入已排好的 `sorted_rows`：先把它们排好，再每行二分定位、整段搬移。
    fn merge_fresh(
        &mut self,
        mut fresh: Vec<RowId>,
        local: &[DownloadTaskView],
        remote: &[DownloadTaskView],
    ) {
        let prefs = &self.prefs;
        fresh.sort_unstable_by(|&left, &right| compare_rows(prefs, local, remote, left, right));
        let base = std::mem::take(&mut self.sorted_rows);
        let mut merged = Vec::with_capacity(base.len() + fresh.len());
        let mut start = 0;
        for id in fresh {
            let at = start
                + base[start..].partition_point(|&probe| {
                    compare_rows(prefs, local, remote, probe, id) == Ordering::Less
                });
            merged.extend_from_slice(&base[start..at]);
            merged.push(id);
            start = at;
        }
        merged.extend_from_slice(&base[start..]);
        self.sorted_rows = merged;
    }

    /// 本地日期跨过零点：按日期分组时「今天 / 昨天」等分桶随之改变，强制重算可见行。
    /// 返回是否需要重绘；不按日期分组时不做任何事。
    pub(crate) fn refresh_for_new_day(&mut self) -> bool {
        if self.prefs.group_by != ViewGroupBy::Date {
            return false;
        }
        self.view_dirty = true;
        self.refresh_view()
    }

    /// 表格内指针活动（移动 / 滚动 / 按下）的记录句柄：推迟由行内容变化引起的重排，
    /// 避免行在光标下跳走。
    pub(crate) fn interaction_hold(&self) -> InteractionHold {
        self.row_order.interaction_hold()
    }

    fn reorder_deadline(&self) -> Option<Instant> {
        self.row_order.deadline(self.prefs.sort_key.is_live())
    }

    fn needs_reorder_timer(&self) -> bool {
        self.reorder_timer.is_none() && self.reorder_deadline().is_some()
    }

    /// 搜索命中：任务名 / 链接 / 站点，或任务所在任务组的显示名（命中组名时整组可见）。
    fn matches_query(&self, task: &DownloadTaskView) -> bool {
        if self.query.is_empty() {
            return true;
        }
        task.name_fold.contains(&self.query)
            || task.url_fold.contains(&self.query)
            || task.site_fold.contains(&self.query)
            || self.group_name_matches(task)
    }

    fn matches_filter(&self, task: &DownloadTaskView) -> bool {
        match &self.filter {
            TableFilter::Download(filter) => filter.matches(task, &self.categories),
            TableFilter::Queue(queue_id) => {
                task.source == TaskSource::Local && task.queue_id == *queue_id
            }
            TableFilter::Device(device) => SidebarSelection::device_matches(device, task),
            TableFilter::Group(group_id) => task.group_id == *group_id,
        }
    }

    /// 按当前排序顺序（`sorted_rows`）取出通过筛选 / 搜索的行；`pick` 决定随行带出什么（分组需要
    /// 行本身，不分组只需要下标）。
    fn filtered_rows<'a, T: Copy>(
        &self,
        local: &'a [DownloadTaskView],
        remote: &'a [DownloadTaskView],
        pick: impl Fn(&'a DownloadTaskView) -> T,
    ) -> Vec<(RowId, T)> {
        self.sorted_rows
            .iter()
            .filter_map(|&id| row_at(local, remote, id).map(|task| (id, task)))
            .filter(|(_, task)| self.matches_filter(task) && self.matches_query(task))
            .map(|(id, task)| (id, pick(task)))
            .collect()
    }

    fn compute_visible(&mut self, content_only: bool, now: Instant) -> Vec<VisibleRow> {
        let store = Rc::clone(&self.store);
        let local = store.local();
        let remote = store.remote();
        // 沿用上次可见行的缓冲，大表重算不再为它重新分配。
        let mut visible = std::mem::take(&mut self.visible);
        visible.clear();
        // 即使排序输入不变，也照常应用保持期：过期顺序和动态键的重排时钟必须一致。
        let live_key = self.prefs.sort_key.is_live();

        if self.prefs.group_by == ViewGroupBy::None && self.groups.is_empty() {
            self.group_rows.clear();
            // 「全部」且无搜索：每行都通过，不必逐行读取行内容（大表里是随机内存访问）。
            let rows = if self.query.is_empty()
                && self.filter == TableFilter::Download(DownloadFilter::ALL)
            {
                self.sorted_rows.iter().map(|&id| (id, ())).collect()
            } else {
                self.filtered_rows(&local, &remote, |_| ())
            };
            let rows = self.row_order.apply(rows, content_only, live_key, now);
            visible.extend(rows.into_iter().map(|(id, ())| VisibleRow::Task(id)));
            return visible;
        }

        let rows = self.filtered_rows(&local, &remote, |task| task);
        let rows = self.row_order.apply(rows, content_only, live_key, now);
        // 任务组折叠成文件夹行，与其余任务一起按当前排序成为顶层实体。
        let entities = self.fold_groups(rows);
        if self.prefs.group_by == ViewGroupBy::None {
            for entity in entities {
                self.push_entity(&mut visible, entity);
            }
            return visible;
        }
        let mut buckets = group_by_key(
            entities.into_iter().filter_map(|entity| {
                self.entity_bucket(entity, &local, &remote)
                    .map(|bucket| (bucket, entity))
            }),
            |bucket: &GroupBucket| bucket.key.as_str(),
        );
        buckets.sort_by(|(left, _), (right, _)| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.label.cmp(&right.label))
        });
        for (bucket, entities) in buckets {
            let collapsed = self.prefs.is_group_collapsed(&bucket.key);
            visible.push(VisibleRow::GroupHeader {
                key: bucket.key,
                label: bucket.label,
                // 分桶计数按任务：组行计入其在本视图内的成员数。
                count: entities
                    .iter()
                    .map(|entity| self.entity_task_count(*entity))
                    .sum(),
                collapsed,
            });
            if !collapsed {
                for entity in entities {
                    self.push_entity(&mut visible, entity);
                }
            }
        }
        visible
    }

    fn group_bucket(&self, task: &DownloadTaskView) -> GroupBucket {
        let strings = &self.strings;
        match self.prefs.group_by {
            ViewGroupBy::None => GroupBucket {
                key: String::new(),
                label: SharedString::default(),
                order: 0,
            },
            ViewGroupBy::Status => GroupBucket {
                key: format!("status:{}", state_group_key(task.state)),
                label: strings.state_label(task.state),
                order: i64::from(task.state.status_rank()),
            },
            ViewGroupBy::Date => {
                let bucket = DateBucket::of(task.created_at_secs);
                GroupBucket {
                    key: format!("date:{}", bucket.key()),
                    label: strings.date_bucket_label(bucket),
                    order: bucket as i64,
                }
            }
            ViewGroupBy::Type => self.type_bucket(self.categories.category_of(task)),
            ViewGroupBy::Queue => {
                if task.source == TaskSource::Remote {
                    return GroupBucket {
                        key: "queue:remote".to_owned(),
                        label: strings.remote_tasks.clone(),
                        order: i64::MAX,
                    };
                }
                let (label, order) = self
                    .queue_names
                    .iter()
                    .enumerate()
                    .find(|(_, (id, _))| *id == task.queue_id)
                    .map_or_else(
                        || (SharedString::from(task.queue_id.clone()), i64::MAX - 1),
                        |(ix, (id, name))| (strings.queue_name(id, name), ix as i64),
                    );
                GroupBucket {
                    key: format!("queue:{}", task.queue_id),
                    label,
                    order,
                }
            }
            ViewGroupBy::Site => {
                let site = task.source_site();
                let label = if site.is_empty() {
                    strings.site_unknown(task)
                } else {
                    SharedString::from(site.to_owned())
                };
                GroupBucket {
                    key: format!("site:{}", label),
                    label,
                    order: 0,
                }
            }
        }
    }

    /// 按分类分桶：分类规则顺序即桶顺序，未知分类归「其他」排最后。
    fn type_bucket(&self, id: &str) -> GroupBucket {
        let (label, order) = self
            .categories
            .rules()
            .iter()
            .enumerate()
            .find(|(_, rule)| rule.dto.id == id)
            .map_or_else(
                || (self.strings.category_other.clone(), i64::MAX),
                |(ix, rule)| (self.strings.category_label(&rule.dto), ix as i64),
            );
        GroupBucket {
            key: format!("type:{id}"),
            label,
            order,
        }
    }

    /// 「可见任务序列」（全选 / Shift 范围 / 表头三态）：顶层任务按行序；组行位置展开为该组
    /// 在本视图内的全部成员（目录树顺序，不论展开 / 目录折叠），展开出的成员行不再重复计入。
    fn visible_task_ids(&self) -> impl Iterator<Item = RowId> + '_ {
        self.visible.iter().flat_map(move |row| {
            let (single, members): (Option<RowId>, &[RowId]) = match row {
                VisibleRow::Task(id) => (Some(*id), &[]),
                VisibleRow::Group(group) => (
                    None,
                    self.group_rows
                        .get(*group)
                        .map_or(&[][..], |group| group.members.as_slice()),
                ),
                VisibleRow::Member { .. }
                | VisibleRow::GroupDir { .. }
                | VisibleRow::GroupHeader { .. } => (None, &[]),
            };
            single.into_iter().chain(members.iter().copied())
        })
    }

    fn visible_task_keys(&self) -> Vec<RowKey> {
        self.visible_task_ids()
            .filter_map(|id| self.store.row(id).map(|row| row.key.clone()))
            .collect()
    }

    /// 可见任务行里（选中数, 总数）；不分配。
    fn visible_selection_counts(&self) -> (usize, usize) {
        let mut selected = 0;
        let mut total = 0;
        for id in self.visible_task_ids() {
            if let Some(row) = self.store.row(id) {
                total += 1;
                selected += usize::from(self.selected_tasks.contains(&row.key));
            }
        }
        (selected, total)
    }

    /// 侧栏计数：存储按当前分类索引增量维护（行增删 / 状态变化只调整对应桶），换分类索引或
    /// 整表替换后才整表重算一次；进度等不影响计数的更新不触及它。
    fn sidebar_counts(&self) -> Ref<'_, SidebarCounts> {
        self.store.sidebar_counts(&self.categories)
    }

    pub(crate) fn count_matching(&self, filter: &DownloadFilter) -> usize {
        // 任务表载入期间行集合是占位：「全部」显示服务端报告的总数，其余筛选不给出计数。
        if let Some(total) = self
            .store
            .loading()
            .filter(|_| self.store.local().is_empty())
        {
            return if *filter == DownloadFilter::ALL {
                total
            } else {
                0
            };
        }
        self.sidebar_counts().filter(filter)
    }

    pub(crate) fn count_in_queue(&self, queue_id: &str) -> usize {
        self.sidebar_counts().queue(queue_id)
    }

    /// 设备计数：与表格筛选同一规则（[`SidebarSelection::device_matches`]）。
    pub(crate) fn count_device(&self, device_id: &str) -> usize {
        self.sidebar_counts().device(device_id)
    }

    fn shown_columns_count(&self) -> usize {
        self.columns.iter().filter(|column| column.visible).count()
    }

    fn shown_column(&self, col_ix: usize) -> Option<&DownloadColumn> {
        if col_ix == 0 {
            return None;
        }
        self.columns
            .iter()
            .filter(|column| column.visible)
            .nth(col_ix - 1)
    }

    fn move_shown_column(&mut self, from_ix: usize, to_ix: usize) {
        if from_ix == 0 || to_ix == 0 {
            return;
        }
        let positions: Vec<_> = self
            .columns
            .iter()
            .enumerate()
            .filter_map(|(ix, column)| column.visible.then_some(ix))
            .collect();
        let Some(&from_position) = positions.get(from_ix - 1) else {
            return;
        };
        let Some(&to_position) = positions.get(to_ix - 1) else {
            return;
        };
        let column = self.columns.remove(from_position);
        self.columns.insert(to_position, column);
    }

    pub(crate) fn move_column_kind(&mut self, from: DownloadColumnKind, to: DownloadColumnKind) {
        let Some(from_ix) = self.columns.iter().position(|column| column.kind == from) else {
            return;
        };
        let Some(to_ix) = self.columns.iter().position(|column| column.kind == to) else {
            return;
        };
        if from_ix == to_ix {
            return;
        }
        let column = self.columns.remove(from_ix);
        self.columns.insert(to_ix, column);
    }

    pub(crate) fn set_column_visible(&mut self, kind: DownloadColumnKind, visible: bool) -> bool {
        if !visible && self.columns.iter().filter(|column| column.visible).count() <= 1 {
            return false;
        }
        let Some(column) = self.columns.iter_mut().find(|column| column.kind == kind) else {
            return false;
        };
        if column.visible == visible {
            return false;
        }
        column.visible = visible;
        true
    }

    /// 恢复默认列集；文件名列同时回到「吸收剩余宽度」模式。
    pub(crate) fn reset_columns(&mut self) {
        self.columns = DownloadColumnKind::ALL
            .into_iter()
            .map(DownloadColumn::new)
            .collect();
        self.prefs.file_name_width = None;
    }

    pub(crate) fn select_all_tasks(&mut self) {
        // 任务表载入期间「全部」不是完整范围：不做全选。
        if self.store.loading().is_some() {
            return;
        }
        let keys: Vec<RowKey> = self
            .visible_task_keys()
            .into_iter()
            .filter(|key| !self.is_pending_delete(key))
            .collect();
        self.selected_tasks.clear();
        self.selected_tasks.extend(keys);
        self.selection_anchor = None;
        self.multi_select = true;
        self.focused_group = None;
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected_tasks.clear();
        self.selection_anchor = None;
        self.multi_select = false;
        self.focused_group = None;
    }

    /// 是否处于显式多选模式（且仍有选中项）：控制行首复选框、表头全选框与表头选择条。
    pub(crate) fn in_multi_select(&self) -> bool {
        self.multi_select && !self.selected_tasks.is_empty()
    }

    /// 右键「选择」：保留当前选区（右键已选中该行）并进入显式多选模式。
    pub(crate) fn enter_multi_select(&mut self) {
        self.multi_select = !self.selected_tasks.is_empty();
    }

    /// 行首复选框点击：未处于多选模式时以该行开启多选（不并入普通单击选中的行），
    /// 已在多选模式时切换该行。
    pub(crate) fn toggle_multi_select(&mut self, key: RowKey) {
        if self.is_pending_delete(&key) {
            return;
        }
        if !self.in_multi_select() {
            self.selected_tasks.clear();
            self.selected_tasks.insert(key.clone());
        } else if !self.selected_tasks.remove(&key) {
            self.selected_tasks.insert(key.clone());
        }
        self.multi_select = true;
        self.focused_group = None;
        self.selection_anchor = Some(key);
    }

    pub(crate) fn select_task(&mut self, key: RowKey, modifiers: SelectModifiers) {
        // 正在删除的任务不参与选择。
        if self.is_pending_delete(&key) {
            return;
        }
        self.focused_group = None;
        if modifiers.shift
            && let Some(anchor) = self.selection_anchor.clone()
        {
            let keys = self.visible_task_keys();
            if let Some(anchor_index) = keys.iter().position(|item| *item == anchor)
                && let Some(task_index) = keys.iter().position(|item| *item == key)
            {
                let (start, end) = if anchor_index <= task_index {
                    (anchor_index, task_index)
                } else {
                    (task_index, anchor_index)
                };
                self.selected_tasks.clear();
                let range: Vec<RowKey> = keys[start..=end]
                    .iter()
                    .filter(|item| !self.is_pending_delete(item))
                    .cloned()
                    .collect();
                self.selected_tasks.extend(range);
                self.selection_anchor = Some(key);
                return;
            }
        }

        if modifiers.toggle {
            if !self.selected_tasks.remove(&key) {
                self.selected_tasks.insert(key.clone());
            }
            self.multi_select = true;
        } else {
            self.selected_tasks.clear();
            self.selected_tasks.insert(key.clone());
            self.multi_select = false;
        }
        self.selection_anchor = Some(key);
    }

    /// 划选落在 `[from, to]`（可见行下标，含端点）上的任务。`additive` 时并入 `base`。
    /// 组行展开为其全部成员；目录行与分组标题不贡献任务。
    pub(crate) fn apply_row_sweep(
        &mut self,
        from: usize,
        to: usize,
        additive: bool,
        base: &HashSet<RowKey>,
    ) {
        let (start, end) = if from <= to { (from, to) } else { (to, from) };
        let mut selected = if additive {
            base.clone()
        } else {
            HashSet::new()
        };
        let mut anchor = None;
        if start < self.visible.len() {
            for ix in start..=end.min(self.visible.len().saturating_sub(1)) {
                self.push_sweep_keys(ix, &mut selected, &mut anchor);
            }
        }
        let multiple = selected.len() > 1 || (additive && !selected.is_empty());
        self.selected_tasks = selected;
        self.multi_select = multiple;
        self.focused_group = None;
        if let Some(anchor) = anchor {
            self.selection_anchor = Some(anchor);
        }
    }

    fn push_sweep_keys(
        &self,
        ix: usize,
        selected: &mut HashSet<RowKey>,
        anchor: &mut Option<RowKey>,
    ) {
        let ids: Vec<RowId> = match self.visible.get(ix) {
            Some(VisibleRow::Task(id) | VisibleRow::Member { id, .. }) => vec![*id],
            Some(VisibleRow::Group(group)) => self
                .group_rows
                .get(*group)
                .map(|group| group.members.clone())
                .unwrap_or_default(),
            Some(VisibleRow::GroupDir { .. } | VisibleRow::GroupHeader { .. }) | None => return,
        };
        for id in ids {
            let Some(row) = self.store.row(id) else {
                continue;
            };
            if self.is_pending_delete(&row.key) {
                continue;
            }
            let key = row.key.clone();
            *anchor = Some(key.clone());
            selected.insert(key);
        }
    }

    /// 锚点所在可见行：任务行 / 成员行优先，否则是包含该成员的组行。
    fn row_index_of_key(&self, key: &RowKey) -> Option<usize> {
        let mut group_hit = None;
        for (ix, row) in self.visible.iter().enumerate() {
            match row {
                VisibleRow::Task(id) | VisibleRow::Member { id, .. } => {
                    if self.store.row(*id).is_some_and(|row| row.key == *key) {
                        return Some(ix);
                    }
                }
                VisibleRow::Group(group) => {
                    if group_hit.is_none()
                        && self.group_rows.get(*group).is_some_and(|group| {
                            group
                                .members
                                .iter()
                                .any(|id| self.store.row(*id).is_some_and(|row| row.key == *key))
                        })
                    {
                        group_hit = Some(ix);
                    }
                }
                VisibleRow::GroupDir { .. } | VisibleRow::GroupHeader { .. } => {}
            }
        }
        group_hit
    }

    pub(super) fn begin_row_sweep(&mut self, row_ix: usize, event: &MouseDownEvent, cx: &App) {
        let on_handle = self.press_on_file_handle;
        self.press_on_file_handle = false;
        self.suppress_sweep_click = false;
        let modifiers = SelectModifiers::from_event(event.modifiers, cx);
        // 把手上的按下照常冒泡到行（行的单击 / 双击靠它），这里只是不为它开划选会话；
        // 其余列都是划选，不需要修饰键。
        if classify_row_press(on_handle) == RowPress::FileDrag {
            self.row_sweep = None;
            return;
        }
        let mut origin_ix = row_ix;
        if modifiers.shift
            && let Some(anchor) = self.selection_anchor.clone()
            && let Some(ix) = self.row_index_of_key(&anchor)
        {
            origin_ix = ix;
        }
        self.row_sweep = Some(RowSweep {
            origin_ix,
            origin_pos: event.position,
            additive: modifiers.toggle,
            base: if modifiers.toggle {
                self.selected_tasks.clone()
            } else {
                HashSet::new()
            },
            active: false,
        });
    }

    /// 指针移到 `row_ix`。返回选区是否变化。文件拖放进行时没有划选会话，本方法直接返回。
    pub(super) fn drag_sweep_to(&mut self, row_ix: usize, event: &MouseMoveEvent) -> bool {
        let Some(sweep) = self.row_sweep.as_mut() else {
            return false;
        };
        if !sweep.active {
            let delta = event.position - sweep.origin_pos;
            if !sweep_past_slop(delta) {
                return false;
            }
            sweep.active = true;
            self.suppress_sweep_click = true;
        }
        let Some(sweep) = self.row_sweep.as_ref() else {
            return false;
        };
        if !sweep.active {
            return false;
        }
        let origin = sweep.origin_ix;
        let additive = sweep.additive;
        let base = sweep.base.clone();
        let before = self.selected_tasks.clone();
        self.apply_row_sweep(origin, row_ix, additive, &base);
        self.selected_tasks != before
    }

    pub(super) fn finish_row_sweep(&mut self) {
        self.row_sweep = None;
    }

    pub(super) fn note_file_drag_started(&mut self) {
        self.suppress_sweep_click = true;
    }

    pub(super) fn take_sweep_suppress(&mut self, cx: &App) -> bool {
        let suppressed = self.suppress_sweep_click || cx.has_active_drag();
        self.suppress_sweep_click = false;
        suppressed
    }

    pub(crate) fn row_key_at(&self, row_ix: usize) -> Option<RowKey> {
        match self.visible.get(row_ix)? {
            VisibleRow::Task(id) | VisibleRow::Member { id, .. } => {
                self.store.row(*id).map(|row| row.key.clone())
            }
            VisibleRow::Group(_) | VisibleRow::GroupDir { .. } | VisibleRow::GroupHeader { .. } => {
                None
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn group_key_at(&self, row_ix: usize) -> Option<&str> {
        match self.visible.get(row_ix)? {
            VisibleRow::GroupHeader { key, .. } => Some(key),
            _ => None,
        }
    }

    pub(crate) fn select_task_for_context_menu(&mut self, key: RowKey) {
        if self.is_pending_delete(&key) {
            // 正在删除的行没有可用操作：清空选区，菜单为空而不会作用于旧选区。
            self.clear_selection();
            return;
        }
        if !self.selected_tasks.contains(&key) {
            self.selected_tasks.clear();
            self.selected_tasks.insert(key.clone());
            self.multi_select = false;
        }
        self.selection_anchor = Some(key);
    }

    /// 是否为已提交删除、尚未消失的本地任务。
    pub(crate) fn is_pending_delete(&self, key: &RowKey) -> bool {
        matches!(key, RowKey::Local(id) if self.pending_delete.contains(id))
    }

    /// 开始一批删除：清空选区，并登记尚未在途的任务（行随即变淡）；返回本次新登记的 id。
    /// 已在另一批删除中的任务不会被重复提交。
    pub(crate) fn begin_delete(&mut self, task_ids: Vec<String>) -> Vec<String> {
        self.clear_selection();
        task_ids
            .into_iter()
            .filter(|id| self.pending_delete.insert(id.clone()))
            .collect()
    }

    /// 一批删除结束（成功 / 失败 / 取消）：释放登记，仍在列表里的行恢复正常。
    pub(crate) fn finish_delete(&mut self, task_ids: &[String]) {
        for id in task_ids {
            self.pending_delete.remove(id);
        }
    }

    pub(crate) fn selected_keys(&self) -> Vec<RowKey> {
        let mut keys: Vec<RowKey> = self.selected_tasks.iter().cloned().collect();
        keys.sort();
        keys
    }

    /// 恰好选中一个任务时返回它（停靠详情面板跟随选中用；O(1)，表格高频通知下可放心调用）。
    pub(crate) fn single_selected_key(&self) -> Option<&RowKey> {
        if self.selected_tasks.len() == 1 {
            self.selected_tasks.iter().next()
        } else {
            None
        }
    }

    /// 「详情」要展示的本地任务：优先选区锚点（右键 / 最后点击的行），否则按序第一个本地选中项。
    pub(crate) fn detail_candidate(&self) -> Option<RowKey> {
        self.selection_anchor
            .as_ref()
            .filter(|key| key.is_local() && self.selected_tasks.contains(*key))
            .cloned()
            .or_else(|| self.selected_keys().into_iter().find(RowKey::is_local))
    }

    /// 选中集合投影（选择条 / 工具栏）：只统计仍存在于 store 的选中任务
    /// （已删除任务不算），完整遍历以得到数量与各类可用性。
    pub(crate) fn selection_summary(&self) -> SelectionSummary {
        let mut summary = SelectionSummary::default();
        let mut any_unopenable = false;
        for key in &self.selected_tasks {
            let Some(row) = self.store.get(key) else {
                continue;
            };
            summary.count += 1;
            summary.any = true;
            summary.any_local |= key.is_local();
            any_unopenable |= !row.has_local_file();
            match row.state {
                TaskState::Downloading | TaskState::Pending => summary.any_active = true,
                TaskState::Paused | TaskState::Failed => summary.any_resumable = true,
                TaskState::Completed => {}
            }
            // 失败的本机任务可暂停（撤销等待中的自动重试，#947）。
            summary.any_active |=
                row.is_seeding() || (key.is_local() && row.state == TaskState::Failed);
            summary.any_resumable |= row.can_resume_seeding();
        }
        summary.all_openable = summary.any && !any_unopenable;
        summary
    }

    /// 选中任务里仍存在且可打开文件的 key（本地 + 已完成 + 文件仍在下载目录）。
    fn openable_selected_keys(&self) -> Vec<RowKey> {
        self.selected_keys()
            .into_iter()
            .filter(|key| self.store.get(key).is_some_and(|row| row.has_local_file()))
            .collect()
    }

    /// 拖出到系统文件管理器的本机路径：锚点行在选区内时带上整个选区，否则只拖锚点行；
    /// 只取本地、已完成且未被判定丢失的任务（磁盘现状由拖出时再探测）。
    pub(crate) fn drag_paths(&self, anchor: &RowKey) -> Vec<std::path::PathBuf> {
        let keys = if self.selected_tasks.contains(anchor) {
            self.selected_keys()
        } else {
            vec![anchor.clone()]
        };
        keys.iter()
            .filter_map(|key| {
                let row = self.store.get(key)?;
                if row.has_local_file() {
                    row.local_file_path()
                } else {
                    None
                }
            })
            .collect()
    }

    /// 能拖出到系统文件管理器的行：本地、已完成、文件仍在，且不在删除中。
    fn dragged_tasks(
        &self,
        task: &DownloadTaskView,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<DraggedTasks> {
        if self.is_pending_delete(&task.key) || !task.has_local_file() {
            return None;
        }
        Some(DraggedTasks {
            anchor: task.key.clone(),
            kind: task.kind,
            name_fold: SharedString::from(task.name_fold.clone()),
            name: SharedString::from(task.name.clone()),
            table: cx.weak_entity(),
            host: self.host.clone(),
        })
    }

    pub(crate) fn toggle_group_collapsed(&mut self, key: &str) {
        self.prefs.toggle_group_collapsed(key);
        self.view_dirty = true;
    }

    /// 文件大类的类别名。
    fn kind_label(&self, kind: FileKind) -> SharedString {
        let strings = &self.strings;
        match kind {
            FileKind::Video => &strings.category_video,
            FileKind::Audio => &strings.category_audio,
            FileKind::Document => &strings.category_document,
            FileKind::Image => &strings.category_image,
            FileKind::Archive | FileKind::DiskImage => &strings.category_archive,
            FileKind::Application | FileKind::Mobile => &strings.category_program,
            FileKind::Other => &strings.category_other,
        }
        .clone()
    }

    /// 状态列主文案：下载中显示「速度 · 剩余时间」（只显示已知部分），其余为状态名。
    fn status_label(&self, task: &DownloadTaskView) -> SharedString {
        match task.remote_status {
            // 取消的远程任务不是「失败」；云端新增的未知状态不冒充任何已知状态。
            Some(RemoteTaskStatus::Canceled) => return self.strings.status_canceled.clone(),
            Some(RemoteTaskStatus::Unknown) => return SharedString::from("—"),
            _ => {}
        }
        if task.state != TaskState::Downloading {
            return self.strings.task_state_label(task);
        }
        let speed = task
            .speed_bytes_per_second
            .filter(|speed| *speed > 0)
            .map(|speed| format!("{}/s", format_bytes(speed)));
        let eta = task
            .eta_seconds
            .filter(|seconds| *seconds <= MAX_ETA_SECS)
            .map(|seconds| self.strings.format_eta(seconds));
        match (speed, eta) {
            (Some(speed), Some(eta)) => SharedString::from(format!("{speed} · {eta}")),
            (Some(speed), None) => SharedString::from(speed),
            (None, Some(eta)) => eta,
            (None, None) => self.strings.status_downloading.clone(),
        }
    }

    /// 状态列第二行（舒适密度）/ 紧凑密度的悬停提示：已下 / 总量、并发连接或 BT
    /// 节点、失败原因首行。
    fn status_detail(&self, task: &DownloadTaskView) -> Option<String> {
        match task.state {
            TaskState::Downloading => {
                let bytes = bytes_progress(task);
                Some(match self.transfer_detail(task) {
                    Some(transfers) => format!("{bytes} · {transfers}"),
                    None => bytes,
                })
            }
            TaskState::Paused => Some(bytes_progress(task)),
            TaskState::Failed => self
                .strings
                .task_error_text(&task.error_message)
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
            TaskState::Pending => self.strings.queued_label(task, &self.store),
            TaskState::Completed => None,
        }
    }

    /// 宽松密度主列副标题（旧 Flutter 默认布局）：未完成为「已下 / 总量 [· 速度]」，完成为
    /// 「大小 · 类别」，末尾接来源域名；进度 / 剩余时间在独立列，未知字段省略（Web
    /// `relaxedMeta` 镜像）。
    fn relaxed_meta(&self, task: &DownloadTaskView) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(4);
        if task.state == TaskState::Completed {
            if task.size_bytes > 0 {
                parts.push(task.size.clone());
            }
            parts.push(self.kind_label(task.kind).to_string());
        } else {
            parts.push(bytes_progress(task));
            if task.state == TaskState::Downloading
                && let Some(speed) = task.speed_bytes_per_second.filter(|speed| *speed > 0)
            {
                parts.push(format!("{}/s", format_bytes(speed)));
            }
        }
        if let Some(site) = self.shown_source_site(task) {
            parts.push(site.to_owned());
        }
        parts.join(" · ")
    }

    /// 实时并发：本地分段连接数与 BT 已连节点数。
    fn transfer_detail(&self, task: &DownloadTaskView) -> Option<String> {
        let active = task.active_transfers();
        let peers = task
            .runtime
            .as_ref()
            .filter(|_| task.runtime_connected && task.protocol == TaskProtocol::Bt)
            .and_then(|runtime| runtime.connected_peers);
        match (active, peers) {
            (Some(active), Some(peers)) => Some(format!(
                "{active} {} · {peers} {}",
                self.strings.active_transfers, self.strings.connected_peers
            )),
            (Some(active), None) => Some(format!("{active} {}", self.strings.active_transfers)),
            (None, Some(peers)) => Some(format!("{peers} {}", self.strings.connected_peers)),
            (None, None) => None,
        }
    }

    /// 选择列图标边长：双行 / 三行密度给文件图标更大的尺寸（与文字块等高感），紧凑单行与
    /// 文字同高。选择列宽随之放宽（[`Self::selection_column_width`]），图标不越出选中底色。
    fn file_icon_size(&self, cx: &App) -> Pixels {
        let lg = f32::from(active_theme(cx).extended().icon.lg);
        px(file_icon_size_for(self.prefs.density, lg))
    }

    /// 固定左列：平时显示系统文件图标（取不到时回退为按类型的图标，元数据加载中为
    /// Spinner）。只有悬停该格本身（而非整行）时图标换成复选框，点击即以该行进入多选；
    /// 横扫列表不会让每行图标闪烁。多选模式下复选框常显。
    /// 隐藏（`invisible`）的元素不绘制、不注册鼠标监听，不会拦截行点击。组内成员
    /// （`in_group`）的图标在文件名列，这一格平时只画树轨。
    fn render_selection_cell(
        &self,
        row_ix: usize,
        task: &DownloadTaskView,
        in_group: bool,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let muted = theme.tokens().colors.muted_foreground;
        let hover_border = theme.tokens().colors.foreground.opacity(0.7);
        let icon_sizes = theme.extended().icon;
        let key = task.key.clone();
        let selected = self.selected_tasks.contains(&key);
        let checkbox_pinned = self.in_multi_select();
        let checked = checkbox_pinned && selected;
        let glyph = (!checkbox_pinned).then(|| {
            if in_group {
                return groups::tree_rail(cx).into_any_element();
            }
            if task.metadata_pending {
                return Spinner::new()
                    .with_size(icon_sizes.md)
                    .icon(IconName::LoaderCircle)
                    .color(muted)
                    .into_any_element();
            }
            task_file_icon(task, self.file_icon_size(cx), window, cx)
        });
        let check = check_mark(
            if checked {
                CheckState::Checked
            } else {
                CheckState::Unchecked
            },
            cx,
        );
        let toggle = cx.listener(move |table, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
            if table.delegate_mut().take_sweep_suppress(cx) {
                return;
            }
            table.delegate_mut().toggle_multi_select(key.clone());
            cx.notify();
        });
        if !checkbox_pinned {
            let tooltip = self.strings.select_task_tooltip.clone();
            // 选择列只负责点进多选。拖出文件只在文件名列，避免和图标点击抢手势。
            return div()
                .id(("download-task-select-cell", row_ix))
                .group(SELECT_CELL_GROUP)
                .size_full()
                .relative()
                .cursor_pointer()
                .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                .when_some(glyph, |this, glyph| {
                    this.child(
                        h_flex()
                            .absolute()
                            .inset_0()
                            .justify_center()
                            .group_hover(SELECT_CELL_GROUP, |style| style.invisible())
                            .child(glyph),
                    )
                })
                .child(
                    h_flex()
                        .absolute()
                        .inset_0()
                        .justify_center()
                        .invisible()
                        .group_hover(SELECT_CELL_GROUP, |style| style.visible())
                        .child(check.group_hover(SELECT_CELL_GROUP, move |style| {
                            style.border_color(hover_border)
                        })),
                )
                .on_click(toggle)
                .into_any_element();
        }
        let checkbox = div()
            .id(("download-task-multi-select", row_ix))
            .p(px(4.))
            .cursor_pointer()
            .child(check.when(!checked, |mark| {
                mark.hover(move |style| style.border_color(hover_border))
            }))
            .on_click(toggle);
        div()
            .size_full()
            .relative()
            .child(
                h_flex()
                    .absolute()
                    .inset_0()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(checkbox),
            )
            .into_any_element()
    }

    /// 文件名列。文件还在下载目录时，这一整列是拖出把手（任意方向，不用修饰键）。
    /// 其他列的按下留给行划选。单击文件名仍选中该行。
    fn render_file_name_cell(
        &self,
        row_ix: usize,
        task: &DownloadTaskView,
        depth: u8,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let body = if depth == 0 {
            self.task_cell(DownloadColumnKind::FileName, task, cx)
        } else {
            let icon = task_file_icon(task, self.file_icon_size(cx), window, cx);
            h_flex()
                .size_full()
                .min_w_0()
                .gap(active_theme(cx).tokens().spacing.sm)
                .when(depth > 1, |this| this.child(self.dir_member_indent(cx)))
                .child(icon)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(self.render_file_cell(task, cx)),
                )
                .into_any_element()
        };
        let Some(dragged) = self.dragged_tasks(task, cx) else {
            return body;
        };
        Self::arm_file_drag(
            div()
                .id(("download-name-drag", row_ix))
                .size_full()
                .min_w_0(),
            dragged,
            cx,
        )
        .child(body)
        .into_any_element()
    }

    /// 副标题里展示的来源站点：站点为空或用户选择隐藏来源时为 `None`。
    fn shown_source_site<'a>(&self, task: &'a DownloadTaskView) -> Option<&'a str> {
        let site = task.source_site();
        (!self.hide_source && !site.is_empty()).then_some(site)
    }

    /// 文件名列的副标题（仅双行密度）：宽松密度用 [`Self::relaxed_meta`]，其余为
    /// 「类别 · 站点」；隐藏来源时去掉站点，类别保留。
    fn file_cell_meta(&self, task: &DownloadTaskView) -> Option<SharedString> {
        if !self.prefs.density.two_line() || task.metadata_pending {
            return None;
        }
        if self.prefs.density == ViewDensity::Relaxed {
            return Some(SharedString::from(self.relaxed_meta(task)));
        }
        let category = self.kind_label(task.kind);
        Some(match self.shown_source_site(task) {
            Some(site) => SharedString::from(format!("{category} · {site}")),
            None => category,
        })
    }

    /// 文件名列：名称 + 双行密度的副标题（[`Self::file_cell_meta`]）。
    fn render_file_cell(&self, task: &DownloadTaskView, cx: &App) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let (name, name_color) = if task.metadata_pending {
            (
                self.strings.metadata_loading.clone(),
                tokens.colors.muted_foreground,
            )
        } else {
            (
                SharedString::from(task.name.clone()),
                // 文件已不在下载目录：文件名退为次要色，与状态列「文件已删除」呼应。
                if task.is_file_missing() {
                    tokens.colors.muted_foreground
                } else {
                    tokens.colors.foreground
                },
            )
        };
        let meta = self.file_cell_meta(task);
        v_flex()
            .size_full()
            .min_w_0()
            .justify_center()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .font_weight(tokens.typography.sm.weight)
                    .text_color(name_color)
                    .child(name),
            )
            .when_some(meta, |this, meta| {
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .font_features(tabular_numbers())
                        .text_color(theme.extended().colors.text_tertiary)
                        .child(meta),
                )
            })
            .into_any_element()
    }

    /// 活动列：主文案按状态着色（仅下载中 primary、失败 destructive），双行密度
    /// 第二行给详情；紧凑密度把详情放进悬停提示，失败总是提示完整原因。
    fn render_status_cell(&self, task: &DownloadTaskView, cx: &App) -> AnyElement {
        let theme = active_theme(cx);
        let typography = &theme.tokens().typography;
        let two_line = self.prefs.density.two_line();
        let detail = self.status_detail(task);
        let tooltip = if task.state == TaskState::Failed && !task.error_message.trim().is_empty() {
            Some(SharedString::from(
                self.strings.task_error_text(&task.error_message).to_owned(),
            ))
        } else if two_line {
            None
        } else {
            detail.clone().map(SharedString::from)
        };
        let label_style = &typography.xs;
        let primary = if task.conflict_pending {
            h_flex()
                .min_w_0()
                .child(conflict_badge(
                    task.key.task_id(),
                    &self.strings,
                    self.host.as_ref(),
                    cx,
                ))
                .into_any_element()
        } else {
            div()
                .min_w_0()
                .truncate()
                .text_size(label_style.size)
                .line_height(label_style.line_height)
                .text_color(task_status_color(task, cx))
                .child(self.status_label(task))
                .into_any_element()
        };
        v_flex()
            .id(SharedString::from(format!(
                "download-status-{}",
                task.key.task_id()
            )))
            .size_full()
            .min_w_0()
            .justify_center()
            .font_features(tabular_numbers())
            .child(primary)
            .when_some(detail.filter(|_| two_line), |this, detail| {
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(typography.xs.size)
                        .line_height(typography.xs.line_height)
                        .text_color(theme.extended().colors.text_tertiary)
                        .child(detail),
                )
            })
            .when_some(
                tooltip.filter(|_| !task.conflict_pending),
                |this, tooltip| {
                    this.tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                },
            )
            .into_any_element()
    }

    /// 进度单元格：`components.progress` 高度 / 圆角的条 + 整数百分比；完成态整格留空
    /// （完成只由状态列表达）。
    fn render_progress_cell(&self, task: &DownloadTaskView, cx: &App) -> AnyElement {
        if task.state == TaskState::Completed {
            return div().into_any_element();
        }
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let colors = &tokens.colors;
        let bar_color = progress_bar_color(task.state, cx);
        let text_scale = f32::from(theme.text_extent(1.));
        let column_width = self
            .columns
            .iter()
            .find(|column| column.kind == DownloadColumnKind::Progress)
            .map_or(
                DownloadColumnKind::Progress.min_width() * text_scale,
                |column| column.kind.display_width(column.width, text_scale),
            );
        // 百分比区随文字放大；条宽 = 列内容宽（扣左右内边距）− 百分比区 − 间距，与下方布局同一组常量。
        let label_width = theme.text_extent(PROGRESS_LABEL_WIDTH);
        let bar_width =
            (column_width - 2. * CELL_PADDING_X - f32::from(label_width) - PROGRESS_GAP).max(0.);
        h_flex()
            .size_full()
            .min_w_0()
            .gap(px(PROGRESS_GAP))
            .child(
                crate::components::segment_progress::render_segment_progress(
                    task.runtime.as_deref(),
                    task.progress,
                    bar_width,
                    theme.components().progress_height,
                    theme.components().progress_radius,
                    bar_color,
                    progress_track_color(cx),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .w(label_width)
                    .text_right()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .font_features(tabular_numbers())
                    .text_color(colors.muted_foreground)
                    .child(percent_label(task.progress)),
            )
            .into_any_element()
    }

    /// 任务（或组行代理视图）在某一列的单元格。
    fn task_cell(&self, kind: DownloadColumnKind, task: &DownloadTaskView, cx: &App) -> AnyElement {
        let theme = active_theme(cx);
        let muted = theme.tokens().colors.muted_foreground;
        let tertiary = theme.extended().colors.text_tertiary;
        let downloading = task.state == TaskState::Downloading;
        match kind {
            DownloadColumnKind::FileName => self.render_file_cell(task, cx),
            DownloadColumnKind::Progress => self.render_progress_cell(task, cx),
            DownloadColumnKind::Size if task.size_bytes > 0 => {
                meta_cell(task.size.clone(), muted, true, cx)
            }
            DownloadColumnKind::Status => self.render_status_cell(task, cx),
            DownloadColumnKind::Speed => match task
                .speed_bytes_per_second
                .filter(|speed| downloading && *speed > 0)
            {
                Some(speed) => meta_cell(format!("{}/s", format_bytes(speed)), muted, true, cx),
                None => match seeding_upload_speed(task) {
                    Some(speed) => meta_cell(
                        format!("↑ {}/s", format_bytes(speed)),
                        theme.extended().colors.success,
                        true,
                        cx,
                    ),
                    None => meta_cell("—", tertiary, true, cx),
                },
            },
            DownloadColumnKind::Eta => match task
                .eta_seconds
                .filter(|seconds| downloading && *seconds <= MAX_ETA_SECS)
            {
                Some(seconds) => meta_cell(self.strings.format_eta(seconds), muted, true, cx),
                None => meta_cell("—", tertiary, true, cx),
            },
            DownloadColumnKind::Size => meta_cell("—", tertiary, true, cx),
            DownloadColumnKind::Created if task.created_at_secs > 0 => meta_cell(
                DownloadStrings::format_datetime(task.created_at_secs),
                muted,
                true,
                cx,
            ),
            DownloadColumnKind::Created => meta_cell("—", tertiary, true, cx),
            DownloadColumnKind::Protocol => meta_cell(task.protocol.label(), muted, false, cx),
            DownloadColumnKind::Source => {
                meta_cell(task.source_site().to_owned(), muted, false, cx)
            }
            DownloadColumnKind::Queue => {
                // 远程任务没有本机队列：这一列显示它所在的目标设备。
                let name = if task.source == TaskSource::Remote {
                    self.device_names
                        .get(&task.to_device)
                        .cloned()
                        .unwrap_or_else(|| task.to_device.clone())
                } else {
                    self.queue_label(&task.queue_id).to_string()
                };
                meta_cell(name, muted, false, cx)
            }
        }
    }

    /// 本机队列显示名；未知队列显示 id。
    fn queue_label(&self, queue_id: &str) -> SharedString {
        self.queue_names
            .iter()
            .find(|(id, _)| id == queue_id)
            .map_or_else(
                || SharedString::from(queue_id.to_owned()),
                |(id, name)| self.strings.queue_name(id, name),
            )
    }

    /// 行悬停操作（挂在右侧留白列上，贴表格可视区右缘，横向滚动时不移出视口）：
    /// 暂停 / 继续 / 重试 / 打开 + 在文件夹中
    /// 显示；停靠详情面板未打开时再加「详情」（面板打开后单击行即切换详情，按钮多余），
    /// 舒适与紧凑密度一致。浮层用 surface 叠选中 / 悬停色的不透明底 + 左侧渐隐，避免盖住
    /// 末列文字时透出；除「详情」外点击不改变选中。
    fn render_row_actions(&self, task: &DownloadTaskView, cx: &App) -> Option<AnyElement> {
        if self.is_pending_delete(&task.key) {
            return None;
        }
        let host = self.host.as_ref()?;
        let is_local = task.key.is_local();
        let with_detail = !self.prefs.detail_open;
        let mut actions = row_actions(task, is_local, with_detail)
            .filter(|action| {
                is_local
                    || action
                        .remote_action()
                        .is_some_and(|remote| task.remote_can(remote))
            })
            .peekable();
        actions.peek()?;
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let (muted, foreground) = (tokens.colors.muted_foreground, tokens.colors.foreground);
        let pressed = extended.colors.nav_selected;
        let selected = self.selected_tasks.contains(&task.key);
        let buttons = actions.map(|action| {
            let (icon, tooltip) = match action {
                RowAction::Pause => (FluxIcon::Pause, self.strings.pause.clone()),
                RowAction::Resume => (FluxIcon::Play, self.strings.resume.clone()),
                RowAction::Retry => (FluxIcon::RotateCw, self.strings.resume.clone()),
                RowAction::Open => (FluxIcon::ExternalLink, self.strings.open_file.clone()),
                RowAction::Reveal => (FluxIcon::FolderOpen, self.strings.open_folder.clone()),
                RowAction::Detail => (FluxIcon::PanelRight, self.strings.detail.clone()),
            };
            let host = host.clone();
            let key = task.key.clone();
            h_flex()
                .id(action.id())
                .flex_none()
                .size(px(ROW_ACTION_SIZE))
                .justify_center()
                .rounded(tokens.radius.sm)
                .cursor_pointer()
                .text_color(muted)
                .hover(move |style| style.bg(pressed).text_color(foreground))
                .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let key = key.clone();

                    let Ok(()) = host.update(cx, |view, cx| {
                        let Some(command) = action.command() else {
                            view.show_row_detail(key, window, cx);
                            return;
                        };
                        let command = view
                            .controller
                            .store()
                            .get(&key)
                            .and_then(|row| task_command(&row, command));
                        if let Some(command) = command {
                            view.execute_commands(vec![command], cx);
                        }
                    }) else {
                        // 视图已释放，结束这次回调而不再更新状态。
                        return;
                    };
                })
                .child(Icon::new(icon).size(extended.icon.md))
        });
        Some(wrap_row_action_strip(selected, buttons, cx))
    }

    /// 分组头：xs 中等字重正文色标签 + 三级色计数，28 高内容贴底，行槽位多出的
    /// 高度作上方留白；不加底色块。
    fn render_group_header(
        &self,
        row_ix: usize,
        key: &str,
        label: &SharedString,
        count: usize,
        collapsed: bool,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let key = key.to_owned();
        let on_click = cx.listener(move |table, _: &ClickEvent, _, cx| {
            if table.delegate_mut().take_sweep_suppress(cx) {
                return;
            }
            let delegate = table.delegate_mut();
            delegate.toggle_group_collapsed(&key);
            delegate.refresh_view();
            // 折叠记忆随视图偏好持久化；表格实体此刻正被更新，放到帧外调用。
            if let Some(host) = delegate.host.clone() {
                cx.defer(move |cx| {
                    let Ok(()) = host.update(cx, |view, cx| view.schedule_persist_prefs(cx)) else {
                        // 视图已释放，结束这次回调而不再更新状态。
                        return;
                    };
                });
            }
            table.refresh(cx);
            cx.notify();
        });
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let tertiary = extended.colors.text_tertiary;
        let row = div()
            .id(("download-group-header", row_ix))
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(on_click)
            .child(
                h_flex()
                    .h(theme.density().control)
                    .min_w_0()
                    .gap(tokens.spacing.xs)
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .child(
                        Icon::new(if collapsed {
                            FluxIcon::ChevronRight
                        } else {
                            FluxIcon::ChevronDown
                        })
                        .size(extended.icon.sm)
                        .text_color(tertiary),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tokens.colors.foreground)
                            .child(label.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_features(tabular_numbers())
                            .text_color(tertiary)
                            .child(count.to_string()),
                    ),
            );
        Self::arm_row_sweep(row, row_ix, false, cx).into_any_element()
    }

    /// 选中集合 → 任务事实；选择滞后于存储事件时任一键缺失即返回 `None`。
    fn selected_menu_facts(&self) -> Option<(Vec<RowKey>, Vec<TaskMenuFacts>)> {
        let selected = self.selected_keys();
        if selected.is_empty() {
            return None;
        }
        let facts = selected
            .iter()
            .map(|key| self.task_menu_facts(key))
            .collect::<Option<Vec<_>>>()?;
        Some((selected, facts))
    }

    pub(crate) fn task_menu_facts(&self, key: &RowKey) -> Option<TaskMenuFacts> {
        task_menu_facts_for(&self.store, key)
    }

    /// 任务行右键菜单：按 [`task_menu_entries`] 的纯函数结果逐项渲染。
    fn task_context_menu(&self, menu: PopupMenu, destructive: Hsla) -> PopupMenu {
        let Some((selected, facts)) = self.selected_menu_facts() else {
            return menu;
        };
        task_menu_entries(&facts, self.in_multi_select(), self.plugin_commands.len())
            .into_iter()
            .fold(menu, |menu, entry| {
                self.apply_menu_entry(menu, entry, &selected, destructive)
            })
    }

    fn apply_menu_entry(
        &self,
        menu: PopupMenu,
        entry: MenuEntry,
        selected: &[RowKey],
        destructive: Hsla,
    ) -> PopupMenu {
        match entry {
            MenuEntry::Resume => menu.menu_with_icon(
                self.strings.resume.clone(),
                FluxIcon::Play,
                Box::new(crate::actions::ResumeSelected),
            ),
            MenuEntry::Pause => menu.menu_with_icon(
                self.strings.pause.clone(),
                FluxIcon::Pause,
                Box::new(crate::actions::PauseSelected),
            ),
            MenuEntry::IgnorePluginRetry => {
                let (Some(host), Some(task_id)) = (
                    self.host.clone(),
                    selected.first().map(|key| key.task_id().to_owned()),
                ) else {
                    return menu;
                };
                menu.item(
                    PopupMenuItem::new(self.strings.ignore_plugin_retry.clone())
                        .icon(FluxIcon::CircleAlert)
                        .on_click(move |_, window, cx| {
                            let task_id = task_id.clone();

                            let Ok(()) = host.update(cx, |view, cx| {
                                view.confirm_ignore_plugin_retry(task_id, window, cx);
                            }) else {
                                // 视图已释放，结束这次回调而不再更新状态。
                                return;
                            };
                        }),
                )
            }
            MenuEntry::Boost | MenuEntry::CancelBoost => {
                let cancel = matches!(entry, MenuEntry::CancelBoost);
                menu.menu_with_icon(
                    if cancel {
                        self.strings.cancel_boost.clone()
                    } else {
                        self.strings.boost_download.clone()
                    },
                    FluxIcon::Zap,
                    Box::new(crate::actions::ToggleBoostSelected),
                )
            }
            MenuEntry::OpenFile => menu.menu_with_icon(
                self.strings.open_file.clone(),
                FluxIcon::ExternalLink,
                Box::new(crate::actions::OpenSelected),
            ),
            MenuEntry::OpenFolder => menu.menu_with_icon(
                self.strings.open_folder.clone(),
                FluxIcon::FolderOpen,
                Box::new(crate::actions::RevealSelected),
            ),
            MenuEntry::CopyFile => menu.menu_with_icon(
                self.strings.copy_file.clone(),
                FluxIcon::File,
                Box::new(crate::actions::CopySelectedFiles),
            ),
            MenuEntry::Rename => menu.menu_with_icon(
                self.strings.rename_task.clone(),
                FluxIcon::Pen,
                Box::new(crate::actions::RenameSelected),
            ),
            MenuEntry::ChangeUrl => menu.menu_with_icon(
                self.strings.change_url.clone(),
                FluxIcon::Globe,
                Box::new(crate::actions::ChangeSelectedUrl),
            ),
            MenuEntry::ChangeSegments => menu.menu_with_icon(
                self.strings.edit_threads.clone(),
                FluxIcon::Layers,
                Box::new(crate::actions::ChangeSelectedSegments),
            ),
            MenuEntry::VerifyIntegrity => menu.menu_with_icon(
                self.strings.verify_integrity.clone(),
                FluxIcon::Shield,
                Box::new(crate::actions::VerifySelected),
            ),
            MenuEntry::RarestFirst
            | MenuEntry::SequentialDownload
            | MenuEntry::FirstLastPiecesFirst => {
                self.piece_picking_menu_item(menu, entry, selected)
            }
            MenuEntry::Redownload => menu.menu_with_icon(
                self.strings.redownload_task.clone(),
                FluxIcon::RotateCw,
                Box::new(crate::actions::RedownloadSelected),
            ),
            MenuEntry::CopyUrl => menu.menu_with_icon(
                self.strings.copy_url.clone(),
                FluxIcon::Copy,
                Box::new(crate::actions::CopySelectedUrl),
            ),
            MenuEntry::CopyMagnet => menu.menu_with_icon(
                self.strings.copy_magnet.clone(),
                FluxIcon::Copy,
                Box::new(crate::actions::CopyMagnetSelected),
            ),
            MenuEntry::ExportTorrent => menu.menu_with_icon(
                self.strings.export_torrent.clone(),
                FluxIcon::Download,
                Box::new(crate::actions::ExportTorrentSelected),
            ),
            MenuEntry::MoveToQueue => self.append_move_to_queue(menu),
            MenuEntry::Separator => menu.separator(),
            MenuEntry::Delete => destructive_menu_item(
                menu,
                destructive,
                self.strings.delete_task.clone(),
                Box::new(crate::actions::DeleteSelected),
            ),
            MenuEntry::DeleteWithFiles => destructive_menu_item(
                menu,
                destructive,
                self.strings.delete_task_and_file.clone(),
                Box::new(crate::actions::DeleteSelectedWithFiles),
            ),
            MenuEntry::ShowDetail => menu.menu_with_icon(
                self.strings.detail.clone(),
                FluxIcon::PanelRight,
                Box::new(crate::actions::ShowSelectedDetail),
            ),
            MenuEntry::Select => menu.menu_with_icon(
                self.strings.select_tasks.clone(),
                FluxIcon::CircleCheck,
                Box::new(crate::actions::EnterMultiSelect),
            ),
            MenuEntry::SelectAll => menu.menu_with_icon(
                self.strings.select_all_tasks.clone(),
                FluxIcon::CheckboxCheck,
                Box::new(crate::actions::SelectAllTasks),
            ),
            MenuEntry::Plugin(index) => self.append_plugin_command(menu, index, selected),
        }
    }

    /// 选片三项：标题不可点（这里拿不到 `Context<PopupMenu>`，不能建子菜单）。
    /// 顺序两项画圆点单选，首尾画方框，关闭时也看得见状态。
    fn piece_picking_menu_item(
        &self,
        menu: PopupMenu,
        entry: MenuEntry,
        selected: &[RowKey],
    ) -> PopupMenu {
        let facts: Vec<_> = selected
            .iter()
            .filter_map(|key| self.task_menu_facts(key))
            .collect();
        let order = piece_picking_order(&facts);
        let (label, icon, action): (SharedString, FluxIcon, Box<dyn gpui::Action>) = match entry {
            MenuEntry::RarestFirst => (
                self.strings.bt_rarest_first.clone(),
                if order == PiecePickingOrder::Rarest {
                    FluxIcon::CircleDot
                } else {
                    FluxIcon::Circle
                },
                Box::new(crate::actions::SetRarestFirstSelected),
            ),
            MenuEntry::SequentialDownload => (
                self.strings.bt_sequential.clone(),
                if order == PiecePickingOrder::Sequential {
                    FluxIcon::CircleDot
                } else {
                    FluxIcon::Circle
                },
                Box::new(crate::actions::SetSequentialSelected),
            ),
            MenuEntry::FirstLastPiecesFirst => (
                self.strings.bt_first_last_pieces.clone(),
                if piece_picking_checked(&facts) {
                    FluxIcon::SquareCheck
                } else {
                    FluxIcon::Square
                },
                Box::new(crate::actions::ToggleFirstLastPiecesSelected),
            ),
            _ => return menu,
        };
        let menu = if matches!(entry, MenuEntry::RarestFirst) {
            menu.item(
                PopupMenuItem::new(self.strings.bt_piece_picking.clone())
                    .icon(FluxIcon::ListFilter)
                    .disabled(true),
            )
        } else {
            menu
        };
        menu.menu_with_icon(label, icon, action)
    }
}

/// 破坏性菜单项：图标与文字取主题 `destructive` 色。
fn destructive_menu_item(
    menu: PopupMenu,
    color: Hsla,
    label: SharedString,
    action: Box<dyn gpui::Action>,
) -> PopupMenu {
    menu.menu_element_with_icon(
        Icon::from(FluxIcon::Trash2).text_color(color),
        action,
        move |_, _| div().text_color(color).child(label.clone()),
    )
}

impl DownloadTableDelegate {
    /// 插件贡献的任务命令：只把本机 daemon 认识的任务 id 交给宿主回调（远程任务不下发）。
    fn append_plugin_command(&self, menu: PopupMenu, index: u16, selected: &[RowKey]) -> PopupMenu {
        let (Some(host), Some(command)) = (
            self.host.clone(),
            self.plugin_commands.get(usize::from(index)),
        ) else {
            return menu;
        };
        let task_ids: Vec<String> = selected
            .iter()
            .filter(|key| key.is_local())
            .map(|key| key.task_id().to_owned())
            .collect();
        // 按菜单构建时的稳定键（插件 identity + 命令 id）分派：菜单打开期间插件列表可能变化，
        // 下标会指向别的命令。
        let identity = command.identity.clone();
        let command_id = command.command.clone();
        menu.item(
            PopupMenuItem::new(command.title.clone())
                .icon(FluxIcon::Sparkles)
                .on_click(move |_, window, cx| {
                    let task_ids = task_ids.clone();
                    let Ok(()) = host.update(cx, |view, cx| {
                        view.run_plugin_command(&identity, &command_id, task_ids, window, cx);
                    }) else {
                        // 视图已释放，结束这次回调。
                        return;
                    };
                }),
        )
    }

    /// 「移动到队列」：固定版 gpui-component 的 `PopupMenu::submenu` 需要
    /// `Context<PopupMenu>`，而 `TableDelegate::context_menu` 只提供
    /// `Context<TableState<Self>>`（两者是不同实体的上下文，无法互转），因此
    /// 这里无法构造真正的嵌套子菜单，改为「带图标的不可点击标题 + 缩进队列项」：
    /// 标题与其余菜单项同列对齐，队列项不带图标，文字落在标题文字列下形成缩进。
    fn append_move_to_queue(&self, menu: PopupMenu) -> PopupMenu {
        let Some(host) = self.host.clone() else {
            return menu;
        };
        if self.queue_names.is_empty() {
            return menu;
        }
        let mut menu = menu.item(
            PopupMenuItem::new(self.strings.move_to_queue.clone())
                .icon(FluxIcon::Rows3)
                .disabled(true),
        );
        for (queue_id, name) in &self.queue_names {
            let host = host.clone();
            let queue_id = queue_id.clone();
            menu = menu.item(
                PopupMenuItem::new(self.strings.queue_name(&queue_id, name)).on_click(
                    move |_, _, cx| {
                        let queue_id = queue_id.clone();

                        let Ok(()) =
                            host.update(cx, |view, cx| view.move_selected_to_queue(queue_id, cx))
                        else {
                            // 视图已释放，结束这次回调而不再更新状态。
                            return;
                        };
                    },
                ),
            );
        }
        menu
    }

    /// 任务组文件夹行右键菜单：可用性按全组成员判定（命令作用于整个组，不受当前筛选影响）。
    fn group_context_menu(&self, group_id: &str, menu: PopupMenu, destructive: Hsla) -> PopupMenu {
        let Some(host) = self.host.as_ref() else {
            return menu;
        };
        let counts = self.whole_group_counts(group_id);
        let has_source = self
            .groups
            .get(group_id)
            .is_some_and(|group| !group.source_url.is_empty());
        let mut menu = menu;
        if counts.any_active() {
            menu = menu.item(group_menu_item(
                self.strings.group_pause_all.clone(),
                FluxIcon::Pause,
                None,
                host,
                group_id,
                DownloadView::group_pause_all,
            ));
        }
        if counts.any_resumable() {
            menu = menu.item(group_menu_item(
                self.strings.group_resume_all.clone(),
                FluxIcon::Play,
                None,
                host,
                group_id,
                DownloadView::group_resume_all,
            ));
        }
        if counts.failed > 0 {
            menu = menu.item(group_menu_item(
                self.strings.group_retry_failed.clone(),
                FluxIcon::RotateCw,
                None,
                host,
                group_id,
                DownloadView::group_retry_failed,
            ));
        }
        menu = menu.item(group_menu_item(
            self.strings.group_open_folder.clone(),
            FluxIcon::FolderOpen,
            None,
            host,
            group_id,
            DownloadView::group_open_folder,
        ));
        if has_source {
            menu = menu.item(group_menu_item_windowed(
                self.strings.group_copy_source_link.clone(),
                FluxIcon::Copy,
                None,
                host,
                group_id,
                DownloadView::group_copy_source_link,
            ));
        }
        menu = menu.separator();
        menu = menu.item(group_menu_item(
            self.strings.group_delete.clone(),
            FluxIcon::Trash2,
            Some(destructive),
            host,
            group_id,
            DownloadView::group_delete,
        ));
        menu = menu.item(group_menu_item_windowed(
            self.strings.group_delete_with_files.clone(),
            FluxIcon::Trash2,
            Some(destructive),
            host,
            group_id,
            DownloadView::confirm_group_delete_with_files,
        ));
        menu.item(group_menu_item_windowed(
            self.strings.open_group_in_window.clone(),
            FluxIcon::AppWindow,
            None,
            host,
            group_id,
            DownloadView::open_group_in_window,
        ))
    }

    /// `start`：这次按下可以开始划选。分组标题 / 目录行只跟踪经过，避免按下就改选区。
    pub(super) fn arm_row_sweep(
        row: Stateful<Div>,
        row_ix: usize,
        start: bool,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let row = if start {
            row.capture_any_mouse_down(cx.listener(|table, event: &MouseDownEvent, _, _| {
                if event.button == MouseButton::Left {
                    table.delegate_mut().press_on_file_handle = false;
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |table, event: &MouseDownEvent, _, cx| {
                    table.delegate_mut().begin_row_sweep(row_ix, event, cx);
                }),
            )
        } else {
            row
        };
        row.on_mouse_move(cx.listener(move |table, event: &MouseMoveEvent, _, cx| {
            if table.delegate_mut().drag_sweep_to(row_ix, event) {
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|table, _: &MouseUpEvent, _, _| {
                table.delegate_mut().finish_row_sweep();
            }),
        )
    }

    /// 文件拖出把手：捕获阶段记下按下，[`Self::begin_row_sweep`] 据此不开划选会话。
    /// gpui 超过约 2px 即启动拖放，方向不限，所以把手不能和划选共用一次移动。
    ///
    /// 按下不得在冒泡阶段截断：gpui 的单击由冒泡阶段的按下登记，截断后行的 `on_click`
    /// 收不到，文件名列就点不中、双击也打不开。没移动就松开 = 行单击；拖起后的那次
    /// 松开由 [`Self::note_file_drag_started`] 抑制，不改选区。
    fn arm_file_drag(
        cell: Stateful<Div>,
        dragged: DraggedTasks,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        cell.capture_any_mouse_down(cx.listener(|table, event: &MouseDownEvent, _, _| {
            if event.button == MouseButton::Left {
                table.delegate_mut().press_on_file_handle = true;
            }
        }))
        .on_drag(dragged, |dragged, click_offset, _, cx| {
            dragged.preview(click_offset, cx)
        })
        .external_drag_payload(|dragged: &DraggedTasks, _, cx| dragged.external_payload(cx))
    }
}

impl TableDelegate for DownloadTableDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.shown_columns_count() + 1
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.visible.len()
    }

    /// 空态：图标 + 标题 + 引导；任务表分页载入期间改为加载态（行集合只是占位，
    /// 不能说「暂无下载任务」）。
    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let (title, subtitle) = match self.store.loading() {
            Some(total) => (
                self.strings.tasks_loading_title.clone(),
                self.strings.format_tasks_loading_subtitle(total),
            ),
            None => (
                self.strings.empty_title.clone(),
                self.strings.empty_subtitle.clone(),
            ),
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(tokens.spacing.sm)
            .child(
                Icon::new(FluxIcon::Download)
                    .size(px(32.))
                    .text_color(theme.extended().colors.text_tertiary),
            )
            .child(
                div()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tokens.colors.foreground)
                    .child(title),
            )
            .child(
                div()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .text_color(tokens.colors.muted_foreground)
                    .child(subtitle),
            )
    }

    /// 排序指示由 `render_th` 自绘，列不设 `sort`（去掉 DataTable 常驻排序图标）。
    fn column(&self, col_ix: usize, cx: &App) -> Column {
        // 列的最小宽 / 默认宽随文字放大；记下本次应用的缩放，主题变化时由 prepaint 触发重建。
        let text_scale = f32::from(active_theme(cx).text_extent(1.));
        self.text_scale.set(text_scale);
        if col_ix == 0 {
            // 不设 `fixed_left`：gpui-component 会给固定列画一条 `border` 色竖线，
            // 而文件名列已吸收剩余宽度，常规列集不会出现横向滚动。
            self.icon_base
                .set(f32::from(active_theme(cx).extended().icon.lg));
            let width = px(self.selection_column_width());
            return Column::new("selection", "")
                .width(width)
                .min_width(width)
                .max_width(width)
                .resizable(false)
                .movable(false)
                .selectable(false)
                .p_0();
        }

        let Some(column) = self.shown_column(col_ix) else {
            return Column::new("missing", "").resizable(false).movable(false);
        };
        let base = Column::new(column.kind.key(), column.kind.label(&self.strings))
            .min_width(px(column.kind.min_width() * text_scale))
            .paddings(Edges {
                top: px(0.),
                right: px(CELL_PADDING_X),
                bottom: px(0.),
                left: px(CELL_PADDING_X),
            });
        if column.kind == DownloadColumnKind::FileName {
            // 默认吸收剩余宽度（其他列拖宽后由它补足）；拖过后固定为用户宽度。
            return base
                .width(px(self.apply_file_name_width()))
                .max_width(px(FILE_NAME_MAX_WIDTH));
        }
        base.width(px(column.kind.display_width(column.width, text_scale)))
            .max_width(px(MAX_COLUMN_WIDTH * text_scale))
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if col_ix == 0 {
            // 非多选模式（如普通单击选中一行）表头全选框按未选处理，不随单选点亮。
            let state = if self.in_multi_select() {
                match self.visible_selection_counts() {
                    (0, _) => CheckState::Unchecked,
                    (selected, total) if selected == total => CheckState::Checked,
                    _ => CheckState::Indeterminate,
                }
            } else {
                CheckState::Unchecked
            };
            let hover_border = active_theme(cx).tokens().colors.foreground.opacity(0.7);
            // 全选框平时隐藏，悬停表头该格或已有选中时出现，避免表头常驻一个空框。
            return h_flex()
                .id("select-all-download-tasks")
                .group(SELECT_ALL_GROUP)
                .size_full()
                .justify_center()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .when(state == CheckState::Unchecked, |this| {
                            this.invisible()
                                .group_hover(SELECT_ALL_GROUP, |style| style.visible())
                        })
                        .child(check_mark(state, cx).when(
                            state == CheckState::Unchecked,
                            |mark| {
                                mark.group_hover(SELECT_ALL_GROUP, move |style| {
                                    style.border_color(hover_border)
                                })
                            },
                        )),
                )
                .on_click(cx.listener(move |table, _: &ClickEvent, _, cx| {
                    let delegate = table.delegate_mut();
                    if state == CheckState::Checked {
                        delegate.clear_selection();
                    } else {
                        delegate.select_all_tasks();
                    }
                    cx.notify();
                }))
                .into_any_element();
        }

        let Some(kind) = self.shown_column(col_ix).map(|column| column.kind) else {
            return div().into_any_element();
        };
        let sort_key = kind.sort_key();
        // 只有当前排序列显示方向箭头；智能（默认）排序不对应任何列。
        let arrow =
            sort_key
                .filter(|key| *key == self.prefs.sort_key)
                .map(|_| match self.prefs.sort_dir {
                    SortDir::Asc => FluxIcon::ArrowUp,
                    SortDir::Desc => FluxIcon::ArrowDown,
                });
        let on_click = sort_key.map(|_| {
            cx.listener(move |table, event: &ClickEvent, _, cx| {
                // 拖动列头换位后在原列松开也会产生 click，位移过大时不当作排序。
                if let ClickEvent::Mouse(click) = event {
                    let delta = click.up.position - click.down.position;
                    if f32::from(delta.x).abs() + f32::from(delta.y).abs() > HEADER_CLICK_SLOP {
                        return;
                    }
                }
                let delegate = table.delegate_mut();
                if !delegate.toggle_sort(kind) {
                    return;
                }
                delegate.refresh_view();
                // 偏好写回走宿主的防抖持久化；表格实体此刻正被更新，放到帧外调用。
                if let Some(host) = delegate.host.clone() {
                    cx.defer(move |cx| {
                        let Ok(()) = host.update(cx, |view, cx| view.schedule_persist_prefs(cx))
                        else {
                            // 视图已释放，结束这次回调而不再更新状态。
                            return;
                        };
                    });
                }
                cx.notify();
            })
        });
        let theme = active_theme(cx);
        let typography = &theme.tokens().typography;
        let extended = theme.extended();
        let label = div().min_w_0().truncate().child(kind.label(&self.strings));
        let arrow = arrow.map(|icon| Icon::new(icon).size(extended.icon.sm));
        // 每列右缘常驻一条短分隔线，提示此处可拖宽。gpui-component 的拖宽柄
        // 取 `table_row_border`（为去网格线已设透明，且同色还控制行线与填充行，不能改），
        // 仅悬停时显示；这里自绘同位置的线：th 内容右缘向外偏 `CELL_PADDING_X`
        // 恰为单元格右缘，与拖宽柄的 1px 线重合，悬停时被其全高高亮覆盖。
        let divider = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(-CELL_PADDING_X))
            .flex()
            .items_center()
            .child(
                div()
                    .w(extended.stroke.thin)
                    .h(px(COLUMN_DIVIDER_HEIGHT))
                    .bg(extended.colors.hairline),
            );
        h_flex()
            .id(("download-column-header", col_ix))
            .size_full()
            .relative()
            .min_w_0()
            .gap(theme.tokens().spacing.xxs)
            .text_size(typography.xs.size)
            .line_height(typography.xs.line_height)
            .font_weight(FontWeight::MEDIUM)
            .text_color(extended.colors.text_tertiary)
            // 表头对齐按视图偏好（默认左对齐）；右对齐时箭头放在标签左侧，保持标签贴右缘。
            .map(|this| match self.prefs.header_align {
                HeaderAlign::Left => this.child(label).children(arrow),
                HeaderAlign::Center => this.justify_center().child(label).children(arrow),
                HeaderAlign::Right => this.justify_end().children(arrow).child(label),
            })
            .when_some(on_click, |this, on_click| {
                this.cursor_pointer().on_click(on_click)
            })
            .child(divider)
            .into_any_element()
    }

    /// 行高按密度覆盖 DataTable 的统一尺寸（DataTable 用 `refine_style` 合并本样式，
    /// 高度以这里为准）；分组头行与任务行等高（uniform_list 要求）。
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        self.trailing_actions = None;
        let theme = active_theme(cx);
        let row_height = self.prefs.density.row_height(theme.density());
        let id = match self.visible.get(row_ix) {
            Some(VisibleRow::Task(id) | VisibleRow::Member { id, .. }) => *id,
            Some(VisibleRow::Group(group)) => {
                let group = *group;
                self.trailing_actions = Some(groups::Entity::Group(group));
                return self.render_group_tr(row_ix, group, cx);
            }
            Some(VisibleRow::GroupDir { .. }) => return self.render_group_dir_tr(row_ix, cx),
            Some(VisibleRow::GroupHeader { .. }) | None => {
                return div().id(("download-group-row", row_ix)).h(row_height);
            }
        };
        let Some(key) = self.store.row(id).map(|row| row.key.clone()) else {
            return div().id(("download-task-row", row_ix)).h(row_height);
        };
        let selected = self.selected_tasks.contains(&key);
        let pending = self.is_pending_delete(&key);
        let (accent, radius) = (
            theme.tokens().colors.accent,
            theme.components().task_row_radius,
        );
        // 双击：已完成打开文件，其余打开详情。独立于选择监听注册（普通闭包，不占用表格
        // 实体），这样 DownloadView 处理时可以自由读写表格状态。
        let activate = self.host.clone().filter(|_| !pending).map(|host| {
            let key = key.clone();
            move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
                let modifiers = event.modifiers();
                if event.click_count() != 2 || modifiers.secondary(cx) || modifiers.shift {
                    return;
                }
                let key = key.clone();

                let Ok(()) = host.update(cx, |view, cx| view.activate_row(key, window, cx)) else {
                    // 视图已释放，结束这次回调而不再更新状态。
                    return;
                };
            }
        });
        self.trailing_actions = Some(groups::Entity::Task(id));

        let row = div()
            .id(("download-task-row", row_ix))
            .h(row_height)
            .group(ROW_GROUP)
            .relative()
            .when(pending, |this| this.opacity(PENDING_DELETE_OPACITY))
            // 覆盖 DataTable 自带的满宽悬浮底色，两种状态共用内缩背景。
            .child(div().absolute().inset_0().bg(theme.tokens().colors.surface))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(SELECTED_INSET_X))
                    .right(px(SELECTED_INSET_X))
                    .rounded(radius)
                    .when(selected, |this| this.bg(accent))
                    .when(!selected, |this| {
                        this.group_hover(ROW_GROUP, |style| {
                            style.bg(theme.extended().colors.row_hover)
                        })
                    }),
            )
            .on_click(cx.listener(move |table, event: &ClickEvent, _, cx| {
                if table.delegate_mut().take_sweep_suppress(cx) {
                    return;
                }
                table.delegate_mut().select_task(
                    key.clone(),
                    SelectModifiers::from_event(event.modifiers(), cx),
                );
                cx.notify();
            }))
            .when_some(activate, |this, activate| this.on_click(activate));
        Self::arm_row_sweep(row, row_ix, true, cx)
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let (id, depth) = match self.visible.get(row_ix) {
            Some(VisibleRow::Task(id)) => (*id, 0),
            Some(VisibleRow::Member { id, depth }) => (*id, *depth),
            Some(VisibleRow::Group(group)) => {
                let group = *group;
                return self.render_group_td(row_ix, col_ix, group, cx);
            }
            Some(VisibleRow::GroupDir { .. }) => {
                return self.render_group_dir_td(row_ix, col_ix, cx);
            }
            Some(VisibleRow::GroupHeader {
                key,
                label,
                count,
                collapsed,
            }) => {
                if col_ix == 1 {
                    let (key, label, count, collapsed) =
                        (key.clone(), label.clone(), *count, *collapsed);
                    return self.render_group_header(row_ix, &key, &label, count, collapsed, cx);
                }
                return div().into_any_element();
            }
            None => return div().into_any_element(),
        };
        let Some(task) = self.store.row(id) else {
            return div().into_any_element();
        };
        let task: &DownloadTaskView = &task;

        if col_ix == 0 {
            return self.render_selection_cell(row_ix, task, depth > 0, window, cx);
        }

        let Some(kind) = self.shown_column(col_ix).map(|column| column.kind) else {
            return div().into_any_element();
        };
        if kind == DownloadColumnKind::FileName {
            return self.render_file_name_cell(row_ix, task, depth, window, cx);
        }
        self.task_cell(kind, task, cx)
    }

    /// 右侧留白与选择列内含的左侧留白对称；任务 / 组行在此挂行悬停操作（见
    /// `render_row_actions` / `render_group_actions`）。
    fn render_last_empty_col(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let actions = match self.trailing_actions.take() {
            Some(groups::Entity::Task(id)) => self
                .store
                .row(id)
                .and_then(|task| self.render_row_actions(&task, cx)),
            Some(groups::Entity::Group(ix)) => self
                .group_rows
                .get(ix)
                .and_then(|group| self.render_group_actions(ix, group, cx)),
            None => None,
        };
        h_flex()
            .w(px(TABLE_TRAILING_GUTTER))
            .h_full()
            .flex_shrink_0()
            .children(actions)
    }

    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        self.move_shown_column(col_ix, to_ix);
    }

    /// 右键选中必须在这里完成：gpui-component 在鼠标冒泡阶段先 `window.defer` 排队构建
    /// 菜单，随后行监听才 emit `RightClickedRow`；effect 按 FIFO 执行，订阅者收到事件时
    /// 菜单早已按旧选区构建完毕（无选中 → 空菜单不显示，只剩复选框常显；已选他行 →
    /// 菜单作用于旧选区）。`right_clicked_row` 在派发时同步写入，构建时可靠。
    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let menu = match self.action_context.clone() {
            Some(handle) => menu.action_context(handle),
            None => menu,
        };
        match self.visible.get(row_ix) {
            Some(VisibleRow::Group(group)) => {
                let Some(group_id) = self.group_rows.get(*group).map(|group| group.id.clone())
                else {
                    return menu;
                };
                self.select_group_for_context_menu(&group_id);
                cx.notify();
                self.group_context_menu(
                    &group_id,
                    menu,
                    active_theme(cx).tokens().colors.destructive,
                )
            }
            Some(VisibleRow::Task(_) | VisibleRow::Member { .. }) => {
                if let Some(key) = self.row_key_at(row_ix) {
                    self.select_task_for_context_menu(key);
                    cx.notify();
                }
                self.task_context_menu(menu, active_theme(cx).tokens().colors.destructive)
            }
            Some(VisibleRow::GroupDir { .. } | VisibleRow::GroupHeader { .. }) | None => menu,
        }
    }
}

/// 左键划选会话。文件名列的按下不进这里。
struct RowSweep {
    origin_ix: usize,
    origin_pos: Point<Pixels>,
    additive: bool,
    base: HashSet<RowKey>,
    active: bool,
}

/// 一次行内左键按下归谁。文件名列是拖出，其他列是划选；都不需要修饰键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowPress {
    /// 文件名列：任意方向交给平台文件拖放。
    FileDrag,
    /// 其余列：划选。
    Sweep,
}

fn classify_row_press(on_file_name: bool) -> RowPress {
    if on_file_name {
        RowPress::FileDrag
    } else {
        RowPress::Sweep
    }
}

fn sweep_past_slop(delta: Point<Pixels>) -> bool {
    let dx = f32::from(delta.x);
    let dy = f32::from(delta.y);
    dx * dx + dy * dy > 36.
}

/// 单击选择时的修饰键语义：`toggle` = 平台次要键（macOS Cmd / 其他 Ctrl，运行期按 `App` 判定），
/// 把单行 / 整组并入或移出选区；`shift` = 从锚点连选。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SelectModifiers {
    pub(crate) shift: bool,
    pub(crate) toggle: bool,
}

impl SelectModifiers {
    pub(crate) fn from_event(modifiers: Modifiers, cx: &App) -> Self {
        Self {
            shift: modifiers.shift,
            toggle: modifiers.secondary(cx),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ToolbarCommand {
    Resume,
    Pause,
    PauseAll,
    ResumeAll,
    RetryFailed,
    Delete,
    Open,
    Reveal,
}

/// 选中集合投影（选择条 / 工具栏 / 快捷键）：只统计仍存在于 store 的选中任务。
/// - `count`：选中数量；`any`：是否有选中（删除 / 取消选择）。
/// - `any_local`：含本地任务（在文件夹中显示；远程任务没有本机文件）。
/// - `all_openable`：全部为本地已完成且文件仍在下载目录的任务（打开文件；未完成的产物
///   尚不存在，被删除 / 移走的已找不到）。
/// - `any_active`：含下载中 / 排队 / 做种中（暂停）；`any_resumable`：含暂停 / 失败 /
///   可恢复做种（继续）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SelectionSummary {
    pub(crate) count: usize,
    pub(crate) any: bool,
    pub(crate) any_local: bool,
    pub(crate) all_openable: bool,
    pub(crate) any_active: bool,
    pub(crate) any_resumable: bool,
}

/// 行悬停操作；除「详情」外执行时映射为 [`ToolbarCommand`]，与工具栏 / 右键菜单同一命令路径。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowAction {
    Pause,
    Resume,
    /// 失败任务的「继续」，图标换成重试。
    Retry,
    Open,
    Reveal,
    /// 在停靠面板中查看详情（与右键「详情」同一入口）。
    Detail,
}

impl RowAction {
    /// 对应的工具栏命令；「详情」不是任务命令，返回 `None`。
    fn command(self) -> Option<ToolbarCommand> {
        match self {
            Self::Pause => Some(ToolbarCommand::Pause),
            Self::Resume | Self::Retry => Some(ToolbarCommand::Resume),
            Self::Open => Some(ToolbarCommand::Open),
            Self::Reveal => Some(ToolbarCommand::Reveal),
            Self::Detail => None,
        }
    }

    /// 远程任务上对应的云端动作（打开 / 显示 / 详情没有远程对应）。
    fn remote_action(self) -> Option<RemoteCommandAction> {
        match self {
            Self::Pause => Some(RemoteCommandAction::Pause),
            Self::Resume | Self::Retry => Some(RemoteCommandAction::Resume),
            Self::Open | Self::Reveal | Self::Detail => None,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Pause => "download-row-action-pause",
            Self::Resume => "download-row-action-resume",
            Self::Retry => "download-row-action-retry",
            Self::Open => "download-row-action-open",
            Self::Reveal => "download-row-action-reveal",
            Self::Detail => "download-row-action-detail",
        }
    }
}

/// 行悬停操作集合：下载中 / 排队 → 暂停；暂停 → 继续；失败 → 重试；完成 → 打开
/// 文件（仅本地，且文件仍在下载目录）；做种中的完成任务先给「暂停」、做种已停止 / 达标的
/// 先给「继续」；本地任务再加「在文件夹中显示」，`with_detail`
/// 时最后加「详情」（远程任务没有本机详情）。远程任务只给可用操作：已结束的远程任务
/// （失败 / 取消 / 完成）不能「继续」——云端只接受暂停中任务的继续。
pub(crate) fn row_actions(
    task: &DownloadTaskView,
    is_local: bool,
    with_detail: bool,
) -> impl Iterator<Item = RowAction> {
    let primary = match task.state {
        TaskState::Downloading | TaskState::Pending => Some(RowAction::Pause),
        TaskState::Paused => Some(RowAction::Resume),
        TaskState::Failed => is_local.then_some(RowAction::Retry),
        TaskState::Completed => (is_local && !task.file_missing).then_some(RowAction::Open),
    };
    let seeding = if task.is_seeding() {
        Some(RowAction::Pause)
    } else if task.can_resume_seeding() {
        Some(RowAction::Resume)
    } else {
        None
    };
    seeding
        .into_iter()
        .chain(primary)
        .chain(is_local.then_some(RowAction::Reveal))
        .chain((is_local && with_detail).then_some(RowAction::Detail))
}

/// 单任务命令：本地任务走 daemon；远程任务只有暂停 / 继续 / 删除经 `agent.remote.command`
/// 转发，且仅限云端状态允许该动作的任务（状态未知的任务不控制）。打开 / 显示没有本机
/// 文件、全局命令不属于单任务，返回 `None`。已完成的本地任务只在做种语义下响应：做种中可
/// 「暂停」（停止做种）、做种已停止 / 达标且产物仍在可「继续」（恢复做种）；其余已完成任务不发
/// 「继续」：引擎对完成态的 resume 会从头重新下载（#935），批量选择里混入的已完成任务必须跳过。
fn task_command(row: &DownloadTaskView, action: ToolbarCommand) -> Option<DownloadsCommand> {
    let task_id = row.key.task_id().to_owned();
    if !row.key.is_local() {
        let remote_action = match action {
            ToolbarCommand::Resume => RemoteCommandAction::Resume,
            ToolbarCommand::Pause => RemoteCommandAction::Pause,
            ToolbarCommand::Delete => RemoteCommandAction::Delete,
            ToolbarCommand::Open
            | ToolbarCommand::Reveal
            | ToolbarCommand::PauseAll
            | ToolbarCommand::ResumeAll
            | ToolbarCommand::RetryFailed => return None,
        };
        return row
            .remote_can(remote_action)
            .then_some(DownloadsCommand::RemoteCommand(RemoteCommandParams {
                task_id,
                action: remote_action,
                command_id: None,
                delete_files: false,
            }));
    }
    Some(match action {
        ToolbarCommand::Resume
            if row.state == TaskState::Completed && !row.can_resume_seeding() =>
        {
            return None;
        }
        ToolbarCommand::Resume => DownloadsCommand::Resume { task_id },
        ToolbarCommand::Pause if row.state == TaskState::Completed && !row.is_seeding() => {
            return None;
        }
        ToolbarCommand::Pause => DownloadsCommand::Pause { task_id },
        ToolbarCommand::Delete => DownloadsCommand::Delete {
            task_id,
            delete_files: false,
        },
        ToolbarCommand::Open => DownloadsCommand::OpenTask { task_id },
        ToolbarCommand::Reveal => DownloadsCommand::RevealTask { task_id },
        ToolbarCommand::PauseAll | ToolbarCommand::ResumeAll | ToolbarCommand::RetryFailed => {
            return None;
        }
    })
}

/// 进度条颜色：下载中 `progressFill`、失败 `statusFailed`、暂停为 `statusPaused` 的 40%、
/// 排队 / 完成取对应状态色。主表格与详情窗口共用。
pub(crate) fn progress_bar_color(state: TaskState, cx: &App) -> Hsla {
    let colors = &active_theme(cx).extended().colors;
    match state {
        TaskState::Downloading => colors.progress_fill,
        TaskState::Paused => colors.status_paused.opacity(PAUSED_BAR_ALPHA),
        TaskState::Failed => colors.status_failed,
        TaskState::Pending => colors.status_queued,
        TaskState::Completed => colors.status_completed,
    }
}

/// 进度轨道颜色（`progressTrack`）。主表格与详情窗口共用。
pub(crate) fn progress_track_color(cx: &App) -> Hsla {
    active_theme(cx).extended().colors.progress_track
}

/// 状态文字色（`status*`）：默认只有下载中（primary）与失败（destructive）着色，暂停为
/// 二级文字，排队 / 完成退为三级文字。主表格与详情窗口共用。
pub(crate) fn status_color(state: TaskState, cx: &App) -> Hsla {
    let colors = &active_theme(cx).extended().colors;
    match state {
        TaskState::Downloading => colors.status_downloading,
        TaskState::Failed => colors.status_failed,
        TaskState::Paused => colors.status_paused,
        TaskState::Pending => colors.status_queued,
        TaskState::Completed => colors.status_completed,
    }
}

/// 行级状态色：做种中 / 排队做种取 `success`（绿），已完成但文件已不在下载目录时取
/// `warning`，其余同 [`status_color`]。
pub(crate) fn task_status_color(task: &DownloadTaskView, cx: &App) -> Hsla {
    if task.is_file_missing() {
        active_theme(cx).extended().colors.warning
    } else if is_seeding_row(task) {
        active_theme(cx).extended().colors.success
    } else {
        status_color(task.state, cx)
    }
}

/// 本地已完成且正在（或排队）做种：`seeding_status` 1 = 做种中，8 = 排队做种。
fn is_seeding_row(task: &DownloadTaskView) -> bool {
    task.source == TaskSource::Local
        && task.state == TaskState::Completed
        && matches!(task.seeding_status, 1 | 8)
}

/// 做种中（`seeding_status == 1`）且有上传速度时的实时上传速率（字节/秒）。
fn seeding_upload_speed(task: &DownloadTaskView) -> Option<u64> {
    (is_seeding_row(task) && task.seeding_status == 1)
        .then_some(task.upload_speed_bytes_per_second)
        .filter(|speed| *speed > 0)
}

/// 已下 / 总量；总量未知时只给已下。
fn bytes_progress(task: &DownloadTaskView) -> String {
    if task.size_bytes > 0 {
        format!("{} / {}", format_bytes(task.downloaded_bytes), task.size)
    } else {
        format_bytes(task.downloaded_bytes)
    }
}

/// 整数百分比（向下取整，未完成不会显示 100%）；介于 0 与 1% 之间显示 `<1%`。
fn percent_label(progress: f32) -> SharedString {
    let percent = progress.clamp(0., 1.) * 100.;
    if percent > 0. && percent < 1. {
        SharedString::from("<1%")
    } else {
        SharedString::from(format!("{}%", percent.floor() as u32))
    }
}

/// 操作条不透明底色：与行选中 / 悬停叠色一致，避免半透明盖住「创建时间」等末列文字透出。
fn row_action_strip_bg(selected: bool, cx: &App) -> Hsla {
    let theme = active_theme(cx);
    let surface = theme.tokens().colors.surface;
    if selected {
        surface.blend(theme.tokens().colors.accent)
    } else {
        surface.blend(theme.extended().colors.row_hover)
    }
}

/// 把已拼好的操作按钮包进贴右缘的悬停浮层（不透明底 + 左侧渐隐）。
///
/// 父节点是右侧留白列：浮层右缘对齐其左缘，即单元格可视区右缘；横向滚动时不移出视口。
pub(super) fn wrap_row_action_strip(
    selected: bool,
    buttons: impl IntoIterator<Item = impl IntoElement>,
    cx: &App,
) -> AnyElement {
    let tokens = active_theme(cx).tokens();
    let strip_bg = row_action_strip_bg(selected, cx);
    h_flex()
        .absolute()
        .top_0()
        .bottom_0()
        .right(px(TABLE_TRAILING_GUTTER))
        .gap(tokens.spacing.xxs)
        .pl(tokens.spacing.xs)
        .bg(strip_bg)
        .invisible()
        .group_hover(ROW_GROUP, |style| style.visible())
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(-ROW_ACTION_FADE))
                .w(px(ROW_ACTION_FADE))
                .bg(linear_gradient(
                    90.,
                    linear_color_stop(strip_bg.opacity(0.), 0.),
                    linear_color_stop(strip_bg, 1.),
                )),
        )
        .children(buttons)
        .into_any_element()
}

/// 单行元信息单元格（xs）；数字列右对齐并用等宽数字。
fn meta_cell(text: impl Into<SharedString>, color: Hsla, numeric: bool, cx: &App) -> AnyElement {
    let typography = &active_theme(cx).tokens().typography;
    h_flex()
        .size_full()
        .min_w_0()
        .when(numeric, |this| {
            this.justify_end().font_features(tabular_numbers())
        })
        .text_size(typography.xs.size)
        .line_height(typography.xs.line_height)
        .text_color(color)
        .child(div().min_w_0().truncate().child(text.into()))
        .into_any_element()
}

/// 下载任务表（主窗口与任务组详情共用）：表头高取 `density.control`、行高按密度、表格底色 surface。
///
/// 文件名列吸收剩余宽度：prepaint 测得容器宽后只做比较，确需更新时用
/// `window.defer` 在帧外写入代理并 `refresh`（不在 prepaint 里同步更新实体），
/// 写入后期望值与已应用值一致，下一帧不再触发，避免逐帧重算或抖动。
///
/// 行顺序稳定：表格内指针活动推迟内容变化引起的重排；prepaint 发现有被推迟的重排
/// 时安排补偿定时器（[`arm_reorder_timer`]）。
pub(crate) fn render_download_table(
    id: &'static str,
    table_state: &Entity<TableState<DownloadTableDelegate>>,
    cx: &App,
) -> Stateful<Div> {
    let observed = table_state.clone();
    div()
        .id(id)
        .relative()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .bg(active_theme(cx).tokens().colors.surface)
        .on_mouse_move(note_pointer::<MouseMoveEvent>(table_state, cx))
        .on_scroll(note_pointer::<ScrollEvent>(table_state, cx))
        .capture_any_mouse_down(note_pointer::<MouseDownEvent>(table_state, cx))
        .capture_any_mouse_down(clear_context_row_on_left_press(table_state))
        .child(
            div().absolute().inset_0().child(
                DataTable::new(table_state)
                    .with_size(Size::Size(table_header_height(cx)))
                    .stripe(false)
                    .bordered(false)
                    .scrollbar_visible(true, true),
            ),
        )
        .on_prepaint(move |bounds, window, cx| {
            let width = f32::from(bounds.size.width);
            let text_scale = f32::from(active_theme(cx).text_extent(1.));
            let icon_base = f32::from(active_theme(cx).extended().icon.lg);
            let delegate = observed.read(cx).delegate();
            let sync_width = delegate.viewport_needs_sync(width);
            let rescale =
                delegate.text_scale_stale(text_scale) || delegate.icon_base_stale(icon_base);
            let arm_timer = delegate.needs_reorder_timer();
            if !sync_width && !rescale && !arm_timer {
                return;
            }
            window.defer(cx, move |_, cx| {
                observed.update(cx, |table, cx| {
                    // 界面缩放 / 字号 / 图标尺寸变化后列的最小宽、默认宽、选择列宽随之变化，重建列。
                    let resized = sync_width && table.delegate_mut().set_viewport_width(width);
                    if resized || rescale {
                        table.refresh(cx);
                    }
                    if arm_timer {
                        arm_reorder_timer(table, cx);
                    }
                });
            });
        })
}

/// 表格指针事件监听：只记录活动时刻，不写表格 entity、不触发重绘。
fn note_pointer<E: 'static>(
    table_state: &Entity<TableState<DownloadTableDelegate>>,
    cx: &App,
) -> impl Fn(&E, &mut Window, &mut App) + 'static {
    let hold = table_state.read(cx).delegate().interaction_hold();
    move |_, _, _| hold.note(Instant::now())
}

/// 表格内左键按下即清除右键高亮行。
///
/// 两张表都 `row_selectable(false)`，选择由委托自管，DataTable 内置的「左键点行清除
/// `right_clicked_row`」分支因此不会执行；它自带的 `on_mouse_down_out` 又只管表格外的
/// 点击。不补这一步，右键过的行边框会一直残留（改选别的任务也不消失），
/// [`arm_reorder_timer`] 也会把它当作菜单仍开着而无限顺延重排。
///
/// 捕获阶段注册：勾选框 / 行内按钮在冒泡阶段 `stop_propagation` 也拦不住。只认左键，
/// 右键另一行由 DataTable 自己改写高亮；菜单浮层 `occlude`，点菜单项不会走到这里，
/// 而菜单是右键当帧 `window.defer` 构建的，此时清除不影响菜单内容。
fn clear_context_row_on_left_press(
    table_state: &Entity<TableState<DownloadTableDelegate>>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    let table_state = table_state.clone();
    move |event, _, cx| {
        if event.button != MouseButton::Left {
            return;
        }
        // 先读后写：没有高亮行时不写表格，免得每次左键都让 gpui-fast 把表格记为已变化。
        if table_state.read(cx).right_clicked_row().is_none() {
            return;
        }
        table_state.update(cx, |table, cx| table.set_right_clicked_row(None, cx));
    }
}

/// 跨过本地零点时刷新按日期分组的表格；表格释放后循环随 `update` 失败结束。
///
/// 日期分桶只在重算可见行时读取时钟，没有任务事件的夜里不会自己换天。
pub(crate) fn spawn_midnight_refresh(
    table_state: &Entity<TableState<DownloadTableDelegate>>,
    cx: &mut App,
) {
    let table = table_state.downgrade();
    cx.spawn(async move |cx| {
        loop {
            let delay = until_next_local_day(chrono::Local::now());
            cx.background_executor().timer(delay).await;
            let Ok(()) = table.update(cx, |table, cx| {
                if table.delegate_mut().refresh_for_new_day() {
                    cx.notify();
                }
            }) else {
                // 表格已释放，结束跨天刷新。
                return;
            };
        }
    })
    .detach();
}

/// 距下一个本地日期开始的时长（多留 1 秒确保已换天）；时区换算失败时 1 分钟后复查。
fn until_next_local_day(now: chrono::DateTime<chrono::Local>) -> Duration {
    const RECHECK: Duration = Duration::from_secs(60);
    let Some(next_day) = now.date_naive().succ_opt() else {
        return RECHECK;
    };
    next_day
        .and_hms_opt(0, 0, 1)
        .and_then(|at| at.and_local_timezone(chrono::Local).earliest())
        .and_then(|at| (at - now).to_std().ok())
        .unwrap_or(RECHECK)
}

/// 为被推迟的重排安排定时器。到期时右键菜单仍开着（高亮行按下标定位，重排会让高亮
/// 落到别的任务上）或指针又活动过则顺延；否则强制按最新排序重排一次。
fn arm_reorder_timer(
    table: &mut TableState<DownloadTableDelegate>,
    cx: &mut Context<TableState<DownloadTableDelegate>>,
) {
    let delegate = table.delegate_mut();
    if delegate.reorder_timer.is_some() {
        return;
    }
    let Some(deadline) = delegate.reorder_deadline() else {
        return;
    };
    let delay = deadline.saturating_duration_since(Instant::now());
    delegate.reorder_timer = Some(cx.spawn(async move |this, cx| {
        cx.background_executor().timer(delay).await;

        let Ok(()) = this.update(cx, |table, cx| {
            let menu_open = table.right_clicked_row().is_some();
            let now = Instant::now();
            let delegate = table.delegate_mut();
            delegate.reorder_timer = None;
            if menu_open {
                delegate.row_order.note_interaction(now);
            }
            match delegate.reorder_deadline() {
                Some(deadline) if deadline > now => arm_reorder_timer(table, cx),
                Some(_) => {
                    delegate.view_dirty = true;
                    if delegate.refresh_view_at(now) {
                        cx.notify();
                    }
                }
                None => {}
            }
        }) else {
            // 视图已释放，结束这次回调而不再更新状态。
            return;
        };
    }));
}

impl DownloadView {
    /// 选中集合 → 命令列表（远程任务走 `agent.remote.command`）。
    pub(crate) fn commands_for_selection(
        &self,
        action: ToolbarCommand,
        cx: &Context<Self>,
    ) -> Vec<DownloadsCommand> {
        let store = self.controller.store();
        let commands_for = |keys: Vec<RowKey>| -> Vec<DownloadsCommand> {
            keys.iter()
                .filter_map(|key| {
                    let row = store.get(key)?;
                    task_command(&row, action)
                })
                .collect()
        };
        match action {
            ToolbarCommand::PauseAll => vec![DownloadsCommand::PauseAll],
            ToolbarCommand::ResumeAll => vec![DownloadsCommand::ResumeAll],
            ToolbarCommand::RetryFailed => self.controller.retry_failed_commands(),
            ToolbarCommand::Open => commands_for(
                self.table_state
                    .read(cx)
                    .delegate()
                    .openable_selected_keys(),
            ),
            _ => commands_for(self.table_state.read(cx).delegate().selected_keys()),
        }
    }

    /// 「删除」命令：本地走 daemon（`delete_files` 决定是否删文件）；远程任务经
    /// `agent.remote.command`（`delete_files` 同样透传给目标设备），状态未知的任务不删。
    pub(crate) fn delete_commands(
        &self,
        keys: &[RowKey],
        delete_files: bool,
    ) -> Vec<DownloadsCommand> {
        let store = self.controller.store();
        keys.iter()
            .filter_map(|key| {
                if key.is_local() {
                    return Some(DownloadsCommand::Delete {
                        task_id: key.task_id().to_owned(),
                        delete_files,
                    });
                }
                let row = store.get(key)?;
                row.remote_can(RemoteCommandAction::Delete).then(|| {
                    DownloadsCommand::RemoteCommand(RemoteCommandParams {
                        task_id: key.task_id().to_owned(),
                        action: RemoteCommandAction::Delete,
                        command_id: None,
                        delete_files,
                    })
                })
            })
            .collect()
    }

    pub(crate) fn execute_toolbar(&mut self, action: ToolbarCommand, cx: &mut Context<Self>) {
        if matches!(action, ToolbarCommand::PauseAll | ToolbarCommand::ResumeAll) {
            let command = if matches!(action, ToolbarCommand::PauseAll) {
                DownloadsCommand::PauseAll
            } else {
                DownloadsCommand::ResumeAll
            };
            // The host must close import admission during the click handler,
            // not after another async submission has been scheduled.
            let future = self.port.execute(command);
            cx.spawn(async move |this, cx| {
                let result = future.await;
                if let Err(error) = this.update(cx, |this, cx| {
                    this.last_error = result.as_ref().err().map(|error| {
                        SharedString::from(error_text(this.translator.read(cx), error))
                    });
                    cx.notify();
                }) {
                    eprintln!("download view released before global command reply: {error:#}");
                }
            })
            .detach();
            return;
        }
        let commands = self.commands_for_selection(action, cx);
        self.execute_commands(commands, cx);
    }

    /// 执行一批命令。本地多任务删除先拆出走分块删除（见 [`crate::deletion`]）；同类多任务
    /// 的暂停 / 继续 / 远程删除先合并成批量 RPC
    /// （见 [`coalesce_commands`]），其余命令按 [`MAX_IN_FLIGHT`] 限制并发；幂等命令被
    /// agent 以可重试的 `Unavailable` 拒绝时退避重试。任一失败在页面横幅提示（按错误
    /// `reason` 给出原因，同一批里先失败的错误一直保留，后完成的成功不会冲掉它）。
    /// 单任务「继续」/「重新下载」在命令前登记旧状态、成功后确认开始；
    /// 批量选择不逐个弹进度窗口。
    pub(crate) fn execute_commands(
        &mut self,
        commands: Vec<DownloadsCommand>,
        cx: &mut Context<Self>,
    ) {
        // 本地多任务删除走分块 + 进度 + 取消的专用路径；单任务删除与远程删除保持原路径。
        let (deletions, commands) = split_local_deletes(commands, MIN_BATCH_DELETE);
        for request in deletions {
            self.start_deletion(request, cx);
        }
        let interactive_start = match commands.as_slice() {
            [DownloadsCommand::Resume { task_id }] => Some((task_id.clone(), false)),
            [DownloadsCommand::Redownload(request, task_id)] if !request.start_paused => {
                Some((task_id.clone(), true))
            }
            _ => None,
        };
        let on_user_started = self.host.on_user_started.clone();
        if let (Some((task_id, replaces_task)), Some(hook)) = (&interactive_start, &on_user_started)
        {
            hook(
                UserStartIntent::RestartRequested {
                    task_id: task_id.clone(),
                    replaces_task: *replaces_task,
                },
                cx,
            );
        }
        let queue = Rc::new(RefCell::new(VecDeque::from(coalesce_commands(commands))));
        let batch = Rc::new(RefCell::new(DispatchSummary::default()));
        let workers = queue.borrow().len().min(MAX_IN_FLIGHT);
        for _ in 0..workers {
            let queue = Rc::clone(&queue);
            let batch = Rc::clone(&batch);
            let interactive_start = interactive_start.clone();
            let on_user_started = on_user_started.clone();
            cx.spawn(async move |this, cx| {
                loop {
                    let Some(mut command) = queue.borrow_mut().pop_front() else {
                        break;
                    };
                    // 打开失败多半是文件已被删除 / 移走：立即重扫，让行上的丢失标记跟上磁盘现状。
                    let rescan_on_failure = matches!(command, DownloadsCommand::OpenTask { .. });
                    let mut retries = 0_u32;
                    let result = loop {
                        let replay = retry_copy(&command);
                        let Ok((future, stale)) = this.update(cx, |this, _| {
                            (this.controller.execute(command), this.controller.is_stale())
                        }) else {
                            if let (Some((task_id, _)), Some(hook)) =
                                (&interactive_start, &on_user_started)
                            {
                                cx.update(|cx| {
                                    hook(UserStartIntent::RestartFailed(task_id.clone()), cx)
                                });
                            }
                            return;
                        };
                        let result = future.await;
                        let wait = match (&result, replay) {
                            (Err(error), Some(replay)) if !stale => {
                                retry_delay(error, retries).map(|delay| (delay, replay))
                            }
                            _ => None,
                        };
                        let Some((delay, replay)) = wait else {
                            break result;
                        };
                        cx.background_executor().timer(delay).await;
                        command = replay;
                        retries += 1;
                    };
                    batch.borrow_mut().record(&result);
                    if let (Some((task_id, creates_new)), Some(hook)) =
                        (&interactive_start, &on_user_started)
                    {
                        cx.update(|cx| match &result {
                            Ok(result) if *creates_new => {
                                let created = result.created_task_ids();
                                if let [created_id] = created.as_slice() {
                                    if created_id == task_id {
                                        hook(
                                            UserStartIntent::RestartSucceeded(task_id.clone()),
                                            cx,
                                        );
                                    } else {
                                        hook(UserStartIntent::Created(created_id.clone()), cx);
                                        hook(UserStartIntent::RestartFailed(task_id.clone()), cx);
                                    }
                                } else {
                                    hook(UserStartIntent::RestartFailed(task_id.clone()), cx);
                                }
                            }
                            Ok(_) => hook(UserStartIntent::RestartSucceeded(task_id.clone()), cx),
                            Err(_) => hook(UserStartIntent::RestartFailed(task_id.clone()), cx),
                        });
                    }
                    let batch = Rc::clone(&batch);

                    let Ok(()) = this.update(cx, |this, cx| {
                        if rescan_on_failure && result.is_err() {
                            this.rescan_files_now(cx);
                        }
                        this.last_error = batch.borrow().first_error.as_ref().map(|error| {
                            SharedString::from(error_text(this.translator.read(cx), error))
                        });
                        cx.notify();
                    }) else {
                        // 视图已释放，结束这次回调而不再更新状态。
                        return;
                    };
                }
            })
            .detach();
        }
    }

    pub(crate) fn render_table(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        render_download_table("download-task-table-container", &self.table_state, cx)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        rc::Rc,
        time::{Duration, Instant},
    };

    use fluxdown_ui_i18n::{I18nCatalog, I18nError};

    use super::{
        DownloadColumnKind, DownloadTableDelegate, DownloadsCommand, RowAction, SelectModifiers,
        SelectionSummary, TableFilter, ToolbarCommand, VisibleRow, group_by_key, is_seeding_row,
        percent_label, render_download_table, row_actions, seeding_upload_speed,
        table_header_height, task_command,
    };
    use crate::{
        model::{
            CategoryIndex, DownloadFilter, DownloadStatusFilter, DownloadTaskView, RowKey,
            TaskSource, TaskState, TaskStore,
            group_tree::GroupMeta,
            view_prefs::{SortDir, ViewGroupBy, ViewPrefs, ViewSortKey},
        },
        strings::DownloadStrings,
    };

    fn delegate(statuses: &[i32]) -> Result<DownloadTableDelegate, I18nError> {
        delegate_with_missing(statuses, &[])
    }

    /// `missing` 中的下标对应任务带 `fileMissing: true`（文件跟踪判定产物已不在磁盘）。
    fn delegate_with_missing(
        statuses: &[i32],
        missing: &[usize],
    ) -> Result<DownloadTableDelegate, I18nError> {
        let catalog = std::sync::Arc::new(I18nCatalog::load_embedded()?);
        let strings = DownloadStrings::from_translator(&catalog.translator("en"));
        let store = Rc::new(TaskStore::default());
        let rows = statuses
            .iter()
            .enumerate()
            .map(|(index, status)| {
                let task =
                    serde_json::from_value::<fluxdown_protocol::TaskDto>(serde_json::json!({
                        "taskId": format!("t{index}"),
                        "url": "https://example.com/file",
                        "fileName": format!("f{index}.bin"),
                        "saveDir": "/tmp",
                        "status": status,
                        "downloadedBytes": 0,
                        "totalBytes": 1,
                        "errorMessage": "",
                        "createdAt": format!("{}", 100 - index),
                        "proxyUrl": "",
                        "queueId": "main",
                        "checksum": "",
                        "fileMissing": missing.contains(&index)
                    }))
                    .expect("task");
                DownloadTaskView::local(&task, None, false)
            })
            .collect();
        store.replace_local(rows);
        let mut delegate = DownloadTableDelegate::new(strings, store);
        delegate.refresh_view();
        Ok(delegate)
    }

    #[test]
    fn hide_source_drops_site_from_subtitle_but_keeps_category() -> Result<(), I18nError> {
        use crate::model::view_prefs::ViewDensity;

        let mut delegate = delegate(&[3])?;
        let task = delegate
            .store
            .get(&RowKey::Local("t0".into()))
            .expect("task t0")
            .clone();
        let category = delegate.kind_label(task.kind).to_string();

        for density in [ViewDensity::Comfortable, ViewDensity::Relaxed] {
            delegate.prefs_mut().density = density;
            delegate.set_hide_source(false);
            let shown = delegate.file_cell_meta(&task).map(String::from);
            assert!(
                shown
                    .as_deref()
                    .is_some_and(|text| text.contains("example.com")),
                "{density:?}: {shown:?}"
            );

            assert!(delegate.set_hide_source(true));
            let hidden = delegate.file_cell_meta(&task).map(String::from);
            assert!(
                hidden.as_deref().is_some_and(|text| {
                    !text.contains("example.com") && text.contains(&category)
                }),
                "{density:?}: {hidden:?}"
            );
            assert!(!delegate.set_hide_source(true));
        }
        Ok(())
    }

    #[test]
    fn context_menu_selection_preserves_selected_rows_and_replaces_unselected_rows()
    -> Result<(), I18nError> {
        let mut delegate = delegate(&[2, 2, 2])?;
        delegate
            .selected_tasks
            .extend([RowKey::Local("t0".into()), RowKey::Local("t1".into())]);

        delegate.select_task_for_context_menu(RowKey::Local("t1".into()));
        assert_eq!(
            delegate.selected_tasks,
            HashSet::from([RowKey::Local("t0".into()), RowKey::Local("t1".into())])
        );

        delegate.select_task_for_context_menu(RowKey::Local("t2".into()));
        assert_eq!(
            delegate.selected_tasks,
            HashSet::from([RowKey::Local("t2".into())])
        );
        Ok(())
    }

    #[test]
    fn checkbox_starts_multi_select_from_its_row_then_toggles() -> Result<(), I18nError> {
        let mut delegate = delegate(&[2, 2, 2])?;
        delegate.refresh_view();
        // 普通单击选中 t0 后点 t1 的复选框：以 t1 开启多选，不并入单击选中的行。
        delegate.select_task(RowKey::Local("t0".into()), SelectModifiers::default());
        delegate.toggle_multi_select(RowKey::Local("t1".into()));
        assert!(delegate.in_multi_select());
        assert_eq!(
            delegate.selected_tasks,
            HashSet::from([RowKey::Local("t1".into())])
        );
        delegate.toggle_multi_select(RowKey::Local("t2".into()));
        delegate.toggle_multi_select(RowKey::Local("t1".into()));
        assert_eq!(
            delegate.selected_tasks,
            HashSet::from([RowKey::Local("t2".into())])
        );
        // 取消最后一项即退出多选。
        delegate.toggle_multi_select(RowKey::Local("t2".into()));
        assert!(!delegate.in_multi_select());

        // 右键「选择」把右键选区（含 shift 范围选）转为多选模式。
        delegate.select_task_for_context_menu(RowKey::Local("t0".into()));
        assert!(!delegate.in_multi_select());
        delegate.enter_multi_select();
        assert!(delegate.in_multi_select());
        Ok(())
    }

    /// 任务表分页载入期间：侧栏「全部」给出服务端总数、其余不给计数，全选不作用于占位表。
    #[test]
    fn loading_state_reports_the_server_total_and_blocks_select_all() -> Result<(), I18nError> {
        let mut delegate = delegate(&[])?;
        assert_eq!(delegate.count_matching(&DownloadFilter::ALL), 0);
        delegate.store.set_loading(Some(230_000));
        assert_eq!(delegate.count_matching(&DownloadFilter::ALL), 230_000);
        assert_eq!(
            delegate.count_matching(&DownloadFilter::status(DownloadStatusFilter::Completed)),
            0
        );
        delegate.select_all_tasks();
        assert!(delegate.selected_tasks.is_empty());
        assert!(!delegate.in_multi_select());

        // 队列位置表单独重载时任务行仍在：计数照常，但全选同样被挡住。
        let mut delegate = delegate_with_missing(&[2, 2], &[])?;
        delegate.store.set_loading(Some(2));
        assert_eq!(delegate.count_matching(&DownloadFilter::ALL), 2);
        delegate.select_all_tasks();
        assert!(delegate.selected_tasks.is_empty());
        delegate.store.set_loading(None);
        delegate.select_all_tasks();
        assert_eq!(delegate.selected_tasks.len(), 2);
        Ok(())
    }

    #[test]
    fn pending_delete_rows_leave_the_selection_and_cannot_be_selected() -> Result<(), I18nError> {
        let mut delegate = delegate(&[2, 2, 2, 2])?;
        delegate.refresh_view();
        delegate.select_all_tasks();
        assert_eq!(delegate.selected_tasks.len(), 4);

        // 开始删除清空选区；已在途的任务不会被重复登记。
        let fresh = delegate.begin_delete(vec!["t0".into(), "t1".into()]);
        assert_eq!(fresh, ["t0", "t1"]);
        assert!(delegate.selected_tasks.is_empty());
        assert!(delegate.begin_delete(vec!["t1".into()]).is_empty());
        assert!(delegate.is_pending_delete(&RowKey::Local("t0".into())));
        assert!(!delegate.is_pending_delete(&RowKey::Remote("t0".into())));

        // 全选 / 点选 / 范围选 / 右键都跳过正在删除的行。
        delegate.select_all_tasks();
        assert_eq!(
            delegate.selected_tasks,
            HashSet::from([RowKey::Local("t2".into()), RowKey::Local("t3".into())])
        );
        delegate.select_task(RowKey::Local("t2".into()), SelectModifiers::default());
        delegate.select_task(RowKey::Local("t1".into()), SelectModifiers::default());
        assert_eq!(
            delegate.selected_tasks,
            HashSet::from([RowKey::Local("t2".into())])
        );
        delegate.select_task_for_context_menu(RowKey::Local("t0".into()));
        assert!(delegate.selected_tasks.is_empty());

        // 结束后恢复可选。
        delegate.finish_delete(&["t0".to_owned(), "t1".to_owned()]);
        delegate.select_task(RowKey::Local("t0".into()), SelectModifiers::default());
        assert_eq!(
            delegate.selected_tasks,
            HashSet::from([RowKey::Local("t0".into())])
        );
        Ok(())
    }

    #[test]
    fn selection_summary_counts_every_live_selected_task() -> Result<(), I18nError> {
        // t0 已暂停、t1 下载中。
        let mut delegate = delegate(&[2, 1])?;
        assert_eq!(delegate.selection_summary(), SelectionSummary::default());

        // 任务已被删除但选中集合仍残留其 key：不计入。
        delegate.selected_tasks.insert(RowKey::Local("gone".into()));
        assert_eq!(delegate.selection_summary(), SelectionSummary::default());

        delegate.selected_tasks.insert(RowKey::Local("t0".into()));
        assert_eq!(
            delegate.selection_summary(),
            SelectionSummary {
                count: 1,
                any: true,
                any_local: true,
                all_openable: false,
                any_active: false,
                any_resumable: true,
            }
        );

        // 遇到本地任务后仍需继续遍历，否则后续下载中任务的可暂停性会丢失。
        delegate.selected_tasks.insert(RowKey::Local("t1".into()));
        assert_eq!(
            delegate.selection_summary(),
            SelectionSummary {
                count: 2,
                any: true,
                any_local: true,
                all_openable: false,
                any_active: true,
                any_resumable: true,
            }
        );
        Ok(())
    }

    #[test]
    fn open_file_requires_every_selected_task_completed() -> Result<(), I18nError> {
        // t0 已完成、t1 已暂停（磁盘上只有 `.fdownloading`）。
        let mut delegate = delegate(&[3, 2])?;
        delegate.selected_tasks.insert(RowKey::Local("t0".into()));
        assert!(delegate.selection_summary().all_openable);
        assert_eq!(
            delegate.openable_selected_keys(),
            [RowKey::Local("t0".into())]
        );

        delegate.selected_tasks.insert(RowKey::Local("t1".into()));
        assert!(!delegate.selection_summary().all_openable);
        assert_eq!(
            delegate.openable_selected_keys(),
            [RowKey::Local("t0".into())]
        );
        Ok(())
    }

    #[test]
    fn missing_files_are_neither_openable_nor_draggable() -> Result<(), I18nError> {
        // t0 已完成、t1 已完成但文件已被删除 / 移走、t2 已暂停。
        let mut delegate = delegate_with_missing(&[3, 3, 2], &[1])?;
        delegate.selected_tasks.insert(RowKey::Local("t1".into()));
        assert!(!delegate.selection_summary().all_openable);
        assert!(delegate.openable_selected_keys().is_empty());
        assert!(delegate.drag_paths(&RowKey::Local("t1".into())).is_empty());

        delegate.selected_tasks.insert(RowKey::Local("t0".into()));
        assert_eq!(
            delegate.openable_selected_keys(),
            [RowKey::Local("t0".into())]
        );
        Ok(())
    }

    #[test]
    fn drag_carries_selection_only_when_anchor_is_selected() -> Result<(), I18nError> {
        // t0 / t1 / t3 已完成、t2 已暂停；t3 文件已丢失。
        let mut delegate = delegate_with_missing(&[3, 3, 2, 3], &[3])?;
        let path = |name: &str| std::path::Path::new("/tmp").join(name);

        // 未选中时拖任意行：只拖该行。
        assert_eq!(
            delegate.drag_paths(&RowKey::Local("t1".into())),
            [path("f1.bin")]
        );

        // 从选区内拖起：带上整个选区里仍可拖出的文件（跳过未完成与已丢失）。
        delegate.select_all_tasks();
        assert_eq!(
            delegate.drag_paths(&RowKey::Local("t1".into())),
            [path("f0.bin"), path("f1.bin")]
        );

        // 从选区外的行拖起：选区不参与。
        delegate.selected_tasks.clear();
        delegate.selected_tasks.insert(RowKey::Local("t0".into()));
        assert_eq!(
            delegate.drag_paths(&RowKey::Local("t1".into())),
            [path("f1.bin")]
        );
        Ok(())
    }

    #[test]
    fn selection_is_pruned_to_tasks_left_in_view() -> Result<(), I18nError> {
        let mut delegate = delegate(&[2, 1, 2])?;
        delegate.select_all_tasks();
        assert_eq!(delegate.selection_summary().count, 3);

        // 搜索收窄：被筛掉的任务移出选中集，汇总与批量动作只看剩余可见任务。
        delegate.set_query("f1.bin");
        delegate.refresh_view();
        assert_eq!(delegate.selected_keys(), vec![RowKey::Local("t1".into())]);
        assert_eq!(delegate.selection_summary().count, 1);

        // 列表为空时不得再报告「已选 N 项」。
        delegate.set_query("no-such-task");
        delegate.refresh_view();
        assert_eq!(delegate.selection_summary(), SelectionSummary::default());

        // 清空搜索不会让已移出的任务「复活」为选中。
        delegate.set_query("");
        delegate.refresh_view();
        assert!(delegate.selected_keys().is_empty());
        Ok(())
    }

    #[test]
    fn collapsing_a_group_keeps_its_selected_members() -> Result<(), I18nError> {
        let mut delegate = delegate(&[1, 3])?;
        delegate.set_prefs(ViewPrefs {
            group_by: ViewGroupBy::Status,
            ..ViewPrefs::default()
        });
        delegate.refresh_view();
        delegate.select_all_tasks();
        let key = delegate
            .group_key_at(0)
            .map(str::to_owned)
            .unwrap_or_default();
        delegate.toggle_group_collapsed(&key);
        delegate.refresh_view();
        // 两个分组各 1 项，折叠首组后只剩 2 个组头 + 1 行任务。
        assert_eq!(delegate.visible.len(), 3);
        assert_eq!(delegate.selection_summary().count, 2);
        Ok(())
    }

    #[test]
    fn grouping_inserts_headers_and_collapsing_hides_members() -> Result<(), I18nError> {
        let mut delegate = delegate(&[1, 3, 1, 3])?;
        let prefs = ViewPrefs {
            group_by: ViewGroupBy::Status,
            ..ViewPrefs::default()
        };
        delegate.set_prefs(prefs);
        delegate.refresh_view();
        assert_eq!(delegate.visible.len(), 6);
        assert!(matches!(
            delegate.visible[0],
            VisibleRow::GroupHeader {
                count: 2,
                collapsed: false,
                ..
            }
        ));

        let key = delegate
            .group_key_at(0)
            .map(str::to_owned)
            .expect("group key");
        delegate.toggle_group_collapsed(&key);
        delegate.refresh_view();
        assert_eq!(delegate.visible.len(), 4);
        assert!(matches!(
            delegate.visible[0],
            VisibleRow::GroupHeader {
                collapsed: true,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn group_by_key_matches_linear_bucketing() {
        // 交错重复的 key：桶顺序为首次出现顺序，成员保持原相对顺序。
        let keys = ["b", "a", "b", "c", "a", "d", "b", "e", "c", "a"];
        let items: Vec<(String, usize)> = keys
            .iter()
            .enumerate()
            .map(|(index, key)| ((*key).to_owned(), index))
            .collect();
        let mut expected: Vec<(String, Vec<usize>)> = Vec::new();
        for (key, value) in items.iter().cloned() {
            match expected.iter_mut().find(|(existing, _)| *existing == key) {
                Some((_, members)) => members.push(value),
                None => expected.push((key, vec![value])),
            }
        }
        assert_eq!(group_by_key(items, |key: &String| key.as_str()), expected);
    }

    #[test]
    fn search_matches_url_and_site_case_insensitively() -> Result<(), I18nError> {
        // 文件名（f0.bin / f1.bin）不含查询词，命中只能来自链接 / 站点。
        let mut delegate = delegate(&[1, 3])?;
        delegate.set_query("EXAMPLE.com/FILE");
        delegate.refresh_view();
        assert_eq!(delegate.visible.len(), 2);
        delegate.set_query("Example.COM");
        delegate.refresh_view();
        assert_eq!(delegate.visible.len(), 2);
        delegate.set_query("other.org");
        delegate.refresh_view();
        assert!(delegate.visible.is_empty());
        Ok(())
    }

    /// t0 完成 / t1 下载中 / t2 暂停（子目录 extras）同属组 `g`，t3 排队且不在组里。
    fn grouped_delegate() -> Result<DownloadTableDelegate, I18nError> {
        let mut delegate = delegate(&[3, 1, 2, 0])?;
        for (ix, dir) in [(0, "/tmp/Pack"), (1, "/tmp/Pack"), (2, "/tmp/Pack/extras")] {
            let mut row = delegate.store.local()[ix].clone();
            row.group_id = "g".into();
            row.save_dir = dir.into();
            delegate.store.set_local(ix, row);
        }
        delegate.set_groups(std::collections::HashMap::from([(
            "g".to_owned(),
            GroupMeta {
                name: "Pack".into(),
                name_fold: "pack".into(),
                save_dir: "/tmp/Pack".into(),
                source_url: String::new(),
                created_at_secs: 99,
            },
        )]));
        delegate.refresh_view();
        Ok(delegate)
    }

    fn local(id: &str) -> RowKey {
        RowKey::Local(id.into())
    }

    #[test]
    fn group_folds_into_one_collapsed_row_and_expands_by_directory() -> Result<(), I18nError> {
        let mut delegate = grouped_delegate()?;
        // 智能排序：组聚合为下载中，排在排队的 t3 之前；默认折叠。
        assert!(matches!(
            delegate.visible.as_slice(),
            [VisibleRow::Group(0), VisibleRow::Task(_)]
        ));
        let counts = delegate.group_rows[0].aggregate.counts;
        assert_eq!(
            (counts.total, counts.done, counts.downloading, counts.paused),
            (3, 1, 1, 1)
        );

        delegate.toggle_group_expanded("g");
        delegate.refresh_view();
        let keys: Vec<Option<RowKey>> = (0..delegate.visible.len())
            .map(|ix| delegate.row_key_at(ix))
            .collect();
        assert_eq!(
            keys,
            [
                None,
                Some(local("t0")),
                Some(local("t1")),
                None,
                Some(local("t2")),
                Some(local("t3"))
            ]
        );
        assert!(matches!(
            &delegate.visible[3],
            VisibleRow::GroupDir { path, count: 1, collapsed: false, .. } if path == "extras"
        ));
        assert!(matches!(
            delegate.visible[4],
            VisibleRow::Member { depth: 2, .. }
        ));
        Ok(())
    }

    #[test]
    fn group_click_selects_members_and_select_all_covers_collapsed_groups() -> Result<(), I18nError>
    {
        let mut delegate = grouped_delegate()?;
        delegate.select_group("g", SelectModifiers::default());
        assert_eq!(
            delegate.selected_tasks,
            HashSet::from([local("t0"), local("t1"), local("t2")])
        );
        assert_eq!(delegate.focused_group(), Some("g"));
        // 选区不再恰好是该组时，停靠面板不再显示组概览。
        delegate.select_task(local("t3"), SelectModifiers::default());
        assert_eq!(delegate.focused_group(), None);

        // 折叠的组也在「全选」范围内。
        delegate.select_all_tasks();
        assert_eq!(delegate.selection_summary().count, 4);

        // Cmd/Ctrl 单击整组移出 / 并入选区。
        delegate.select_group(
            "g",
            SelectModifiers {
                toggle: true,
                ..SelectModifiers::default()
            },
        );
        assert_eq!(delegate.selected_tasks, HashSet::from([local("t3")]));
        Ok(())
    }

    #[test]
    fn group_row_aggregates_only_members_in_view() -> Result<(), I18nError> {
        let mut delegate = grouped_delegate()?;
        delegate.set_filter(TableFilter::Download(DownloadFilter::status(
            DownloadStatusFilter::Completed,
        )));
        delegate.refresh_view();
        let group = &delegate.group_rows[0];
        assert_eq!(group.members.len(), 1);
        assert_eq!(group.aggregate.state(), TaskState::Completed);

        // 搜索命中组名：全部成员可见，组强制展开。
        delegate.set_filter(TableFilter::Download(DownloadFilter::ALL));
        delegate.set_query("PACK");
        delegate.refresh_view();
        assert_eq!(delegate.group_rows[0].members.len(), 3);
        assert!(delegate.group_rows[0].expanded);
        assert_eq!(delegate.visible.len(), 5);
        Ok(())
    }

    #[test]
    fn shift_range_selection_skips_group_headers() -> Result<(), I18nError> {
        let mut delegate = delegate(&[1, 3, 1, 3])?;
        let prefs = ViewPrefs {
            group_by: ViewGroupBy::Status,
            ..ViewPrefs::default()
        };
        delegate.set_prefs(prefs);
        delegate.refresh_view();
        let first = delegate.row_key_at(1).expect("first task");
        let last = delegate.row_key_at(5).expect("last task");
        delegate.select_task(first, SelectModifiers::default());
        delegate.select_task(
            last,
            SelectModifiers {
                shift: true,
                ..SelectModifiers::default()
            },
        );
        assert_eq!(delegate.selected_tasks.len(), 4);
        Ok(())
    }

    #[test]
    fn drag_sweep_selects_the_closed_row_span_and_can_add() -> Result<(), I18nError> {
        let mut delegate = delegate(&[1, 1, 1, 1])?;
        delegate.apply_row_sweep(0, 2, false, &HashSet::new());
        assert_eq!(delegate.selected_tasks.len(), 3);
        assert!(delegate.in_multi_select());
        let kept = delegate.selected_tasks.clone();
        delegate.apply_row_sweep(3, 3, true, &kept);
        assert_eq!(delegate.selected_tasks.len(), 4);
        Ok(())
    }

    /// 文件名列是拖出把手，其他列是划选，都不用修饰键；把手上没移动就松开仍是单击选中。
    #[test]
    fn file_name_column_clicks_select_drags_the_file_and_other_columns_sweep()
    -> Result<(), I18nError> {
        use fluxdown_ui_theme::active_theme;
        use gpui::{
            AppContext as _, Context, Entity, IntoElement, Modifiers, MouseButton, MouseMoveEvent,
            ParentElement as _, Pixels, Point, Render, Styled as _, TestApp, Window, div, point,
            px,
        };
        use gpui_component::table::TableState;

        struct Host {
            table: Entity<TableState<DownloadTableDelegate>>,
        }

        impl Render for Host {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().flex().child(render_download_table(
                    "sweep-test-table",
                    &self.table,
                    cx,
                ))
            }
        }

        // 四个已完成且文件仍在的任务：文件名列都有拖出把手。
        let delegate = delegate(&[3, 3, 3, 3])?;
        let keys = delegate.visible_task_keys();
        let mut app = TestApp::new();
        app.update(|cx| {
            gpui_component::init(cx);
            fluxdown_ui_theme::init(cx);
        });
        let mut win = app.open_window(move |window, cx| Host {
            table: cx.new(|cx| {
                TableState::new(delegate, window, cx)
                    .row_selectable(false)
                    .col_selectable(false)
            }),
        });
        // 首帧测得容器宽后帧外回填文件名列宽，再画一帧才是稳定布局。
        for _ in 0..3 {
            win.draw();
            app.run_until_parked();
        }

        let (header, row, name_x, other_x) = win.read(|host, cx| {
            let delegate = host.table.read(cx).delegate();
            let selection = delegate.selection_column_width();
            (
                f32::from(table_header_height(cx)),
                f32::from(
                    delegate
                        .prefs
                        .density
                        .row_height(active_theme(cx).density()),
                ),
                selection + 24.,
                selection + delegate.applied_file_width.get() + 12.,
            )
        });
        let at = |x: f32, row_ix: usize| -> Point<Pixels> {
            point(px(x), px(header + row * (row_ix as f32 + 0.5)))
        };
        let selected = |win: &gpui::TestAppWindow<Host>| {
            win.read(|host, cx| host.table.read(cx).delegate().selected_keys())
        };
        let drag_to = |win: &mut gpui::TestAppWindow<Host>, position: Point<Pixels>| {
            win.simulate_event(MouseMoveEvent {
                position,
                modifiers: Modifiers::default(),
                pressed_button: Some(MouseButton::Left),
            });
        };

        // 把手上按下、不移动就松开：选中该行；再点另一行改选。
        win.simulate_mouse_move(at(name_x, 0));
        win.simulate_click(at(name_x, 0), MouseButton::Left);
        assert_eq!(selected(&win), [keys[0].clone()]);
        win.simulate_mouse_move(at(name_x, 1));
        win.simulate_click(at(name_x, 1), MouseButton::Left);
        assert_eq!(selected(&win), [keys[1].clone()]);

        // 把手上按下后跨行移动：文件拖放，不划选；在别的行松开不改选区。
        win.simulate_mouse_move(at(name_x, 0));
        win.simulate_mouse_down(at(name_x, 0), MouseButton::Left);
        drag_to(&mut win, at(name_x, 2));
        assert!(app.read(|cx| cx.has_active_drag()));
        win.simulate_mouse_up(at(name_x, 2), MouseButton::Left);
        assert!(!app.read(|cx| cx.has_active_drag()));
        assert_eq!(selected(&win), [keys[1].clone()]);

        // 拖起后在同一行松开：这次单击被抑制，选区不变。
        win.simulate_mouse_move(at(name_x, 0));
        win.simulate_mouse_down(at(name_x, 0), MouseButton::Left);
        drag_to(&mut win, at(name_x + 12., 0));
        assert!(app.read(|cx| cx.has_active_drag()));
        win.simulate_mouse_up(at(name_x + 12., 0), MouseButton::Left);
        assert_eq!(selected(&win), [keys[1].clone()]);

        // 其他列按下后跨行移动：划选经过的三行，不启动文件拖放。
        win.simulate_mouse_move(at(other_x, 0));
        win.simulate_mouse_down(at(other_x, 0), MouseButton::Left);
        drag_to(&mut win, at(other_x, 2));
        assert!(!app.read(|cx| cx.has_active_drag()));
        win.simulate_mouse_up(at(other_x, 2), MouseButton::Left);
        assert_eq!(selected(&win).len(), 3);
        Ok(())
    }

    fn width_of(delegate: &DownloadTableDelegate, kind: DownloadColumnKind) -> Option<f32> {
        delegate
            .columns
            .iter()
            .find(|column| column.kind == kind)
            .map(|column| column.width)
    }

    #[test]
    fn sync_column_widths_maps_shown_columns_and_clamps() -> Result<(), I18nError> {
        let mut delegate = delegate(&[1])?;
        let _ = delegate.set_viewport_width(1400.);
        let applied = delegate.apply_file_name_width();
        let shown: Vec<DownloadColumnKind> = delegate
            .columns
            .iter()
            .filter(|column| column.visible)
            .map(|column| column.kind)
            .collect();
        let widths_with = |file_name: f32| -> Vec<gpui::Pixels> {
            // 首项是勾选列；大小列拖宽、状态列拖到下限以下。
            std::iter::once(gpui::px(36.))
                .chain(shown.iter().map(|kind| match kind {
                    DownloadColumnKind::Size => gpui::px(180.),
                    DownloadColumnKind::Status => gpui::px(10.),
                    DownloadColumnKind::FileName => gpui::px(file_name),
                    other => gpui::px(other.default_width()),
                }))
                .collect()
        };
        // 文件名列宽等于已应用的回算值：没被拖，保持自适应。
        let widths = widths_with(applied);
        assert!(delegate.sync_column_widths(&widths));
        assert_eq!(width_of(&delegate, DownloadColumnKind::Size), Some(180.));
        assert_eq!(
            width_of(&delegate, DownloadColumnKind::Status),
            Some(DownloadColumnKind::Status.min_width())
        );
        assert_eq!(delegate.prefs.file_name_width, None);
        // 隐藏列不参与映射，宽度不变。
        assert_eq!(
            width_of(&delegate, DownloadColumnKind::Speed),
            Some(DownloadColumnKind::Speed.default_width())
        );
        assert!(!delegate.sync_column_widths(&widths));

        // 文件名列被拖（宽度偏离回算值）→ 固定，不再随容器变化；过窄钳到下限。
        assert!(delegate.sync_column_widths(&widths_with(applied + 120.)));
        assert_eq!(delegate.prefs.file_name_width, Some(applied + 120.));
        assert_eq!(delegate.file_name_width_for(900.), applied + 120.);
        assert!(!delegate.viewport_needs_sync(1400.));
        assert!(delegate.sync_column_widths(&widths_with(10.)));
        assert_eq!(
            delegate.file_name_width_for(1400.),
            DownloadColumnKind::FileName.min_width()
        );

        // 重置列恢复自适应。
        delegate.reset_columns();
        assert_eq!(delegate.prefs.file_name_width, None);
        Ok(())
    }

    #[test]
    fn file_name_absorbs_remaining_width_and_respects_min() -> Result<(), I18nError> {
        let mut delegate = delegate(&[1])?;
        let wide = delegate.file_name_width_for(1400.);
        let narrower = delegate.file_name_width_for(1300.);
        assert!((wide - narrower - 100.).abs() < 0.01);

        // 其他列拖宽 50 → 文件名列让出 50。
        let size = width_of(&delegate, DownloadColumnKind::Size).unwrap_or_default();
        if let Some(column) = delegate
            .columns
            .iter_mut()
            .find(|column| column.kind == DownloadColumnKind::Size)
        {
            column.width = size + 50.;
        }
        assert!((wide - delegate.file_name_width_for(1400.) - 50.).abs() < 0.01);

        // 容器过窄时不低于最小宽度（超出部分走横向滚动）。
        assert_eq!(
            delegate.file_name_width_for(300.),
            DownloadColumnKind::FileName.min_width()
        );

        // 容器宽写入后一次 refresh 即收敛：应用新宽度后不再报告需要同步。
        assert!(delegate.set_viewport_width(1400.));
        let _ = delegate.apply_file_name_width();
        assert!(!delegate.viewport_needs_sync(1400.));
        Ok(())
    }

    fn paired_delegates(
        statuses: &[i32],
    ) -> Result<(DownloadTableDelegate, DownloadTableDelegate), I18nError> {
        let seed = delegate(statuses)?;
        let fast = DownloadTableDelegate::new(seed.strings.clone(), Rc::clone(&seed.store));
        let mut full = DownloadTableDelegate::new(seed.strings.clone(), Rc::clone(&seed.store));
        full.force_full_sort = true;
        Ok((fast, full))
    }

    fn refresh_pair(
        fast: &mut DownloadTableDelegate,
        full: &mut DownloadTableDelegate,
        now: Instant,
    ) {
        assert_eq!(fast.refresh_view_at(now), full.refresh_view_at(now));
        assert_eq!(fast.visible, full.visible);
        assert_eq!(fast.selected_tasks, full.selected_tasks);
        assert_eq!(fast.selection_anchor, full.selection_anchor);
        assert_eq!(fast.reorder_deadline(), full.reorder_deadline());
    }

    #[test]
    fn unchanged_sort_values_match_full_sort_across_event_sequences() -> Result<(), I18nError> {
        let sort_keys = [
            ViewSortKey::Smart,
            ViewSortKey::Created,
            ViewSortKey::Name,
            ViewSortKey::Size,
            ViewSortKey::Progress,
            ViewSortKey::Speed,
            ViewSortKey::Status,
        ];
        let groupings = [
            ViewGroupBy::None,
            ViewGroupBy::Status,
            ViewGroupBy::Date,
            ViewGroupBy::Type,
            ViewGroupBy::Queue,
            ViewGroupBy::Site,
        ];
        for sort_key in sort_keys {
            for sort_dir in [SortDir::Asc, SortDir::Desc] {
                for group_by in groupings {
                    let (mut fast, mut full) = paired_delegates(&[1, 1, 0, 3, 2, 4])?;
                    let template = fast.store.local()[0].clone();
                    let mut remote = template.clone();
                    remote.key = RowKey::Remote("remote".into());
                    remote.source = TaskSource::Remote;
                    remote.to_device = "device".into();
                    fast.store.replace_remote(vec![remote]);
                    let categories = Rc::new(CategoryIndex::from_dtos(
                        fluxdown_protocol::CustomCategoryDto::builtin_defaults(),
                    ));
                    fast.set_categories(Rc::clone(&categories));
                    full.set_categories(categories);
                    // `group0` 是已知任务组（折叠成文件夹行），`group1` 是孤儿 id（按普通任务显示）。
                    let groups = std::collections::HashMap::from([(
                        "group0".to_owned(),
                        GroupMeta {
                            name: "Pack".into(),
                            name_fold: "pack".into(),
                            save_dir: "/tmp".into(),
                            source_url: "https://site0.test/pack".into(),
                            created_at_secs: 97,
                        },
                    )]);
                    fast.set_groups(groups.clone());
                    full.set_groups(groups);
                    let prefs = ViewPrefs {
                        sort_key,
                        sort_dir,
                        group_by,
                        ..ViewPrefs::default()
                    };
                    fast.set_prefs(prefs.clone());
                    full.set_prefs(prefs);
                    let start = Instant::now();
                    refresh_pair(&mut fast, &mut full, start);
                    fast.select_all_tasks();
                    full.select_all_tasks();
                    let mut random = 0xa076_1d64_78bd_642f_u64;
                    for step in 1..=192 {
                        random ^= random << 13;
                        random ^= random >> 7;
                        random ^= random << 17;
                        let now = start + Duration::from_millis(step * 137);
                        if let Some(deadline) = fast.reorder_deadline().filter(|at| *at <= now) {
                            fast.view_dirty = true;
                            full.view_dirty = true;
                            refresh_pair(&mut fast, &mut full, deadline);
                        }
                        let count = fast.store.local().len();
                        let ix = (random >> 32) as usize % count;
                        let mut row = fast.store.local()[ix].clone();
                        match random % 16 {
                            0 => {
                                row.downloaded_bytes += 1;
                                row.progress = (random % 101) as f32 / 100.;
                                fast.store.set_local(ix, row);
                            }
                            1 => {
                                row.speed_bytes_per_second = Some(random % 500);
                                fast.store.set_local(ix, row);
                            }
                            2 => {
                                row.size_bytes = random % 200;
                                fast.store.set_local(ix, row);
                            }
                            3 => {
                                row.state = if row.state == TaskState::Completed {
                                    TaskState::Downloading
                                } else {
                                    TaskState::Completed
                                };
                                fast.store.set_local(ix, row);
                            }
                            4 => {
                                row.boosted = !row.boosted;
                                row.preparing = !row.preparing;
                                row.queue_key = Some((random % 5) as i64).filter(|key| *key > 0);
                                fast.store.set_local(ix, row);
                            }
                            5 => {
                                row.queue_order += 1;
                                fast.store.set_local(ix, row);
                            }
                            6 => {
                                row.name = format!("episode{}.zip", random % 12);
                                row.name_fold = row.name.to_lowercase();
                                row.file_extension = "zip".into();
                                fast.store.set_local(ix, row);
                            }
                            7 => {
                                row.queue_id = format!("queue{}", random % 3);
                                row.group_id = format!("group{}", random % 2);
                                row.referrer = format!("https://site{}.test/page", random % 4);
                                fast.store.set_local(ix, row);
                            }
                            8 => {
                                let query = if fast.query.is_empty() { "episode" } else { "" };
                                fast.set_query(query);
                                full.set_query(query);
                            }
                            9 => {
                                if let Some(key) = fast.visible.iter().find_map(|row| match row {
                                    VisibleRow::GroupHeader { key, .. } => Some(key.clone()),
                                    _ => None,
                                }) {
                                    fast.toggle_group_collapsed(&key);
                                    full.toggle_group_collapsed(&key);
                                }
                                if let Some(id) =
                                    fast.group_rows.first().map(|group| group.id.clone())
                                {
                                    fast.toggle_group_expanded(&id);
                                    full.toggle_group_expanded(&id);
                                }
                            }
                            10 => {
                                let mut rows = fast.store.local().to_vec();
                                rows[ix].progress = (random % 101) as f32 / 100.;
                                rows[ix].speed_bytes_per_second = Some(random % 500);
                                fast.store.replace_local(rows);
                                let rows = fast.store.remote().to_vec();
                                fast.store.replace_remote(rows);
                            }
                            11 => {
                                if count > 3 {
                                    fast.store.swap_remove_local(ix);
                                } else {
                                    let mut added = template.clone();
                                    added.key = RowKey::Local(format!("added{step}"));
                                    fast.store.push_local(added);
                                }
                            }
                            12 => {
                                fast.row_order.note_interaction(now);
                                full.row_order.note_interaction(now);
                            }
                            13 => {
                                row.created_at_secs += 1;
                                fast.store.set_local(ix, row);
                            }
                            14 => {
                                let filter = match random >> 8 & 3 {
                                    0 => TableFilter::Download(DownloadFilter::ALL),
                                    1 => TableFilter::Download(DownloadFilter::status(
                                        DownloadStatusFilter::Incomplete,
                                    )),
                                    2 => TableFilter::Queue("main".into()),
                                    _ => TableFilter::Device("device".into()),
                                };
                                fast.set_filter(filter.clone());
                                full.set_filter(filter);
                            }
                            _ => {
                                row.runtime_connected = !row.runtime_connected;
                                row.eta_seconds = Some(random % 600);
                                fast.store.set_local(ix, row);
                            }
                        }
                        refresh_pair(&mut fast, &mut full, now);
                        if step % 19 == 0
                            && let Some(key) = fast.visible_task_keys().first().cloned()
                        {
                            fast.select_task(key.clone(), SelectModifiers::default());
                            full.select_task(key, SelectModifiers::default());
                        }
                    }
                    assert!(fast.full_sort_count < full.full_sort_count);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn incremental_sorted_order_matches_full_sort_under_batched_row_churn() -> Result<(), I18nError>
    {
        for (sort_key, sort_dir) in [
            (ViewSortKey::Smart, SortDir::Desc),
            (ViewSortKey::Created, SortDir::Asc),
            (ViewSortKey::Name, SortDir::Asc),
            (ViewSortKey::Size, SortDir::Desc),
            (ViewSortKey::Status, SortDir::Asc),
        ] {
            let statuses: Vec<i32> = (0..90).map(|ix| ix % 6).collect();
            let (mut fast, mut full) = paired_delegates(&statuses)?;
            let prefs = ViewPrefs {
                sort_key,
                sort_dir,
                ..ViewPrefs::default()
            };
            fast.set_prefs(prefs.clone());
            full.set_prefs(prefs);
            let start = Instant::now();
            refresh_pair(&mut fast, &mut full, start);
            let template = fast.store.local()[0].clone();
            let mut random = 0x2545_f491_4f6c_dd1d_u64;
            let mut serial = 0_u64;
            for step in 1..=300_u64 {
                // 一帧内先发生 1-4 个变更，再刷新：重放要处理交错的更新 / 追加 / swap_remove。
                for _ in 0..=(random % 4) {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    let count = fast.store.local().len();
                    let ix = (random >> 32) as usize % count.max(1);
                    match random % 7 {
                        0 | 1 if count > 0 => {
                            let mut row = fast.store.local()[ix].clone();
                            row.state = match (random >> 8) % 5 {
                                0 => TaskState::Downloading,
                                1 => TaskState::Pending,
                                2 => TaskState::Paused,
                                3 => TaskState::Completed,
                                _ => TaskState::Failed,
                            };
                            row.boosted = (random >> 12).is_multiple_of(4);
                            fast.store.set_local(ix, row);
                        }
                        2 if count > 0 => {
                            let mut row = fast.store.local()[ix].clone();
                            row.created_at_secs += (random >> 9) as i64 % 40 - 20;
                            row.size_bytes = (random >> 20) % 50;
                            fast.store.set_local(ix, row);
                        }
                        3 if count > 0 => {
                            let mut row = fast.store.local()[ix].clone();
                            row.name = format!("episode{}.zip", (random >> 10) % 30);
                            row.name_fold = row.name.to_lowercase();
                            fast.store.set_local(ix, row);
                        }
                        4 => {
                            for _ in 0..=(random >> 40) % 3 {
                                serial += 1;
                                let mut added = template.clone();
                                added.key = RowKey::Local(format!("added{serial}"));
                                added.created_at_secs = (serial as i64 * 7) % 120;
                                added.state = if serial.is_multiple_of(2) {
                                    TaskState::Pending
                                } else {
                                    TaskState::Downloading
                                };
                                fast.store.append_local(added);
                            }
                            fast.store.commit_appends();
                        }
                        5 if count > 8 => {
                            for _ in 0..=(random >> 40) % 3 {
                                let count = fast.store.local().len();
                                if count > 8 {
                                    let ix = (random >> (8 + serial % 20)) as usize % count;
                                    serial += 1;
                                    fast.store.swap_remove_local_quiet(ix);
                                }
                            }
                            fast.store.commit_removals();
                        }
                        _ if count > 0 => {
                            let key = Some((random >> 16) as i64 % 6).filter(|key| *key > 0);
                            fast.store.set_queue_key(ix, key);
                        }
                        _ => {}
                    }
                }
                if step % 37 == 0 {
                    let query = if fast.query.is_empty() { "episode" } else { "" };
                    fast.set_query(query);
                    full.set_query(query);
                }
                let now = start + Duration::from_millis(step * 53);
                refresh_pair(&mut fast, &mut full, now);
                assert_eq!(
                    fast.sorted_rows, full.sorted_rows,
                    "{sort_key:?} step {step}"
                );
            }
            assert!(
                fast.sort_all_count * 4 < full.sort_all_count,
                "{sort_key:?}: {} whole-table sorts vs {}",
                fast.sort_all_count,
                full.sort_all_count
            );
        }
        Ok(())
    }

    #[test]
    fn cached_sorted_input_preserves_deferred_reorder_and_live_clock() -> Result<(), I18nError> {
        for sort_key in [ViewSortKey::Speed, ViewSortKey::Progress] {
            let (mut fast, mut full) = paired_delegates(&[1, 1])?;
            let prefs = ViewPrefs {
                sort_key,
                ..ViewPrefs::default()
            };
            fast.set_prefs(prefs.clone());
            full.set_prefs(prefs);
            let now = Instant::now();
            refresh_pair(&mut fast, &mut full, now);
            let previous = fast.visible.clone();
            let mut row = fast.store.local()[1].clone();
            row.speed_bytes_per_second = Some(100);
            row.progress = 0.8;
            fast.store.set_local(1, row);
            refresh_pair(&mut fast, &mut full, now + Duration::from_millis(100));
            assert_eq!(fast.visible, previous);
            let deadline = fast.reorder_deadline().expect("live sort deferred");
            let sorts = fast.full_sort_count;
            let mut row = fast.store.local()[1].clone();
            row.eta_seconds = Some(3);
            fast.store.set_local(1, row);
            refresh_pair(&mut fast, &mut full, now + Duration::from_millis(200));
            assert_eq!(fast.full_sort_count, sorts);
            assert_eq!(fast.reorder_deadline(), Some(deadline));

            let mut row = fast.store.local()[1].clone();
            row.eta_seconds = Some(2);
            fast.store.set_local(1, row);
            refresh_pair(&mut fast, &mut full, deadline);
            assert_eq!(fast.full_sort_count, sorts);
            assert_eq!(
                fast.visible_task_keys(),
                [RowKey::Local("t1".into()), RowKey::Local("t0".into())]
            );
            assert_eq!(fast.reorder_deadline(), None);

            let mut row = fast.store.local()[0].clone();
            row.speed_bytes_per_second = Some(200);
            row.progress = 0.9;
            fast.store.set_local(0, row);
            refresh_pair(&mut fast, &mut full, deadline + Duration::from_millis(100));
            assert_eq!(
                fast.visible_task_keys(),
                [RowKey::Local("t1".into()), RowKey::Local("t0".into())]
            );
            assert_eq!(
                fast.reorder_deadline(),
                Some(deadline + Duration::from_secs(2))
            );
        }
        Ok(())
    }

    #[test]
    fn changed_sort_keys_and_tie_breakers_take_full_path() -> Result<(), I18nError> {
        for sort_key in [
            ViewSortKey::Smart,
            ViewSortKey::Created,
            ViewSortKey::Name,
            ViewSortKey::Size,
            ViewSortKey::Progress,
            ViewSortKey::Speed,
            ViewSortKey::Status,
        ] {
            let (mut fast, mut full) = paired_delegates(&[1, 1])?;
            let prefs = ViewPrefs {
                sort_key,
                ..ViewPrefs::default()
            };
            fast.set_prefs(prefs.clone());
            full.set_prefs(prefs);
            let now = Instant::now();
            refresh_pair(&mut fast, &mut full, now);
            let sorts = fast.full_sort_count;
            let mut row = fast.store.local()[0].clone();
            row.eta_seconds = Some(10);
            fast.store.set_local(0, row);
            refresh_pair(&mut fast, &mut full, now + Duration::from_millis(10));
            assert_eq!(fast.full_sort_count, sorts);
            let mut row = fast.store.local()[0].clone();
            match sort_key {
                ViewSortKey::Smart => row.boosted = true,
                ViewSortKey::Created => row.created_at_secs += 1,
                ViewSortKey::Name => {
                    row.name = "a.bin".into();
                    row.name_fold = "a.bin".into();
                }
                ViewSortKey::Size => row.size_bytes += 1,
                ViewSortKey::Progress => row.progress = 0.5,
                ViewSortKey::Speed => row.speed_bytes_per_second = Some(10),
                ViewSortKey::Status => row.state = TaskState::Completed,
            }
            fast.store.set_local(0, row);
            refresh_pair(&mut fast, &mut full, now + Duration::from_millis(20));
            assert_eq!(fast.full_sort_count, sorts + 1);
            let mut row = fast.store.local()[0].clone();
            row.queue_order += 1;
            fast.store.set_local(0, row);
            refresh_pair(&mut fast, &mut full, now + Duration::from_millis(30));
            assert_eq!(fast.full_sort_count, sorts + 2);
        }
        Ok(())
    }

    #[test]
    fn snapshot_and_view_changes_invalidate_cached_sort() -> Result<(), I18nError> {
        let (mut fast, mut full) = paired_delegates(&[1, 1, 3])?;
        let now = Instant::now();
        fast.set_prefs(ViewPrefs::default());
        full.set_prefs(ViewPrefs::default());
        refresh_pair(&mut fast, &mut full, now);
        let mut expected = fast.full_sort_count;
        let rows = fast.store.local().to_vec();
        let structure = fast.store.structure_generation();
        fast.store.replace_local(rows);
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(10));
        expected += 1;
        assert_eq!(fast.full_sort_count, expected);
        assert_eq!(fast.store.structure_generation(), structure);
        fast.store.replace_remote(Vec::new());
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(20));
        expected += 1;
        assert_eq!(fast.full_sort_count, expected);
        fast.prefs_mut().sort_key = ViewSortKey::Name;
        full.prefs_mut().sort_key = ViewSortKey::Name;
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(30));
        expected += 1;
        assert_eq!(fast.full_sort_count, expected);
        let categories = Rc::new(CategoryIndex::default());
        fast.set_categories(Rc::clone(&categories));
        full.set_categories(categories);
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(40));
        expected += 1;
        assert_eq!(fast.full_sort_count, expected);
        fast.select_all_tasks();
        full.select_all_tasks();
        fast.set_query("f0");
        full.set_query("f0");
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(50));
        expected += 1;
        assert_eq!(fast.full_sort_count, expected);
        assert_eq!(fast.selected_keys(), [RowKey::Local("t0".into())]);
        let filter = TableFilter::Download(DownloadFilter::status(DownloadStatusFilter::Completed));
        fast.set_filter(filter.clone());
        full.set_filter(filter);
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(60));
        expected += 1;
        assert_eq!(fast.full_sort_count, expected);
        assert!(fast.selected_tasks.is_empty());
        Ok(())
    }

    #[test]
    fn smart_sort_tracks_effective_tiers_and_queue_slots() -> Result<(), I18nError> {
        let (mut fast, mut full) = paired_delegates(&[1, 0])?;
        let now = Instant::now();
        fast.set_prefs(ViewPrefs::default());
        full.set_prefs(ViewPrefs::default());
        refresh_pair(&mut fast, &mut full, now);
        let sorts = fast.full_sort_count;
        let mut active = fast.store.local()[0].clone();
        active.queue_key = Some(9);
        active.preparing = true;
        fast.store.set_local(0, active);
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(10));
        assert_eq!(fast.full_sort_count, sorts);

        let mut queued = fast.store.local()[1].clone();
        queued.queue_key = Some(1);
        fast.store.set_local(1, queued);
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(20));
        assert_eq!(fast.full_sort_count, sorts + 1);
        let mut queued = fast.store.local()[1].clone();
        queued.preparing = true;
        fast.store.set_local(1, queued);
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(30));
        assert_eq!(fast.full_sort_count, sorts + 2);
        let mut preparing = fast.store.local()[1].clone();
        preparing.queue_key = Some(3);
        fast.store.set_local(1, preparing);
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(40));
        assert_eq!(fast.full_sort_count, sorts + 2);
        let mut preparing = fast.store.local()[1].clone();
        preparing.boosted = true;
        fast.store.set_local(1, preparing);
        refresh_pair(&mut fast, &mut full, now + Duration::from_millis(50));
        assert_eq!(fast.full_sort_count, sorts + 3);
        Ok(())
    }

    #[test]
    fn unordered_progress_values_conservatively_take_full_path() -> Result<(), I18nError> {
        let (mut fast, mut full) = paired_delegates(&[1, 1])?;
        let prefs = ViewPrefs {
            sort_key: ViewSortKey::Progress,
            ..ViewPrefs::default()
        };
        fast.set_prefs(prefs.clone());
        full.set_prefs(prefs);
        let now = Instant::now();
        refresh_pair(&mut fast, &mut full, now);
        let sorts = fast.full_sort_count;
        for (step, progress) in [f32::NAN, f32::NAN, 0.5].into_iter().enumerate() {
            let mut row = fast.store.local()[0].clone();
            row.progress = progress;
            fast.store.set_local(0, row);
            refresh_pair(
                &mut fast,
                &mut full,
                now + Duration::from_millis((step as u64 + 1) * 10),
            );
            assert_eq!(fast.full_sort_count, sorts + step + 1);
        }
        Ok(())
    }

    #[test]
    fn state_change_under_pointer_keeps_row_order_until_hold_ends() -> Result<(), I18nError> {
        // 两个下载中任务：活跃档按添加正序，t1（created 99）在 t0（created 100）之前。
        let mut delegate = delegate(&[1, 1])?;
        let t0 = RowKey::Local("t0".into());
        let t1 = RowKey::Local("t1".into());
        assert_eq!(delegate.visible_task_keys(), [t1.clone(), t0.clone()]);

        let now = Instant::now();
        delegate.row_order.note_interaction(now);
        // t1 下载完成，按智能排序应沉到历史档；指针仍在表格上，行不动。
        let mut done = delegate.store.local()[1].clone();
        done.state = TaskState::Completed;
        delegate.store.set_local(1, done);
        assert!(delegate.refresh_view_at(now + Duration::from_millis(100)));
        assert_eq!(delegate.visible_task_keys(), [t1.clone(), t0.clone()]);

        // 保持期结束，补偿定时器强制重排。
        let deadline = delegate.reorder_deadline().expect("reorder deferred");
        delegate.view_dirty = true;
        assert!(delegate.refresh_view_at(deadline));
        assert_eq!(delegate.visible_task_keys(), [t0, t1]);
        assert_eq!(delegate.reorder_deadline(), None);
        Ok(())
    }

    #[test]
    fn header_click_cycles_default_dir_reverse_then_reset() -> Result<(), I18nError> {
        let mut delegate = delegate(&[1])?;
        assert_eq!(delegate.prefs.sort_key, ViewSortKey::Smart);

        assert!(delegate.toggle_sort(DownloadColumnKind::FileName));
        assert_eq!(
            (delegate.prefs.sort_key, delegate.prefs.sort_dir),
            (ViewSortKey::Name, SortDir::Asc)
        );
        assert!(delegate.toggle_sort(DownloadColumnKind::FileName));
        assert_eq!(delegate.prefs.sort_dir, SortDir::Desc);
        // 第三次点击恢复默认排序。
        assert!(delegate.toggle_sort(DownloadColumnKind::FileName));
        assert_eq!(delegate.prefs.sort_key, ViewSortKey::Smart);

        // 切到别的列从该列默认方向开始，即使上一列停在反方向。
        assert!(delegate.toggle_sort(DownloadColumnKind::Size));
        assert!(delegate.toggle_sort(DownloadColumnKind::Size));
        assert_eq!(
            (delegate.prefs.sort_key, delegate.prefs.sort_dir),
            (ViewSortKey::Size, SortDir::Asc)
        );
        assert!(delegate.toggle_sort(DownloadColumnKind::Status));
        assert_eq!(
            (delegate.prefs.sort_key, delegate.prefs.sort_dir),
            (ViewSortKey::Status, SortDir::Desc)
        );
        assert!(delegate.toggle_sort(DownloadColumnKind::Status));
        assert!(delegate.toggle_sort(DownloadColumnKind::Status));
        assert_eq!(delegate.prefs.sort_key, ViewSortKey::Smart);

        assert!(!delegate.toggle_sort(DownloadColumnKind::Protocol));
        Ok(())
    }

    fn local_view(status: i32, seeding_status: i32, file_missing: bool) -> DownloadTaskView {
        let task = serde_json::from_value::<fluxdown_protocol::TaskDto>(serde_json::json!({
            "taskId": "t1",
            "url": "magnet:?xt=urn:btih:abc",
            "fileName": "f.bin",
            "saveDir": "/tmp",
            "status": status,
            "downloadedBytes": 1,
            "totalBytes": 1,
            "errorMessage": "",
            "createdAt": "100",
            "proxyUrl": "",
            "queueId": "main",
            "checksum": ""
        }))
        .expect("task");
        let mut view = DownloadTaskView::local(&task, None, false);
        view.seeding_status = seeding_status;
        view.file_missing = file_missing;
        view
    }

    #[test]
    fn only_active_seeding_rows_show_upload_speed() {
        let with_upload = |status, seeding| {
            let mut view = local_view(status, seeding, false);
            view.upload_speed_bytes_per_second = 2048;
            view
        };
        assert_eq!(seeding_upload_speed(&with_upload(3, 1)), Some(2048));
        // 排队做种 / 已停止 / 下载中都不显示做种上传速度。
        assert_eq!(seeding_upload_speed(&with_upload(3, 8)), None);
        assert_eq!(seeding_upload_speed(&with_upload(3, 2)), None);
        assert_eq!(seeding_upload_speed(&with_upload(1, 1)), None);
        assert_eq!(seeding_upload_speed(&local_view(3, 1, false)), None);
        assert!(is_seeding_row(&local_view(3, 8, false)));
        assert!(!is_seeding_row(&local_view(3, 0, false)));
    }

    #[test]
    fn row_actions_offer_only_available_operations() {
        let actions = |state, local| {
            let status = match state {
                TaskState::Downloading => 1,
                TaskState::Paused => 2,
                TaskState::Completed => 3,
                TaskState::Failed => 4,
                TaskState::Pending => 0,
            };
            row_actions(&local_view(status, 0, false), local, false).collect::<Vec<_>>()
        };
        assert_eq!(
            actions(TaskState::Downloading, true),
            [RowAction::Pause, RowAction::Reveal]
        );
        assert_eq!(actions(TaskState::Pending, false), [RowAction::Pause]);
        assert_eq!(
            actions(TaskState::Failed, true),
            [RowAction::Retry, RowAction::Reveal]
        );
        assert_eq!(
            actions(TaskState::Completed, true),
            [RowAction::Open, RowAction::Reveal]
        );
        // 远程已完成任务没有本机文件：无任何行操作；远程失败 / 取消的任务不能「继续」。
        assert!(actions(TaskState::Completed, false).is_empty());
        assert!(actions(TaskState::Failed, false).is_empty());
        // 文件已被删除 / 移走的已完成任务：不给「打开」，仍可「在文件夹中显示」。
        assert_eq!(
            row_actions(&local_view(3, 0, true), true, false).collect::<Vec<_>>(),
            [RowAction::Reveal]
        );
        // 「详情」排在最后，只给本地任务（远程任务没有本机详情）。
        assert_eq!(
            row_actions(&local_view(2, 0, false), true, true).collect::<Vec<_>>(),
            [RowAction::Resume, RowAction::Reveal, RowAction::Detail]
        );
        assert_eq!(
            row_actions(&local_view(2, 0, false), false, true).collect::<Vec<_>>(),
            [RowAction::Resume]
        );
    }

    /// 做种语义：做种中（1 / 8）给「暂停」、达标 / 停止（2..=7）且文件在给「继续」，
    /// 并分别映射为 pause / resume 命令；seeding_status 0 的已完成任务仍不可继续（#935）。
    #[test]
    fn seeding_rows_get_pause_and_stopped_seeds_get_resume() {
        for seeding in [1, 8] {
            let view = local_view(3, seeding, false);
            assert_eq!(
                row_actions(&view, true, false).collect::<Vec<_>>(),
                [RowAction::Pause, RowAction::Open, RowAction::Reveal]
            );
            assert!(matches!(
                task_command(&view, ToolbarCommand::Pause),
                Some(DownloadsCommand::Pause { task_id }) if task_id == "t1"
            ));
            assert!(task_command(&view, ToolbarCommand::Resume).is_none());
        }
        for seeding in 2..=7 {
            let view = local_view(3, seeding, false);
            assert_eq!(
                row_actions(&view, true, false).collect::<Vec<_>>(),
                [RowAction::Resume, RowAction::Open, RowAction::Reveal]
            );
            assert!(matches!(
                task_command(&view, ToolbarCommand::Resume),
                Some(DownloadsCommand::Resume { task_id }) if task_id == "t1"
            ));
            assert!(task_command(&view, ToolbarCommand::Pause).is_none());
        }
        // 文件丢失的停止做种任务不可继续。
        let missing = local_view(3, 4, true);
        assert!(task_command(&missing, ToolbarCommand::Resume).is_none());
        assert!(!row_actions(&missing, true, false).any(|action| action == RowAction::Resume));
    }

    #[test]
    fn remote_task_commands_carry_typed_params_and_skip_uncontrollable_rows() {
        use fluxdown_protocol::{RemoteCommandAction, RemoteTaskDto};
        let row = |status: &str| {
            let dto = serde_json::from_value::<RemoteTaskDto>(serde_json::json!({
                "id": "r1", "toDevice": "dev-b", "url": "https://example.com/a", "status": status
            }))
            .expect("remote task");
            DownloadTaskView::remote(&dto)
        };
        let Some(DownloadsCommand::RemoteCommand(params)) =
            task_command(&row("paused"), ToolbarCommand::Resume)
        else {
            panic!("paused remote task must resume through remote.command");
        };
        assert_eq!(params.task_id, "r1");
        assert_eq!(params.action, RemoteCommandAction::Resume);
        assert!(!params.delete_files);
        // 状态不允许 / 未知 / 无远程对应的动作：不发命令。
        assert!(task_command(&row("completed"), ToolbarCommand::Pause).is_none());
        assert!(task_command(&row("paused"), ToolbarCommand::Pause).is_none());
        assert!(task_command(&row("brandNewState"), ToolbarCommand::Delete).is_none());
        assert!(task_command(&row("downloading"), ToolbarCommand::Reveal).is_none());
        assert!(matches!(
            task_command(&row("completed"), ToolbarCommand::Delete),
            Some(DownloadsCommand::RemoteCommand(params))
                if params.action == RemoteCommandAction::Delete
        ));
    }

    /// #935：批量「继续」不得把已完成的本地任务交给 daemon（引擎对完成态的 resume 会从头重下）；
    /// 暂停 / 失败的本地任务照常继续，已完成任务的删除 / 打开不受影响。
    #[test]
    fn local_resume_skips_completed_tasks() {
        let row = |status: i32| {
            let task = serde_json::from_value::<fluxdown_protocol::TaskDto>(serde_json::json!({
                "taskId": "t1",
                "url": "https://example.com/file",
                "fileName": "f.bin",
                "saveDir": "/tmp",
                "status": status,
                "downloadedBytes": 1,
                "totalBytes": 1,
                "errorMessage": "",
                "createdAt": "100",
                "proxyUrl": "",
                "queueId": "main",
                "checksum": ""
            }))
            .expect("task");
            DownloadTaskView::local(&task, None, false)
        };
        assert!(task_command(&row(3), ToolbarCommand::Resume).is_none());
        assert!(task_command(&row(3), ToolbarCommand::Pause).is_none());
        for status in [2, 4] {
            assert!(matches!(
                task_command(&row(status), ToolbarCommand::Resume),
                Some(DownloadsCommand::Resume { task_id }) if task_id == "t1"
            ));
        }
        assert!(matches!(
            task_command(&row(3), ToolbarCommand::Delete),
            Some(DownloadsCommand::Delete { .. })
        ));
        assert!(matches!(
            task_command(&row(3), ToolbarCommand::Open),
            Some(DownloadsCommand::OpenTask { .. })
        ));
    }

    #[test]
    fn percent_label_floors_and_marks_sub_percent_progress() {
        assert_eq!(percent_label(0.).as_ref(), "0%");
        assert_eq!(percent_label(0.004).as_ref(), "<1%");
        assert_eq!(percent_label(0.01).as_ref(), "1%");
        assert_eq!(percent_label(0.999).as_ref(), "99%");
        assert_eq!(percent_label(1.).as_ref(), "100%");
    }
}

#[cfg(test)]
mod context_menu_tests {
    use super::{
        MenuEntry, PLUGIN_ERROR_PREFIX, PiecePickingOrder, TaskMenuFacts, context_menu_items,
        is_bt_url, is_replaceable_url, piece_picking_checked, piece_picking_order,
        piece_picking_order_targets, piece_picking_toggle_target, task_menu_entries,
    };
    use crate::model::TaskState;

    fn task(is_local: bool, state: TaskState) -> TaskMenuFacts {
        TaskMenuFacts {
            is_local,
            controllable: true,
            state,
            preparing: false,
            boosted: false,
            is_torrent_sentinel: false,
            is_bt: false,
            bt_sequential: false,
            bt_first_last_pieces_first: false,
            url_replaceable: true,
            is_plugin_retry_error: false,
            file_missing: false,
            is_seeding: false,
            can_resume_seeding: false,
        }
    }

    #[test]
    fn empty_selection_has_no_menu_items() {
        assert!(context_menu_items(&[]).is_empty());
    }

    #[test]
    fn seeding_rows_offer_pause_and_stopped_seeds_offer_resume() {
        let mut seeding = task(true, TaskState::Completed);
        seeding.is_seeding = true;
        let items = context_menu_items(&[seeding]);
        assert!(items.contains(&MenuEntry::Pause));
        assert!(!items.contains(&MenuEntry::Resume));

        let mut stopped = task(true, TaskState::Completed);
        stopped.can_resume_seeding = true;
        let items = context_menu_items(&[stopped]);
        assert!(items.contains(&MenuEntry::Resume));
        assert!(!items.contains(&MenuEntry::Pause));

        // 普通已完成任务（含 seeding_status 0、文件丢失）两者都不给。
        let items = context_menu_items(&[task(true, TaskState::Completed)]);
        assert!(!items.contains(&MenuEntry::Pause));
        assert!(!items.contains(&MenuEntry::Resume));
    }

    #[test]
    fn failed_local_task_offers_both_resume_and_pause() {
        // #947：失败任务可能正等待自动重试，暂停用于撤销重试。
        let items = context_menu_items(&[task(true, TaskState::Failed)]);
        assert!(items.contains(&MenuEntry::Resume));
        assert!(items.contains(&MenuEntry::Pause));
        // 远程任务没有对应的云端暂停命令。
        let items = context_menu_items(&[task(false, TaskState::Failed)]);
        assert!(!items.contains(&MenuEntry::Pause));
    }

    #[test]
    fn uncontrollable_remote_task_offers_no_control_entries() {
        let mut unknown = task(false, TaskState::Pending);
        unknown.controllable = false;
        let items = context_menu_items(&[unknown]);
        assert!(items.contains(&MenuEntry::CopyUrl));
        for entry in [
            MenuEntry::Pause,
            MenuEntry::Resume,
            MenuEntry::Delete,
            MenuEntry::DeleteWithFiles,
        ] {
            assert!(!items.contains(&entry), "{entry:?}");
        }
        // 与可控任务混选时，控制项仍可用（不可控的行在命令层被跳过）。
        let mut unknown = task(false, TaskState::Pending);
        unknown.controllable = false;
        let mixed = context_menu_items(&[unknown, task(false, TaskState::Paused)]);
        assert!(mixed.contains(&MenuEntry::Delete));
    }

    #[test]
    fn single_local_completed_task_shows_full_menu() {
        let items = context_menu_items(&[task(true, TaskState::Completed)]);
        assert_eq!(
            items,
            vec![
                MenuEntry::Boost,
                MenuEntry::OpenFile,
                MenuEntry::OpenFolder,
                MenuEntry::CopyFile,
                MenuEntry::Rename,
                MenuEntry::Redownload,
                MenuEntry::CopyUrl,
                MenuEntry::MoveToQueue,
                MenuEntry::Separator,
                MenuEntry::Delete,
                MenuEntry::DeleteWithFiles,
                MenuEntry::ShowDetail,
            ]
        );
    }

    #[test]
    fn multi_select_only_keeps_items_valid_for_every_selected_task() {
        // 一个本地已完成 + 一个远程下载中：单选专属项（重命名/加速/忽略插件重试）
        // 消失；要求全体本地的项（打开文件/目录/移动队列/重下载）因远程任务
        // 不满足而消失；resume 因两者都不满足「非完成非下载中」而消失；pause
        // 因远程任务处于下载中而显示（存在语义）；复制链接/删除类始终显示；
        // 「详情」因存在本地任务而显示。
        let selection = [
            task(true, TaskState::Completed),
            task(false, TaskState::Downloading),
        ];
        let items = context_menu_items(&selection);
        assert_eq!(
            items,
            vec![
                MenuEntry::Pause,
                MenuEntry::CopyUrl,
                MenuEntry::Separator,
                MenuEntry::Delete,
                MenuEntry::DeleteWithFiles,
                MenuEntry::ShowDetail,
            ]
        );
    }

    #[test]
    fn ignore_plugin_retry_only_for_single_failed_plugin_task() {
        let mut plugin_failure = task(true, TaskState::Failed);
        plugin_failure.is_plugin_retry_error = true;
        assert!(context_menu_items(&[plugin_failure]).contains(&MenuEntry::IgnorePluginRetry));

        // 双选即使都满足条件也不显示（单选专属）。
        assert!(
            !context_menu_items(&[plugin_failure, plugin_failure])
                .contains(&MenuEntry::IgnorePluginRetry)
        );

        // 失败但错误消息不带插件前缀不显示。
        let plain_failure = task(true, TaskState::Failed);
        assert!(!context_menu_items(&[plain_failure]).contains(&MenuEntry::IgnorePluginRetry));
        assert_eq!(PLUGIN_ERROR_PREFIX, "[插件]");
    }

    #[test]
    fn plugin_retry_fact_excludes_task_group_members() {
        use fluxdown_protocol::TaskDto;

        use super::task_menu_facts_for;
        use crate::model::{DownloadTaskView, RowKey, TaskStore};

        let store = TaskStore::default();
        for (id, group) in [("solo", ""), ("member", "g1")] {
            let dto: TaskDto = serde_json::from_value(serde_json::json!({
                "taskId": id, "url": "https://example.com/f", "fileName": "f.zip",
                "saveDir": "/tmp", "status": 4, "downloadedBytes": 0, "totalBytes": 10,
                "errorMessage": "[插件] yt: boom", "createdAt": "1", "proxyUrl": "",
                "queueId": "", "checksum": "", "groupId": group
            }))
            .expect("task");
            store.push_local(DownloadTaskView::local(&dto, None, false));
        }
        let fact = |id: &str| {
            task_menu_facts_for(&store, &RowKey::Local(id.to_owned()))
                .expect("facts")
                .is_plugin_retry_error
        };
        assert!(fact("solo"));
        assert!(!fact("member"));
    }

    #[test]
    fn piece_picking_facts_follow_the_task_dto_and_default_off() {
        use fluxdown_protocol::TaskDto;

        use super::task_menu_facts_for;
        use crate::model::{DownloadTaskView, RowKey, TaskStore};

        let store = TaskStore::default();
        let dto: TaskDto = serde_json::from_value(serde_json::json!({
            "taskId": "bt", "url": "magnet:?xt=urn:btih:abc", "fileName": "a",
            "saveDir": "/tmp", "status": 1, "downloadedBytes": 0, "totalBytes": 10,
            "errorMessage": "", "createdAt": "1", "proxyUrl": "", "queueId": "",
            "checksum": "", "btSequential": true
        }))
        .expect("task");
        store.push_local(DownloadTaskView::local(&dto, None, false));
        let facts = task_menu_facts_for(&store, &RowKey::Local("bt".to_owned())).expect("facts");
        assert!(facts.bt_sequential);
        assert!(!facts.bt_first_last_pieces_first);
    }

    #[test]
    fn boost_toggles_label_by_boosted_state() {
        let mut boosted = task(true, TaskState::Downloading);
        boosted.boosted = true;
        assert!(context_menu_items(&[boosted]).contains(&MenuEntry::CancelBoost));

        let not_boosted = task(true, TaskState::Downloading);
        assert!(context_menu_items(&[not_boosted]).contains(&MenuEntry::Boost));

        // 已结束任务上的残留标记不提供「取消优先」，只给会提示的「优先下载」。
        for state in [TaskState::Completed, TaskState::Failed] {
            let mut stale = task(true, state);
            stale.boosted = true;
            let items = context_menu_items(&[stale]);
            assert!(items.contains(&MenuEntry::Boost));
            assert!(!items.contains(&MenuEntry::CancelBoost));
        }
    }

    #[test]
    fn missing_file_hides_open_and_copy_file_but_keeps_folder_and_redownload() {
        let mut missing = task(true, TaskState::Completed);
        missing.file_missing = true;
        let items = context_menu_items(&[missing]);
        assert!(!items.contains(&MenuEntry::OpenFile));
        assert!(!items.contains(&MenuEntry::CopyFile));
        assert!(items.contains(&MenuEntry::OpenFolder));
        assert!(items.contains(&MenuEntry::Redownload));
        // 与可打开的任务混选：「打开文件」「复制文件」要求全体成立。
        let mixed = context_menu_items(&[task(true, TaskState::Completed), missing]);
        assert!(!mixed.contains(&MenuEntry::OpenFile));
        assert!(!mixed.contains(&MenuEntry::CopyFile));
        // 多个产物都在：一次复制全部。
        let both = [
            task(true, TaskState::Completed),
            task(true, TaskState::Completed),
        ];
        assert!(context_menu_items(&both).contains(&MenuEntry::CopyFile));
    }

    #[test]
    fn torrent_sentinel_and_remote_tasks_cannot_redownload() {
        let mut sentinel = task(true, TaskState::Completed);
        sentinel.is_torrent_sentinel = true;
        assert!(!context_menu_items(&[sentinel]).contains(&MenuEntry::Redownload));

        let remote = task(false, TaskState::Completed);
        assert!(!context_menu_items(&[remote]).contains(&MenuEntry::Redownload));
    }

    #[test]
    fn change_url_needs_single_local_inactive_unfinished_http_ftp_task() {
        for state in [TaskState::Paused, TaskState::Failed, TaskState::Pending] {
            assert!(
                context_menu_items(&[task(true, state)]).contains(&MenuEntry::ChangeUrl),
                "{state:?}"
            );
        }
        // 下载中（引擎 task-active）与已完成（task-completed）不可换链。
        for state in [TaskState::Downloading, TaskState::Completed] {
            assert!(
                !context_menu_items(&[task(true, state)]).contains(&MenuEntry::ChangeUrl),
                "{state:?}"
            );
        }
        // 非 http(s)/ftp 源（BT / ed2k 等）、远程任务、多选均不显示。
        let mut unsupported = task(true, TaskState::Paused);
        unsupported.url_replaceable = false;
        assert!(!context_menu_items(&[unsupported]).contains(&MenuEntry::ChangeUrl));
        assert!(
            !context_menu_items(&[task(false, TaskState::Paused)]).contains(&MenuEntry::ChangeUrl)
        );
        let pair = [task(true, TaskState::Paused), task(true, TaskState::Paused)];
        assert!(!context_menu_items(&pair).contains(&MenuEntry::ChangeUrl));
    }

    #[test]
    fn change_segments_needs_single_local_unfinished_non_bt_task() {
        // 下载中也允许：引擎静默暂停 → 改 → 恢复。
        for state in [
            TaskState::Downloading,
            TaskState::Paused,
            TaskState::Failed,
            TaskState::Pending,
        ] {
            assert!(
                context_menu_items(&[task(true, state)]).contains(&MenuEntry::ChangeSegments),
                "{state:?}"
            );
        }
        assert!(
            !context_menu_items(&[task(true, TaskState::Completed)])
                .contains(&MenuEntry::ChangeSegments)
        );
        let mut bt = task(true, TaskState::Paused);
        bt.is_bt = true;
        assert!(!context_menu_items(&[bt]).contains(&MenuEntry::ChangeSegments));
        assert!(
            !context_menu_items(&[task(false, TaskState::Paused)])
                .contains(&MenuEntry::ChangeSegments)
        );
        let pair = [task(true, TaskState::Paused), task(true, TaskState::Failed)];
        assert!(!context_menu_items(&pair).contains(&MenuEntry::ChangeSegments));
    }

    #[test]
    fn verify_integrity_needs_single_local_completed_bt_task_with_files() {
        let mut bt = task(true, TaskState::Completed);
        bt.is_bt = true;
        let items = context_menu_items(&[bt]);
        let at = items
            .iter()
            .position(|entry| *entry == MenuEntry::VerifyIntegrity)
            .expect("completed BT task offers verification");
        assert_eq!(items.get(at + 1), Some(&MenuEntry::Redownload));

        let mut missing = bt;
        missing.file_missing = true;
        let mut unfinished = bt;
        unfinished.state = TaskState::Paused;
        let mut remote = bt;
        remote.is_local = false;
        for facts in [
            missing,
            unfinished,
            remote,
            task(true, TaskState::Completed),
        ] {
            assert!(
                !context_menu_items(&[facts]).contains(&MenuEntry::VerifyIntegrity),
                "{facts:?}"
            );
        }
        assert!(!context_menu_items(&[bt, bt]).contains(&MenuEntry::VerifyIntegrity));
    }

    fn unfinished_bt(state: TaskState) -> TaskMenuFacts {
        let mut facts = task(true, state);
        facts.is_bt = true;
        facts.url_replaceable = false;
        facts
    }

    #[test]
    fn piece_picking_shows_for_local_unfinished_bt_and_hides_otherwise() {
        for state in [
            TaskState::Downloading,
            TaskState::Paused,
            TaskState::Pending,
            TaskState::Failed,
        ] {
            let items = context_menu_items(&[unfinished_bt(state)]);
            let at = items
                .iter()
                .position(|entry| *entry == MenuEntry::RarestFirst)
                .expect("unfinished local BT offers piece picking");
            assert_eq!(
                items.get(at + 1),
                Some(&MenuEntry::SequentialDownload),
                "{state:?}"
            );
            assert_eq!(
                items.get(at + 2),
                Some(&MenuEntry::FirstLastPiecesFirst),
                "{state:?}"
            );
            let next = if state == TaskState::Failed {
                MenuEntry::Redownload
            } else {
                MenuEntry::CopyUrl
            };
            assert_eq!(items.get(at + 3), Some(&next), "{state:?}");
        }

        let pair = [
            unfinished_bt(TaskState::Paused),
            unfinished_bt(TaskState::Downloading),
        ];
        assert!(context_menu_items(&pair).contains(&MenuEntry::RarestFirst));
        assert!(context_menu_items(&pair).contains(&MenuEntry::SequentialDownload));
        assert!(context_menu_items(&pair).contains(&MenuEntry::FirstLastPiecesFirst));

        let mut completed = unfinished_bt(TaskState::Completed);
        completed.is_torrent_sentinel = true;
        let mut remote = unfinished_bt(TaskState::Paused);
        remote.is_local = false;
        let mixed = [
            unfinished_bt(TaskState::Paused),
            task(true, TaskState::Paused),
        ];
        for selection in [
            &[completed][..],
            &[remote][..],
            &[task(true, TaskState::Downloading)][..],
            &mixed[..],
            &[][..],
        ] {
            let items = context_menu_items(selection);
            assert!(!items.contains(&MenuEntry::RarestFirst), "{selection:?}");
            assert!(
                !items.contains(&MenuEntry::SequentialDownload),
                "{selection:?}"
            );
            assert!(
                !items.contains(&MenuEntry::FirstLastPiecesFirst),
                "{selection:?}"
            );
            assert!(piece_picking_order_targets(selection, true).is_empty());
            assert!(piece_picking_toggle_target(selection).is_none());
        }
    }

    #[test]
    fn piece_picking_order_is_unanimous_and_set_skips_matches() {
        let off = unfinished_bt(TaskState::Failed);
        let mut sequential = unfinished_bt(TaskState::Paused);
        sequential.bt_sequential = true;
        let mut both = unfinished_bt(TaskState::Downloading);
        both.bt_sequential = true;
        both.bt_first_last_pieces_first = true;
        let mut first_last_only = unfinished_bt(TaskState::Pending);
        first_last_only.bt_first_last_pieces_first = true;

        // 缺省（全体 sequential=false）选中最稀有优先，即使首尾已开。
        assert_eq!(
            piece_picking_order(&[off, first_last_only]),
            PiecePickingOrder::Rarest
        );
        assert_eq!(
            piece_picking_order(&[sequential, both]),
            PiecePickingOrder::Sequential
        );
        assert_eq!(
            piece_picking_order(&[off, sequential]),
            PiecePickingOrder::Mixed
        );
        assert_eq!(piece_picking_order(&[]), PiecePickingOrder::Mixed);

        // 设为最稀有优先只改尚未关闭 sequential 的任务；已是目标值则不下发。
        assert_eq!(
            piece_picking_order_targets(&[off, sequential, both], false),
            vec![1, 2]
        );
        assert!(piece_picking_order_targets(&[off, first_last_only], false).is_empty());
        // 设为按顺序下载只改尚未打开 sequential 的任务。
        assert_eq!(
            piece_picking_order_targets(&[sequential, off, first_last_only], true),
            vec![1, 2]
        );
        assert!(piece_picking_order_targets(&[sequential, both], true).is_empty());

        // 首尾勾选仍是独立开关：全部已开才勾，点击写入 !all_on。
        assert!(piece_picking_checked(&[both, both]));
        assert!(!piece_picking_checked(&[both, sequential]));
        assert!(!piece_picking_checked(&[off]));
        assert_eq!(piece_picking_toggle_target(&[both, sequential]), Some(true));
        assert_eq!(piece_picking_toggle_target(&[both, both]), Some(false));
        // 混合视为未勾，点击把全体设为开。
        assert_eq!(
            piece_picking_toggle_target(&[off, first_last_only]),
            Some(true)
        );
    }

    #[test]
    fn torrent_share_entries_need_single_local_bt_task_in_any_state() {
        let mut bt = task(true, TaskState::Completed);
        bt.is_bt = true;
        for state in [
            TaskState::Downloading,
            TaskState::Paused,
            TaskState::Completed,
            TaskState::Failed,
        ] {
            let mut facts = bt;
            facts.state = state;
            let items = context_menu_items(&[facts]);
            let at = items
                .iter()
                .position(|entry| *entry == MenuEntry::CopyUrl)
                .expect("copy URL is always offered");
            assert_eq!(items.get(at + 1), Some(&MenuEntry::CopyMagnet), "{state:?}");
            assert_eq!(
                items.get(at + 2),
                Some(&MenuEntry::ExportTorrent),
                "{state:?}"
            );
        }

        let mut remote = bt;
        remote.is_local = false;
        for facts in [remote, task(true, TaskState::Completed)] {
            let items = context_menu_items(&[facts]);
            assert!(!items.contains(&MenuEntry::CopyMagnet), "{facts:?}");
            assert!(!items.contains(&MenuEntry::ExportTorrent), "{facts:?}");
        }
        let items = context_menu_items(&[bt, bt]);
        assert!(!items.contains(&MenuEntry::CopyMagnet));
        assert!(!items.contains(&MenuEntry::ExportTorrent));
    }

    #[test]
    fn failed_local_task_menu_places_edit_entries_between_rename_and_redownload() {
        assert_eq!(
            context_menu_items(&[task(true, TaskState::Failed)]),
            vec![
                MenuEntry::Resume,
                MenuEntry::Pause,
                MenuEntry::Boost,
                MenuEntry::OpenFolder,
                MenuEntry::Rename,
                MenuEntry::ChangeUrl,
                MenuEntry::ChangeSegments,
                MenuEntry::Redownload,
                MenuEntry::CopyUrl,
                MenuEntry::MoveToQueue,
                MenuEntry::Separator,
                MenuEntry::Delete,
                MenuEntry::DeleteWithFiles,
                MenuEntry::ShowDetail,
            ]
        );
    }

    #[test]
    fn rename_is_offered_only_where_the_engine_accepts_it() {
        // 引擎 `rename_task`：BT 任务一律 `bt-unsupported`，下载 / 准备中 `task-active`。
        for state in [
            TaskState::Pending,
            TaskState::Paused,
            TaskState::Failed,
            TaskState::Completed,
        ] {
            assert!(
                context_menu_items(&[task(true, state)]).contains(&MenuEntry::Rename),
                "{state:?}"
            );
            let bt = TaskMenuFacts {
                is_bt: true,
                url_replaceable: false,
                ..task(true, state)
            };
            assert!(
                !context_menu_items(&[bt]).contains(&MenuEntry::Rename),
                "BT {state:?}"
            );
        }
        let preparing = TaskMenuFacts {
            preparing: true,
            ..task(true, TaskState::Pending)
        };
        for facts in [
            task(true, TaskState::Downloading),
            preparing,
            task(false, TaskState::Paused),
        ] {
            assert!(
                !context_menu_items(&[facts]).contains(&MenuEntry::Rename),
                "{facts:?}"
            );
        }
        let paused = task(true, TaskState::Paused);
        assert!(!context_menu_items(&[paused, paused]).contains(&MenuEntry::Rename));
    }

    #[test]
    fn url_classification_matches_engine_supported_protocols() {
        for url in ["http://a/b", "https://a/b", "HTTPS://a/b", "ftp://a/b"] {
            assert!(is_replaceable_url(url), "{url}");
            assert!(!is_bt_url(url), "{url}");
        }
        for url in ["magnet:?xt=urn:btih:abc", "torrent-file://local"] {
            assert!(is_bt_url(url), "{url}");
            assert!(!is_replaceable_url(url), "{url}");
        }
        for url in [
            "ed2k://|file|a|1|h|/",
            "sftp://a/b",
            "ftps://a/b",
            "",
            "htt",
        ] {
            assert!(!is_replaceable_url(url), "{url}");
            assert!(!is_bt_url(url), "{url}");
        }
    }

    #[test]
    fn select_entries_only_outside_multi_select_and_before_delete_group() {
        let selection = [task(true, TaskState::Completed)];
        let items = task_menu_entries(&selection, false, 0);
        let select = items.iter().position(|entry| *entry == MenuEntry::Select);
        let delete = items.iter().position(|entry| *entry == MenuEntry::Delete);
        assert!(matches!((select, delete), (Some(s), Some(d)) if s < d));
        assert!(items.contains(&MenuEntry::SelectAll));

        let in_mode = task_menu_entries(&selection, true, 0);
        assert!(!in_mode.contains(&MenuEntry::Select));
        assert!(!in_mode.contains(&MenuEntry::SelectAll));
        assert!(task_menu_entries(&[], false, 0).is_empty());
        assert!(task_menu_entries(&[], false, 3).is_empty());
    }

    #[test]
    fn plugin_commands_sit_between_select_group_and_delete_group() {
        let selection = [task(true, TaskState::Completed)];
        let items = task_menu_entries(&selection, false, 2);
        let pos = |entry: MenuEntry| items.iter().position(|e| *e == entry);
        let (select, p0, p1, delete) = (
            pos(MenuEntry::Select),
            pos(MenuEntry::Plugin(0)),
            pos(MenuEntry::Plugin(1)),
            pos(MenuEntry::Delete),
        );
        let (Some(select), Some(p0), Some(p1), Some(delete)) = (select, p0, p1, delete) else {
            panic!("missing entries: {items:?}");
        };
        assert!(select < p0 && p0 + 1 == p1 && p1 < delete);
        assert_eq!(items[p0 - 1], MenuEntry::Separator);
        assert_eq!(items[p1 + 1], MenuEntry::Separator);
        assert_eq!(items[p1 + 2], MenuEntry::Delete);

        let none = task_menu_entries(&selection, false, 0);
        assert!(!none.iter().any(|e| matches!(e, MenuEntry::Plugin(_))));

        let in_mode = task_menu_entries(&selection, true, 1);
        assert!(in_mode.contains(&MenuEntry::Plugin(0)));
        assert!(!in_mode.contains(&MenuEntry::Select));
    }
}

#[cfg(test)]
mod midnight_tests {
    use super::until_next_local_day;
    use chrono::{Local, TimeZone};

    /// 跨天定时器必须恰好落在下一个本地日期，不提前（同一天重算无效）也不晚一整天。
    #[test]
    fn next_day_timer_lands_just_after_local_midnight() {
        for (h, m, s) in [(0, 0, 0), (12, 30, 0), (23, 59, 59)] {
            let Some(now) = Local.with_ymd_and_hms(2026, 3, 14, h, m, s).earliest() else {
                continue;
            };
            let delay = until_next_local_day(now);
            let Ok(delay) = chrono::Duration::from_std(delay) else {
                panic!("delay out of range");
            };
            let fired = now + delay;
            assert_eq!(
                fired.date_naive(),
                now.date_naive().succ_opt().unwrap_or_default()
            );
            assert!(delay <= chrono::Duration::hours(25), "{h}:{m}:{s}");
        }
    }
}
