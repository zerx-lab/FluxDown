//! 任务详情「来源」「网络」页签：eD2K 实况（`daemon.task.ed2kStatus`）的轮询、渲染与热生效编辑项。
//!
//! 轮询生命周期与 BT 页签相同（见 `task_detail_bt.rs`）：页签可见时 [`TaskDetailView::ed2k_touch`]
//! 每帧登记「最近一次渲染」并在没有轮询循环时启动一个；循环每 2 s 请求一次（不重叠），失败保留旧
//! 数据。编辑项（UPnP / Kad 开关、同时连接来源数）乐观更新本地显示，写入成功后立即补拉一次状态，
//! 待补拉结果到达再撤掉乐观值；失败回滚并提示。纯映射（状态文案、选项归一、相对时间）都是纯函数。

use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use fluxdown_protocol::{
    DaemonEd2kStatusParams, Ed2kIdKind, Ed2kKadNodeDto, Ed2kKadNodeOrigin, Ed2kKadNodeState,
    Ed2kKadPhase, Ed2kNetworkDto, Ed2kPeerDto, Ed2kPeerOrigin, Ed2kPeerState, Ed2kServerDto,
    Ed2kServerSource, Ed2kServerStatus, Ed2kStatusDto, Ed2kTaskPhase, Ed2kUpnpStatus, RpcErrorData,
};
use fluxdown_ui_components::{ControlExt as _, FluxIcon, tabular_numbers, tooltip_view};
use fluxdown_ui_i18n::Translator;
use gpui::{
    Anchor, AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement as _, Styled, Window, canvas, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    switch::Switch,
    v_flex,
};

use super::{
    DetailTab, TaskDetailView,
    bt::{
        BT_CLOCK_VISIBLE, BT_PEER_RENDER_CAP, BT_POLL_INTERVAL, BT_VISIBLE_GRACE, BtLayout,
        CopyItem, DASH, Look, Span, bytes_el, chip, copy_menu, copy_to_clipboard, dot,
        ellipsis_cell, empty_state, fixed, header_cell, header_row, note, relative_text,
        skeleton_rows, span_of, span_text, speed_el, table_frame,
    },
};
use crate::{
    components::task_table::task_menu_facts_for,
    controller::{DownloadsCommand, DownloadsResult, PortFuture},
    model::RowKey,
    strings::{error_text, set_segments_error_text},
};

/// 等待下一次轮询期间检查「立即刷新」请求的步长。
const REFRESH_STEP: Duration = Duration::from_millis(250);
/// 「同时连接来源数」可选值：0 = 自动，其余为固定上限。
pub(super) const MAX_SOURCES_OPTIONS: [i32; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];
/// 同时连接来源数上限。
const MAX_SOURCES_LIMIT: i32 = 8;

// 列宽（界面缩放前的 px）。
const COL_ORIGIN: f32 = 64.;
const COL_CLIENT: f32 = 150.;
const COL_STATE: f32 = 150.;
const COL_PARTS: f32 = 72.;
const COL_BYTES: f32 = 84.;
const COL_SPEED: f32 = 92.;
const COL_SERVER_STATUS: f32 = 140.;
const COL_SERVER_SOURCES: f32 = 64.;
const COL_TIME: f32 = 100.;
const COL_KAD_ID: f32 = 250.;
const COL_KAD_STATE: f32 = 96.;
const COL_KAD_ORIGIN: f32 = 84.;
const COL_KAD_VERSION: f32 = 56.;
/// 网络页键值表标签列宽。
const LABEL_WIDTH: f32 = 140.;

// ───────────────────────── 纯映射 ─────────────────────────

/// 「同时连接来源数」当前选项：任务线程数设置 0 / 负数 = 自动，>8 视为 8。
pub(super) fn max_sources_choice(segments: i32) -> i32 {
    segments.clamp(0, MAX_SOURCES_LIMIT)
}

/// `at`（unix 秒）在未来时返回剩余时长；0 / 已过期 = `None`。
pub(super) fn future_span(at: i64, now: i64) -> Option<Span> {
    (at > 0 && at > now).then(|| span_of(at.saturating_sub(now).unsigned_abs()))
}

/// 源状态的显示意图（文案在渲染时按语言拼装）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PeerStateText {
    Key(&'static str),
    /// 排队名次。
    QueuedRank(u32),
    /// 正在下载的块（从 1 起）。
    TransferringPart(u32),
    /// 冷却结束前的剩余时间。
    CooldownUntil(Span),
}

pub(super) fn peer_state_text(peer: &Ed2kPeerDto, now: i64) -> PeerStateText {
    match peer.state {
        Ed2kPeerState::Known => PeerStateText::Key("ed2kPeerStateKnown"),
        Ed2kPeerState::Connecting => PeerStateText::Key("ed2kPeerStateConnecting"),
        Ed2kPeerState::Queued if peer.queue_rank > 0 => PeerStateText::QueuedRank(peer.queue_rank),
        Ed2kPeerState::Queued => PeerStateText::Key("ed2kPeerStateQueued"),
        Ed2kPeerState::Transferring => peer
            .part
            .map_or(PeerStateText::Key("ed2kPeerStateTransferring"), |part| {
                PeerStateText::TransferringPart(part.saturating_add(1))
            }),
        Ed2kPeerState::Connected => PeerStateText::Key("ed2kPeerStateConnected"),
        Ed2kPeerState::Cooldown => future_span(peer.retry_at, now).map_or(
            PeerStateText::Key("ed2kPeerStateCooldown"),
            PeerStateText::CooldownUntil,
        ),
        Ed2kPeerState::Failed => PeerStateText::Key("ed2kPeerStateFailed"),
        Ed2kPeerState::Banned => PeerStateText::Key("ed2kPeerStateBanned"),
    }
}

/// 源地址的显示意图：HighID 源 `ip:port`；LowID 源无地址，显示其 client ID。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PeerAddr {
    Addr(String),
    LowId(u32),
    Unknown,
}

pub(super) fn peer_addr(peer: &Ed2kPeerDto) -> PeerAddr {
    let addr = peer.addr.trim();
    if !addr.is_empty() {
        PeerAddr::Addr(addr.to_owned())
    } else if peer.low_id > 0 {
        PeerAddr::LowId(peer.low_id)
    } else {
        PeerAddr::Unknown
    }
}

fn origin_key(origin: Ed2kPeerOrigin) -> &'static str {
    match origin {
        Ed2kPeerOrigin::Server => "ed2kPeerOriginServer",
        Ed2kPeerOrigin::Kad => "ed2kPeerOriginKad",
    }
}

pub(super) fn phase_key(phase: Ed2kTaskPhase) -> &'static str {
    match phase {
        Ed2kTaskPhase::Idle => "ed2kPhaseIdle",
        Ed2kTaskPhase::Searching => "ed2kPhaseSearching",
        Ed2kTaskPhase::Downloading => "ed2kPhaseDownloading",
        Ed2kTaskPhase::Waiting => "ed2kPhaseWaiting",
        Ed2kTaskPhase::Verifying => "ed2kPhaseVerifying",
    }
}

fn id_key(id: Ed2kIdKind) -> &'static str {
    match id {
        Ed2kIdKind::High => "ed2kIdHigh",
        Ed2kIdKind::Low => "ed2kIdLow",
        Ed2kIdKind::None => "ed2kIdNone",
    }
}

fn id_hint_key(id: Ed2kIdKind) -> Option<&'static str> {
    match id {
        Ed2kIdKind::High => Some("ed2kIdHighHint"),
        Ed2kIdKind::Low => Some("ed2kIdLowHint"),
        Ed2kIdKind::None => None,
    }
}

fn upnp_key(status: Ed2kUpnpStatus) -> &'static str {
    match status {
        Ed2kUpnpStatus::Disabled => "ed2kUpnpDisabled",
        Ed2kUpnpStatus::Pending => "ed2kUpnpPending",
        Ed2kUpnpStatus::Mapped => "ed2kUpnpMapped",
        Ed2kUpnpStatus::Failed => "ed2kUpnpFailed",
    }
}

fn server_status_key(status: Ed2kServerStatus) -> &'static str {
    match status {
        Ed2kServerStatus::Querying => "ed2kServerStatusQuerying",
        Ed2kServerStatus::Working => "ed2kServerStatusWorking",
        Ed2kServerStatus::NotWorking => "ed2kServerStatusNotWorking",
    }
}

fn server_source_key(source: Ed2kServerSource) -> &'static str {
    match source {
        Ed2kServerSource::Manual => "ed2kServerSourceManual",
        Ed2kServerSource::Subscription => "ed2kServerSourceSubscription",
    }
}

fn kad_phase_key(phase: Ed2kKadPhase) -> &'static str {
    match phase {
        Ed2kKadPhase::Idle => "ed2kKadPhaseIdle",
        Ed2kKadPhase::Bootstrapping => "ed2kKadPhaseBootstrapping",
        Ed2kKadPhase::FindingNodes => "ed2kKadPhaseFindingNodes",
        Ed2kKadPhase::Searching => "ed2kKadPhaseSearching",
        Ed2kKadPhase::Done => "ed2kKadPhaseDone",
    }
}

fn kad_state_key(state: Ed2kKadNodeState) -> &'static str {
    match state {
        Ed2kKadNodeState::Known => "ed2kKadNodeStateKnown",
        Ed2kKadNodeState::Queried => "ed2kKadNodeStateQueried",
        Ed2kKadNodeState::Responded => "ed2kKadNodeStateResponded",
    }
}

fn kad_origin_key(origin: Ed2kKadNodeOrigin) -> &'static str {
    match origin {
        Ed2kKadNodeOrigin::Bootstrap => "ed2kKadNodeOriginBootstrap",
        Ed2kKadNodeOrigin::Discovered => "ed2kKadNodeOriginDiscovered",
    }
}

/// 上次查询：0 = 尚无结果（`—`）；时钟回拨按「0 秒前」。
fn ago_text(translator: &Translator, at: i64, now: i64) -> String {
    if at <= 0 {
        DASH.to_owned()
    } else {
        translator.text_with(
            "btTrackerAgo",
            &[(
                "time",
                &span_text(
                    translator,
                    span_of(now.saturating_sub(at).max(0).unsigned_abs()),
                ),
            )],
        )
    }
}

fn in_text(translator: &Translator, span: Span) -> String {
    relative_text(translator, super::bt::RelativeTime::In(span))
}

fn peer_state_label(translator: &Translator, text: PeerStateText) -> String {
    match text {
        PeerStateText::Key(key) => translator.text(key).to_owned(),
        PeerStateText::QueuedRank(rank) => {
            translator.text_with("ed2kPeerStateQueuedRank", &[("rank", &rank.to_string())])
        }
        PeerStateText::TransferringPart(part) => translator.text_with(
            "ed2kPeerStateTransferringPart",
            &[("part", &part.to_string())],
        ),
        PeerStateText::CooldownUntil(span) => translator.text_with(
            "ed2kPeerStateCooldownUntil",
            &[("time", &in_text(translator, span))],
        ),
    }
}

fn peer_state_color(look: &Look, state: Ed2kPeerState) -> Hsla {
    match state {
        Ed2kPeerState::Transferring => look.success,
        Ed2kPeerState::Connected | Ed2kPeerState::Connecting | Ed2kPeerState::Queued => look.info,
        Ed2kPeerState::Cooldown => look.warning,
        Ed2kPeerState::Failed | Ed2kPeerState::Banned => look.destructive,
        Ed2kPeerState::Known => look.tertiary,
    }
}

fn server_status_color(look: &Look, status: Ed2kServerStatus) -> Hsla {
    match status {
        Ed2kServerStatus::Querying => look.info,
        Ed2kServerStatus::Working => look.success,
        Ed2kServerStatus::NotWorking => look.destructive,
    }
}

fn kad_state_color(look: &Look, state: Ed2kKadNodeState) -> Hsla {
    match state {
        Ed2kKadNodeState::Known => look.tertiary,
        Ed2kKadNodeState::Queried => look.warning,
        Ed2kKadNodeState::Responded => look.success,
    }
}

fn trimmed_or_dash(value: &str) -> SharedString {
    let value = value.trim();
    SharedString::from(if value.is_empty() {
        DASH.to_owned()
    } else {
        value.to_owned()
    })
}

// ───────────────────────── 状态 ─────────────────────────

/// 乐观写入的本地显示值：写入在途时 `settle_after = None`；写入成功后记下当时最后发出的状态
/// 请求序号，此后发出的请求结果到达即撤掉（服务端已反映新值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending<T> {
    value: T,
    settle_after: Option<u64>,
}

impl<T> Pending<T> {
    fn in_flight(&self) -> bool {
        self.settle_after.is_none()
    }
}

fn settle<T>(slot: &mut Option<Pending<T>>, request_seq: u64) {
    if slot
        .as_ref()
        .and_then(|pending| pending.settle_after)
        .is_some_and(|after| request_seq > after)
    {
        *slot = None;
    }
}

/// 网络页的两个全局开关。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlobalToggle {
    Upnp,
    Kad,
}

impl GlobalToggle {
    /// 云同步目录键（`ed2k_enable_upnp` / `ed2k_enable_kad` 的同步名）。
    fn key(self) -> &'static str {
        match self {
            Self::Upnp => "ed2k.enable_upnp",
            Self::Kad => "ed2k.enable_kad",
        }
    }
}

/// 页签的轮询状态与最近一次成功数据（随任务切换整体重置）。
#[derive(Default)]
pub(super) struct Ed2kLive {
    status: Option<Box<Ed2kStatusDto>>,
    /// 最近一次请求失败：保留旧数据，只在页内显示一行提示。
    failed: bool,
    polling: bool,
    /// 任务切换代次：旧循环 / 旧响应据此作废。
    generation: u64,
    last_render: Option<Instant>,
    /// 内容区宽度（实际 px），由 canvas 量得。
    width: Rc<Cell<f32>>,
    /// 编辑项写入成功后请求立即补拉一次（轮询等待期间检查）。
    refresh: bool,
    /// 已发出的状态请求序号。
    seq: u64,
    upnp: Option<Pending<bool>>,
    kad: Option<Pending<bool>>,
    segments: Option<Pending<i32>>,
    /// ID 检测结果：覆盖网络状态段直到下一次轮询结果到达。
    id_override: Option<Ed2kNetworkDto>,
    id_checking: bool,
    /// 最近一次 ID 检测失败的原因（下一次检测前保留）。
    id_failure: Option<String>,
}

impl Ed2kLive {
    /// 切换任务：丢弃数据并作废在途循环与回调。
    pub(super) fn reset(&mut self) {
        *self = Self {
            generation: self.generation.wrapping_add(1),
            width: Rc::clone(&self.width),
            ..Self::default()
        };
    }

    fn slot(&mut self, toggle: GlobalToggle) -> &mut Option<Pending<bool>> {
        match toggle {
            GlobalToggle::Upnp => &mut self.upnp,
            GlobalToggle::Kad => &mut self.kad,
        }
    }
}

// ───────────────────────── 渲染积木 ─────────────────────────

fn section_title(look: &Look, text: SharedString) -> Div {
    div()
        .text_size(look.sm_size)
        .line_height(look.sm_line)
        .font_weight(FontWeight::MEDIUM)
        .text_color(look.fg)
        .child(text)
}

/// 键值行：宽 / 中布局标签在左，窄布局标签在上。
fn kv_row(look: &Look, layout: BtLayout, label: SharedString, value: impl IntoElement) -> Div {
    let label_el = div()
        .text_size(look.sm_size)
        .line_height(look.sm_line)
        .text_color(look.muted);
    let value_el = div()
        .flex_1()
        .min_w_0()
        .text_size(look.sm_size)
        .line_height(look.sm_line)
        .text_color(look.fg)
        .child(value);
    if layout == BtLayout::Narrow {
        v_flex()
            .w_full()
            .gap(look.xxs)
            .child(label_el.child(label))
            .child(value_el)
    } else {
        h_flex()
            .w_full()
            .items_start()
            .gap(look.sm)
            .child(
                label_el
                    .flex_none()
                    .w(look.ext(LABEL_WIDTH))
                    .truncate()
                    .child(label),
            )
            .child(value_el)
    }
}

fn row_frame(base: Div, id: (&'static str, usize), look: &Look, last: bool) -> gpui::Stateful<Div> {
    base.id(id)
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
}

fn sub_line(look: &Look, text: SharedString) -> Div {
    div()
        .min_w_0()
        .truncate()
        .text_size(look.xs_size)
        .line_height(look.xs_line)
        .text_color(look.muted)
        .child(text)
}

/// 带状态点的状态格；`tip` 为失败原因等补充说明。
fn status_cell(
    id: (&'static str, usize),
    look: &Look,
    color: Hsla,
    label: SharedString,
    tip: Option<SharedString>,
) -> gpui::Stateful<Div> {
    h_flex()
        .id(id)
        .items_center()
        .gap(look.xs)
        .min_w_0()
        .child(dot(look, color))
        .child(div().min_w_0().truncate().child(label))
        .when_some(tip, |this, tip| {
            this.tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
        })
}

/// 一个源行渲染所需的已本地化文本。
struct PeerCells {
    addr: SharedString,
    addr_mono: bool,
    addr_tip: Option<SharedString>,
    /// 可复制的地址（LowID 源无地址）。
    copy: Option<SharedString>,
    origin: SharedString,
    client: SharedString,
    nickname: Option<SharedString>,
    state: SharedString,
    state_color: Hsla,
    message: Option<SharedString>,
    parts: SharedString,
}

fn peer_cells(
    peer: &Ed2kPeerDto,
    parts_total: u32,
    look: &Look,
    translator: &Translator,
    now: i64,
) -> PeerCells {
    let (addr, addr_mono, addr_tip, copy) = match peer_addr(peer) {
        PeerAddr::Addr(addr) => {
            let addr = SharedString::from(addr);
            (addr.clone(), true, Some(addr.clone()), Some(addr))
        }
        PeerAddr::LowId(id) => (
            SharedString::from(translator.text_with("ed2kPeerLowId", &[("id", &id.to_string())])),
            false,
            Some(SharedString::from(
                translator.text("ed2kPeerLowIdHint").to_owned(),
            )),
            None,
        ),
        PeerAddr::Unknown => (SharedString::from(DASH), false, None, None),
    };
    let nickname = peer.nickname.trim();
    let message = peer.message.trim();
    PeerCells {
        addr,
        addr_mono,
        addr_tip,
        copy,
        origin: SharedString::from(translator.text(origin_key(peer.origin)).to_owned()),
        client: trimmed_or_dash(&peer.client),
        nickname: (!nickname.is_empty()).then(|| SharedString::from(nickname.to_owned())),
        state: SharedString::from(peer_state_label(translator, peer_state_text(peer, now))),
        state_color: peer_state_color(look, peer.state),
        message: (!message.is_empty()).then(|| SharedString::from(message.to_owned())),
        parts: SharedString::from(match peer.parts {
            Some(parts) => format!("{parts} / {parts_total}"),
            None => DASH.to_owned(),
        }),
    }
}

fn peer_row_el(
    index: usize,
    peer: &Ed2kPeerDto,
    cells: PeerCells,
    copy_label: &SharedString,
    look: &Look,
    layout: BtLayout,
    last: bool,
) -> AnyElement {
    let addr_cell = ellipsis_cell(
        ("ed2k-peer-addr", index),
        cells.addr.clone(),
        cells.addr_tip,
    )
    .when(cells.addr_mono, |this| this.font_family(look.mono.clone()))
    .when(!cells.addr_mono, |this| this.text_color(look.muted));
    let state_cell = status_cell(
        ("ed2k-peer-state", index),
        look,
        cells.state_color,
        cells.state.clone(),
        cells.message.clone(),
    );
    let speed = speed_el(look, peer.down_bps, look.accent);
    let base = match layout {
        BtLayout::Wide => h_flex()
            .items_center()
            .gap(look.sm)
            .child(addr_cell)
            .child(fixed(look, COL_ORIGIN).child(h_flex().child(chip(look, cells.origin))))
            .child(
                fixed(look, COL_CLIENT)
                    .child(div().truncate().text_color(look.muted).child(cells.client))
                    .when_some(cells.nickname, |this, nick| {
                        this.child(sub_line(look, nick))
                    }),
            )
            .child(fixed(look, COL_STATE).child(state_cell))
            .child(
                fixed(look, COL_PARTS)
                    .text_right()
                    .text_color(look.muted)
                    .child(cells.parts),
            )
            .child(
                fixed(look, COL_BYTES)
                    .text_right()
                    .child(bytes_el(look, peer.downloaded)),
            )
            .child(fixed(look, COL_SPEED).text_right().child(speed)),
        BtLayout::Medium | BtLayout::Narrow => {
            let mut sub = vec![cells.origin.to_string(), cells.client.to_string()];
            if let Some(nick) = &cells.nickname {
                sub.push(nick.to_string());
            }
            sub.push(cells.parts.to_string());
            if peer.downloaded > 0 {
                sub.push(format!("↓ {}", crate::model::format_bytes(peer.downloaded)));
            }
            if let Some(message) = &cells.message {
                sub.push(message.to_string());
            }
            let sub = SharedString::from(sub.join(" · "));
            let head = if layout == BtLayout::Medium {
                addr_cell.into_any_element()
            } else {
                h_flex()
                    .items_center()
                    .gap(look.sm)
                    .min_w_0()
                    .child(addr_cell)
                    .child(
                        div()
                            .flex_none()
                            .max_w(look.ext(COL_STATE))
                            .child(state_cell),
                    )
                    .into_any_element()
            };
            let row = h_flex().items_center().gap(look.sm).child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(look.xxs)
                    .child(head)
                    .child(sub_line(look, sub)),
            );
            if layout == BtLayout::Medium {
                row.child(fixed(look, COL_STATE).child(status_cell(
                    ("ed2k-peer-state-m", index),
                    look,
                    cells.state_color,
                    cells.state,
                    cells.message,
                )))
                .child(fixed(look, COL_SPEED).text_right().child(speed))
            } else {
                row.child(fixed(look, COL_SPEED).text_right().child(speed))
            }
        }
    };
    let row = row_frame(base, ("ed2k-peer", index), look, last);
    let items = cells
        .copy
        .map(|value| CopyItem {
            label: copy_label.clone(),
            value,
            done: None,
        })
        .into_iter()
        .collect();
    copy_menu(row, items)
}

fn server_row_el(
    index: usize,
    server: &Ed2kServerDto,
    text: &ServerText,
    look: &Look,
    layout: BtLayout,
    last: bool,
) -> AnyElement {
    let addr = SharedString::from(server.addr.trim().to_owned());
    let message = server.message.trim();
    let message = (!message.is_empty()).then(|| SharedString::from(message.to_owned()));
    let source = SharedString::from(
        text.translator
            .text(server_source_key(server.source))
            .to_owned(),
    );
    let addr_cell = h_flex()
        .items_center()
        .gap(look.xs)
        .min_w_0()
        .flex_1()
        .child(
            ellipsis_cell(
                ("ed2k-server-addr", index),
                addr.clone(),
                Some(addr.clone()),
            )
            .font_family(look.mono.clone()),
        )
        .when(server.connected, |this| {
            this.child(chip(look, text.connected.clone()).text_color(look.success))
        });
    let status = status_cell(
        ("ed2k-server-status", index),
        look,
        server_status_color(look, server.status),
        SharedString::from(
            text.translator
                .text(server_status_key(server.status))
                .to_owned(),
        ),
        message.clone(),
    );
    let sources = SharedString::from(
        server
            .sources
            .map_or_else(|| DASH.to_owned(), |n| n.to_string()),
    );
    let last_query = SharedString::from(ago_text(&text.translator, server.last_query_at, text.now));
    let base = if layout == BtLayout::Wide {
        h_flex()
            .items_center()
            .gap(look.sm)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(look.xxs)
                    .child(addr_cell)
                    .child(sub_line(look, source)),
            )
            .child(
                fixed(look, COL_SERVER_STATUS)
                    .child(status)
                    .when_some(message, |this, message| this.child(sub_line(look, message))),
            )
            .child(
                fixed(look, COL_SERVER_SOURCES)
                    .text_right()
                    .text_color(look.muted)
                    .child(sources),
            )
            .child(
                fixed(look, COL_TIME)
                    .text_right()
                    .text_color(look.muted)
                    .child(last_query),
            )
    } else {
        let mut sub = vec![source.to_string(), format!("{}: {sources}", text.sources)];
        sub.push(format!("{}: {last_query}", text.last_query));
        if let Some(message) = &message {
            sub.push(message.to_string());
        }
        h_flex().items_center().gap(look.sm).child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(look.xxs)
                .child(
                    h_flex()
                        .items_center()
                        .gap(look.sm)
                        .min_w_0()
                        .child(addr_cell)
                        .child(
                            div()
                                .flex_none()
                                .max_w(look.ext(COL_SERVER_STATUS))
                                .child(status),
                        ),
                )
                .child(sub_line(look, SharedString::from(sub.join(" · ")))),
        )
    };
    let row = row_frame(base, ("ed2k-server", index), look, last);
    copy_menu(
        row,
        vec![CopyItem {
            label: text.copy.clone(),
            value: addr,
            done: None,
        }],
    )
}

fn kad_row_el(
    index: usize,
    node: &Ed2kKadNodeDto,
    text: &ServerText,
    look: &Look,
    layout: BtLayout,
    last: bool,
) -> AnyElement {
    let addr = SharedString::from(node.addr.trim().to_owned());
    let id = trimmed_or_dash(&node.id);
    let state = status_cell(
        ("ed2k-kad-state", index),
        look,
        kad_state_color(look, node.state),
        SharedString::from(text.translator.text(kad_state_key(node.state)).to_owned()),
        None,
    );
    let origin = SharedString::from(text.translator.text(kad_origin_key(node.origin)).to_owned());
    let version = SharedString::from(if node.version == 0 {
        DASH.to_owned()
    } else {
        node.version.to_string()
    });
    let addr_cell = ellipsis_cell(("ed2k-kad-addr", index), addr.clone(), Some(addr.clone()))
        .font_family(look.mono.clone());
    let id_cell = |base: Div| {
        base.child(
            ellipsis_cell(("ed2k-kad-id", index), id.clone(), Some(id.clone()))
                .font_family(look.mono.clone())
                .text_color(look.muted),
        )
    };
    let base = if layout == BtLayout::Wide {
        h_flex()
            .items_center()
            .gap(look.sm)
            .child(addr_cell)
            .child(id_cell(fixed(look, COL_KAD_ID).flex()))
            .child(fixed(look, COL_KAD_STATE).child(state))
            .child(
                fixed(look, COL_KAD_ORIGIN)
                    .text_color(look.muted)
                    .truncate()
                    .child(origin),
            )
            .child(
                fixed(look, COL_KAD_VERSION)
                    .text_right()
                    .text_color(look.muted)
                    .child(version),
            )
    } else {
        h_flex().items_center().gap(look.sm).child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(look.xxs)
                .child(
                    h_flex()
                        .items_center()
                        .gap(look.sm)
                        .min_w_0()
                        .child(addr_cell)
                        .child(
                            div()
                                .flex_none()
                                .max_w(look.ext(COL_KAD_STATE))
                                .child(state),
                        ),
                )
                .child(id_cell(
                    h_flex()
                        .min_w_0()
                        .text_size(look.xs_size)
                        .line_height(look.xs_line),
                ))
                .child(sub_line(
                    look,
                    SharedString::from(format!("{origin} · {}: {version}", text.version)),
                )),
        )
    };
    let row = row_frame(base, ("ed2k-kad", index), look, last);
    copy_menu(
        row,
        vec![CopyItem {
            label: text.copy.clone(),
            value: addr,
            done: None,
        }],
    )
}

/// 服务器 / Kad 行共用的已本地化文本。
struct ServerText {
    translator: Translator,
    connected: SharedString,
    copy: SharedString,
    sources: SharedString,
    last_query: SharedString,
    version: SharedString,
    now: i64,
}

// ───────────────────────── 视图 ─────────────────────────

impl TaskDetailView {
    pub(super) fn ed2k_tab_active(&self) -> bool {
        matches!(self.tab, DetailTab::Ed2kSources | DetailTab::Ed2kNetwork)
    }

    fn is_ed2k(&self) -> bool {
        self.task_protocol() == Some(crate::model::TaskProtocol::Ed2k)
    }

    /// 编辑项可用：本机、未完成（与「修改线程数」同一事实源）。
    fn ed2k_editable(&self) -> bool {
        task_menu_facts_for(&self.store, &RowKey::Local(self.task_id.clone()))
            .is_some_and(|facts| facts.can_change_segments())
    }

    /// 页签渲染时调用：登记「仍可见」并在没有轮询循环时启动一个。
    pub(super) fn ed2k_touch(&mut self, cx: &mut Context<Self>) {
        self.ed2k.last_render = Some(Instant::now());
        if self.ed2k.polling || self.closed || !self.activity_online || !self.is_ed2k() {
            return;
        }
        self.ed2k.polling = true;
        let generation = self.ed2k.generation;
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(request) = this.update(cx, |this, _| this.ed2k_poll_request(generation))
                else {
                    // 视图已释放。
                    return;
                };
                let Some((future, seq)) = request else {
                    return;
                };
                let result = future.await;
                let Ok(keep_going) = this.update(cx, |this, cx| {
                    this.ed2k_poll_finish(generation, seq, result, cx)
                }) else {
                    // 视图已释放。
                    return;
                };
                if !keep_going {
                    return;
                }
                // 等到下一次轮询；编辑项写入成功后提前补拉。
                let mut waited = Duration::ZERO;
                while waited < BT_POLL_INTERVAL {
                    cx.background_executor().timer(REFRESH_STEP).await;
                    waited += REFRESH_STEP;
                    let Ok(refresh) =
                        this.update(cx, |this, _| std::mem::take(&mut this.ed2k.refresh))
                    else {
                        // 视图已释放。
                        return;
                    };
                    if refresh {
                        break;
                    }
                }
            }
        })
        .detach();
    }

    /// 发出下一次请求；不再需要轮询时返回 `None` 并让循环退出。
    fn ed2k_poll_request(&mut self, generation: u64) -> Option<(PortFuture<DownloadsResult>, u64)> {
        if self.ed2k.generation != generation {
            return None;
        }
        let visible = self
            .ed2k
            .last_render
            .is_some_and(|at| at.elapsed() < BT_VISIBLE_GRACE);
        if self.closed
            || !self.activity_online
            || !self.ed2k_tab_active()
            || !visible
            || !self.is_ed2k()
        {
            self.ed2k.polling = false;
            return None;
        }
        self.ed2k.refresh = false;
        self.ed2k.seq = self.ed2k.seq.wrapping_add(1);
        let future = self
            .port
            .execute(DownloadsCommand::Ed2kStatus(DaemonEd2kStatusParams {
                task_id: self.task_id.clone(),
            }));
        Some((future, self.ed2k.seq))
    }

    /// 应用一次响应；返回循环是否继续。失败时保留旧数据。
    fn ed2k_poll_finish(
        &mut self,
        generation: u64,
        seq: u64,
        result: Result<DownloadsResult, RpcErrorData>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.ed2k.generation != generation {
            return false;
        }
        match result {
            Ok(DownloadsResult::Ed2kStatus(status)) => {
                self.ed2k.status = Some(status);
                self.ed2k.failed = false;
                self.ed2k.id_override = None;
                settle(&mut self.ed2k.upnp, seq);
                settle(&mut self.ed2k.kad, seq);
                settle(&mut self.ed2k.segments, seq);
            }
            Ok(_) | Err(_) => self.ed2k.failed = true,
        }
        cx.notify();
        true
    }

    /// 1 s 定时器调用：页签可见时重绘，让相对时间在两次轮询之间走字（不发请求）。
    pub(super) fn tick_ed2k_clock(&self, cx: &mut Context<Self>) {
        if self.ed2k_tab_active()
            && self
                .ed2k
                .last_render
                .is_some_and(|at| at.elapsed() < BT_CLOCK_VISIBLE)
        {
            cx.notify();
        }
    }

    fn ed2k_layout(&self, window: &Window, look: &Look) -> BtLayout {
        let measured = self.ed2k.width.get();
        let width = if measured > 0. {
            measured
        } else {
            window.viewport_size().width.as_f32()
        };
        BtLayout::for_width(width / look.scale.max(0.01))
    }

    /// 写入失败提示：`操作失败: 原因`。
    fn ed2k_push_error(&self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(
            Notification::error(format!("{}: {text}", self.strings.action_failed)),
            cx,
        );
    }

    /// UPnP / Kad 开关：乐观更新 → 写同步偏好 → 成功补拉 / 失败回滚。
    fn ed2k_set_global(
        &mut self,
        toggle: GlobalToggle,
        value: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slot = self.ed2k.slot(toggle);
        if slot.as_ref().is_some_and(Pending::in_flight) {
            return;
        }
        *slot = Some(Pending {
            value,
            settle_after: None,
        });
        cx.notify();
        let generation = self.ed2k.generation;
        let write = self.port.execute(DownloadsCommand::SetSyncedPreference {
            key: toggle.key(),
            value: serde_json::Value::Bool(value),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = write.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                let current = this.ed2k.generation == generation;
                match result {
                    Ok(_) => {
                        if current {
                            let seq = this.ed2k.seq;
                            if let Some(pending) = this.ed2k.slot(toggle) {
                                pending.settle_after = Some(seq);
                            }
                            this.ed2k.refresh = true;
                        }
                    }
                    Err(error) => {
                        if current {
                            *this.ed2k.slot(toggle) = None;
                        }
                        let text = error_text(this.translator.read(cx), &error);
                        this.ed2k_push_error(text, window, cx);
                    }
                }
                cx.notify();
            }) else {
                // 视图或窗口已释放。
                return;
            };
        })
        .detach();
    }

    /// 同时连接来源数：乐观更新 → `daemon.task.setSegments` → 成功补拉 / 失败回滚。
    fn ed2k_set_segments(&mut self, segments: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.ed2k.segments.as_ref().is_some_and(Pending::in_flight) {
            return;
        }
        self.ed2k.segments = Some(Pending {
            value: segments,
            settle_after: None,
        });
        cx.notify();
        let generation = self.ed2k.generation;
        let write = self.port.execute(DownloadsCommand::SetSegments {
            task_id: self.task_id.clone(),
            segments,
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = write.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                let current = this.ed2k.generation == generation;
                match result {
                    Ok(_) => {
                        if current {
                            let seq = this.ed2k.seq;
                            if let Some(pending) = &mut this.ed2k.segments {
                                pending.settle_after = Some(seq);
                            }
                            this.ed2k.refresh = true;
                        }
                    }
                    Err(error) => {
                        if current {
                            this.ed2k.segments = None;
                        }
                        let text = set_segments_error_text(this.translator.read(cx), &error);
                        this.ed2k_push_error(text, window, cx);
                    }
                }
                cx.notify();
            }) else {
                // 视图或窗口已释放。
                return;
            };
        })
        .detach();
    }

    /// 「检测」按钮：`daemon.ed2k.idCheck`（慢方法），结果覆盖网络状态段直到下一次轮询。
    fn ed2k_run_id_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ed2k.id_checking {
            return;
        }
        self.ed2k.id_checking = true;
        self.ed2k.id_failure = None;
        cx.notify();
        let generation = self.ed2k.generation;
        let check = self.port.execute(DownloadsCommand::Ed2kIdCheck);
        cx.spawn_in(window, async move |this, cx| {
            let result = check.await;
            let Ok(()) = this.update_in(cx, |this, _, cx| {
                if this.ed2k.generation != generation {
                    return;
                }
                this.ed2k.id_checking = false;
                match result {
                    Ok(DownloadsResult::Ed2kNetwork(network)) => {
                        if network.id == Ed2kIdKind::None {
                            this.ed2k.id_failure = Some(network.error.trim().to_owned());
                        }
                        this.ed2k.id_override = Some(network);
                    }
                    Ok(_) => this.ed2k.id_failure = Some(String::new()),
                    Err(error) => {
                        this.ed2k.id_failure = Some(error_text(this.translator.read(cx), &error));
                    }
                }
                cx.notify();
            }) else {
                // 视图或窗口已释放。
                return;
            };
        })
        .detach();
    }

    /// 页签外框：量宽 canvas + 失败提示行 + 内容。
    fn ed2k_frame(&self, content: AnyElement, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let entity_id = cx.entity_id();
        let width = Rc::clone(&self.ed2k.width);
        let scale = look.scale.max(0.01);
        let failed = self.ed2k.failed.then(|| self.t(cx, "ed2kStatusLoadFailed"));
        v_flex()
            .relative()
            .w_full()
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
            .when_some(failed, |this, text| {
                this.child(note(look, text, look.warning))
            })
            .child(content)
            .into_any_element()
    }

    pub(super) fn render_ed2k_sources(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = Look::new(cx);
        let layout = self.ed2k_layout(window, &look);
        let Some(status) = self.ed2k.status.as_deref() else {
            let content = if self.ed2k.failed {
                div().into_any_element()
            } else {
                skeleton_rows(&look, 4).into_any_element()
            };
            return self.ed2k_frame(content, &look, cx);
        };
        let translator = self.translator.read(cx).clone();
        let t = |key: &str| SharedString::from(translator.text(key).to_owned());
        let now = chrono::Utc::now().timestamp();
        let counts = status.peer_counts;
        let summary = SharedString::from(translator.text_with(
            "ed2kPeersSummary",
            &[
                ("total", &counts.total.to_string()),
                ("transferring", &counts.transferring.to_string()),
                ("connected", &counts.connected.to_string()),
                ("queued", &counts.queued.to_string()),
                ("connecting", &counts.connecting.to_string()),
                ("cooldown", &counts.cooldown.to_string()),
                ("failed", &counts.failed.to_string()),
                ("banned", &counts.banned.to_string()),
            ],
        ));
        let peers = status
            .peers
            .get(..status.peers.len().min(BT_PEER_RENDER_CAP))
            .unwrap_or(&status.peers);
        let content = if peers.is_empty() {
            empty_state(
                &look,
                t(if status.phase == Ed2kTaskPhase::Idle {
                    "ed2kIdleHint"
                } else {
                    "ed2kPeersEmpty"
                }),
            )
            .into_any_element()
        } else {
            let mut table = table_frame(&look);
            let head = |label: &str, width: Option<f32>, right: bool| {
                header_cell(&look, t(label), width, right)
            };
            match layout {
                BtLayout::Wide => {
                    table = table.child(header_row(
                        &look,
                        vec![
                            head("ed2kPeerAddr", None, false),
                            head("ed2kPeerOrigin", Some(COL_ORIGIN), false),
                            head("ed2kPeerClient", Some(COL_CLIENT), false),
                            head("ed2kPeerState", Some(COL_STATE), false),
                            head("ed2kPeerParts", Some(COL_PARTS), true),
                            head("ed2kPeerDownloaded", Some(COL_BYTES), true),
                            head("ed2kPeerSpeed", Some(COL_SPEED), true),
                        ],
                    ));
                }
                BtLayout::Medium => {
                    table = table.child(header_row(
                        &look,
                        vec![
                            head("ed2kPeerAddr", None, false),
                            head("ed2kPeerState", Some(COL_STATE), false),
                            head("ed2kPeerSpeed", Some(COL_SPEED), true),
                        ],
                    ));
                }
                BtLayout::Narrow => {}
            }
            let copy_label = t("btCopyPeerAddr");
            for (index, peer) in peers.iter().enumerate() {
                let cells = peer_cells(peer, status.parts_total, &look, &translator, now);
                table = table.child(peer_row_el(
                    index,
                    peer,
                    cells,
                    &copy_label,
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
        self.ed2k_frame(body, &look, cx)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn render_ed2k_network(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = Look::new(cx);
        let layout = self.ed2k_layout(window, &look);
        let Some(status) = self.ed2k.status.as_deref() else {
            let content = if self.ed2k.failed {
                div().into_any_element()
            } else {
                skeleton_rows(&look, 3).into_any_element()
            };
            return self.ed2k_frame(content, &look, cx);
        };
        let translator = self.translator.read(cx).clone();
        let t = |key: &str| SharedString::from(translator.text(key).to_owned());
        let now = chrono::Utc::now().timestamp();
        let editable = self.ed2k_editable();
        let network = self.ed2k.id_override.as_ref().unwrap_or(&status.network);

        // ── 网络状态 ──
        let id_color = match network.id {
            Ed2kIdKind::High => look.success,
            Ed2kIdKind::Low => look.warning,
            Ed2kIdKind::None => look.muted,
        };
        let checking = self.ed2k.id_checking;
        let id_failure = self.ed2k.id_failure.as_ref().map(|error| {
            let failed = translator.text("ed2kIdCheckFailed");
            SharedString::from(if error.is_empty() {
                failed.to_owned()
            } else {
                format!("{failed}: {error}")
            })
        });
        let id_value = v_flex()
            .gap(look.xxs)
            .child(
                h_flex()
                    .flex_wrap()
                    .items_center()
                    .gap(look.sm)
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(id_color)
                            .child(t(id_key(network.id))),
                    )
                    .child(
                        Button::new("ed2k-id-check")
                            .outline()
                            .control(cx)
                            .label(t(if checking {
                                "ed2kIdCheckRunning"
                            } else {
                                "ed2kIdCheckRun"
                            }))
                            .loading(checking)
                            .disabled(checking || !self.activity_online)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.ed2k_run_id_check(window, cx);
                            })),
                    ),
            )
            .when_some(id_hint_key(network.id), |this, key| {
                this.child(note(&look, t(key), look.muted))
            })
            .when_some(id_failure, |this, text| {
                this.child(note(&look, text, look.destructive))
            });
        let switch_value = |pending: Option<Pending<bool>>, server: bool| {
            pending.map_or(server, |pending| pending.value)
        };
        let upnp_on = switch_value(self.ed2k.upnp, network.upnp_enabled);
        let kad_on = switch_value(self.ed2k.kad, network.kad_enabled);
        let toggle = |id: &'static str, which: GlobalToggle, on: bool, cx: &mut Context<Self>| {
            if editable {
                Switch::new(id)
                    .checked(on)
                    .disabled(self.ed2k.slot_in_flight(which))
                    .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                        this.ed2k_set_global(which, *checked, window, cx);
                    }))
                    .into_any_element()
            } else {
                div()
                    .text_color(look.muted)
                    .child(t(if on {
                        "ed2kUpnpMapped"
                    } else {
                        "ed2kUpnpDisabled"
                    }))
                    .into_any_element()
            }
        };
        let upnp_value = h_flex()
            .flex_wrap()
            .items_center()
            .gap(look.sm)
            .child(toggle("ed2k-upnp", GlobalToggle::Upnp, upnp_on, cx))
            .child(chip(&look, t(upnp_key(network.upnp))));
        let kad_value =
            h_flex()
                .items_center()
                .child(toggle("ed2k-kad", GlobalToggle::Kad, kad_on, cx));
        let mono_text = |text: SharedString, id: &'static str| {
            h_flex().min_w_0().child(
                ellipsis_cell((id, 0), text.clone(), Some(text)).font_family(look.mono.clone()),
            )
        };
        let mut net = v_flex()
            .w_full()
            .gap(look.xs)
            .child(section_title(&look, t("ed2kNetStatus")))
            .child(kv_row(&look, layout, t("ed2kNetId"), id_value));
        if network.client_id != 0 {
            net = net.child(kv_row(
                &look,
                layout,
                t("ed2kNetClientId"),
                div()
                    .font_features(tabular_numbers())
                    .child(network.client_id.to_string()),
            ));
        }
        net = net
            .child(kv_row(
                &look,
                layout,
                t("ed2kNetServer"),
                mono_text(trimmed_or_dash(&network.server), "ed2k-net-server"),
            ))
            .child(kv_row(
                &look,
                layout,
                t("ed2kNetListenPort"),
                div()
                    .font_features(tabular_numbers())
                    .child(if network.listen_port == 0 {
                        DASH.to_owned()
                    } else {
                        network.listen_port.to_string()
                    }),
            ));
        if !network.external_ip.trim().is_empty() {
            net = net.child(kv_row(
                &look,
                layout,
                t("ed2kNetExternalIp"),
                mono_text(trimmed_or_dash(&network.external_ip), "ed2k-net-ip"),
            ));
        }
        net = net.child(kv_row(&look, layout, t("ed2kNetUpnp"), upnp_value));
        if network.upnp == Ed2kUpnpStatus::Failed && !network.upnp_error.trim().is_empty() {
            net = net.child(note(
                &look,
                network.upnp_error.trim().to_owned().into(),
                look.muted,
            ));
        }
        net = net
            .child(kv_row(&look, layout, t("ed2kNetKad"), kad_value))
            .child(note(&look, t("ed2kNetGlobalHint"), look.tertiary));

        // ── 本任务 ──
        let mut task = v_flex()
            .w_full()
            .gap(look.xs)
            .child(section_title(&look, t("ed2kNetTask")))
            .child(kv_row(
                &look,
                layout,
                t("ed2kNetPhase"),
                t(phase_key(status.phase)),
            ));
        if let Some(span) = future_span(status.next_search_at, now) {
            task = task.child(kv_row(
                &look,
                layout,
                t("ed2kNetNextSearch"),
                in_text(&translator, span),
            ));
        }
        let hash = trimmed_or_dash(&status.file_hash);
        let copy_hash = (!status.file_hash.trim().is_empty()).then(|| {
            let value = hash.clone();
            let done = t("fileHashCopied");
            Button::new("ed2k-hash-copy")
                .ghost()
                .control(cx)
                .icon(FluxIcon::Copy)
                .tooltip(t("fileHashCopy"))
                .on_click(move |_, window, cx| {
                    copy_to_clipboard(&value, Some(&done), window, cx);
                })
        });
        task = task
            .child(kv_row(
                &look,
                layout,
                t("ed2kNetParts"),
                div()
                    .font_features(tabular_numbers())
                    .child(format!("{} / {}", status.parts_done, status.parts_total)),
            ))
            .child(kv_row(
                &look,
                layout,
                t("ed2kNetFileHash"),
                h_flex()
                    .items_center()
                    .gap(look.xs)
                    .min_w_0()
                    .child(mono_text(hash, "ed2k-net-hash").flex_1())
                    .children(copy_hash),
            ));
        if editable {
            let current = self
                .ed2k
                .segments
                .map_or(max_sources_choice(status.segments), |pending| {
                    max_sources_choice(pending.value)
                });
            let auto = t("ed2kNetMaxSourcesAuto");
            let option_label = move |value: i32| {
                if value == 0 {
                    auto.clone()
                } else {
                    SharedString::from(value.to_string())
                }
            };
            let options: Vec<(i32, SharedString)> = MAX_SOURCES_OPTIONS
                .iter()
                .map(|value| (*value, option_label(*value)))
                .collect();
            let this = cx.weak_entity();
            let busy = self.ed2k.segments.as_ref().is_some_and(Pending::in_flight);
            let picker = Button::new("ed2k-max-sources")
                .outline()
                .control(cx)
                .label(option_label(current))
                .disabled(busy)
                .dropdown_caret(true)
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                    options.iter().fold(menu, |menu, (value, label)| {
                        let value = *value;
                        let this = this.clone();
                        menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(value == current)
                                .on_click(move |_, window, cx| {
                                    if value == current {
                                        return;
                                    }
                                    let Ok(()) = this.update(cx, |this, cx| {
                                        this.ed2k_set_segments(value, window, cx);
                                    }) else {
                                        // 视图已释放，结束这次选择。
                                        return;
                                    };
                                }),
                        )
                    })
                });
            let effective = (current == 0 && status.max_sources > 0)
                .then(|| SharedString::from(format!("= {}", status.max_sources)));
            task = task.child(kv_row(
                &look,
                layout,
                t("ed2kNetMaxSources"),
                v_flex()
                    .gap(look.xxs)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(look.sm)
                            .child(picker)
                            .when_some(effective, |this, text| {
                                this.child(
                                    div()
                                        .text_color(look.muted)
                                        .font_features(tabular_numbers())
                                        .child(text),
                                )
                            }),
                    )
                    .child(note(&look, t("ed2kNetMaxSourcesDesc"), look.tertiary)),
            ));
        }

        // ── 服务器 / Kad 共用文本 ──
        let row_text = ServerText {
            translator: translator.clone(),
            connected: t("ed2kServerConnected"),
            copy: t("btCopyPeerAddr"),
            sources: t("ed2kServerSources"),
            last_query: t("ed2kServerLastQuery"),
            version: t("ed2kKadNodeVersion"),
            now,
        };

        // ── 服务器 ──
        let servers_summary = SharedString::from(translator.text_with(
            "ed2kServersSummary",
            &[
                ("queried", &status.servers.len().to_string()),
                ("total", &status.network.server_count.to_string()),
            ],
        ));
        let servers_content = if status.servers.is_empty() {
            note(&look, t("ed2kServersEmpty"), look.muted).into_any_element()
        } else {
            let mut table = table_frame(&look);
            if layout == BtLayout::Wide {
                table = table.child(header_row(
                    &look,
                    vec![
                        header_cell(&look, t("ed2kServerAddr"), None, false),
                        header_cell(&look, t("ed2kServerStatus"), Some(COL_SERVER_STATUS), false),
                        header_cell(
                            &look,
                            t("ed2kServerSources"),
                            Some(COL_SERVER_SOURCES),
                            true,
                        ),
                        header_cell(&look, t("ed2kServerLastQuery"), Some(COL_TIME), true),
                    ],
                ));
            }
            for (index, server) in status.servers.iter().enumerate() {
                table = table.child(server_row_el(
                    index,
                    server,
                    &row_text,
                    &look,
                    layout,
                    index + 1 == status.servers.len(),
                ));
            }
            table.into_any_element()
        };
        let servers = v_flex()
            .w_full()
            .gap(look.xs)
            .child(section_title(&look, t("ed2kNetServers")))
            .child(note(&look, servers_summary, look.muted).font_features(tabular_numbers()))
            .child(servers_content);

        // ── Kad 节点 ──（开关乐观值优先，避免关掉后仍显示节点表）
        let kad = &status.kad;
        let kad_content = if !kad_on {
            note(&look, t("ed2kKadOff"), look.muted).into_any_element()
        } else if status.network.kad_nodes == 0 {
            note(&look, t("ed2kKadNoBootstrap"), look.muted).into_any_element()
        } else {
            let summary = SharedString::from(translator.text_with(
                "ed2kKadSummary",
                &[
                    ("phase", translator.text(kad_phase_key(kad.phase))),
                    ("bootstrap", &kad.bootstrap_nodes.to_string()),
                    ("responded", &kad.responded.to_string()),
                    ("discovered", &kad.discovered.to_string()),
                    ("sources", &kad.sources.to_string()),
                ],
            ));
            let nodes = if kad.nodes.is_empty() {
                note(&look, t("ed2kKadNodesEmpty"), look.muted).into_any_element()
            } else {
                let mut table = table_frame(&look);
                if layout == BtLayout::Wide {
                    table = table.child(header_row(
                        &look,
                        vec![
                            header_cell(&look, t("ed2kKadNodeAddr"), None, false),
                            header_cell(&look, t("ed2kKadNodeId"), Some(COL_KAD_ID), false),
                            header_cell(&look, t("ed2kKadNodeState"), Some(COL_KAD_STATE), false),
                            header_cell(&look, t("ed2kKadNodeOrigin"), Some(COL_KAD_ORIGIN), false),
                            header_cell(
                                &look,
                                t("ed2kKadNodeVersion"),
                                Some(COL_KAD_VERSION),
                                true,
                            ),
                        ],
                    ));
                }
                for (index, node) in kad.nodes.iter().enumerate() {
                    table = table.child(kad_row_el(
                        index,
                        node,
                        &row_text,
                        &look,
                        layout,
                        index + 1 == kad.nodes.len(),
                    ));
                }
                table.into_any_element()
            };
            v_flex()
                .w_full()
                .gap(look.xs)
                .child(note(&look, summary, look.muted).font_features(tabular_numbers()))
                .child(nodes)
                .into_any_element()
        };
        let kad_section = v_flex()
            .w_full()
            .gap(look.xs)
            .child(section_title(&look, t("ed2kNetKadNodes")))
            .child(kad_content);

        let body = v_flex()
            .w_full()
            .gap(look.ext(20.))
            .child(net)
            .child(task)
            .child(servers)
            .child(kad_section)
            .into_any_element();
        self.ed2k_frame(body, &look, cx)
    }
}

impl Ed2kLive {
    fn slot_in_flight(&self, toggle: GlobalToggle) -> bool {
        let slot = match toggle {
            GlobalToggle::Upnp => &self.upnp,
            GlobalToggle::Kad => &self.kad,
        };
        slot.as_ref().is_some_and(Pending::in_flight)
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{Ed2kPeerDto, Ed2kPeerState};

    use super::{
        MAX_SOURCES_OPTIONS, PeerAddr, PeerStateText, Pending, future_span, max_sources_choice,
        peer_addr, peer_state_text, settle,
    };
    use crate::pages::task_detail::bt::{Span, SpanUnit};

    fn peer(state: Ed2kPeerState) -> Ed2kPeerDto {
        Ed2kPeerDto {
            state,
            ..Ed2kPeerDto::default()
        }
    }

    #[test]
    fn ed2k_max_sources_choice_normalizes_segments() {
        assert_eq!(max_sources_choice(0), 0);
        assert_eq!(max_sources_choice(-3), 0);
        assert_eq!(max_sources_choice(1), 1);
        assert_eq!(max_sources_choice(8), 8);
        assert_eq!(max_sources_choice(9), 8);
        assert_eq!(max_sources_choice(i32::MAX), 8);
        assert_eq!(MAX_SOURCES_OPTIONS.first(), Some(&0));
        assert_eq!(MAX_SOURCES_OPTIONS.last(), Some(&8));
    }

    #[test]
    fn ed2k_future_span_only_for_future_times() {
        assert_eq!(future_span(0, 100), None);
        assert_eq!(future_span(100, 100), None);
        assert_eq!(future_span(50, 100), None);
        assert_eq!(
            future_span(190, 100),
            Some(Span {
                unit: SpanUnit::Minutes,
                value: 1
            })
        );
    }

    #[test]
    fn ed2k_peer_state_queued_and_transferring_details() {
        let mut queued = peer(Ed2kPeerState::Queued);
        assert_eq!(
            peer_state_text(&queued, 0),
            PeerStateText::Key("ed2kPeerStateQueued")
        );
        queued.queue_rank = 12;
        assert_eq!(peer_state_text(&queued, 0), PeerStateText::QueuedRank(12));

        let mut transferring = peer(Ed2kPeerState::Transferring);
        assert_eq!(
            peer_state_text(&transferring, 0),
            PeerStateText::Key("ed2kPeerStateTransferring")
        );
        transferring.part = Some(0);
        assert_eq!(
            peer_state_text(&transferring, 0),
            PeerStateText::TransferringPart(1)
        );
        transferring.part = Some(u32::MAX);
        assert_eq!(
            peer_state_text(&transferring, 0),
            PeerStateText::TransferringPart(u32::MAX)
        );
    }

    #[test]
    fn ed2k_peer_cooldown_expired_falls_back_to_plain() {
        let mut cooling = peer(Ed2kPeerState::Cooldown);
        assert_eq!(
            peer_state_text(&cooling, 1_000),
            PeerStateText::Key("ed2kPeerStateCooldown")
        );
        cooling.retry_at = 900;
        assert_eq!(
            peer_state_text(&cooling, 1_000),
            PeerStateText::Key("ed2kPeerStateCooldown")
        );
        cooling.retry_at = 1_030;
        assert_eq!(
            peer_state_text(&cooling, 1_000),
            PeerStateText::CooldownUntil(Span {
                unit: SpanUnit::Seconds,
                value: 30
            })
        );
    }

    #[test]
    fn ed2k_peer_state_plain_keys() {
        for (state, key) in [
            (Ed2kPeerState::Known, "ed2kPeerStateKnown"),
            (Ed2kPeerState::Connecting, "ed2kPeerStateConnecting"),
            (Ed2kPeerState::Connected, "ed2kPeerStateConnected"),
            (Ed2kPeerState::Failed, "ed2kPeerStateFailed"),
            (Ed2kPeerState::Banned, "ed2kPeerStateBanned"),
        ] {
            assert_eq!(peer_state_text(&peer(state), 0), PeerStateText::Key(key));
        }
    }

    #[test]
    fn ed2k_peer_addr_low_id_and_missing() {
        let mut low = peer(Ed2kPeerState::Known);
        low.low_id = 4_242;
        assert_eq!(peer_addr(&low), PeerAddr::LowId(4_242));
        low.addr = "  ".into();
        assert_eq!(peer_addr(&low), PeerAddr::LowId(4_242));
        assert_eq!(peer_addr(&peer(Ed2kPeerState::Known)), PeerAddr::Unknown);
        let mut high = peer(Ed2kPeerState::Known);
        high.addr = " 1.2.3.4:4662 ".into();
        assert_eq!(peer_addr(&high), PeerAddr::Addr("1.2.3.4:4662".into()));
    }

    #[test]
    fn ed2k_pending_settles_only_after_later_request() {
        let mut slot = Some(Pending {
            value: true,
            settle_after: None,
        });
        settle(&mut slot, 10);
        assert!(slot.is_some(), "write still in flight");
        slot = Some(Pending {
            value: true,
            settle_after: Some(10),
        });
        settle(&mut slot, 10);
        assert!(slot.is_some(), "request sent before the write landed");
        settle(&mut slot, 11);
        assert!(slot.is_none());
    }
}
