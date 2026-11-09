//! 下载页视图偏好（全局单套，设备本地，键 `desktop.downloads.view`）。

use std::cmp::Ordering;

use chrono::{Datelike, Local, NaiveDate, TimeZone};
use fluxdown_ui_theme::DensityTokens;
use gpui::{Pixels, px};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{DownloadTaskView, TaskState};

pub(crate) const VIEW_PREFS_KEY: &str = "desktop.downloads.view";
/// 设备本地偏好（缺省关）：任务列表 / 详情副标题不展示来源站点；设置「通用 › 任务列表」写入，
/// `crates/settings/src/sections/general.rs` 重复声明同名键（设置能力不依赖下载能力）。
pub(crate) const HIDE_TASK_SOURCE_PREF: &str = "ui.hide_task_source";
/// 宽松密度行高 = 旧 Flutter 默认列表的 64px（Web `rowHeightClass` 同值）。字号放大到舒适行
/// 本身超过它时取舒适行高，保证两行文字不被裁切。
const RELAXED_ROW_HEIGHT: f32 = 64.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ViewDensity {
    #[default]
    Comfortable,
    Compact,
    Relaxed,
}

impl ViewDensity {
    /// 任务行高：舒适双行取 `density.taskRow`（默认 44 = 13/18 正文 + 12/16 元信息 + 上下留白，
    /// 随字号增长），紧凑单行取 `density.taskRowCompact`（默认 30），宽松双行固定 64（旧 Flutter
    /// 默认列表行高，不随字号叠加），仅在舒适行高更大时跟随它。
    pub(crate) fn row_height(self, density: &DensityTokens) -> Pixels {
        match self {
            Self::Comfortable => density.task_row,
            Self::Compact => density.task_row_compact,
            Self::Relaxed => density.task_row.max(px(RELAXED_ROW_HEIGHT)),
        }
    }

    /// 是否渲染第二行元信息（名称列的类别 · 域名、状态列的详情）。
    pub(crate) fn two_line(self) -> bool {
        self != Self::Compact
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Comfortable => Self::Relaxed,
            Self::Relaxed => Self::Compact,
            Self::Compact => Self::Comfortable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ViewGroupBy {
    #[default]
    None,
    Status,
    Date,
    Type,
    Queue,
    Site,
}

impl ViewGroupBy {
    const CYCLE: [Self; 6] = [
        Self::None,
        Self::Status,
        Self::Date,
        Self::Type,
        Self::Queue,
        Self::Site,
    ];

    pub(crate) fn next(self) -> Self {
        let ix = Self::CYCLE
            .iter()
            .position(|item| *item == self)
            .unwrap_or(0);
        Self::CYCLE[(ix + 1) % Self::CYCLE.len()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ViewSortKey {
    /// 默认排序：状态优先级，再按添加时间新→旧。
    #[default]
    Smart,
    Created,
    Name,
    Size,
    Progress,
    Speed,
    /// 按状态优先级排序，可升降序（「状态」列表头）。
    Status,
}

impl ViewSortKey {
    pub(super) const CYCLE: [Self; 7] = [
        Self::Smart,
        Self::Created,
        Self::Name,
        Self::Size,
        Self::Progress,
        Self::Speed,
        Self::Status,
    ];

    pub(crate) fn next(self) -> Self {
        let ix = Self::CYCLE
            .iter()
            .position(|item| *item == self)
            .unwrap_or(0);
        Self::CYCLE[(ix + 1) % Self::CYCLE.len()]
    }

    /// 首次选中该键时的方向：名称 A→Z，其余（含智能）从大到小。
    pub(crate) fn default_dir(self) -> SortDir {
        match self {
            Self::Name => SortDir::Asc,
            _ => SortDir::Desc,
        }
    }

    /// 排序值随每次进度节拍变化的键（速度 / 进度）：重排需要限频，否则列表持续跳动。
    pub(crate) fn is_live(self) -> bool {
        matches!(self, Self::Progress | Self::Speed)
    }

    /// 该键在 [`TaskStore`](super::TaskStore) 变更记录里的位掩码（与 `RowChange::Updated::sort`
    /// 的下标一致）。
    pub(crate) fn change_bit(self) -> u16 {
        Self::CYCLE
            .iter()
            .position(|candidate| *candidate == self)
            .map_or(u16::MAX, |ix| 1 << ix)
    }

    /// 比较器实际读取的值是否相同；所有排序都包含添加顺序与任务 key 的平局回退。
    /// NaN 不视为相同，无法证明其与其他行的比较结果不变时重新排序。
    pub(super) fn same_value(self, left: &DownloadTaskView, right: &DownloadTaskView) -> bool {
        if left.key != right.key || added_order(left, right) != Ordering::Equal {
            return false;
        }
        match self {
            Self::Smart => compare_smart(left, right) == Ordering::Equal,
            Self::Created => true,
            Self::Name => left.name_fold == right.name_fold,
            Self::Size => left.size_bytes == right.size_bytes,
            Self::Progress => left.progress == right.progress,
            Self::Speed => {
                left.speed_bytes_per_second.unwrap_or(0)
                    == right.speed_bytes_per_second.unwrap_or(0)
            }
            Self::Status => left.state.status_rank() == right.state.status_rank(),
        }
    }

    /// 该行的排队键是否影响排序：只有智能排序的排队档按键排列。
    pub(super) fn orders_by_queue_key(self, task: &DownloadTaskView) -> bool {
        matches!(self, Self::Smart) && smart_tier(task) == 2
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SortDir {
    Asc,
    #[default]
    Desc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DetailPlacement {
    #[default]
    Bottom,
    Right,
}

impl DetailPlacement {
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Bottom => Self::Right,
            Self::Right => Self::Bottom,
        }
    }
}

/// 任务表表头文字对齐（视图菜单 › 显示）；默认左对齐，单元格内容对齐不受影响。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HeaderAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ColumnPref {
    /// 列键（`DownloadColumnKind::key`）。
    pub(crate) key: String,
    pub(crate) visible: bool,
    #[serde(default)]
    pub(crate) width: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct ViewPrefs {
    pub(crate) density: ViewDensity,
    pub(crate) header_align: HeaderAlign,
    pub(crate) group_by: ViewGroupBy,
    pub(crate) sort_key: ViewSortKey,
    pub(crate) sort_dir: SortDir,
    /// 空 = 使用默认列集。
    pub(crate) columns: Vec<ColumnPref>,
    /// 文件名列拖过后的固定宽度；`None` = 吸收表格剩余宽度（默认）。
    pub(crate) file_name_width: Option<f32>,
    pub(crate) detail_placement: DetailPlacement,
    pub(crate) detail_open: bool,
    pub(crate) detail_size: f32,
    pub(crate) sidebar_width: f32,
    pub(crate) sidebar_collapsed: bool,
    pub(crate) collapsed_groups: Vec<String>,
}

impl Default for ViewPrefs {
    fn default() -> Self {
        Self {
            density: ViewDensity::default(),
            header_align: HeaderAlign::default(),
            group_by: ViewGroupBy::default(),
            sort_key: ViewSortKey::default(),
            sort_dir: SortDir::default(),
            columns: Vec::new(),
            file_name_width: None,
            detail_placement: DetailPlacement::default(),
            detail_open: false,
            detail_size: 260.,
            sidebar_width: 200.,
            sidebar_collapsed: false,
            collapsed_groups: Vec::new(),
        }
    }
}

impl ViewPrefs {
    /// 逐字段容错：某个字段缺失或取值未知（例如新版本写入的排序键）只让该字段回到默认值，
    /// 不连带重置列宽、分组等其他偏好；非对象返回默认值。
    pub(crate) fn from_value(value: &serde_json::Value) -> Self {
        let mut prefs = Self::default();
        let Some(map) = value.as_object() else {
            return prefs;
        };
        read_field(map, "density", &mut prefs.density);
        read_field(map, "header_align", &mut prefs.header_align);
        read_field(map, "group_by", &mut prefs.group_by);
        read_field(map, "sort_key", &mut prefs.sort_key);
        read_field(map, "sort_dir", &mut prefs.sort_dir);
        if let Some(serde_json::Value::Array(items)) = map.get("columns") {
            prefs.columns = items
                .iter()
                .filter_map(|item| ColumnPref::deserialize(item).ok())
                .collect();
        }
        read_field(map, "file_name_width", &mut prefs.file_name_width);
        read_field(map, "detail_placement", &mut prefs.detail_placement);
        read_field(map, "detail_open", &mut prefs.detail_open);
        read_field(map, "detail_size", &mut prefs.detail_size);
        read_field(map, "sidebar_width", &mut prefs.sidebar_width);
        read_field(map, "sidebar_collapsed", &mut prefs.sidebar_collapsed);
        read_field(map, "collapsed_groups", &mut prefs.collapsed_groups);
        prefs
    }

    pub(crate) fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    pub(crate) fn cycle_density(&mut self) {
        self.density = self.density.next();
    }

    pub(crate) fn cycle_group_by(&mut self) {
        self.group_by = self.group_by.next();
    }

    pub(crate) fn cycle_sort(&mut self) {
        self.select_sort_key(self.sort_key.next());
    }

    /// 选择排序键；键真正变化时方向重置为该键的默认方向（与 web / 表头一致）。
    pub(crate) fn select_sort_key(&mut self, key: ViewSortKey) {
        if self.sort_key != key {
            self.sort_key = key;
            self.sort_dir = key.default_dir();
        }
    }

    pub(crate) fn is_group_collapsed(&self, key: &str) -> bool {
        self.collapsed_groups.iter().any(|item| item == key)
    }

    pub(crate) fn toggle_group_collapsed(&mut self, key: &str) {
        if let Some(ix) = self.collapsed_groups.iter().position(|item| item == key) {
            self.collapsed_groups.remove(ix);
        } else {
            self.collapsed_groups.push(key.to_owned());
        }
    }

    /// 排序比较。`Smart` 见 [`compare_smart`]（忽略 `sort_dir`）；其他键按 `sort_dir`，
    /// 同值时回落到添加顺序（新→旧）。
    pub(crate) fn compare(&self, left: &DownloadTaskView, right: &DownloadTaskView) -> Ordering {
        let ordering = match self.sort_key {
            ViewSortKey::Smart => return compare_smart(left, right),
            ViewSortKey::Created => left.created_at_secs.cmp(&right.created_at_secs),
            ViewSortKey::Name => natural_cmp(&left.name_fold, &right.name_fold),
            ViewSortKey::Size => left.size_bytes.cmp(&right.size_bytes),
            ViewSortKey::Progress => left
                .progress
                .partial_cmp(&right.progress)
                .unwrap_or(Ordering::Equal),
            ViewSortKey::Speed => left
                .speed_bytes_per_second
                .unwrap_or(0)
                .cmp(&right.speed_bytes_per_second.unwrap_or(0)),
            ViewSortKey::Status => left.state.status_rank().cmp(&right.state.status_rank()),
        };
        let ordering = match self.sort_dir {
            SortDir::Asc => ordering,
            SortDir::Desc => ordering.reverse(),
        };
        ordering
            .then_with(|| added_order(right, left))
            .then_with(|| left.key.cmp(&right.key))
    }
}

fn read_field<T: DeserializeOwned>(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    slot: &mut T,
) {
    if let Some(value) = map.get(key).and_then(|value| T::deserialize(value).ok()) {
        *slot = value;
    }
}

/// 智能排序档位：优先下载 → 活跃（下载中 / 准备中）→ 排队 → 失败 → 暂停 → 完成。
fn smart_tier(task: &DownloadTaskView) -> u8 {
    match task.state {
        TaskState::Downloading | TaskState::Pending if task.boosted => 0,
        TaskState::Downloading => 1,
        TaskState::Pending if task.preparing => 1,
        TaskState::Pending => 2,
        TaskState::Failed => 3,
        TaskState::Paused => 4,
        TaskState::Completed => 5,
    }
}

/// 添加顺序（旧→新）：`created_at` 只精确到秒，同一秒批量添加时用队列插入序定先后。
fn added_order(left: &DownloadTaskView, right: &DownloadTaskView) -> Ordering {
    left.created_at_secs
        .cmp(&right.created_at_secs)
        .then_with(|| left.queue_order.cmp(&right.queue_order))
}

/// 引擎待启动顺序（排序键升序）；不在队列中的排在所有已知键之后。
fn queue_slot(task: &DownloadTaskView) -> i64 {
    task.queue_key.unwrap_or(i64::MAX)
}

/// 智能排序：先按 [`smart_tier`]；活跃档按添加顺序正序（新任务接在末尾，不把其他行往下推），
/// 排队档按引擎实际启动顺序，其余历史档按添加顺序倒序（最新在前）。
fn compare_smart(left: &DownloadTaskView, right: &DownloadTaskView) -> Ordering {
    let tier = smart_tier(left);
    tier.cmp(&smart_tier(right))
        .then_with(|| match tier {
            0 | 1 => added_order(left, right),
            2 => queue_slot(left)
                .cmp(&queue_slot(right))
                .then_with(|| added_order(left, right)),
            _ => added_order(right, left),
        })
        .then_with(|| left.key.cmp(&right.key))
}

/// 自然序：连续 ASCII 数字段按数值比较（`ep2 < ep10`），其余按字节比较；
/// 各段都相等时由原始字符串定序（`a01` 与 `a1` 不相等）。不分配内存。
pub(crate) fn natural_cmp(left: &str, right: &str) -> Ordering {
    let (mut a, mut b) = (left, right);
    while let (Some(a_first), Some(b_first)) = (a.bytes().next(), b.bytes().next()) {
        let (a_run, a_rest) = split_run(a, a_first.is_ascii_digit());
        let (b_run, b_rest) = split_run(b, b_first.is_ascii_digit());
        let ordering = if a_first.is_ascii_digit() && b_first.is_ascii_digit() {
            let a_digits = a_run.trim_start_matches('0');
            let b_digits = b_run.trim_start_matches('0');
            a_digits
                .len()
                .cmp(&b_digits.len())
                .then_with(|| a_digits.cmp(b_digits))
        } else {
            a_run.cmp(b_run)
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
        (a, b) = (a_rest, b_rest);
    }
    a.len().cmp(&b.len()).then_with(|| left.cmp(right))
}

/// 切出开头的一段（全数字或全非数字）。ASCII 数字是单字节字符，切点必在字符边界上。
fn split_run(value: &str, digits: bool) -> (&str, &str) {
    let end = value
        .bytes()
        .position(|byte| byte.is_ascii_digit() != digits)
        .unwrap_or(value.len());
    value.split_at(end)
}

/// 日期分组桶（基于本地日期）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum DateBucket {
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    Older,
}

impl DateBucket {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::ThisWeek => "this_week",
            Self::ThisMonth => "this_month",
            Self::Older => "older",
        }
    }

    pub(crate) fn of(created_at_secs: i64) -> Self {
        Self::relative_to(created_at_secs, Local::now().date_naive())
    }

    fn relative_to(created_at_secs: i64, today: NaiveDate) -> Self {
        let Some(created) = Local
            .timestamp_opt(created_at_secs, 0)
            .single()
            .map(|value| value.date_naive())
        else {
            return Self::Older;
        };
        if created >= today {
            Self::Today
        } else if created == today.pred_opt().unwrap_or(today) {
            Self::Yesterday
        } else if created.iso_week() == today.iso_week() && created.year() == today.year() {
            Self::ThisWeek
        } else if created.month() == today.month() && created.year() == today.year() {
            Self::ThisMonth
        } else {
            Self::Older
        }
    }
}

/// 状态分组键（稳定，用于折叠记忆）。
pub(crate) fn state_group_key(state: TaskState) -> &'static str {
    match state {
        TaskState::Pending => "pending",
        TaskState::Downloading => "downloading",
        TaskState::Paused => "paused",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Local, NaiveDate, TimeZone};

    use std::cmp::Ordering;

    use super::{
        DateBucket, SortDir, ViewDensity, ViewGroupBy, ViewPrefs, ViewSortKey, natural_cmp,
    };
    use crate::model::DownloadTaskView;

    fn task(id: &str, status: i32, created: &str, size: i64) -> DownloadTaskView {
        let dto = serde_json::from_value::<fluxdown_protocol::TaskDto>(serde_json::json!({
            "taskId":id,"url":"https://example.com/x","fileName":format!("{id}.bin"),
            "saveDir":"/tmp","status":status,"downloadedBytes":0,"totalBytes":size,
            "errorMessage":"","createdAt":created,"proxyUrl":"","queueId":"main","checksum":""
        }))
        .expect("task");
        DownloadTaskView::local(&dto, None, false)
    }

    fn sorted_ids(prefs: &ViewPrefs, mut rows: Vec<DownloadTaskView>) -> Vec<String> {
        rows.sort_by(|a, b| prefs.compare(a, b));
        rows.iter()
            .map(|row| row.key.task_id().to_owned())
            .collect()
    }

    #[test]
    fn selecting_sort_key_resets_direction_only_when_key_changes() {
        let mut prefs = ViewPrefs::default();
        prefs.select_sort_key(ViewSortKey::Name);
        assert_eq!(
            (prefs.sort_key, prefs.sort_dir),
            (ViewSortKey::Name, SortDir::Asc)
        );
        prefs.sort_dir = SortDir::Desc;
        prefs.select_sort_key(ViewSortKey::Name);
        assert_eq!(prefs.sort_dir, SortDir::Desc);
        prefs.select_sort_key(ViewSortKey::Size);
        assert_eq!(
            (prefs.sort_key, prefs.sort_dir),
            (ViewSortKey::Size, SortDir::Desc)
        );
    }

    #[test]
    fn density_cycle_preserves_each_choice_across_reload() {
        let mut prefs = ViewPrefs {
            density: ViewDensity::Compact,
            ..ViewPrefs::default()
        };
        for density in [
            ViewDensity::Comfortable,
            ViewDensity::Relaxed,
            ViewDensity::Compact,
        ] {
            prefs.cycle_density();
            prefs = ViewPrefs::from_value(&prefs.to_value());
            assert_eq!(prefs.density, density);
            assert_eq!(prefs.density.two_line(), density != ViewDensity::Compact);
        }
    }

    #[test]
    fn prefs_round_trip_and_tolerate_unknown_fields() {
        let mut prefs = ViewPrefs {
            density: ViewDensity::Compact,
            group_by: ViewGroupBy::Site,
            sort_key: ViewSortKey::Size,
            sort_dir: SortDir::Asc,
            sidebar_collapsed: true,
            ..ViewPrefs::default()
        };
        prefs.collapsed_groups.push("x".to_owned());
        let value = prefs.to_value();
        assert_eq!(ViewPrefs::from_value(&value), prefs);
        assert_eq!(
            ViewPrefs::from_value(&serde_json::json!({"density":"compact","bogus":1})).density,
            ViewDensity::Compact
        );
        assert_eq!(
            ViewPrefs::from_value(&serde_json::json!("garbage")),
            ViewPrefs::default()
        );
        // 未知取值（例如新版本写入的排序键）只回退该字段，其余偏好保留。
        let future = ViewPrefs::from_value(&serde_json::json!({
            "density":"compact","sort_key":"future_key","sidebar_width":240.0,
            "columns":[{"key":"size","visible":false,"width":90.0},{"bogus":true}]
        }));
        assert_eq!(future.sort_key, ViewSortKey::Smart);
        assert_eq!(future.density, ViewDensity::Compact);
        assert_eq!(future.sidebar_width, 240.);
        assert_eq!(future.columns.len(), 1);
    }

    #[test]
    fn smart_sort_lists_work_in_execution_order_then_history_newest_first() {
        let with = |id: &str, status: i32, created: &str, edit: fn(&mut DownloadTaskView)| {
            let mut row = task(id, status, created, 1);
            edit(&mut row);
            row
        };
        let rows = vec![
            task("done-old", 3, "50", 1),
            task("done-new", 3, "60", 1),
            // 优先下载标记残留在已完成任务上不影响历史档。
            with("done-boosted", 3, "55", |row| row.boosted = true),
            task("paused", 2, "70", 1),
            task("failed", 4, "40", 1),
            task("dl-new", 1, "30", 1),
            task("dl-old", 1, "15", 1),
            task("preparing", 5, "35", 1),
            with("queued-2", 0, "10", |row| row.queue_key = Some(200)),
            with("queued-1", 0, "20", |row| row.queue_key = Some(100)),
            task("queued-unknown", 0, "5", 1),
            with("boosted", 0, "90", |row| row.boosted = true),
        ];
        assert_eq!(
            sorted_ids(&ViewPrefs::default(), rows),
            [
                "boosted",
                "dl-old",
                "dl-new",
                "preparing",
                "queued-1",
                "queued-2",
                "queued-unknown",
                "failed",
                "paused",
                "done-new",
                "done-boosted",
                "done-old",
            ]
        );
    }

    #[test]
    fn same_second_batch_follows_insertion_order_not_task_id() {
        let batch = |id: &str, status: i32, queue_order: i32| {
            let mut row = task(id, status, "100", 1);
            row.queue_order = queue_order;
            row
        };
        let prefs = ViewPrefs::default();
        // 历史档最新在前：后插入的（queue_order 大）在上。
        assert_eq!(
            sorted_ids(
                &prefs,
                vec![batch("x", 3, 1), batch("z", 3, 3), batch("y", 3, 2)]
            ),
            ["z", "y", "x"]
        );
        // 活跃档按添加正序。
        assert_eq!(
            sorted_ids(&prefs, vec![batch("a", 1, 2), batch("z", 1, 1)]),
            ["z", "a"]
        );
        // 显式列排序同值时同样回落到添加顺序（新→旧）。
        let by_size = ViewPrefs {
            sort_key: ViewSortKey::Size,
            ..ViewPrefs::default()
        };
        assert_eq!(
            sorted_ids(&by_size, vec![batch("x", 3, 1), batch("y", 3, 2)]),
            ["y", "x"]
        );
    }

    #[test]
    fn name_sort_is_natural_and_case_insensitive() {
        let prefs = ViewPrefs {
            sort_key: ViewSortKey::Name,
            sort_dir: SortDir::Asc,
            ..ViewPrefs::default()
        };
        let rows = ["Ep10", "ep2", "EP1", "ep02b"]
            .into_iter()
            .map(|id| task(id, 3, "1", 1))
            .collect();
        assert_eq!(sorted_ids(&prefs, rows), ["EP1", "ep2", "ep02b", "Ep10"]);
        assert_eq!(natural_cmp("file9", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("a", "a1"), Ordering::Less);
        assert_ne!(natural_cmp("a01", "a1"), Ordering::Equal);
        assert_eq!(natural_cmp("视频2", "视频10"), Ordering::Less);
    }

    #[test]
    fn status_sort_puts_failed_before_paused() {
        let prefs = ViewPrefs {
            sort_key: ViewSortKey::Status,
            sort_dir: SortDir::Asc,
            ..ViewPrefs::default()
        };
        let rows = vec![
            task("completed", 3, "1", 1),
            task("paused", 2, "1", 1),
            task("failed", 4, "1", 1),
            task("pending", 0, "1", 1),
            task("downloading", 1, "1", 1),
        ];
        assert_eq!(
            sorted_ids(&prefs, rows),
            ["downloading", "pending", "failed", "paused", "completed"]
        );
    }

    #[test]
    fn date_buckets_use_local_day_boundaries() {
        let today = NaiveDate::from_ymd_opt(2026, 3, 18).expect("date"); // Wednesday
        let at = |y, m, d, h| {
            Local
                .with_ymd_and_hms(y, m, d, h, 0, 0)
                .single()
                .expect("ts")
                .timestamp()
        };
        assert_eq!(
            DateBucket::relative_to(at(2026, 3, 18, 0), today),
            DateBucket::Today
        );
        assert_eq!(
            DateBucket::relative_to(at(2026, 3, 17, 23), today),
            DateBucket::Yesterday
        );
        assert_eq!(
            DateBucket::relative_to(at(2026, 3, 16, 1), today),
            DateBucket::ThisWeek
        );
        assert_eq!(
            DateBucket::relative_to(at(2026, 3, 2, 1), today),
            DateBucket::ThisMonth
        );
        assert_eq!(
            DateBucket::relative_to(at(2026, 2, 27, 1), today),
            DateBucket::Older
        );
    }
}
