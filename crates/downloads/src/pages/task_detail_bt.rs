//! 任务详情「Tracker」「连接」页签：BT 实况（`daemon.task.btStatus`）的轮询与渲染。
//!
//! 轮询生命周期完全由渲染驱动：页签可见时 [`TaskDetailView::bt_touch`] 每帧登记「最近一次渲染」
//! 并在没有轮询循环时启动一个；循环每 2 s 请求一次（不重叠），发现页签不再激活 / 面板不再渲染 /
//! 任务切换 / 视图释放就自行退出。数据展示逻辑（相对时间、列布局、横幅判定、行映射）都是纯函数。

use std::{
    cell::Cell,
    collections::HashSet,
    rc::Rc,
    time::{Duration, Instant},
};

use fluxdown_protocol::{
    BtPeerDto, BtTrackerDto, BtTrackerSource, BtTrackerStatus, DaemonBtStatusParams, RpcErrorData,
};
use fluxdown_ui_components::{FluxIcon, tabular_numbers, tooltip_view};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ListAlignment, ListSizingBehavior, ListState, ParentElement, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled, Window, canvas, div, list,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Icon, WindowExt as _, h_flex,
    menu::{ContextMenuExt as _, PopupMenuItem},
    notification::Notification,
    skeleton::Skeleton,
    v_flex,
};

use super::{DetailTab, TaskDetailView, rate_text};
use crate::{
    controller::{DownloadsCommand, DownloadsResult, PortFuture},
    flags::flag_slot,
    model::bt_guard::{
        BanChoice, GuardMenuAction, PeerAction, PeerGuard, Suspicion, action_text, any_guard,
        entry_params, rules_text, tooltip_text,
    },
    model::bt_trackers::{
        TrackerCounts, TrackerFilter, TrackerSort, TrackerSortKey, next_sort, tracker_view,
    },
    model::{RowKey, TaskState, format_bytes},
    strings::error_text,
};

/// 轮询间隔。
const BT_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 距最近一次渲染超过该时长即视为页签 / 面板已不可见，轮询循环退出。
const BT_VISIBLE_GRACE: Duration = Duration::from_secs(5);
/// 相对时间随 1 s 定时器重绘所需的「仍在显示」判定窗口。
const BT_CLOCK_VISIBLE: Duration = Duration::from_millis(2_500);
/// 连接页最多渲染的 peer 数（服务端已按活跃度排序）。
const BT_PEER_RENDER_CAP: usize = 200;
/// 宽 / 中布局的分界宽度（界面缩放前的 px）。
const WIDE_MIN_WIDTH: f32 = 800.;
const MEDIUM_MIN_WIDTH: f32 = 520.;
/// 用 `—` 表示未知 / 无。
const DASH: &str = "—";
/// tracker 虚拟列表视口外预渲染高度（px）。
const TRACKER_LIST_OVERDRAW: f32 = 240.;
/// 未量过的 tracker 行高估值（px），用于首帧总高与滚动条。
const TRACKER_ROW_HINT: f32 = 32.;

// 列宽（界面缩放前的 px）。
const COL_STATUS: f32 = 128.;
const COL_PEERS: f32 = 56.;
const COL_MESSAGE: f32 = 190.;
const COL_TIME: f32 = 100.;
const COL_CLIENT: f32 = 140.;
const COL_TRANSPORT: f32 = 60.;
const COL_BYTES: f32 = 84.;
const COL_SPEED: f32 = 92.;
const COL_SPEED_STACK: f32 = 96.;
const COL_GUARD_BYTES: f32 = 88.;
const COL_GUARD_STATE: f32 = 164.;
const COL_GUARD_ACTION: f32 = 150.;
/// 连接表并排防吸血三列所需的最小内容宽度（界面缩放前的 px）。
const GUARD_COLUMNS_MIN_WIDTH: f32 = 1_180.;
/// 国旗槽位宽度（界面缩放前的 px；高 = 宽 × 3/4，即 16×12）。
const FLAG_WIDTH: f32 = 16.;

/// 页签内容区的响应式布局档。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BtLayout {
    /// 全部列并排。
    Wide,
    /// 次要列并入第二行（Tracker 的消息、连接的客户端 / 累计量）。
    Medium,
    /// 窄面板：每行堆叠为多行，不再有表头。
    Narrow,
}

impl BtLayout {
    /// `width` 为界面缩放前的 px；非正（尚未量到）按宽布局。
    pub(super) fn for_width(width: f32) -> Self {
        if width <= 0. || width >= WIDE_MIN_WIDTH {
            Self::Wide
        } else if width >= MEDIUM_MIN_WIDTH {
            Self::Medium
        } else {
            Self::Narrow
        }
    }
}

/// 相对时间的量级单位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SpanUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Span {
    pub(super) unit: SpanUnit,
    pub(super) value: u64,
}

/// 把秒数折成最粗的整数单位（向下取整）。
pub(super) fn span_of(seconds: u64) -> Span {
    let (unit, value) = if seconds < 60 {
        (SpanUnit::Seconds, seconds)
    } else if seconds < 3_600 {
        (SpanUnit::Minutes, seconds / 60)
    } else if seconds < 86_400 {
        (SpanUnit::Hours, seconds / 3_600)
    } else {
        (SpanUnit::Days, seconds / 86_400)
    };
    Span { unit, value }
}

/// tracker 时间列的显示意图（文案在渲染时按语言拼装）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RelativeTime {
    /// 服务端不知道（0 / 未汇报）。
    Unknown,
    /// 从未成功。
    Never,
    In(Span),
    Ago(Span),
}

/// 下次 announce：0 = 未知；已过期按「0 秒后」。
pub(super) fn next_announce(next_at: i64, now: i64) -> RelativeTime {
    if next_at <= 0 {
        RelativeTime::Unknown
    } else {
        RelativeTime::In(span_of(next_at.saturating_sub(now).max(0).unsigned_abs()))
    }
}

/// 最近成功：0 = 从未；时钟回拨时按「0 秒前」。
pub(super) fn last_success(last_ok_at: i64, now: i64) -> RelativeTime {
    if last_ok_at <= 0 {
        RelativeTime::Never
    } else {
        RelativeTime::Ago(span_of(
            now.saturating_sub(last_ok_at).max(0).unsigned_abs(),
        ))
    }
}

/// 「没有任何 tracker 应答」横幅是否显示：有 tracker、宿主提供状态（无 `unknown`）、没有
/// `working`，且至少一个确认失败（`notWorking`）。全部仍在 `updating` / `notContacted`
/// （刚启动或任务未运行）时不报警，避免首次 announce 期间误报。
pub(super) fn none_working(trackers: &[BtTrackerDto]) -> bool {
    let any = |status: BtTrackerStatus| trackers.iter().any(|tracker| tracker.status == status);
    !trackers.is_empty()
        && !any(BtTrackerStatus::Unknown)
        && !any(BtTrackerStatus::Working)
        && any(BtTrackerStatus::NotWorking)
}

/// 一个 tracker 行的展示数据（纯映射，便于测试）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TrackerRow {
    pub(super) url: String,
    pub(super) source: BtTrackerSource,
    pub(super) status: BtTrackerStatus,
    pub(super) peers: Option<u64>,
    pub(super) message: String,
    /// 原始错误文本（供复制）；服务端未给时回退到 `message`。
    pub(super) detail: String,
    pub(super) next: RelativeTime,
    pub(super) last: RelativeTime,
}

pub(super) fn tracker_row(dto: &BtTrackerDto, now: i64) -> TrackerRow {
    // 未在汇报（暂停 / 未运行 / 宿主不采集）时「下次 announce」没有意义。
    let reporting = matches!(
        dto.status,
        BtTrackerStatus::Working | BtTrackerStatus::Updating | BtTrackerStatus::NotWorking
    );
    TrackerRow {
        url: dto.url.clone(),
        source: dto.source,
        status: dto.status,
        peers: dto.peers,
        message: dto.message.trim().to_owned(),
        detail: {
            let detail = dto.detail.trim();
            if detail.is_empty() {
                dto.message.trim()
            } else {
                detail
            }
            .to_owned()
        },
        next: if reporting {
            next_announce(dto.next_announce_at, now)
        } else {
            RelativeTime::Unknown
        },
        last: last_success(dto.last_ok_at, now),
    }
}

/// 连接类型徽标文案：`tcp` / `utp` / `socks`，未知值原样显示。
pub(super) fn transport_label(transport: &str) -> String {
    match transport {
        "tcp" => "TCP".to_owned(),
        "utp" => "uTP".to_owned(),
        "socks" => "SOCKS".to_owned(),
        other => other.to_owned(),
    }
}

/// 中 / 窄布局下 peer 第二行：`客户端 · ↓ 累计下载 · ↑ 累计上传`（累计均为 0 时省略）。
pub(super) fn peer_sub_line(peer: &BtPeerDto) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(3);
    let client = peer.client.trim();
    if !client.is_empty() {
        parts.push(client.to_owned());
    }
    if peer.downloaded > 0 || peer.uploaded > 0 {
        parts.push(format!("↓ {}", format_bytes(peer.downloaded)));
        parts.push(format!("↑ {}", format_bytes(peer.uploaded)));
    }
    parts.join(" · ")
}

// ───────────────────────── 防吸血 ─────────────────────────

/// 防吸血三列的表头文案。
struct GuardHeaderText {
    uploaded: SharedString,
    state: SharedString,
    action: SharedString,
}

/// 连接行右键菜单里的防吸血命令。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuardCommand {
    /// 给该 IP 组加封禁 / 限速 / 白名单条目。
    Entry(GuardMenuAction),
    /// 打开证据对话框。
    Evidence,
}

type GuardHandler = Rc<dyn Fn(BtPeerDto, GuardCommand, &mut Window, &mut App)>;

/// 右键菜单里防吸血项的文案（按 [`BanChoice::ALL`] 顺序的封禁项）。
struct GuardMenuLabels {
    bans: [SharedString; 4],
    throttle: SharedString,
    allow: SharedString,
    evidence: SharedString,
}

/// 连接行右键菜单里防吸血部分所需的一切（菜单打开时才用）。
#[derive(Clone)]
struct GuardMenu {
    peer: BtPeerDto,
    labels: Rc<GuardMenuLabels>,
    handler: GuardHandler,
    /// 该 IP 组有动作在途：条目类菜单项禁用。
    busy: bool,
}

/// 一个连接的防吸血展示格（文案已本地化、颜色已选定）。
struct PeerGuardCells {
    /// 宽屏并排三列；否则只有地址旁徽标、悬浮提示与第二行的命中规则。
    columns: bool,
    delivered: u64,
    suspicion: Option<(SharedString, Hsla)>,
    rules: SharedString,
    action: Option<(SharedString, Hsla)>,
    tooltip: SharedString,
    /// 中 / 窄布局第二行追加的命中规则文字。
    sub: String,
}

/// 嫌疑等级的文字 / 描边色：弱 = 提示，中 = 警示，强 = 危险。
fn suspicion_color(look: &Look, suspicion: Suspicion) -> Hsla {
    match suspicion {
        Suspicion::Strong => look.destructive,
        Suspicion::Medium => look.warning,
        Suspicion::Weak => look.info,
        Suspicion::None => look.muted,
    }
}

/// 处置色：仅观察（本应执行而未执行）用弱化色，封禁危险色，限速 / 停止上传警示色。
fn action_color(look: &Look, guard: &PeerGuard<'_>) -> Hsla {
    if guard.observe_only {
        look.muted
    } else if guard.action == PeerAction::Banned {
        look.destructive
    } else {
        look.warning
    }
}

fn peer_guard_cells(
    peer: &BtPeerDto,
    look: &Look,
    translator: &Translator,
    columns: bool,
) -> Option<PeerGuardCells> {
    let guard = PeerGuard::of(peer)?;
    let suspicion = (guard.suspicion > Suspicion::None).then(|| {
        (
            SharedString::from(translator.text(guard.suspicion.label_key()).to_owned()),
            suspicion_color(look, guard.suspicion),
        )
    });
    let action = action_text(translator, &guard)
        .map(|label| (SharedString::from(label), action_color(look, &guard)));
    let rules = rules_text(translator, guard.rules);
    Some(PeerGuardCells {
        columns,
        delivered: guard.delivered,
        suspicion,
        rules: SharedString::from(rules.clone()),
        action,
        tooltip: SharedString::from(tooltip_text(translator, &guard)),
        sub: rules,
    })
}

fn tinted_chip(look: &Look, text: SharedString, color: Hsla) -> Div {
    div()
        .flex_none()
        .px(look.xs)
        .rounded(look.radius)
        .border_1()
        .border_color(color)
        .text_size(look.caption_size)
        .line_height(look.caption_line)
        .text_color(color)
        .whitespace_nowrap()
        .child(text)
}

/// 地址旁的徽标（窄于三列宽度时）：有处置显示处置，否则显示嫌疑等级。
fn guard_badge(look: &Look, cells: &PeerGuardCells) -> Option<Div> {
    cells
        .action
        .as_ref()
        .or(cells.suspicion.as_ref())
        .map(|(label, color)| tinted_chip(look, label.clone(), *color))
}

/// 「嫌疑 / 命中规则」列：嫌疑徽标 + 规则名；两者皆无为 `—`。
fn guard_state_cell(look: &Look, cells: &PeerGuardCells) -> Div {
    if cells.suspicion.is_none() && cells.rules.is_empty() {
        return div().text_color(look.tertiary).child(DASH);
    }
    h_flex()
        .items_center()
        .gap(look.xs)
        .min_w_0()
        .when_some(cells.suspicion.clone(), |this, (label, color)| {
            this.child(tinted_chip(look, label, color))
        })
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(look.muted)
                .child(cells.rules.clone()),
        )
}

/// 「处置」列：处置徽标；放行为 `—`。
fn guard_action_cell(look: &Look, cells: &PeerGuardCells) -> Div {
    match &cells.action {
        Some((label, color)) => h_flex().child(tinted_chip(look, label.clone(), *color)),
        None => div().text_color(look.tertiary).child(DASH),
    }
}

/// 宽布局下是否有足够宽度并排防吸血三列（`width` 为界面缩放前的 px）。
fn guard_columns_fit(width: f32) -> bool {
    width >= GUARD_COLUMNS_MIN_WIDTH
}

fn span_text(translator: &Translator, span: Span) -> String {
    let value = span.value.to_string();
    match span.unit {
        SpanUnit::Seconds => translator.text_with("etaSeconds", &[("n", &value)]),
        SpanUnit::Minutes => format!("{value} {}", translator.text("timeUnitMinutes")),
        SpanUnit::Hours => format!("{value} {}", translator.text("timeUnitHours")),
        SpanUnit::Days => format!("{value} {}", translator.text("timeUnitDays")),
    }
}

fn relative_text(translator: &Translator, time: RelativeTime) -> String {
    match time {
        RelativeTime::Unknown => DASH.to_owned(),
        RelativeTime::Never => translator.text("btTrackerNever").to_owned(),
        RelativeTime::In(span) => {
            translator.text_with("btTrackerIn", &[("time", &span_text(translator, span))])
        }
        RelativeTime::Ago(span) => {
            translator.text_with("btTrackerAgo", &[("time", &span_text(translator, span))])
        }
    }
}

fn status_key(status: BtTrackerStatus) -> &'static str {
    match status {
        BtTrackerStatus::Working => "btTrackerStatusWorking",
        BtTrackerStatus::Updating => "btTrackerStatusUpdating",
        BtTrackerStatus::NotWorking => "btTrackerStatusNotWorking",
        BtTrackerStatus::NotContacted => "btTrackerStatusNotContacted",
        BtTrackerStatus::Unknown => "btTrackerStatusUnknown",
    }
}

/// 页签的轮询状态与最近一次成功数据（随任务切换整体重置）。
pub(super) struct BtLive {
    status: Option<fluxdown_protocol::BtStatusDto>,
    /// 最近一次请求失败：保留旧数据，只在页内显示一行提示。
    failed: bool,
    /// 轮询循环存活中。
    polling: bool,
    /// 任务切换代次：旧循环 / 旧响应据此作废。
    generation: u64,
    last_render: Option<Instant>,
    /// 内容区宽度（实际 px），由 canvas 量得。
    width: Rc<Cell<f32>>,
    /// 正在执行加封禁 / 限速 / 加白的 IP 组：其右键动作禁用，防重复点击。
    guard_pending: HashSet<String>,
    /// Tracker 页筛选 / 排序（视图偏好，切换任务保留）。
    tracker_filter: TrackerFilter,
    tracker_sort: Option<TrackerSort>,
    /// 筛选 + 排序后要显示的 `status.trackers` 下标。
    tracker_view: Vec<usize>,
    /// tracker 行虚拟列表（行高随消息行可变）。
    tracker_list: ListState,
    /// 上次渲染的布局档；变化时行高需重量。
    tracker_layout: Cell<Option<BtLayout>>,
}

impl Default for BtLive {
    fn default() -> Self {
        Self {
            status: None,
            failed: false,
            polling: false,
            generation: 0,
            last_render: None,
            width: Rc::default(),
            guard_pending: HashSet::new(),
            tracker_filter: TrackerFilter::default(),
            tracker_sort: None,
            tracker_view: Vec::new(),
            tracker_list: ListState::new(0, ListAlignment::Top, px(TRACKER_LIST_OVERDRAW)),
            tracker_layout: Cell::new(None),
        }
    }
}

impl BtLive {
    /// 登记某 IP 组的在途防吸血动作；已在途返回 `false`。
    fn try_begin_guard(&mut self, group: &str) -> bool {
        self.guard_pending.insert(group.to_owned())
    }

    /// 切换任务：丢弃数据并作废在途循环。
    pub(super) fn reset(&mut self) {
        self.status = None;
        self.failed = false;
        self.polling = false;
        self.generation = self.generation.wrapping_add(1);
        self.last_render = None;
        self.tracker_view.clear();
        self.tracker_list.reset(0);
    }

    /// 数据 / 筛选 / 排序变化后重算可见行；行数不变时只重量行高（保留滚动位置）。
    fn refresh_tracker_view(&mut self) {
        let view = self.status.as_ref().map_or_else(Vec::new, |status| {
            tracker_view(&status.trackers, self.tracker_filter, self.tracker_sort)
        });
        if view.len() == self.tracker_list.item_count() {
            self.tracker_list.remeasure();
        } else {
            self.tracker_list
                .reset_with_uniform_height(view.len(), px(TRACKER_ROW_HINT));
        }
        self.tracker_view = view;
    }
}

/// 渲染用的主题取值（一次取出，避免在各行构造里反复读主题）。
#[derive(Clone)]
struct Look {
    scale: f32,
    xxs: Pixels,
    xs: Pixels,
    sm: Pixels,
    radius: Pixels,
    thin: Pixels,
    icon_sm: Pixels,
    xs_size: Pixels,
    xs_line: Pixels,
    sm_size: Pixels,
    sm_line: Pixels,
    caption_size: Pixels,
    caption_line: Pixels,
    mono: SharedString,
    fg: Hsla,
    muted: Hsla,
    tertiary: Hsla,
    destructive: Hsla,
    success: Hsla,
    info: Hsla,
    warning: Hsla,
    accent: Hsla,
    hairline: Hsla,
    hover: Hsla,
}

impl Look {
    fn new(cx: &App) -> Self {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        Self {
            scale: theme.text_extent(1.).as_f32(),
            xxs: tokens.spacing.xxs,
            xs: tokens.spacing.xs,
            sm: tokens.spacing.sm,
            radius: tokens.radius.sm,
            thin: extended.stroke.thin,
            icon_sm: extended.icon.sm,
            xs_size: tokens.typography.xs.size,
            xs_line: tokens.typography.xs.line_height,
            sm_size: tokens.typography.sm.size,
            sm_line: tokens.typography.sm.line_height,
            caption_size: extended.caption.size,
            caption_line: extended.caption.line_height,
            mono: tokens.typography.mono.clone(),
            fg: tokens.colors.foreground,
            muted: tokens.colors.muted_foreground,
            tertiary: extended.colors.text_tertiary,
            destructive: tokens.colors.destructive,
            success: extended.colors.success,
            info: extended.colors.info,
            warning: extended.colors.warning,
            accent: extended.colors.accent_text,
            hairline: extended.colors.hairline,
            hover: extended.colors.row_hover,
        }
    }

    /// 随界面缩放 / 字号伸缩的固定尺寸。
    fn ext(&self, base: f32) -> Pixels {
        px(base * self.scale)
    }

    fn status_color(&self, status: BtTrackerStatus) -> Hsla {
        match status {
            BtTrackerStatus::Working => self.success,
            BtTrackerStatus::Updating => self.info,
            BtTrackerStatus::NotWorking => self.destructive,
            BtTrackerStatus::NotContacted | BtTrackerStatus::Unknown => self.tertiary,
        }
    }

    /// 状态文字色：失败用危险色，其余沿用正文 / 弱化色（颜色之外始终有文字标签）。
    fn status_text_color(&self, status: BtTrackerStatus) -> Hsla {
        match status {
            BtTrackerStatus::NotWorking => self.destructive,
            BtTrackerStatus::Working | BtTrackerStatus::Updating => self.fg,
            BtTrackerStatus::NotContacted | BtTrackerStatus::Unknown => self.muted,
        }
    }
}

/// 一行 tracker 渲染所需的全部已本地化文本与颜色（含 DHT 伪行）。
struct TrackerCells {
    dot: Hsla,
    label_color: Hsla,
    status_label: SharedString,
    url: SharedString,
    is_dht: bool,
    source_label: Option<SharedString>,
    /// 来源说明（徽标 / URL 悬停提示）。
    source_hint: Option<SharedString>,
    peers: Option<SharedString>,
    message: SharedString,
    /// 原始错误文本；空 = 无可复制错误。
    detail: SharedString,
    next: SharedString,
    last: SharedString,
    /// 窄布局元信息行的标签。
    meta: TrackerMeta,
}

/// 窄布局元信息行的列名（Peer 数 / 下次汇报 / 上次成功）。
#[derive(Clone)]
struct TrackerMeta {
    peers: SharedString,
    next: SharedString,
    last: SharedString,
}

/// 虚拟列表逐行渲染共用的取值（每次渲染构造一次，行回调按需借用）。
struct TrackerItemText {
    look: Look,
    copy: TrackerCopyText,
    meta: TrackerMeta,
    layout: BtLayout,
    now: i64,
}

/// 一个 tracker 的展示格（状态文案、来源说明、相对时间都已本地化）。
fn tracker_cells(
    dto: &BtTrackerDto,
    look: &Look,
    translator: &Translator,
    now: i64,
    meta: &TrackerMeta,
) -> TrackerCells {
    let t = |key: &str| SharedString::from(translator.text(key).to_owned());
    let row = tracker_row(dto, now);
    let (source_label, source_hint) = match row.source {
        BtTrackerSource::Torrent => ("btTrackerSourceTorrent", "btTrackerSourceTorrentHint"),
        BtTrackerSource::Session => ("btTrackerSourceSession", "btTrackerSourceSessionHint"),
    };
    TrackerCells {
        dot: look.status_color(row.status),
        label_color: look.status_text_color(row.status),
        status_label: t(status_key(row.status)),
        url: SharedString::from(row.url),
        is_dht: false,
        source_label: Some(t(source_label)),
        source_hint: Some(t(source_hint)),
        peers: row.peers.map(|count| SharedString::from(count.to_string())),
        message: SharedString::from(row.message),
        detail: SharedString::from(row.detail),
        next: SharedString::from(relative_text(translator, row.next)),
        last: SharedString::from(relative_text(translator, row.last)),
        meta: meta.clone(),
    }
}

/// 所有 tracker 行共用的复制文案。
struct TrackerCopyText {
    copy_url: SharedString,
    copy_error: SharedString,
    /// 错误消息悬停提示的末行（「点击复制错误信息」）。
    click_to_copy_error: SharedString,
    url_copied: SharedString,
    error_copied: SharedString,
}

fn dot(look: &Look, color: Hsla) -> Div {
    div()
        .flex_none()
        .size(look.ext(8.))
        .rounded_full()
        .bg(color)
}

fn chip(look: &Look, text: SharedString) -> Div {
    div()
        .flex_none()
        .px(look.xs)
        .rounded(look.radius)
        .border_1()
        .border_color(look.hairline)
        .text_size(look.caption_size)
        .line_height(look.caption_line)
        .text_color(look.muted)
        .whitespace_nowrap()
        .child(text)
}

fn fixed(look: &Look, base: f32) -> Div {
    div()
        .flex_none()
        .w(look.ext(base))
        .min_w_0()
        .overflow_hidden()
}

fn header_cell(look: &Look, label: SharedString, width: Option<f32>, right: bool) -> Div {
    let cell = match width {
        Some(base) => fixed(look, base),
        None => div().flex_1().min_w_0().overflow_hidden(),
    };
    cell.whitespace_nowrap()
        .text_ellipsis()
        .when(right, |this| this.text_right())
        .text_size(look.caption_size)
        .line_height(look.caption_line)
        .text_color(look.tertiary)
        .child(label)
}

fn header_row<E: IntoElement>(look: &Look, cells: Vec<E>) -> Div {
    h_flex()
        .items_center()
        .gap(look.sm)
        .px(look.sm)
        .py(look.xs)
        .border_b(look.thin)
        .border_color(look.hairline)
        .children(cells)
}

fn table_frame(look: &Look) -> Div {
    v_flex()
        .w_full()
        .rounded(look.radius)
        .border_1()
        .border_color(look.hairline)
        .overflow_hidden()
}

fn note(look: &Look, text: SharedString, color: Hsla) -> Div {
    div()
        .text_size(look.xs_size)
        .line_height(look.xs_line)
        .text_color(color)
        .child(text)
}

fn warning_banner(look: &Look, text: SharedString) -> Div {
    h_flex()
        .items_start()
        .gap(look.sm)
        .px(look.sm)
        .py(look.xs)
        .rounded(look.radius)
        .bg(look.warning.opacity(0.08))
        .border_1()
        .border_color(look.warning.opacity(0.32))
        .child(
            div()
                .flex_none()
                .h(look.xs_line)
                .flex()
                .items_center()
                .child(
                    Icon::new(FluxIcon::TriangleAlert)
                        .size(look.icon_sm)
                        .text_color(look.warning),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(look.xs_size)
                .line_height(look.xs_line)
                .text_color(look.fg)
                .child(text),
        )
}

fn empty_state(look: &Look, text: SharedString) -> Div {
    v_flex()
        .w_full()
        .items_center()
        .gap(look.sm)
        .py(look.ext(24.))
        .child(
            Icon::new(FluxIcon::Network)
                .size(look.ext(32.))
                .text_color(look.tertiary),
        )
        .child(
            div()
                .text_center()
                .text_size(look.sm_size)
                .line_height(look.sm_line)
                .font_weight(FontWeight::MEDIUM)
                .text_color(look.muted)
                .child(text),
        )
}

fn skeleton_rows(look: &Look, count: usize) -> Div {
    let mut frame = table_frame(look);
    for index in 0..count {
        frame = frame.child(
            h_flex()
                .items_center()
                .gap(look.sm)
                .px(look.sm)
                .py(look.sm)
                .when(index + 1 < count, |row| {
                    row.border_b(look.thin).border_color(look.hairline)
                })
                .child(
                    Skeleton::new()
                        .w(look.ext(72.))
                        .h(look.xs_line)
                        .rounded(look.radius),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        Skeleton::new()
                            .w_full()
                            .h(look.xs_line)
                            .rounded(look.radius),
                    ),
                )
                .child(
                    Skeleton::new()
                        .w(look.ext(48.))
                        .h(look.xs_line)
                        .rounded(look.radius),
                ),
        );
    }
    frame
}

/// 带悬浮完整内容的单行省略文本（中间省略，保留头尾）。
fn ellipsis_cell(
    id: (&'static str, usize),
    text: SharedString,
    tip: Option<SharedString>,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis_middle()
        .when_some(tip, |this, tip| {
            this.tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
        })
        .child(text)
}

/// 一个「复制」菜单项：文案、复制内容、复制后的成功提示（`None` = 不提示）。
#[derive(Clone)]
struct CopyItem {
    label: SharedString,
    value: SharedString,
    done: Option<SharedString>,
}

fn copy_to_clipboard(
    value: &SharedString,
    done: Option<&SharedString>,
    window: &mut Window,
    cx: &mut App,
) {
    cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
    if let Some(done) = done {
        window.push_notification(Notification::success(done.clone()), cx);
    }
}

/// 右键菜单：逐项复制。
fn copy_menu(row: gpui::Stateful<Div>, items: Vec<CopyItem>) -> AnyElement {
    if items.is_empty() {
        return row.into_any_element();
    }
    row.context_menu(move |menu, _window, _cx| {
        items.iter().fold(menu, |menu, item| {
            let item = item.clone();
            menu.item(
                PopupMenuItem::new(item.label.clone())
                    .icon(FluxIcon::Copy)
                    .on_click(move |_, window, cx| {
                        copy_to_clipboard(&item.value, item.done.as_ref(), window, cx);
                    }),
            )
        })
    })
    .into_any_element()
}

/// 连接行右键菜单：逐项复制，其后（有防吸血信息时）是封禁 / 限速 / 加白 / 查看证据。
fn peer_menu(
    row: gpui::Stateful<Div>,
    items: Vec<CopyItem>,
    guard: Option<GuardMenu>,
) -> AnyElement {
    if items.is_empty() && guard.is_none() {
        return row.into_any_element();
    }
    row.context_menu(move |menu, _window, _cx| {
        let mut menu = items.iter().fold(menu, |menu, item| {
            let item = item.clone();
            menu.item(
                PopupMenuItem::new(item.label.clone())
                    .icon(FluxIcon::Copy)
                    .on_click(move |_, window, cx| {
                        copy_to_clipboard(&item.value, item.done.as_ref(), window, cx);
                    }),
            )
        });
        let Some(guard) = &guard else {
            return menu;
        };
        let entry_item = |label: &SharedString, icon: FluxIcon, action: GuardMenuAction| {
            let (peer, handler) = (guard.peer.clone(), Rc::clone(&guard.handler));
            PopupMenuItem::new(label.clone())
                .disabled(guard.busy)
                .icon(icon)
                .on_click(move |_, window, cx| {
                    handler(peer.clone(), GuardCommand::Entry(action), window, cx);
                })
        };
        menu = menu.separator();
        for (choice, label) in BanChoice::ALL.iter().zip(guard.labels.bans.iter()) {
            menu = menu.item(entry_item(
                label,
                FluxIcon::X,
                GuardMenuAction::Ban(*choice),
            ));
        }
        menu = menu
            .item(entry_item(
                &guard.labels.throttle,
                FluxIcon::Gauge,
                GuardMenuAction::Throttle,
            ))
            .item(entry_item(
                &guard.labels.allow,
                FluxIcon::CircleCheck,
                GuardMenuAction::Allow,
            ))
            .separator();
        let (peer, handler) = (guard.peer.clone(), Rc::clone(&guard.handler));
        menu.item(
            PopupMenuItem::new(guard.labels.evidence.clone())
                .icon(FluxIcon::Search)
                .on_click(move |_, window, cx| {
                    handler(peer.clone(), GuardCommand::Evidence, window, cx);
                }),
        )
    })
    .into_any_element()
}

/// 表头点击排序回调（参数为被点的列）。
type SortHandler = Rc<dyn Fn(&TrackerSortKey, &mut Window, &mut App)>;

/// 可排序表头格：标签后缀当前方向箭头，点击按 [`next_sort`] 三态切换。
fn sort_cell(
    look: &Look,
    label: &SharedString,
    width: Option<f32>,
    right: bool,
    key: TrackerSortKey,
    header: &TrackerHeaderText,
) -> AnyElement {
    let arrow = match header.sort {
        Some(sort) if sort.key == key && sort.desc => " ↓",
        Some(sort) if sort.key == key => " ↑",
        _ => "",
    };
    let active = !arrow.is_empty();
    let text = if active {
        SharedString::from(format!("{label}{arrow}"))
    } else {
        label.clone()
    };
    let hint = header.sort_hint.clone();
    let on_sort = Rc::clone(&header.on_sort);
    header_cell(look, text, width, right)
        .id(("bt-tracker-sort", key as usize))
        .cursor_pointer()
        .when(active, |this| this.text_color(look.fg))
        .hover({
            let fg = look.fg;
            move |style| style.text_color(fg)
        })
        .tooltip(move |window, cx| tooltip_view(hint.clone(), window, cx))
        .on_click(move |_, window, cx| on_sort(&key, window, cx))
        .into_any_element()
}

fn tracker_header(look: &Look, layout: BtLayout, text: &TrackerHeaderText) -> Option<Div> {
    let status = sort_cell(
        look,
        &text.status,
        Some(COL_STATUS),
        false,
        TrackerSortKey::Status,
        text,
    );
    let url = sort_cell(look, &text.url, None, false, TrackerSortKey::Url, text);
    let peers = sort_cell(
        look,
        &text.peers,
        Some(COL_PEERS),
        true,
        TrackerSortKey::Peers,
        text,
    );
    let next = sort_cell(
        look,
        &text.next,
        Some(COL_TIME),
        false,
        TrackerSortKey::NextAnnounce,
        text,
    );
    let last = sort_cell(
        look,
        &text.last,
        Some(COL_TIME),
        false,
        TrackerSortKey::LastOk,
        text,
    );
    match layout {
        BtLayout::Wide => {
            let message = header_cell(look, text.message.clone(), Some(COL_MESSAGE), false)
                .into_any_element();
            Some(header_row(
                look,
                vec![status, url, peers, message, next, last],
            ))
        }
        BtLayout::Medium => Some(header_row(look, vec![status, url, peers, next, last])),
        BtLayout::Narrow => None,
    }
}

struct TrackerHeaderText {
    status: SharedString,
    url: SharedString,
    peers: SharedString,
    message: SharedString,
    next: SharedString,
    last: SharedString,
    sort: Option<TrackerSort>,
    sort_hint: SharedString,
    on_sort: SortHandler,
}

fn tracker_row_el(
    index: usize,
    cells: &TrackerCells,
    copy: &TrackerCopyText,
    look: &Look,
    layout: BtLayout,
    last: bool,
) -> AnyElement {
    let status_cell = h_flex()
        .items_center()
        .gap(look.xs)
        .min_w_0()
        .child(dot(look, cells.dot))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(cells.label_color)
                .child(cells.status_label.clone()),
        );
    // 悬停 URL：完整地址 + 来源说明（中布局不放来源徽标，说明只在这里）。
    let url_tip = (!cells.is_dht).then(|| match &cells.source_hint {
        Some(hint) => SharedString::from(format!("{}\n{hint}", cells.url)),
        None => cells.url.clone(),
    });
    let show_chip = layout != BtLayout::Medium;
    let url_line = || {
        h_flex()
            .items_center()
            .gap(look.xs)
            .min_w_0()
            .child(ellipsis_cell(
                ("bt-tracker-url", index),
                cells.url.clone(),
                url_tip.clone(),
            ))
            .when(show_chip, |this| {
                this.when_some(cells.source_label.clone(), |this, label| {
                    let hint = cells.source_hint.clone();
                    this.child(
                        chip(look, label)
                            .id(("bt-tracker-source", index))
                            .when_some(hint, |this, hint| {
                                this.tooltip(move |window, cx| {
                                    tooltip_view(hint.clone(), window, cx)
                                })
                            }),
                    )
                })
            })
            .when(cells.is_dht, |this| this.font_weight(FontWeight::MEDIUM))
    };
    let message_line = |clamp: usize| {
        // 悬停显示原始错误并提示可点击；点击一键复制原始错误。
        let has_detail = !cells.detail.is_empty();
        let tip = if has_detail {
            SharedString::from(format!("{}\n{}", cells.detail, copy.click_to_copy_error))
        } else {
            cells.message.clone()
        };
        div()
            .id(("bt-tracker-msg", index))
            .min_w_0()
            .overflow_hidden()
            .line_clamp(clamp)
            .text_size(look.xs_size)
            .line_height(look.xs_line)
            .text_color(look.destructive)
            .when(has_detail, |this| {
                let value = cells.detail.clone();
                let done = copy.error_copied.clone();
                this.cursor_pointer().on_click(move |_, window, cx| {
                    copy_to_clipboard(&value, Some(&done), window, cx);
                })
            })
            .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
            .child(cells.message.clone())
    };
    let peers_text = cells
        .peers
        .clone()
        .unwrap_or_else(|| SharedString::from(DASH));
    let base = match layout {
        BtLayout::Wide => h_flex()
            .items_center()
            .gap(look.sm)
            .child(fixed(look, COL_STATUS).child(status_cell))
            .child(div().flex_1().min_w_0().child(url_line()))
            .child(
                fixed(look, COL_PEERS)
                    .text_right()
                    .text_color(look.muted)
                    .child(peers_text),
            )
            .child(fixed(look, COL_MESSAGE).child(if cells.message.is_empty() {
                div()
                    .text_color(look.tertiary)
                    .child(DASH)
                    .into_any_element()
            } else {
                message_line(1).into_any_element()
            }))
            .child(fixed(look, COL_TIME).truncate().child(cells.next.clone()))
            .child(
                fixed(look, COL_TIME)
                    .truncate()
                    .text_color(look.muted)
                    .child(cells.last.clone()),
            ),
        BtLayout::Medium => h_flex()
            .items_start()
            .gap(look.sm)
            .child(fixed(look, COL_STATUS).child(status_cell))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(look.xxs)
                    .child(url_line())
                    .when(!cells.message.is_empty(), |this| {
                        this.child(message_line(2))
                    }),
            )
            .child(
                fixed(look, COL_PEERS)
                    .text_right()
                    .text_color(look.muted)
                    .child(peers_text),
            )
            .child(fixed(look, COL_TIME).truncate().child(cells.next.clone()))
            .child(
                fixed(look, COL_TIME)
                    .truncate()
                    .text_color(look.muted)
                    .child(cells.last.clone()),
            ),
        BtLayout::Narrow => {
            let mut meta: Vec<String> = Vec::with_capacity(3);
            if let Some(peers) = &cells.peers {
                meta.push(format!("{}: {peers}", cells.meta.peers));
            }
            meta.push(format!("{}: {}", cells.meta.next, cells.next));
            meta.push(format!("{}: {}", cells.meta.last, cells.last));
            h_flex().items_start().child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(look.xxs)
                    .child(status_cell)
                    .child(url_line())
                    .when(!cells.message.is_empty(), |this| {
                        this.child(message_line(3))
                    })
                    .when(!cells.is_dht, |this| {
                        this.child(
                            h_flex()
                                .flex_wrap()
                                .gap_x(look.sm)
                                .text_size(look.xs_size)
                                .line_height(look.xs_line)
                                .text_color(look.muted)
                                .children(meta.into_iter().map(|text| div().child(text))),
                        )
                    }),
            )
        }
    };
    let row = base
        .id(("bt-tracker", index))
        .w_full()
        .px(look.sm)
        .py(look.xs)
        .text_size(look.sm_size)
        .line_height(look.sm_line)
        .text_color(look.fg)
        .font_features(tabular_numbers())
        .when(!last, |this| {
            this.border_b(look.thin).border_color(look.hairline)
        })
        .hover({
            let hover = look.hover;
            move |style| style.bg(hover)
        });
    let mut copies: Vec<CopyItem> = Vec::with_capacity(2);
    if !cells.is_dht {
        copies.push(CopyItem {
            label: copy.copy_url.clone(),
            value: cells.url.clone(),
            done: Some(copy.url_copied.clone()),
        });
    }
    if !cells.detail.is_empty() {
        copies.push(CopyItem {
            label: copy.copy_error.clone(),
            value: cells.detail.clone(),
            done: Some(copy.error_copied.clone()),
        });
    }
    copy_menu(row, copies)
}

fn peer_header(look: &Look, layout: BtLayout, text: &PeerHeaderText) -> Option<Div> {
    // 与行内「国旗槽位 + 间距」对齐，表头文字落在 IP 正上方。
    let addr = header_cell(look, text.addr.clone(), None, false).pl(look.ext(FLAG_WIDTH) + look.xs);
    let transport = header_cell(look, text.transport.clone(), Some(COL_TRANSPORT), false);
    let down_speed = header_cell(look, text.down_speed.clone(), Some(COL_SPEED), true);
    let up_speed = header_cell(look, text.up_speed.clone(), Some(COL_SPEED), true);
    match layout {
        BtLayout::Wide => {
            let mut cells = vec![
                addr,
                header_cell(look, text.client.clone(), Some(COL_CLIENT), false),
                transport,
                header_cell(look, text.downloaded.clone(), Some(COL_BYTES), true),
                header_cell(look, text.uploaded.clone(), Some(COL_BYTES), true),
                down_speed,
                up_speed,
            ];
            if let Some(guard) = &text.guard {
                cells.push(header_cell(
                    look,
                    guard.uploaded.clone(),
                    Some(COL_GUARD_BYTES),
                    true,
                ));
                cells.push(header_cell(
                    look,
                    guard.state.clone(),
                    Some(COL_GUARD_STATE),
                    false,
                ));
                cells.push(header_cell(
                    look,
                    guard.action.clone(),
                    Some(COL_GUARD_ACTION),
                    false,
                ));
            }
            Some(header_row(look, cells))
        }
        BtLayout::Medium => Some(header_row(
            look,
            vec![addr, transport, down_speed, up_speed],
        )),
        BtLayout::Narrow => None,
    }
}

struct PeerHeaderText {
    addr: SharedString,
    client: SharedString,
    transport: SharedString,
    downloaded: SharedString,
    uploaded: SharedString,
    down_speed: SharedString,
    up_speed: SharedString,
    /// 防吸血三列的表头；`None` = 不显示这三列。
    guard: Option<GuardHeaderText>,
}

fn speed_el(look: &Look, bps: u64, active_color: Hsla) -> Div {
    if bps == 0 {
        div().text_color(look.tertiary).child(DASH)
    } else {
        div().text_color(active_color).child(rate_text(bps))
    }
}

fn bytes_el(look: &Look, bytes: u64) -> Div {
    div()
        .text_color(if bytes == 0 {
            look.tertiary
        } else {
            look.muted
        })
        .child(format_bytes(bytes))
}

/// peer 的国家代码（去空白；缺失 / 空串 = 未知）。
fn peer_country(peer: &BtPeerDto) -> Option<&str> {
    peer.country
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty())
}

#[allow(
    clippy::too_many_arguments,
    reason = "one row needs the peer, its flag/guard cells, the context menu and the layout inputs"
)]
fn peer_row_el(
    index: usize,
    peer: &BtPeerDto,
    flag: AnyElement,
    copy_label: &SharedString,
    guard: Option<&PeerGuardCells>,
    menu: Option<GuardMenu>,
    look: &Look,
    layout: BtLayout,
    last: bool,
) -> AnyElement {
    let addr = SharedString::from(peer.addr.clone());
    let transport = SharedString::from(transport_label(&peer.transport));
    let addr_el = |id_index: usize| {
        div()
            .id(("bt-peer-addr", id_index))
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis_middle()
            .font_family(look.mono.clone())
            .child(addr.clone())
    };
    // 国旗槽位恒占位（国家信息晚到 / 内网无国家都不会让行内布局跳动）；悬浮提示 = 国家代码。
    let country = peer_country(peer);
    let flag_el = div()
        .id(("bt-peer-flag", index))
        .flex_none()
        .child(flag)
        .when_some(country, |this, code| {
            let tip = SharedString::from(code.to_ascii_uppercase());
            this.tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
        });
    let badge = guard
        .filter(|cells| !cells.columns)
        .and_then(|cells| guard_badge(look, cells));
    let addr_cell = move |id_index: usize| {
        h_flex()
            .items_center()
            .gap(look.xs)
            .min_w_0()
            .child(flag_el)
            .child(addr_el(id_index))
            .when_some(badge, |this, badge| this.child(badge))
    };
    let sub = match guard.filter(|cells| !cells.columns && !cells.sub.is_empty()) {
        Some(cells) => {
            let base = peer_sub_line(peer);
            if base.is_empty() {
                cells.sub.clone()
            } else {
                format!("{base} · {}", cells.sub)
            }
        }
        None => peer_sub_line(peer),
    };
    let sub_el = || {
        div()
            .min_w_0()
            .truncate()
            .text_size(look.xs_size)
            .line_height(look.xs_line)
            .text_color(look.muted)
            .child(sub.clone())
    };
    let base = match layout {
        BtLayout::Wide => h_flex()
            .items_center()
            .gap(look.sm)
            .child(addr_cell(index).flex_1())
            .child(
                fixed(look, COL_CLIENT)
                    .truncate()
                    .text_color(look.muted)
                    .child(if peer.client.trim().is_empty() {
                        SharedString::from(DASH)
                    } else {
                        SharedString::from(peer.client.trim().to_owned())
                    }),
            )
            .child(fixed(look, COL_TRANSPORT).child(h_flex().child(chip(look, transport))))
            .child(
                fixed(look, COL_BYTES)
                    .text_right()
                    .child(bytes_el(look, peer.downloaded)),
            )
            .child(
                fixed(look, COL_BYTES)
                    .text_right()
                    .child(bytes_el(look, peer.uploaded)),
            )
            .child(fixed(look, COL_SPEED).text_right().child(speed_el(
                look,
                peer.down_bps,
                look.accent,
            )))
            .child(fixed(look, COL_SPEED).text_right().child(speed_el(
                look,
                peer.up_bps,
                look.success,
            )))
            .when_some(guard.filter(|cells| cells.columns), |row, cells| {
                row.child(
                    fixed(look, COL_GUARD_BYTES)
                        .text_right()
                        .child(bytes_el(look, cells.delivered)),
                )
                .child(fixed(look, COL_GUARD_STATE).child(guard_state_cell(look, cells)))
                .child(fixed(look, COL_GUARD_ACTION).child(guard_action_cell(look, cells)))
            }),
        BtLayout::Medium => h_flex()
            .items_center()
            .gap(look.sm)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(look.xxs)
                    .child(addr_cell(index))
                    .when(!sub.is_empty(), |this| this.child(sub_el())),
            )
            .child(fixed(look, COL_TRANSPORT).child(h_flex().child(chip(look, transport))))
            .child(fixed(look, COL_SPEED).text_right().child(speed_el(
                look,
                peer.down_bps,
                look.accent,
            )))
            .child(fixed(look, COL_SPEED).text_right().child(speed_el(
                look,
                peer.up_bps,
                look.success,
            ))),
        BtLayout::Narrow => h_flex()
            .items_center()
            .gap(look.sm)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(look.xxs)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(look.xs)
                            .min_w_0()
                            .child(addr_cell(index).flex_1())
                            .child(chip(look, transport)),
                    )
                    .when(!sub.is_empty(), |this| this.child(sub_el())),
            )
            .child(
                fixed(look, COL_SPEED_STACK)
                    .text_right()
                    .text_size(look.xs_size)
                    .line_height(look.xs_line)
                    .child(
                        h_flex()
                            .justify_end()
                            .gap(look.xxs)
                            .child(div().text_color(look.tertiary).child("↓"))
                            .child(speed_el(look, peer.down_bps, look.accent)),
                    )
                    .child(
                        h_flex()
                            .justify_end()
                            .gap(look.xxs)
                            .child(div().text_color(look.tertiary).child("↑"))
                            .child(speed_el(look, peer.up_bps, look.success)),
                    ),
            ),
    };
    let row = base
        .id(("bt-peer", index))
        .w_full()
        .px(look.sm)
        .py(look.xs)
        .text_size(look.sm_size)
        .line_height(look.sm_line)
        .text_color(look.fg)
        .font_features(tabular_numbers())
        .when(!last, |this| {
            this.border_b(look.thin).border_color(look.hairline)
        })
        .hover({
            let hover = look.hover;
            move |style| style.bg(hover)
        })
        .when_some(guard.map(|cells| cells.tooltip.clone()), |this, tip| {
            this.tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
        });
    peer_menu(
        row,
        vec![CopyItem {
            label: copy_label.clone(),
            value: addr,
            done: None,
        }],
        menu,
    )
}

impl TaskDetailView {
    pub(super) fn bt_tab_active(&self) -> bool {
        matches!(self.tab, DetailTab::Trackers | DetailTab::Peers)
    }

    /// 任务是否在运行（下载中 / 做种中），决定连接页空状态文案。
    fn bt_task_running(&self) -> bool {
        self.store
            .get(&RowKey::Local(self.task_id.clone()))
            .is_some_and(|row| {
                row.state == TaskState::Downloading
                    || (row.state == TaskState::Completed && row.seeding_status == 1)
            })
    }

    /// 页签渲染时调用：登记「仍可见」并在没有轮询循环时启动一个。
    pub(super) fn bt_touch(&mut self, cx: &mut Context<Self>) {
        self.bt.last_render = Some(Instant::now());
        if self.bt.polling || self.closed || !self.activity_online || !self.is_bt() {
            return;
        }
        self.bt.polling = true;
        let generation = self.bt.generation;
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(request) = this.update(cx, |this, _| this.bt_poll_request(generation))
                else {
                    // 视图已释放。
                    return;
                };
                let Some(future) = request else {
                    return;
                };
                let result = future.await;
                let Ok(keep_going) =
                    this.update(cx, |this, cx| this.bt_poll_finish(generation, result, cx))
                else {
                    // 视图已释放。
                    return;
                };
                if !keep_going {
                    return;
                }
                cx.background_executor().timer(BT_POLL_INTERVAL).await;
            }
        })
        .detach();
    }

    /// 发出下一次请求；不再需要轮询时返回 `None` 并让循环退出。
    fn bt_poll_request(&mut self, generation: u64) -> Option<PortFuture<DownloadsResult>> {
        if self.bt.generation != generation {
            // 任务已切换：新代次的循环归新代次管理，不动 `polling`。
            return None;
        }
        let visible = self
            .bt
            .last_render
            .is_some_and(|at| at.elapsed() < BT_VISIBLE_GRACE);
        if self.closed
            || !self.activity_online
            || !self.bt_tab_active()
            || !visible
            || !self.is_bt()
        {
            self.bt.polling = false;
            return None;
        }
        Some(
            self.port
                .execute(DownloadsCommand::BtStatus(DaemonBtStatusParams {
                    task_id: self.task_id.clone(),
                })),
        )
    }

    /// 应用一次响应；返回循环是否继续。失败时保留旧数据。
    fn bt_poll_finish(
        &mut self,
        generation: u64,
        result: Result<DownloadsResult, RpcErrorData>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.bt.generation != generation {
            return false;
        }
        match result {
            Ok(DownloadsResult::BtStatus(status)) => {
                self.bt.status = Some(status);
                self.bt.failed = false;
                self.bt.refresh_tracker_view();
            }
            Ok(_) | Err(_) => self.bt.failed = true,
        }
        cx.notify();
        true
    }

    /// 1 s 定时器调用：Tracker 页可见时重绘，让相对时间在两次轮询之间走字（不发请求）。
    pub(super) fn tick_bt_clock(&self, cx: &mut Context<Self>) {
        if self.tab == DetailTab::Trackers
            && self
                .bt
                .last_render
                .is_some_and(|at| at.elapsed() < BT_CLOCK_VISIBLE)
        {
            cx.notify();
        }
    }

    /// 内容区宽度（界面缩放前的 px）；尚未量到时用窗口宽度估计一次（停靠面板 ≤ 窗口宽）。
    fn bt_width(&self, window: &Window, look: &Look) -> f32 {
        let measured = self.bt.width.get();
        let width = if measured > 0. {
            measured
        } else {
            window.viewport_size().width.as_f32()
        };
        width / look.scale.max(0.01)
    }

    fn bt_layout(&self, window: &Window, look: &Look) -> BtLayout {
        BtLayout::for_width(self.bt_width(window, look))
    }

    /// 连接行右键菜单的防吸血命令：加封禁 / 限速 / 白名单条目，或打开证据对话框。
    fn bt_guard_command(
        &mut self,
        peer: BtPeerDto,
        command: GuardCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = match command {
            GuardCommand::Evidence => {
                crate::pages::task_bt_guard_evidence::open(
                    self.translator.clone(),
                    std::sync::Arc::clone(&self.port),
                    peer,
                    window,
                    cx,
                );
                return;
            }
            GuardCommand::Entry(action) => action,
        };
        let Some(params) = entry_params(&peer.ip_group, action) else {
            return;
        };
        let group = params.cidr.clone();
        // 同一 IP 组已有动作在途：忽略重复点击。
        let key = peer.ip_group.trim().to_owned();
        if !self.bt.try_begin_guard(&key) {
            return;
        }
        cx.notify();
        let pending = self.port.execute(DownloadsCommand::BtGuardAddEntry(params));
        cx.spawn_in(window, async move |this, cx| {
            let result = pending.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.bt.guard_pending.remove(&key);
                cx.notify();
                let translator = this.translator.read(cx);
                let notification = match result {
                    Ok(_) => Notification::success(
                        translator.text_with(action.success_key(), &[("group", &group)]),
                    ),
                    Err(error) => Notification::error(format!(
                        "{}: {}",
                        translator.text("btGuardPeerActionFailed"),
                        error_text(translator, &error)
                    )),
                };
                window.push_notification(notification, cx);
            }) else {
                // 视图或窗口已释放。
                return;
            };
        })
        .detach();
    }

    /// 页签外框：量宽 canvas + 失败提示行 + 内容。`fill` = 撑满页签高度（内容自带虚拟滚动）。
    fn bt_frame(
        &self,
        content: AnyElement,
        fill: bool,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity_id = cx.entity_id();
        let width = Rc::clone(&self.bt.width);
        let scale = look.scale.max(0.01);
        v_flex()
            .relative()
            .w_full()
            .when(fill, |this| this.h_full().min_h_0())
            .gap(look.sm)
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let measured = bounds.size.width.as_f32();
                        let previous = width.get();
                        if (measured - previous).abs() > 0.5 {
                            width.set(measured);
                            // 只在布局档变化时重绘，避免滚动条出没带来的抖动。
                            if BtLayout::for_width(previous / scale)
                                != BtLayout::for_width(measured / scale)
                                || guard_columns_fit(previous / scale)
                                    != guard_columns_fit(measured / scale)
                            {
                                cx.notify(entity_id);
                            }
                        }
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .when(self.bt.failed, |this| {
                this.child(note(look, self.strings.action_failed.clone(), look.warning))
            })
            .child(content)
            .into_any_element()
    }

    fn set_tracker_filter(&mut self, filter: TrackerFilter, cx: &mut Context<Self>) {
        if self.bt.tracker_filter == filter {
            return;
        }
        self.bt.tracker_filter = filter;
        self.bt.refresh_tracker_view();
        cx.notify();
    }

    fn set_tracker_sort(&mut self, key: TrackerSortKey, cx: &mut Context<Self>) {
        self.bt.tracker_sort = next_sort(self.bt.tracker_sort, key);
        self.bt.refresh_tracker_view();
        cx.notify();
    }

    /// 状态筛选标签行：「全部」常显，其余只在有计数或正被选中时出现。
    fn tracker_filter_bar(
        &self,
        look: &Look,
        counts: &TrackerCounts,
        translator: &Translator,
        cx: &Context<Self>,
    ) -> Div {
        let active = self.bt.tracker_filter;
        let chips = TrackerFilter::ALL
            .into_iter()
            .filter(|&filter| {
                filter == TrackerFilter::All || filter == active || counts.get(filter) > 0
            })
            .map(|filter| {
                let key = match filter {
                    TrackerFilter::All => "btTrackerFilterAll",
                    TrackerFilter::Working => status_key(BtTrackerStatus::Working),
                    TrackerFilter::Updating => status_key(BtTrackerStatus::Updating),
                    TrackerFilter::NotWorking => status_key(BtTrackerStatus::NotWorking),
                    TrackerFilter::NotContacted => status_key(BtTrackerStatus::NotContacted),
                };
                let selected = filter == active;
                let color = if selected { look.accent } else { look.muted };
                h_flex()
                    .id(("bt-tracker-filter", filter as usize))
                    .flex_none()
                    .items_center()
                    .gap(look.xs)
                    .px(look.sm)
                    .rounded(look.radius)
                    .border_1()
                    .border_color(if selected { look.accent } else { look.hairline })
                    .text_size(look.xs_size)
                    .line_height(look.xs_line)
                    .text_color(color)
                    .font_features(tabular_numbers())
                    .cursor_pointer()
                    .hover({
                        let hover = look.hover;
                        move |style| style.bg(hover)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_tracker_filter(filter, cx);
                    }))
                    .child(translator.text(key).to_owned())
                    .child(
                        div()
                            .text_color(if selected { look.accent } else { look.tertiary })
                            .child(counts.get(filter).to_string()),
                    )
            });
        h_flex().flex_wrap().gap(look.xs).children(chips)
    }

    /// 虚拟列表的第 `position` 行（筛选 + 排序后的位置）。
    fn tracker_item(&self, position: usize, item: &TrackerItemText, cx: &App) -> AnyElement {
        let dto = self
            .bt
            .tracker_view
            .get(position)
            .and_then(|&index| self.bt.status.as_ref()?.trackers.get(index));
        let Some(dto) = dto else {
            return div().into_any_element();
        };
        let translator = self.translator.read(cx);
        let cells = tracker_cells(dto, &item.look, translator, item.now, &item.meta);
        // 下标 0 留给 DHT 伪行，元素 id 不冲突。
        tracker_row_el(
            position + 1,
            &cells,
            &item.copy,
            &item.look,
            item.layout,
            position + 1 == self.bt.tracker_view.len(),
        )
    }

    pub(super) fn render_bt_trackers(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let look = Look::new(cx);
        let layout = self.bt_layout(window, &look);
        let Some(status) = self.bt.status.as_ref() else {
            let content = if self.bt.failed {
                div().into_any_element()
            } else {
                skeleton_rows(&look, 3).into_any_element()
            };
            return self.bt_frame(content, false, &look, cx);
        };
        // 布局档变化（宽 / 中 / 窄）改变行高，已量过的行需重量。
        if self.bt.tracker_layout.get() != Some(layout) {
            self.bt.tracker_layout.set(Some(layout));
            self.bt.tracker_list.remeasure();
        }
        let translator = self.translator.read(cx);
        let t = |key: &str| SharedString::from(translator.text(key).to_owned());
        let now = chrono::Utc::now().timestamp();

        let header = TrackerHeaderText {
            status: t("btTrackerStatus"),
            url: t("btTrackerUrl"),
            peers: t("btTrackerPeers"),
            message: t("btTrackerMessage"),
            next: t("btTrackerNextAnnounce"),
            last: t("btTrackerLastOk"),
            sort: self.bt.tracker_sort,
            sort_hint: t("btTrackerSortHint"),
            on_sort: Rc::new(cx.listener(|this, key: &TrackerSortKey, _, cx| {
                this.set_tracker_sort(*key, cx);
            })),
        };
        let meta = TrackerMeta {
            peers: header.peers.clone(),
            next: header.next.clone(),
            last: header.last.clone(),
        };
        let item = Rc::new(TrackerItemText {
            look: look.clone(),
            copy: TrackerCopyText {
                copy_url: t("btCopyTrackerUrl"),
                copy_error: t("btCopyTrackerError"),
                click_to_copy_error: t("btTrackerErrorClickToCopy"),
                url_copied: t("btTrackerUrlCopied"),
                error_copied: t("btTrackerErrorCopied"),
            },
            meta: meta.clone(),
            layout,
            now,
        });
        let dht_on = status.dht_enabled;
        let dht = TrackerCells {
            dot: if dht_on { look.success } else { look.tertiary },
            label_color: if dht_on { look.fg } else { look.muted },
            status_label: t(if dht_on {
                "btTrackerDhtOn"
            } else {
                "btTrackerDhtOff"
            }),
            url: t("btTrackerDht"),
            is_dht: true,
            source_label: None,
            source_hint: None,
            peers: None,
            message: SharedString::default(),
            detail: SharedString::default(),
            next: SharedString::from(DASH),
            last: SharedString::from(DASH),
            meta,
        };
        let banner = none_working(&status.trackers).then(|| t("btTrackersNoneWorking"));
        let empty = status.trackers.is_empty().then(|| t("btTrackersEmpty"));
        let filtered_out = (!status.trackers.is_empty() && self.bt.tracker_view.is_empty())
            .then(|| t("btTrackersFilterEmpty"));
        let counts = TrackerCounts::of(&status.trackers);
        let filter_bar = (!status.trackers.is_empty())
            .then(|| self.tracker_filter_bar(&look, &counts, translator, cx));

        let mut table = table_frame(&look).min_h_0();
        if let Some(head) = tracker_header(&look, layout, &header) {
            table = table.child(head);
        }
        table = table.child(tracker_row_el(
            0,
            &dht,
            &item.copy,
            &look,
            layout,
            self.bt.tracker_view.is_empty(),
        ));
        let rows = list(
            self.bt.tracker_list.clone(),
            cx.processor(move |this, position: usize, _window, cx| {
                this.tracker_item(position, &item, cx)
            }),
        )
        .with_sizing_behavior(ListSizingBehavior::Infer)
        .w_full()
        .min_h_0();
        let content = v_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .gap(look.sm)
            .when_some(banner, |this, text| this.child(warning_banner(&look, text)))
            .when_some(filter_bar, |this, bar| this.child(bar))
            .child(table.child(rows))
            .when_some(filtered_out, |this, text| {
                this.child(empty_state(&look, text))
            })
            .when_some(empty, |this, text| this.child(empty_state(&look, text)))
            .into_any_element();
        self.bt_frame(content, true, &look, cx)
    }

    pub(super) fn render_bt_peers(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = Look::new(cx);
        let layout = self.bt_layout(window, &look);
        let Some(status) = self.bt.status.as_ref() else {
            let content = if self.bt.failed {
                div().into_any_element()
            } else {
                skeleton_rows(&look, 4).into_any_element()
            };
            return self.bt_frame(content, false, &look, cx);
        };
        let guard_busy = &self.bt.guard_pending;
        let running = self.bt_task_running();
        let translator = self.translator.read(cx).clone();
        let t = |key: &str| SharedString::from(translator.text(key).to_owned());
        let counts = status.peer_counts;
        let summary = SharedString::from(translator.text_with(
            "btPeersSummary",
            &[
                ("live", &counts.live.to_string()),
                ("connecting", &counts.connecting.to_string()),
                ("queued", &counts.queued.to_string()),
                ("seen", &counts.seen.to_string()),
                ("dead", &counts.dead.to_string()),
            ],
        ));
        let columns = layout == BtLayout::Wide
            && any_guard(&status.peers)
            && guard_columns_fit(self.bt_width(window, &look));
        let header = PeerHeaderText {
            addr: t("btPeerAddr"),
            client: t("btPeerClient"),
            transport: t("btPeerTransport"),
            downloaded: t("btPeerDownloaded"),
            uploaded: t("btPeerUploaded"),
            down_speed: t("btPeerDownSpeed"),
            up_speed: t("btPeerUpSpeed"),
            guard: columns.then(|| GuardHeaderText {
                uploaded: t("btGuardPeerUploaded"),
                state: SharedString::from(format!(
                    "{} / {}",
                    translator.text("btGuardPeerSuspicion"),
                    translator.text("btGuardPeerRules")
                )),
                action: t("btGuardPeerAction"),
            }),
        };
        let copy_label = t("btCopyPeerAddr");
        let menu_labels = Rc::new(GuardMenuLabels {
            bans: BanChoice::ALL.map(|choice| t(choice.label_key())),
            throttle: t("btGuardPeerThrottle"),
            allow: t("btGuardPeerAllow"),
            evidence: t("btGuardPeerViewEvidence"),
        });
        let view = cx.entity();
        let handler: GuardHandler = Rc::new(move |peer, command, window, cx| {
            view.update(cx, |this, cx| {
                this.bt_guard_command(peer, command, window, cx);
            });
        });
        let empty = t(if running {
            "btPeersEmpty"
        } else {
            "btPeersIdleHint"
        });

        let peers = &status.peers[..status.peers.len().min(BT_PEER_RENDER_CAP)];
        let content = if peers.is_empty() {
            empty_state(&look, empty).into_any_element()
        } else {
            let mut table = table_frame(&look);
            if let Some(head) = peer_header(&look, layout, &header) {
                table = table.child(head);
            }
            for (index, peer) in peers.iter().enumerate() {
                let flag = flag_slot(
                    peer_country(peer),
                    look.ext(FLAG_WIDTH),
                    look.radius,
                    look.hairline,
                    window,
                    cx,
                );
                let guard = peer_guard_cells(peer, &look, &translator, columns);
                let menu = guard.is_some().then(|| GuardMenu {
                    peer: peer.clone(),
                    labels: Rc::clone(&menu_labels),
                    handler: Rc::clone(&handler),
                    busy: guard_busy.contains(peer.ip_group.trim()),
                });
                table = table.child(peer_row_el(
                    index,
                    peer,
                    flag,
                    &copy_label,
                    guard.as_ref(),
                    menu,
                    &look,
                    layout,
                    index + 1 == peers.len(),
                ));
            }
            table.into_any_element()
        };
        let body = v_flex()
            .w_full()
            .gap(look.sm)
            .child(note(&look, summary, look.muted).font_features(tabular_numbers()))
            .child(content)
            .into_any_element();
        self.bt_frame(body, false, &look, cx)
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{BtPeerDto, BtTrackerDto, BtTrackerSource, BtTrackerStatus};

    use super::{
        BtLayout, RelativeTime, Span, SpanUnit, last_success, next_announce, none_working,
        peer_sub_line, span_of, tracker_row, transport_label,
    };

    fn tracker(status: BtTrackerStatus) -> BtTrackerDto {
        BtTrackerDto {
            url: "udp://t.example:1337/announce".to_owned(),
            status,
            ..Default::default()
        }
    }

    #[test]
    fn spans_pick_the_coarsest_whole_unit() {
        let cases = [
            (0, SpanUnit::Seconds, 0),
            (59, SpanUnit::Seconds, 59),
            (60, SpanUnit::Minutes, 1),
            (1_680, SpanUnit::Minutes, 28),
            (3_599, SpanUnit::Minutes, 59),
            (3_600, SpanUnit::Hours, 1),
            (86_399, SpanUnit::Hours, 23),
            (86_400, SpanUnit::Days, 1),
            (u64::MAX, SpanUnit::Days, u64::MAX / 86_400),
        ];
        for (seconds, unit, value) in cases {
            assert_eq!(span_of(seconds), Span { unit, value }, "{seconds}s");
        }
    }

    #[test]
    fn relative_times_handle_unknown_never_and_clock_skew() {
        let now = 1_000_000;
        assert_eq!(next_announce(0, now), RelativeTime::Unknown);
        assert_eq!(next_announce(-5, now), RelativeTime::Unknown);
        assert_eq!(
            next_announce(now + 1_680, now),
            RelativeTime::In(Span {
                unit: SpanUnit::Minutes,
                value: 28
            })
        );
        // 已过期：显示 0 秒后，而不是负数。
        assert_eq!(
            next_announce(now - 30, now),
            RelativeTime::In(Span {
                unit: SpanUnit::Seconds,
                value: 0
            })
        );
        assert_eq!(last_success(0, now), RelativeTime::Never);
        assert_eq!(
            last_success(now - 300, now),
            RelativeTime::Ago(Span {
                unit: SpanUnit::Minutes,
                value: 5
            })
        );
        // 时钟回拨（最近成功在「未来」）：按 0 秒前。
        assert_eq!(
            last_success(now + 10, now),
            RelativeTime::Ago(Span {
                unit: SpanUnit::Seconds,
                value: 0
            })
        );
        // 极端值不溢出。
        assert_eq!(
            next_announce(i64::MAX, i64::MIN),
            RelativeTime::In(span_of(i64::MAX.unsigned_abs()))
        );
    }

    #[test]
    fn layout_collapses_columns_as_width_shrinks() {
        assert_eq!(BtLayout::for_width(0.), BtLayout::Wide);
        assert_eq!(BtLayout::for_width(1_200.), BtLayout::Wide);
        assert_eq!(BtLayout::for_width(800.), BtLayout::Wide);
        assert_eq!(BtLayout::for_width(799.), BtLayout::Medium);
        assert_eq!(BtLayout::for_width(520.), BtLayout::Medium);
        assert_eq!(BtLayout::for_width(519.), BtLayout::Narrow);
        assert_eq!(BtLayout::for_width(300.), BtLayout::Narrow);
    }

    #[test]
    fn banner_needs_a_confirmed_failure_and_no_working_or_unknown_tracker() {
        use BtTrackerStatus::{NotContacted, NotWorking, Unknown, Updating, Working};
        let show = |statuses: &[BtTrackerStatus]| {
            let trackers: Vec<_> = statuses.iter().copied().map(tracker).collect();
            none_working(&trackers)
        };
        assert!(!show(&[]));
        assert!(show(&[NotWorking]));
        assert!(show(&[NotWorking, Updating, NotContacted]));
        assert!(!show(&[NotWorking, Working]));
        // 宿主不提供状态：不报警。
        assert!(!show(&[Unknown]));
        assert!(!show(&[NotWorking, Unknown]));
        // 刚启动 / 任务未运行：全是 updating / notContacted 时不报警。
        assert!(!show(&[Updating, Updating]));
        assert!(!show(&[NotContacted, NotContacted]));
        assert!(!show(&[Updating, NotContacted]));
    }

    #[test]
    fn tracker_rows_map_times_and_hide_next_announce_when_not_reporting() {
        let now = 2_000_000;
        let mut dto = tracker(BtTrackerStatus::Working);
        dto.source = BtTrackerSource::Session;
        dto.peers = Some(12);
        dto.message = "  ".to_owned();
        dto.last_ok_at = now - 120;
        dto.next_announce_at = now + 90;
        let row = tracker_row(&dto, now);
        assert_eq!(row.source, BtTrackerSource::Session);
        assert_eq!(row.peers, Some(12));
        assert!(row.message.is_empty());
        assert_eq!(
            row.next,
            RelativeTime::In(Span {
                unit: SpanUnit::Minutes,
                value: 1
            })
        );
        assert_eq!(
            row.last,
            RelativeTime::Ago(Span {
                unit: SpanUnit::Minutes,
                value: 2
            })
        );

        dto.status = BtTrackerStatus::NotContacted;
        dto.message = " timed out ".to_owned();
        let idle = tracker_row(&dto, now);
        assert_eq!(idle.next, RelativeTime::Unknown);
        assert_eq!(idle.message, "timed out");
        assert_eq!(idle.detail, "timed out");
        dto.detail = " deadline has elapsed ".to_owned();
        assert_eq!(tracker_row(&dto, now).detail, "deadline has elapsed");
    }

    #[test]
    fn peer_labels_and_sub_lines() {
        assert_eq!(transport_label("tcp"), "TCP");
        assert_eq!(transport_label("utp"), "uTP");
        assert_eq!(transport_label("socks"), "SOCKS");
        assert_eq!(transport_label("quic"), "quic");

        let mut peer = BtPeerDto {
            addr: "203.0.113.7:6881".to_owned(),
            client: " qBittorrent/5.0 ".to_owned(),
            transport: "tcp".to_owned(),
            downloaded: 3 * 1024 * 1024,
            uploaded: 512,
            ..Default::default()
        };
        assert_eq!(peer_sub_line(&peer), "qBittorrent/5.0 · ↓ 3.0 MB · ↑ 512 B");
        peer.downloaded = 0;
        peer.uploaded = 0;
        assert_eq!(peer_sub_line(&peer), "qBittorrent/5.0");
        peer.client.clear();
        assert_eq!(peer_sub_line(&peer), "");
    }
}

#[cfg(test)]
mod guard_tests {
    use super::{BtLive, GUARD_COLUMNS_MIN_WIDTH, guard_columns_fit};

    #[test]
    fn guard_action_is_single_flight_per_group() {
        let mut bt = BtLive::default();
        assert!(bt.try_begin_guard("1.2.3.4"));
        assert!(!bt.try_begin_guard("1.2.3.4"));
        assert!(bt.try_begin_guard("5.6.7.8"));
        bt.guard_pending.remove("1.2.3.4");
        assert!(bt.try_begin_guard("1.2.3.4"));
    }

    #[test]
    fn guard_columns_need_the_wide_threshold() {
        assert!(!guard_columns_fit(0.));
        assert!(!guard_columns_fit(GUARD_COLUMNS_MIN_WIDTH - 1.));
        assert!(guard_columns_fit(GUARD_COLUMNS_MIN_WIDTH));
        assert!(guard_columns_fit(4_000.));
    }
}
