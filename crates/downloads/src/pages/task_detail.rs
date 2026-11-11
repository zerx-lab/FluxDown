//! 任务详情：主窗口停靠面板与独立任务窗口共用同一套渲染逻辑。
//!
//! [`DetailMode::Docked`] 由 [`super::downloads::DownloadView`] 持有，不订阅会话，
//! 由宿主在事件后调用 [`TaskDetailView::sync`] 刷新；[`DetailMode::Window`] 由
//! app 侧独立窗口通过 [`crate::session::attach`]（[`SessionConsumer`] 三个同名
//! `pub fn`）订阅，自带一个私有 [`DownloadsController`] 维护任务列表。
#[path = "task_detail_activity.rs"]
mod activity;
#[path = "task_detail_bt.rs"]
mod bt;
#[path = "task_detail_ed2k.rs"]
mod ed2k;
#[path = "task_detail_files.rs"]
mod files;
#[path = "task_detail_hash.rs"]
mod hash;
#[path = "task_detail_source.rs"]
mod source_page;

use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{Local, TimeZone as _};
use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, DaemonEvent, ServiceEvent, TaskDto, TaskRuntimeDto,
    TaskSpeedHistoryDto, TaskSpeedHistoryQuery, WsServerMsg,
};
use fluxdown_ui_components::{
    ControlExt as _, FluxIcon, IconControlExt as _, form, form_field, segmented_tabs,
    tabular_numbers,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, AnyView, App, AppContext as _, ClipboardItem, Context, Div, Entity,
    EventEmitter, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Icon, WindowExt as _,
    button::{Button, ButtonVariants as _},
    chart::{AreaChart, PieChart},
    h_flex,
    input::{InputState, NumberInput},
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    v_flex,
};

use activity::ActivityFeed;
use bt::BtLive;
use ed2k::Ed2kLive;
use files::BtFilesLive;

use crate::{
    UserStartIntent,
    components::{
        segment_progress::render_segment_progress,
        task_table::{
            progress_bar_color, progress_track_color, task_menu_facts_for, task_status_color,
        },
    },
    controller::{
        DownloadsCommand, DownloadsController, DownloadsPort, DownloadsResult,
        SEED_LIMIT_FOLLOW_GLOBAL, SeedLimits,
    },
    model::{
        DownloadTaskView, RowKey, TaskProtocol, TaskState, TaskStore,
        file_hash::DEFAULT_HASH_ALGORITHM,
        format_bytes,
        source_composition::{SourceKind, compose, format_percent},
        task_route::route_label,
    },
    pages::{
        downloads::{DownloadHostActions, PluginTaskTab},
        task_edit::{TaskEditTarget, open_change_segments_dialog, open_change_url_dialog},
    },
    strings::DownloadStrings,
};

/// 做种页实时上传曲线保留的采样点数（每秒一次）。
const SPEED_HISTORY_CAPACITY: usize = 120;
/// 速度页下载中的刷新间隔；1 s 定时器的抖动留出余量。
const SPEED_REFRESH_INTERVAL: Duration = Duration::from_millis(1_500);
/// 常规页信息列的折行基准宽度：可用宽度容不下「信息列 + 来源构成」时来源换行。
const GENERAL_INFO_MIN_WIDTH: f32 = 300.;
/// 常规页「来源构成」区块宽度（环形图 140 + 图例）。
const SOURCES_SECTION_WIDTH: f32 = 360.;
/// 键值表标签列宽：详情、做种、高级与任务组概览共用，保证值列左缘对齐。
const DETAIL_LABEL_WIDTH: f32 = 112.;
/// 活动日志时间戳宽度（`YYYY-MM-DD HH:MM:SS.mmm` 等宽数字），条目头部单独一行显示。
const ACTIVITY_TIME_WIDTH: f32 = 168.;
/// 空状态图标边长。
const EMPTY_ICON_SIZE: f32 = 32.;

/// 面板承载方式：决定是否渲染顶部进度头。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailMode {
    /// 主窗口停靠面板：不渲染进度头（列表已可见进度）。
    Docked,
    /// 独立任务窗口：渲染进度头和完整详情 Tab 区。
    Window,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetailTab {
    General,
    /// BT 任务的文件表（优先级 / 重命名）。
    Files,
    Speed,
    Seeding,
    /// BT 任务的 tracker 汇报状态。
    Trackers,
    /// BT 任务的已连接 peer。
    Peers,
    /// eD2K 任务的来源。
    Ed2kSources,
    /// eD2K 任务的服务器 / Kad / 本机网络。
    Ed2kNetwork,
    Log,
    Advanced,
    /// 插件贡献的 tab，下标对应 `TaskDetailView::plugin_tabs`。
    Plugin(u16),
}

/// 可见 tab 顺序：内置 tab（做种 / Tracker / 连接仅 BT 任务，来源 / 网络仅 eD2K 任务）后接
/// 全部插件 tab。
fn visible_tabs(protocol: Option<TaskProtocol>, plugin_tabs: usize) -> Vec<DetailTab> {
    let is_bt = protocol == Some(TaskProtocol::Bt);
    let is_ed2k = protocol == Some(TaskProtocol::Ed2k);
    let builtin = [
        DetailTab::General,
        DetailTab::Files,
        DetailTab::Speed,
        DetailTab::Seeding,
        DetailTab::Trackers,
        DetailTab::Peers,
        DetailTab::Ed2kSources,
        DetailTab::Ed2kNetwork,
        DetailTab::Log,
        DetailTab::Advanced,
    ]
    .into_iter()
    .filter(move |tab| match tab {
        DetailTab::Files | DetailTab::Seeding | DetailTab::Trackers | DetailTab::Peers => is_bt,
        DetailTab::Ed2kSources | DetailTab::Ed2kNetwork => is_ed2k,
        _ => true,
    });
    let plugin = (0..u16::try_from(plugin_tabs).unwrap_or(u16::MAX)).map(DetailTab::Plugin);
    builtin.chain(plugin).collect()
}

/// 面板对外事件：任务被删除 / 快照中不再存在时发出，宿主据此关闭承载窗口。
pub enum TaskDetailEvent {
    Closed,
}

pub struct TaskDetailView {
    task_id: String,
    mode: DetailMode,
    /// 仅 `DetailMode::Window` 持有：独立窗口自带的会话状态。
    controller: Option<DownloadsController>,
    store: Rc<TaskStore>,
    port: Arc<dyn DownloadsPort>,
    host: DownloadHostActions,
    translator: Entity<Translator>,
    strings: DownloadStrings,
    tab: DetailTab,
    /// 插件贡献的 tab（下标即 `DetailTab::Plugin`）。
    plugin_tabs: Vec<PluginTaskTab>,
    /// 插件 tab 视图缓存：`(tab 下标, task_id)` → 视图。
    plugin_views: HashMap<(u16, String), AnyView>,
    /// 传输诊断页签视图缓存：任务 id → 视图。
    /// 完整 DTO（校验和 / 代理 / 做种详情等 `DownloadTaskView` 未覆盖的字段）。
    /// 停靠模式由宿主经 [`Self::sync`] 提供；窗口模式来自私有 controller。
    dto: Option<TaskDto>,
    queue_names: Vec<(String, String)>,
    /// 停靠面板：宿主同步的「隐藏任务来源」偏好；窗口模式读私有 controller 的偏好，见
    /// [`Self::source_hidden`]。
    hide_source: bool,
    /// 客户端 1 Hz 实时采样环（仅做种页上传曲线使用）。
    speed_history: VecDeque<(Instant, u64)>,
    /// daemon 持久化的全生命周期速度曲线与统计（速度页）。
    speed_stats: Option<TaskSpeedHistoryDto>,
    speed_fetching: bool,
    /// 请求在途时又出现刷新需求：返回后补拉一次。
    speed_refetch: bool,
    speed_generation: u64,
    speed_was_downloading: bool,
    speed_last_fetch: Option<Instant>,
    activity: ActivityFeed,
    /// Tracker / 连接页签的 BT 实况轮询状态。
    bt: BtLive,
    /// 来源 / 网络页签的 eD2K 实况轮询状态。
    ed2k: Ed2kLive,
    /// 文件页签的 BT 文件表轮询状态与选区。
    bt_files: BtFilesLive,
    runtime: Option<TaskRuntimeDto>,
    activity_stale: bool,
    activity_online: bool,
    closed: bool,
    last_error: Option<SharedString>,
    seed_ratio: Entity<InputState>,
    seed_post_ratio: Entity<InputState>,
    seed_time_limit: Entity<InputState>,
    seed_inactive_limit: Entity<InputState>,
    seed_upload_limit: Entity<InputState>,
    /// 四个限制项（总分享率、做种后分享率、做种时长、不活跃时长）当前选中的模式。
    seed_modes: [SeedMode; 4],
    /// 做种限制输入框上次预填所用的限制值；`None` = 尚未预填。保存时未改动的输入项按它还原。
    seed_baseline: Option<SeedLimits>,
    /// 错误信息复制按钮的反馈代次；非零 = 显示「已复制」勾选态，定时回落时比对代次防连点提前复位。
    error_copied: u32,
    /// 日志「复制全部」按钮的反馈代次，语义同 `error_copied`。
    log_copied: u32,
    /// 高级页「来源页」复制按钮的反馈代次，语义同 `error_copied`。
    source_page_copied: u32,
    /// 文件哈希算法选择。任务切换或行内 `fileHash` 算法变化时重置。
    file_hash_algorithm: String,
    /// 上次同步的任务 id，用来识别切换。
    file_hash_task: Option<String>,
    /// 上次同步的行内 `fileHash`，用来识别算法变化。
    file_hash_noted: Option<String>,
    /// 非零 = 复制按钮显示「已复制」。
    file_hash_copied: u32,
    /// 已为其排过「打开即计算」的任务 id：每次打开（含切换回来）只自动发起一次，失败后不反复重试。
    file_hash_auto: Option<String>,
    /// 打开即计算正在停留期（尚未发起请求）：行内照样显示「计算中」。
    file_hash_pending: bool,
}

impl EventEmitter<TaskDetailEvent> for TaskDetailView {}

impl TaskDetailView {
    /// 独立任务窗口构造（`DetailMode::Window`）：自建私有 `DownloadsController`，
    /// 不需要外部 store（`TaskStore` 是 `pub(crate)`，不能出现在跨 crate 公开签名里）。
    pub fn new(
        translator: Entity<Translator>,
        task_id: String,
        port: Arc<dyn DownloadsPort>,
        host: DownloadHostActions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_internal(
            translator,
            task_id,
            None,
            port,
            host,
            DetailMode::Window,
            window,
            cx,
        )
    }

    /// 停靠面板构造（`DetailMode::Docked`）：借用宿主 `DownloadView` 已有的
    /// `Rc<TaskStore>`；仅同 crate 可调用。
    pub(crate) fn new_docked(
        translator: Entity<Translator>,
        task_id: String,
        store: Rc<TaskStore>,
        port: Arc<dyn DownloadsPort>,
        host: DownloadHostActions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_internal(
            translator,
            task_id,
            Some(store),
            port,
            host,
            DetailMode::Docked,
            window,
            cx,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "assembles every session/host input the detail view needs across docked and windowed modes"
    )]
    fn new_internal(
        translator: Entity<Translator>,
        task_id: String,
        store: Option<Rc<TaskStore>>,
        port: Arc<dyn DownloadsPort>,
        host: DownloadHostActions,
        mode: DetailMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let strings = DownloadStrings::from_translator(translator.read(cx));
        let controller =
            matches!(mode, DetailMode::Window).then(|| DownloadsController::new(Arc::clone(&port)));
        let store = match &controller {
            Some(controller) => Rc::clone(controller.store()),
            None => store.unwrap_or_default(),
        };
        let seed_ratio = Self::seed_input(window, cx, |t| parse_seed_ratio(t).is_some(), "1.00");
        let seed_post_ratio =
            Self::seed_input(window, cx, |t| parse_seed_ratio(t).is_some(), "1.00");
        let seed_time_limit =
            Self::seed_input(window, cx, |t| parse_seed_minutes(t).is_some(), "1440");
        let seed_inactive_limit =
            Self::seed_input(window, cx, |t| parse_seed_minutes(t).is_some(), "1440");
        let seed_upload_limit =
            Self::seed_input(window, cx, |t| parse_seed_upload_limit(t).is_some(), "0");

        cx.observe(&translator, |this, translator, cx| {
            this.strings = DownloadStrings::from_translator(translator.read(cx));
            cx.notify();
        })
        .detach();
        Self::observe_file_hash_inflight(cx);

        let mut this = Self {
            activity: ActivityFeed::new(task_id.clone()),
            bt: BtLive::default(),
            ed2k: Ed2kLive::default(),
            bt_files: BtFilesLive::default(),
            runtime: None,
            activity_stale: false,
            activity_online: true,
            task_id,
            mode,
            controller,
            store,
            port,
            host,
            translator,
            strings,
            tab: DetailTab::General,
            plugin_tabs: Vec::new(),
            plugin_views: HashMap::new(),
            dto: None,
            queue_names: Vec::new(),
            hide_source: false,
            speed_history: VecDeque::new(),
            speed_stats: None,
            speed_fetching: false,
            speed_refetch: false,
            speed_generation: 0,
            speed_was_downloading: false,
            speed_last_fetch: None,
            closed: false,
            last_error: None,
            seed_ratio,
            seed_post_ratio,
            seed_time_limit,
            seed_inactive_limit,
            seed_upload_limit,
            seed_modes: [SeedMode::Global; 4],
            seed_baseline: None,
            error_copied: 0,
            log_copied: 0,
            source_page_copied: 0,
            file_hash_algorithm: DEFAULT_HASH_ALGORITHM.to_owned(),
            file_hash_task: None,
            file_hash_noted: None,
            file_hash_copied: 0,
            file_hash_auto: None,
            file_hash_pending: false,
        };
        this.sync_file_hash(cx);
        Self::spawn_speed_ticker(cx);
        this
    }

    fn seed_input(
        window: &mut Window,
        cx: &mut Context<Self>,
        valid: fn(&str) -> bool,
        default_text: &str,
    ) -> Entity<InputState> {
        cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .validate(move |text, _| text.trim().is_empty() || valid(text.trim()));
            state.set_value(default_text, window, cx);
            state
        })
    }

    fn spawn_speed_ticker(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this
                    .update(cx, |this, cx| {
                        this.sample_speed(cx);
                        this.tick_speed_history(cx);
                        this.tick_bt_clock(cx);
                        this.tick_ed2k_clock(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// 切换承载的任务（停靠面板双击另一行时调用）；随后宿主应立即调用 [`Self::sync`]。
    pub fn set_task(&mut self, task_id: String, cx: &mut Context<Self>) {
        if self.task_id == task_id {
            return;
        }
        self.task_id = task_id;
        self.tab = DetailTab::General;
        self.plugin_views.clear();
        self.dto = None;
        self.activity.switch_task(self.task_id.clone());
        self.bt.reset();
        self.ed2k.reset();
        self.bt_files.reset();
        self.activity_stale = false;
        self.speed_history.clear();
        self.speed_stats = None;
        self.speed_fetching = false;
        self.speed_refetch = false;
        self.speed_generation = self.speed_generation.wrapping_add(1);
        self.speed_was_downloading = false;
        self.speed_last_fetch = None;
        self.runtime = None;
        self.closed = false;
        self.error_copied = 0;
        self.log_copied = 0;
        self.source_page_copied = 0;
        self.file_hash_algorithm = DEFAULT_HASH_ALGORITHM.to_owned();
        self.file_hash_task = None;
        self.file_hash_noted = None;
        self.file_hash_copied = 0;
        self.file_hash_auto = None;
        self.file_hash_pending = false;
        self.seed_baseline = None;
        self.fetch_activity(cx);
        cx.notify();
    }

    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// 当前任务文件名（用于独立窗口标题）；未解析或任务不存在时为 `None`。
    #[must_use]
    pub fn file_name(&self) -> Option<String> {
        self.store
            .get(&RowKey::Local(self.task_id.clone()))
            .map(|row| row.name.clone())
            .filter(|name| !name.is_empty())
    }

    /// 停靠面板：宿主在事件后传入最新 DTO（校验和 / 代理等字段来源）刷新视图。
    pub fn sync(&mut self, dto: Option<TaskDto>, cx: &mut Context<Self>) {
        self.dto = dto;
        self.refresh_from_store(cx);
        self.fetch_activity(cx);
    }

    /// 停靠面板与独立窗口共用的即时传输状态（不是持久历史）。
    pub fn sync_runtime(&mut self, runtime: Option<TaskRuntimeDto>, cx: &mut Context<Self>) {
        if self.runtime != runtime {
            self.runtime = runtime;
            cx.notify();
        }
    }

    /// 停靠面板：宿主同步队列名（`DownloadView::sync_delegate_context` 一并调用）。
    pub fn set_queue_names(&mut self, queues: Vec<(String, String)>, cx: &mut Context<Self>) {
        if self.queue_names != queues {
            self.queue_names = queues;
            cx.notify();
        }
    }

    /// 停靠面板：宿主同步「隐藏任务来源」偏好。
    pub(crate) fn set_hide_source(&mut self, hide: bool, cx: &mut Context<Self>) {
        if self.hide_source != hide {
            self.hide_source = hide;
            cx.notify();
        }
    }

    /// 副标题是否隐藏来源站点：窗口模式实时读私有 controller 的偏好（偏好事件已触发重绘）。
    pub(super) fn source_hidden(&self) -> bool {
        match &self.controller {
            Some(controller) => controller.hide_task_source(),
            None => self.hide_source,
        }
    }

    fn current_dto(&self) -> Option<&TaskDto> {
        match &self.controller {
            Some(controller) => controller.task_dto(&self.task_id),
            None => self.dto.as_ref(),
        }
    }

    fn queue_label(&self, queue_id: &str) -> Option<String> {
        if queue_id.is_empty() {
            return None;
        }
        let name = match &self.controller {
            Some(controller) => controller.queue_name(queue_id),
            None => self
                .queue_names
                .iter()
                .find(|(id, _)| id == queue_id)
                .map(|(_, name)| name.as_str()),
        }
        .unwrap_or(queue_id);
        Some(self.strings.queue_name(queue_id, name).to_string())
    }

    fn is_bt(&self) -> bool {
        self.task_protocol() == Some(TaskProtocol::Bt)
    }

    fn task_protocol(&self) -> Option<TaskProtocol> {
        self.store
            .get(&RowKey::Local(self.task_id.clone()))
            .map(|row| row.protocol)
    }

    fn t(&self, cx: &App, key: &str) -> SharedString {
        SharedString::from(self.translator.read(cx).text(key).to_owned())
    }

    fn refresh_from_store(&mut self, cx: &mut Context<Self>) {
        // 任务表分页载入期间行集合只是占位：行不在不代表任务已被删除。
        if self.store.loading().is_some() {
            cx.notify();
            return;
        }
        let Some(metadata_pending) = self
            .store
            .get(&RowKey::Local(self.task_id.clone()))
            .map(|row| row.metadata_pending)
        else {
            self.close(cx);
            return;
        };
        self.bt.observe_metadata_pending(metadata_pending);
        self.sync_file_hash(cx);
        cx.notify();
    }

    fn sample_speed(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return;
        };
        // 做种页曲线画上传速度：下载中取实时上传，本地做种中取做种上传，其余为 0。
        let uploading = row.state == TaskState::Downloading
            || (row.state == TaskState::Completed && row.seeding_status == 1);
        let speed = if uploading {
            row.upload_speed_bytes_per_second
        } else {
            0
        };
        drop(row);
        push_bounded(
            &mut self.speed_history,
            (Instant::now(), speed),
            SPEED_HISTORY_CAPACITY,
        );
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.closed {
            self.closed = true;
            cx.emit(TaskDetailEvent::Closed);
        }
    }

    // ---- SessionConsumer (独立任务窗口经 `attach` 驱动；停靠面板不调用) ----

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) {
        if let Some(controller) = &mut self.controller {
            controller.replace_snapshot(snapshot);
            self.dto = controller.task_dto(&self.task_id).cloned();
            self.runtime = controller.task_runtime(&self.task_id).cloned();
        }
        if snapshot.daemon_connected {
            if !self.activity_online {
                self.bt.rearm_general();
            }
            self.activity_online = true;
            self.last_error = None;
            if self.activity.loaded() || self.activity_stale {
                self.activity.reconnect();
            }
            self.activity_stale = false;
        } else {
            self.activity_online = false;
            self.activity_stale = true;
            self.activity.suspend();
            self.runtime = None;
            self.last_error = Some(self.strings.disconnected.clone());
        }
        if snapshot.daemon_connected {
            self.refresh_from_store(cx);
        } else {
            // 服务未就绪的空快照不代表任务已删除；等 daemon 已连接的快照再判定。
            cx.notify();
        }
        self.fetch_activity(cx);
    }

    pub fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>) {
        if let ServiceEvent::Agent(AgentEvent::DaemonConnectionChanged(connected)) = event {
            if *connected {
                if !self.activity_online {
                    self.bt.rearm_general();
                }
                self.activity_online = true;
                self.activity.reconnect();
                self.activity_stale = false;
                self.last_error = None;
                self.fetch_activity(cx);
            } else {
                self.mark_stale(cx);
            }
        }
        if let ServiceEvent::Agent(AgentEvent::Daemon(daemon_event)) = event {
            match daemon_event {
                DaemonEvent::TaskDeleted { task_id } if *task_id == self.task_id => {
                    self.close(cx);
                    return;
                }
                DaemonEvent::TasksDeleted { task_ids } if task_ids.contains(&self.task_id) => {
                    self.close(cx);
                    return;
                }
                DaemonEvent::TaskActivityAdded(entry) => {
                    if self.activity.add(entry.clone()) {
                        cx.notify();
                    }
                }
                DaemonEvent::TaskRuntimeChanged(runtime) if runtime.task_id == self.task_id => {
                    self.sync_runtime(Some(runtime.clone()), cx);
                }
                _ => {}
            }
        }
        let concerns_task = self.event_concerns_task(event);
        if let Some(controller) = &mut self.controller {
            // 控制器照常应用；与本任务无关的单任务事件不再克隆 DTO / 重绘。
            let changed = controller.apply_event(event);
            if changed && concerns_task {
                self.dto = controller.task_dto(&self.task_id).cloned();
                self.refresh_from_store(cx);
            }
        }
    }

    /// 单任务的高频事件（任务变更 / 进度 / 运行态 / 元数据）只有涉及本任务才需要刷新；
    /// 其余事件一律视为相关。
    fn event_concerns_task(&self, event: &ServiceEvent) -> bool {
        let ServiceEvent::Agent(AgentEvent::Daemon(event)) = event else {
            return true;
        };
        let task_id = match event {
            DaemonEvent::TaskChanged(task) => &task.task_id,
            DaemonEvent::TaskRuntimeChanged(runtime) => &runtime.task_id,
            DaemonEvent::Engine(
                WsServerMsg::TaskProgress { task_id, .. }
                | WsServerMsg::TaskMetaProbed { task_id, .. },
            ) => task_id,
            _ => return true,
        };
        *task_id == self.task_id
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        if let Some(controller) = &mut self.controller {
            controller.mark_stale();
        }
        self.runtime = None;
        self.activity_stale = true;
        self.activity.suspend();
        self.activity_online = false;
        self.last_error = Some(self.strings.disconnected.clone());
        cx.notify();
    }

    fn fetch_activity(&mut self, cx: &mut Context<Self>) {
        if self.closed || !self.activity_online {
            return;
        }
        let Some(ticket) = self.activity.begin() else {
            return;
        };
        let future = self
            .port
            .execute(DownloadsCommand::TaskActivity(ticket.query.clone()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let page = match future.await {
                Ok(DownloadsResult::TaskActivity(page)) => Some(page),
                _ => None,
            };

            let Ok(()) = this.update(cx, |this, cx| {
                if this.activity.finish(&ticket, page) {
                    cx.notify();
                    this.fetch_activity(cx);
                }
            }) else {
                // 视图已释放，结束回调而不再更新状态。
                return;
            };
        })
        .detach();
    }

    fn task_downloading(&self) -> bool {
        self.store
            .get(&RowKey::Local(self.task_id.clone()))
            .is_some_and(|row| row.state == TaskState::Downloading)
    }

    /// 速度页的定时刷新决策（随 1 s 定时器调用）。
    fn tick_speed_history(&mut self, cx: &mut Context<Self>) {
        let downloading = self.task_downloading();
        let was_downloading = std::mem::replace(&mut self.speed_was_downloading, downloading);
        if self.tab != DetailTab::Speed {
            return;
        }
        if speed_refresh_due(
            was_downloading,
            downloading,
            self.speed_last_fetch.map(|at| at.elapsed()),
        ) {
            self.fetch_speed_history(cx);
        }
    }

    fn fetch_speed_history(&mut self, cx: &mut Context<Self>) {
        if self.closed || !self.activity_online || self.tab != DetailTab::Speed {
            return;
        }
        if self.speed_fetching {
            self.speed_refetch = true;
            return;
        }
        self.speed_fetching = true;
        self.speed_refetch = false;
        self.speed_last_fetch = Some(Instant::now());
        let generation = self.speed_generation;
        let task_id = self.task_id.clone();
        let future = self
            .port
            .execute(DownloadsCommand::TaskSpeedHistory(TaskSpeedHistoryQuery {
                task_id: task_id.clone(),
            }));
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, |this, cx| {
                if this.speed_generation != generation {
                    return;
                }
                this.speed_fetching = false;
                // 失败（含旧宿主不支持该方法）保留上次成功的数据。
                if let Ok(DownloadsResult::TaskSpeedHistory(history)) = result
                    && history.as_ref().is_none_or(|dto| dto.task_id == task_id)
                {
                    this.speed_stats = history;
                }
                cx.notify();
                if this.speed_refetch {
                    this.fetch_speed_history(cx);
                }
            }) else {
                // 视图已释放。
                return;
            };
        })
        .detach();
    }

    fn load_older_activity(&mut self, cx: &mut Context<Self>) {
        self.activity.load_older();
        self.fetch_activity(cx);
    }

    fn retry_activity(&mut self, cx: &mut Context<Self>) {
        self.activity.retry();
        self.fetch_activity(cx);
    }

    // ---- actions ----

    fn select_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            if tab == DetailTab::Speed {
                self.speed_was_downloading = self.task_downloading();
                self.fetch_speed_history(cx);
            }
            cx.notify();
        }
    }

    fn run_command(&mut self, command: DownloadsCommand, cx: &mut Context<Self>) {
        let future = self.port.execute(command);
        cx.spawn(async move |this, cx| {
            let failed = future.await.is_err();

            let Ok(()) = this.update(cx, |this, cx| {
                if failed {
                    this.last_error = Some(this.strings.action_failed.clone());
                }
                cx.notify();
            }) else {
                // 视图已释放，结束回调而不再更新状态。
                return;
            };
        })
        .detach();
    }

    fn reveal(&mut self, cx: &mut Context<Self>) {
        self.run_command(
            DownloadsCommand::RevealTask {
                task_id: self.task_id.clone(),
            },
            cx,
        );
    }

    fn open_file(&mut self, cx: &mut Context<Self>) {
        self.run_command(
            DownloadsCommand::OpenTask {
                task_id: self.task_id.clone(),
            },
            cx,
        );
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return;
        };
        let active = matches!(row.state, TaskState::Downloading | TaskState::Pending);
        drop(row);
        if active {
            self.run_command(
                DownloadsCommand::Pause {
                    task_id: self.task_id.clone(),
                },
                cx,
            );
            return;
        }
        let task_id = self.task_id.clone();
        let on_user_started = self.host.on_user_started.clone();
        if let Some(hook) = &on_user_started {
            hook(
                UserStartIntent::RestartRequested {
                    task_id: task_id.clone(),
                    replaces_task: false,
                },
                cx,
            );
        }
        let future = self.port.execute(DownloadsCommand::Resume {
            task_id: task_id.clone(),
        });
        cx.spawn(async move |this, cx| {
            let failed = future.await.is_err();
            if let Some(hook) = on_user_started {
                let intent = if failed {
                    UserStartIntent::RestartFailed(task_id)
                } else {
                    UserStartIntent::RestartSucceeded(task_id)
                };
                cx.update(|cx| hook(intent, cx));
            }

            let Ok(()) = this.update(cx, |this, cx| {
                if failed {
                    this.last_error = Some(this.strings.action_failed.clone());
                }
                cx.notify();
            }) else {
                // 视图已释放，结束回调而不再更新状态。
                return;
            };
        })
        .detach();
    }

    fn copy_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return;
        };
        let url = row.share_url().to_owned();
        drop(row);
        cx.write_to_clipboard(ClipboardItem::new_string(url));
        window.push_notification(Notification::success(self.strings.url_copied.clone()), cx);
    }

    /// 详情「常规」页「更换链接 / 修改线程数」按钮的编辑上下文（与右键菜单同一对话框）。
    fn edit_target(&self) -> TaskEditTarget {
        TaskEditTarget {
            key: RowKey::Local(self.task_id.clone()),
            store: Rc::clone(&self.store),
            port: Arc::clone(&self.port),
            strings: self.strings.clone(),
            translator: self.translator.clone(),
        }
    }

    fn copy_error(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return;
        };
        let message = self.strings.task_error_text(&row.error_message).to_owned();
        drop(row);
        if message.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(message));
        self.flash_copied(cx, |this| &mut this.error_copied);
    }

    /// 日志页「复制全部」：按界面显示顺序（新→旧）导出已加载的全部条目。
    fn copy_log(&mut self, cx: &mut Context<Self>) {
        let mut text = String::new();
        for entry in self.activity.entries().rev() {
            let line = self.activity_line_text(entry, cx);
            text.push_str(&format_activity_timestamp(entry.timestamp_ms));
            text.push(' ');
            text.push_str(&line);
            text.push('\n');
        }
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.flash_copied(cx, |this| &mut this.log_copied);
    }

    /// 复制按钮勾选态：递增代次并在 1.5s 后回落；代次不符说明期间又点过，不提前复位。
    fn flash_copied(&mut self, cx: &mut Context<Self>, slot: fn(&mut Self) -> &mut u32) {
        let counter = slot(self);
        let generation = counter.wrapping_add(1).max(1);
        *counter = generation;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;

            let Ok(()) = this.update(cx, |this, cx| {
                let counter = slot(this);
                if *counter == generation {
                    *counter = 0;
                    cx.notify();
                }
            }) else {
                // 视图已释放，结束回调而不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 切换某个限制项的模式：非自定义时数字框回到 qBittorrent 默认值，自定义保留当前文本。
    fn set_seed_mode(
        &mut self,
        index: usize,
        mode: SeedMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.seed_modes[index] = mode;
        if mode != SeedMode::Custom {
            let input = [
                &self.seed_ratio,
                &self.seed_post_ratio,
                &self.seed_time_limit,
                &self.seed_inactive_limit,
            ][index];
            input.update(cx, |input, cx| {
                input.set_value(SEED_DEFAULT_TEXTS[index], window, cx);
            });
        }
        cx.notify();
    }

    fn save_seed_limits(&mut self, cx: &mut Context<Self>) {
        let baseline = self
            .seed_baseline
            .clone()
            .unwrap_or_else(SeedLimits::inherit_all);
        let values = [
            self.seed_ratio.read(cx).value(),
            self.seed_post_ratio.read(cx).value(),
            self.seed_time_limit.read(cx).value(),
            self.seed_inactive_limit.read(cx).value(),
            self.seed_upload_limit.read(cx).value(),
        ];
        let limits = seed_limits_from_form(
            self.seed_modes,
            [&values[0], &values[1], &values[2], &values[3], &values[4]],
            &baseline,
        );
        self.run_command(
            DownloadsCommand::SetSeedLimits {
                task_id: self.task_id.clone(),
                limits,
            },
            cx,
        );
    }

    /// 用任务当前的做种限制预填输入框（「跟随全局」显示为空）。限制值与上次预填来源不同
    /// （首次 / 切换任务 / 别处改动）才重填：无关的进度事件不会覆盖用户正在编辑的内容。
    fn sync_seed_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(limits) = self.current_dto().map(SeedLimits::from_dto) else {
            return;
        };
        if self.seed_baseline.as_ref() == Some(&limits) {
            return;
        }
        let inputs = [
            &self.seed_ratio,
            &self.seed_post_ratio,
            &self.seed_time_limit,
            &self.seed_inactive_limit,
            &self.seed_upload_limit,
        ];
        let (modes, texts) = seed_form_state(&limits);
        self.seed_modes = modes;
        for (input, text) in inputs.into_iter().zip(texts) {
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
        self.seed_baseline = Some(limits);
    }

    fn open_group(&mut self, group_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(opener) = self.host.open_group_window.clone() {
            opener(group_id, window, cx);
        }
    }

    // ---- rendering ----

    /// 插件 tab 内容：按 `(下标, 任务 id)` 缓存宿主工厂产出的视图，任务变化后重建。
    fn render_plugin_tab(
        &mut self,
        index: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(factory) = self.host.plugin_tab_view.clone() else {
            return div().into_any_element();
        };
        let Some(tab) = self.plugin_tabs.get(usize::from(index)).cloned() else {
            return div().into_any_element();
        };
        let key = (index, self.task_id.clone());
        let view = match self.plugin_views.get(&key) {
            Some(view) => view.clone(),
            None => {
                let view = factory(&tab, self.task_id.clone(), window, cx);
                self.plugin_views.insert(key, view.clone());
                view
            }
        };
        div().size_full().child(view).into_any_element()
    }
    /// 替换插件贡献的 tab；旧视图缓存随之作废。当前选中的插件 tab 消失时回到「常规」。
    pub fn set_plugin_tabs(&mut self, mut tabs: Vec<PluginTaskTab>, cx: &mut Context<Self>) {
        tabs.truncate(usize::from(u16::MAX));
        if self.plugin_tabs == tabs {
            return;
        }
        self.plugin_tabs = tabs;
        self.plugin_views.clear();
        if let DetailTab::Plugin(index) = self.tab
            && usize::from(index) >= self.plugin_tabs.len()
        {
            self.tab = DetailTab::General;
        }
        cx.notify();
    }

    fn tab_label(&self, tab: DetailTab, cx: &App) -> SharedString {
        let key = match tab {
            DetailTab::General => "detailTabGeneral",
            DetailTab::Files => "btTabFiles",
            DetailTab::Speed => "detailTabSpeed",
            DetailTab::Seeding => "tabSeeding",
            DetailTab::Trackers => "btTabTrackers",
            DetailTab::Peers => "btTabPeers",
            DetailTab::Ed2kSources => "ed2kTabSources",
            DetailTab::Ed2kNetwork => "ed2kTabNetwork",
            DetailTab::Log => "detailTabLog",
            DetailTab::Advanced => "detailTabAdvanced",
            DetailTab::Plugin(index) => {
                return self
                    .plugin_tabs
                    .get(usize::from(index))
                    .map_or_else(SharedString::default, |tab| tab.title.clone());
            }
        };
        self.t(cx, key)
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let tabs = visible_tabs(self.task_protocol(), self.plugin_tabs.len());
        let labels: Vec<SharedString> = tabs.iter().map(|tab| self.tab_label(*tab, cx)).collect();
        let selected = tabs.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        let this = cx.weak_entity();
        h_flex()
            .px(tokens.spacing.md)
            .pt(tokens.spacing.md)
            .child(segmented_tabs(
                "detail-tabs",
                labels,
                selected,
                move |index, _, cx| {
                    if let Some(tab) = tabs.get(index).copied() {
                        let Ok(()) = this.update(cx, |this, cx| this.select_tab(tab, cx)) else {
                            // 视图已释放，结束回调而不再更新状态。
                            return;
                        };
                    }
                },
                cx,
            ))
    }

    /// 独立窗口头部的任务名：窗口级标题字号。
    fn render_head_title(name: SharedString, cx: &App) -> Div {
        let title = active_theme(cx).extended().title;
        div()
            .min_w_0()
            .truncate()
            .text_size(title.size)
            .line_height(title.line_height)
            .font_weight(title.weight)
            .child(name)
    }

    fn render_completed_head(&self, row: &DownloadTaskView, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let size = if row.size_bytes > 0 {
            row.size.clone()
        } else {
            format_bytes(row.downloaded_bytes)
        };
        // 完成态不着色：状态只用中性三级文字表达（与主表格一致）。
        let meta = SharedString::from(format!(
            "{} · {size}",
            self.strings.state_label(TaskState::Completed)
        ));
        v_flex()
            .gap(tokens.spacing.md)
            .p(tokens.spacing.md)
            .border_b(extended.stroke.thin)
            .border_color(extended.colors.hairline)
            .child(
                h_flex()
                    .items_start()
                    .gap(tokens.spacing.sm)
                    .child(
                        div()
                            .flex_none()
                            .h(extended.title.line_height)
                            .flex()
                            .items_center()
                            .child(
                                Icon::new(FluxIcon::CircleCheck)
                                    .size(extended.icon.lg)
                                    .text_color(extended.colors.text_tertiary),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(tokens.spacing.xxs)
                            .child(Self::render_head_title(
                                SharedString::from(row.name.clone()),
                                cx,
                            ))
                            .child(
                                div()
                                    .text_size(tokens.typography.xs.size)
                                    .line_height(tokens.typography.xs.line_height)
                                    .font_features(tabular_numbers())
                                    .text_color(extended.colors.text_tertiary)
                                    .child(meta),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("detail-open-file")
                            .primary()
                            .control(cx)
                            .icon(FluxIcon::File)
                            .label(self.strings.open_file.clone())
                            .disabled(row.file_missing)
                            .on_click(cx.listener(|this, _, _, cx| this.open_file(cx))),
                    )
                    .child(
                        Button::new("detail-open-folder")
                            .outline()
                            .control(cx)
                            .icon(FluxIcon::FolderOpen)
                            .label(self.strings.open_folder.clone())
                            .on_click(cx.listener(|this, _, _, cx| this.reveal(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_progress_head(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return div().into_any_element();
        };
        if row.state == TaskState::Completed {
            return self.render_completed_head(&row, cx);
        }
        let downloading = row.state == TaskState::Downloading;
        let name = SharedString::from(row.name.clone());
        let progress_label = SharedString::from(row.progress_label.clone());
        let progress = row.progress;
        let speed = row
            .speed_bytes_per_second
            .filter(|_| downloading)
            .map(|speed| SharedString::from(format!("{}/s", format_bytes(speed))));
        let eta = row
            .eta_seconds
            .filter(|_| downloading)
            .map(|seconds| self.strings.format_eta(seconds));
        let size_label = SharedString::from(if row.size_bytes > 0 {
            format!("{} / {}", format_bytes(row.downloaded_bytes), row.size)
        } else {
            format_bytes(row.downloaded_bytes)
        });
        let active = matches!(row.state, TaskState::Downloading | TaskState::Pending);
        let bar_color = progress_bar_color(row.state, cx);
        let active_transfers = row.active_transfers().filter(|_| downloading).map(|count| {
            self.runtime
                .as_ref()
                .and_then(|runtime| runtime.parallelism_limit)
                .map_or_else(|| count.to_string(), |limit| format!("{count} / {limit}"))
        });
        let connected_peers = self
            .runtime
            .as_ref()
            .filter(|_| downloading && row.runtime_connected && self.is_bt())
            .and_then(|runtime| runtime.connected_peers);
        drop(row);

        v_flex()
            .gap(tokens.spacing.sm)
            .p(tokens.spacing.md)
            .border_b(extended.stroke.thin)
            .border_color(extended.colors.hairline)
            .child(Self::render_head_title(name, cx))
            .child(render_segment_progress(
                self.runtime.as_ref(),
                progress,
                (f32::from(window.viewport_size().width) - 2. * f32::from(tokens.spacing.md))
                    .max(0.),
                active_theme(cx).components().progress_height,
                active_theme(cx).components().progress_radius,
                bar_color,
                progress_track_color(cx),
            ))
            .child(
                h_flex()
                    .justify_between()
                    .gap(tokens.spacing.sm)
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .font_features(tabular_numbers())
                    .text_color(tokens.colors.muted_foreground)
                    .child(div().child(progress_label))
                    .child(div().flex_1().text_right().child(size_label))
                    .when_some(speed, |this, speed| {
                        this.child(div().text_color(tokens.colors.primary).child(speed))
                    })
                    .when_some(eta, |this, eta| this.child(div().child(eta))),
            )
            .when_some(active_transfers, |this, count| {
                this.child(detail_row(
                    self.t(cx, "detailActiveTransfers"),
                    SharedString::from(count),
                    cx,
                ))
            })
            .when_some(connected_peers, |this, count| {
                this.child(detail_row(
                    self.t(cx, "detailConnectedPeers"),
                    SharedString::from(count.to_string()),
                    cx,
                ))
            })
            .child(
                h_flex()
                    .items_center()
                    .gap(tokens.spacing.xs)
                    .child(
                        Button::new("detail-toggle-pause")
                            .ghost()
                            .icon(if active {
                                FluxIcon::Pause
                            } else {
                                FluxIcon::Play
                            })
                            .control_icon(cx)
                            .tooltip(if active {
                                self.strings.pause.clone()
                            } else {
                                self.strings.resume.clone()
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx))),
                    )
                    .child(
                        Button::new("detail-open-folder")
                            .ghost()
                            .icon(FluxIcon::FolderOpen)
                            .control_icon(cx)
                            .tooltip(self.strings.open_folder.clone())
                            .on_click(cx.listener(|this, _, _, cx| this.reveal(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_general(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return div().into_any_element();
        };
        let dto = self.current_dto();
        let follow_global = self.t(cx, "detailFollowGlobal");
        // 期望哈希只在填写过时展示（多数任务没有，「未设置」一行是噪音）。
        let checksum = dto
            .map(|dto| dto.checksum.clone())
            .filter(|value| !value.is_empty())
            .map(SharedString::from);
        let proxy = dto
            .map(|dto| dto.proxy_url.clone())
            .filter(|value| !value.is_empty())
            .map(SharedString::from)
            .unwrap_or_else(|| follow_global.clone());
        let ignore_tls = dto.is_some_and(|dto| dto.ignore_tls_errors);
        let route = dto
            .map(|dto| dto.auto_route.as_str())
            .filter(|route| !route.is_empty())
            .map(|route| route_label(route, self.translator.read(cx)));
        let queue_label = self
            .queue_label(&row.queue_id)
            .map(SharedString::from)
            .unwrap_or_else(|| SharedString::from(row.queue_id.clone()));
        let group_id = row.group_id.clone();
        let boosted = row.boosted;

        let mut list = v_flex()
            .child(
                v_flex()
                    .gap(tokens.spacing.xxs)
                    .pb(tokens.spacing.sm)
                    .when(self.mode == DetailMode::Docked, |this| {
                        this.child(
                            div()
                                .text_size(tokens.typography.sm.size)
                                .line_height(tokens.typography.sm.line_height)
                                .font_weight(FontWeight::MEDIUM)
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(row.name.clone())),
                        )
                    })
                    .child(self.render_source_line(&row, cx))
                    .when(boosted, |this| {
                        this.child(
                            div()
                                .text_size(tokens.typography.xs.size)
                                .line_height(tokens.typography.xs.line_height)
                                .text_color(extended.colors.warning)
                                .child(self.t(cx, "detailBoostActive")),
                        )
                    }),
            )
            .when(
                self.mode == DetailMode::Docked || row.state != TaskState::Completed,
                |this| {
                    this.child(detail_row(
                        self.t(cx, "infoStatus"),
                        div()
                            .text_color(task_status_color(&row, cx))
                            .child(self.strings.task_status_text(&row, &self.store)),
                        cx,
                    ))
                    .when(row.size_bytes > 0, |this| {
                        this.child(detail_row(
                            self.t(cx, "infoSize"),
                            SharedString::from(row.size.clone()),
                            cx,
                        ))
                    })
                },
            )
            .when(row.state != TaskState::Completed, |this| {
                this.child(detail_row(
                    self.t(cx, "infoDownloaded"),
                    SharedString::from(format_bytes(row.downloaded_bytes)),
                    cx,
                ))
            })
            .when(row.state == TaskState::Downloading, |this| {
                this.when_some(row.speed_bytes_per_second, |this, speed| {
                    this.child(detail_row(
                        self.t(cx, "infoSpeed"),
                        SharedString::from(format!("{}/s", format_bytes(speed))),
                        cx,
                    ))
                })
                .when_some(row.eta_seconds, |this, seconds| {
                    this.child(detail_row(
                        self.t(cx, "infoRemaining"),
                        self.strings.format_eta(seconds),
                        cx,
                    ))
                })
            })
            .when(row.created_at_secs > 0, |this| {
                this.child(detail_row(
                    self.t(cx, "infoStartedAt"),
                    DownloadStrings::format_datetime(row.created_at_secs),
                    cx,
                ))
            })
            .when(row.completed_at_secs > 0, |this| {
                this.child(detail_row(
                    self.t(cx, "infoCompletedAt"),
                    DownloadStrings::format_datetime(row.completed_at_secs),
                    cx,
                ))
            })
            .child(
                div()
                    .id("detail-reveal-row")
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.reveal(cx)))
                    .child(detail_row(
                        self.t(cx, "infoPath"),
                        div()
                            .truncate()
                            .text_color(tokens.colors.primary)
                            .child(SharedString::from(row.save_dir.clone())),
                        cx,
                    )),
            )
            .child(detail_row(self.t(cx, "taskQueueLabel"), queue_label, cx))
            .when_some(checksum, |this, checksum| {
                this.child(detail_row(self.t(cx, "taskChecksum"), checksum, cx))
            })
            .when_some(self.bt_info_hash_v2(), |this, hash| {
                this.child(detail_row(
                    self.t(cx, "btInfoHashV2"),
                    self.render_info_hash_v2(hash, cx),
                    cx,
                ))
            })
            .when(row.state == TaskState::Completed, |this| {
                this.child(self.render_file_hash_row(&row, cx))
            })
            .child(detail_row(self.t(cx, "taskProxy"), proxy, cx));
        if let Some(route) = route {
            list = list.child(detail_row(self.t(cx, "taskRoute"), route, cx));
        }

        if ignore_tls {
            list = list.child(detail_row(
                self.t(cx, "taskIgnoreTlsErrors"),
                SharedString::from("✓"),
                cx,
            ));
        }
        if !row.error_message.is_empty() {
            let copied = self.error_copied != 0;
            let success = active_theme(cx).extended().colors.success;
            let tooltip = self.t(
                cx,
                if copied {
                    "detailErrorCopied"
                } else {
                    "detailCopyError"
                },
            );
            list = list.child(detail_row(
                self.t(cx, "infoError"),
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap(tokens.spacing.sm)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(tokens.colors.destructive)
                            .child(SharedString::from(
                                self.strings.task_error_text(&row.error_message).to_owned(),
                            )),
                    )
                    .child(
                        Button::new("detail-copy-error")
                            .ghost()
                            .control_icon(cx)
                            .icon(if copied {
                                FluxIcon::Check
                            } else {
                                FluxIcon::Copy
                            })
                            .when(copied, |this| this.text_color(success))
                            .tooltip(tooltip)
                            .on_click(cx.listener(|this, _, _, cx| this.copy_error(cx))),
                    ),
                cx,
            ));
        }
        drop(row);
        if !group_id.is_empty() {
            list = list.child(detail_row(
                self.t(cx, "groupMemberOfLabel"),
                Button::new("detail-open-group")
                    .ghost()
                    .control(cx)
                    .label(self.strings.open_group_in_window.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_group(group_id.clone(), window, cx);
                    })),
                cx,
            ));
        }
        let facts = task_menu_facts_for(&self.store, &RowKey::Local(self.task_id.clone()));
        let can_change_url = facts.is_some_and(|facts| facts.can_change_url());
        let can_change_segments = facts.is_some_and(|facts| facts.can_change_segments());
        let list = list.child(
            h_flex()
                .flex_wrap()
                .gap(tokens.spacing.sm)
                .pt(tokens.spacing.md)
                .child(
                    Button::new("detail-copy-link")
                        .outline()
                        .control(cx)
                        .icon(FluxIcon::Copy)
                        .label(self.strings.copy_url.clone())
                        .on_click(cx.listener(|this, _, window, cx| this.copy_link(window, cx))),
                )
                .when(can_change_url, |this| {
                    this.child(
                        Button::new("detail-change-url")
                            .outline()
                            .control(cx)
                            .icon(FluxIcon::Globe)
                            .label(self.strings.change_url.clone())
                            .on_click(cx.listener(|this, _, window, cx| {
                                open_change_url_dialog(this.edit_target(), window, cx);
                            })),
                    )
                })
                .when(can_change_segments, |this| {
                    this.child(
                        Button::new("detail-change-segments")
                            .outline()
                            .control(cx)
                            .icon(FluxIcon::Layers)
                            .label(self.strings.edit_threads.clone())
                            .on_click(cx.listener(|this, _, window, cx| {
                                open_change_segments_dialog(this.edit_target(), window, cx);
                            })),
                    )
                }),
        );
        // 响应式：宽度够（横向底部面板 / 宽窗口）时信息列与来源构成并排，
        // 窄（右侧面板）时来源构成自动换到信息列下方——按实际可用宽度折行，
        // 与停靠位置无关，独立任务窗口同样适用。
        h_flex()
            // wrap-reverse：折行时来源构成排到信息列上方（窄面板先看图表）；
            // 反向交叉轴下 items_end 即顶端对齐，并排时两列仍顶对齐。
            .flex_wrap_reverse()
            .items_end()
            .gap_x(tokens.spacing.xl)
            .gap_y(tokens.spacing.lg)
            .child(
                list.flex_grow_1()
                    .flex_shrink_1()
                    .flex_basis(active_theme(cx).text_extent(GENERAL_INFO_MIN_WIDTH))
                    .min_w_0(),
            )
            .children(self.render_sources_section(cx))
            .into_any_element()
    }

    /// 「速度」页：当前 / 平均 / 峰值 / 下载时长四块统计 + 全生命周期面积图（daemon 持久化数据）+ 明细行。
    fn render_speed(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return div().into_any_element();
        };
        let downloading = row.state == TaskState::Downloading;
        let current = row
            .speed_bytes_per_second
            .filter(|_| downloading)
            .unwrap_or(0);
        let eta = row
            .eta_seconds
            .filter(|_| downloading)
            .map(|seconds| self.strings.format_eta(seconds));
        let active_transfers = row.active_transfers().filter(|_| downloading).map(|count| {
            self.runtime
                .as_ref()
                .and_then(|runtime| runtime.parallelism_limit)
                .map_or_else(|| count.to_string(), |limit| format!("{count} / {limit}"))
        });
        let connected_peers = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.connected_peers);
        drop(row);

        let (average, peak, active_secs, chart_points) = match &self.speed_stats {
            Some(stats) => (
                u64::try_from(stats.average_bps()).unwrap_or(0),
                u64::try_from(stats.peak_bps).unwrap_or(0),
                stats.active_secs,
                speed_chart_points(stats),
            ),
            None => (0, 0, 0, Vec::new()),
        };
        let has_chart = chart_points.len() > 1 && peak > 0;
        let duration = self.strings.format_eta(active_secs.max(0) as u64);

        let tile = |label: SharedString, value: SharedString, color: Hsla| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(tokens.spacing.xs)
                .p(tokens.spacing.sm)
                .rounded(tokens.radius.sm)
                .border_1()
                .border_color(extended.colors.hairline)
                .child(
                    div()
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .text_color(tokens.colors.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_size(tokens.typography.sm.size)
                        .line_height(tokens.typography.sm.line_height)
                        .font_weight(FontWeight::MEDIUM)
                        .font_features(tabular_numbers())
                        .text_color(color)
                        .child(value),
                )
        };

        v_flex()
            .gap(tokens.spacing.md)
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .child(tile(
                        self.t(cx, "infoSpeed"),
                        rate_text(current),
                        tokens.colors.primary,
                    ))
                    .child(tile(
                        self.t(cx, "speedAverage"),
                        rate_text(average),
                        tokens.colors.foreground,
                    ))
                    .child(tile(
                        self.t(cx, "speedPeak"),
                        rate_text(peak),
                        tokens.colors.foreground,
                    ))
                    .child(tile(
                        self.t(cx, "speedActiveTime"),
                        duration.clone(),
                        tokens.colors.foreground,
                    )),
            )
            .child(if has_chart {
                v_flex()
                    .w_full()
                    .child(
                        div().h(px(120.)).w_full().child(
                            AreaChart::new(chart_points)
                                .x(|point: &(SharedString, f64)| point.0.clone())
                                .y(|point: &(SharedString, f64)| point.1)
                                .stroke(tokens.colors.primary)
                                .name(self.t(cx, "infoSpeed"))
                                .tooltip_value(|_, _, value| rate_text(value.max(0.) as u64))
                                .y_axis(true)
                                .y_axis_label_placement(
                                    gpui_component::plot::AxisLabelPlacement::Inside,
                                )
                                .y_tick_count(3)
                                .y_tick_format(|value| rate_text(value.max(0.) as u64))
                                .linear()
                                .x_axis(false)
                                .grid(false),
                        ),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .text_size(tokens.typography.xs.size)
                            .line_height(tokens.typography.xs.line_height)
                            .text_color(tokens.colors.muted_foreground)
                            .child(self.t(cx, "speedAxisStart"))
                            .child(duration),
                    )
                    .into_any_element()
            } else {
                h_flex()
                    .h(px(120.))
                    .w_full()
                    .items_center()
                    .justify_center()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .text_color(tokens.colors.muted_foreground)
                    .child(self.t(cx, "speedChartEmpty"))
                    .into_any_element()
            })
            .child(
                v_flex()
                    .when_some(eta, |this, eta| {
                        this.child(detail_row(self.t(cx, "infoRemaining"), eta, cx))
                    })
                    .when_some(active_transfers, |this, count| {
                        this.child(detail_row(
                            self.t(cx, "detailActiveTransfers"),
                            SharedString::from(count),
                            cx,
                        ))
                    })
                    .when_some(connected_peers.filter(|_| self.is_bt()), |this, count| {
                        this.child(detail_row(
                            self.t(cx, "detailConnectedPeers"),
                            SharedString::from(count.to_string()),
                            cx,
                        ))
                    }),
            )
            .into_any_element()
    }

    /// 常规页的「来源构成」区块：环形图 + 图例 + 一行摘要；尚无已下载字节时不渲染。
    fn render_sources_section(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let row = self.store.get(&RowKey::Local(self.task_id.clone()))?;
        let protocol = row.protocol;
        let downloaded = row.downloaded_bytes;
        drop(row);
        let bytes = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.source_bytes)
            .or_else(|| self.current_dto().map(|dto| dto.source_bytes))
            .unwrap_or_default();
        let composition = compose(
            protocol,
            i64::try_from(downloaded).unwrap_or(i64::MAX),
            bytes.cdn_bytes,
            bytes.proxy_bytes,
            bytes.nic_bytes,
        );
        if composition.is_empty() {
            return None;
        }

        // 主题没有第四种强调色：多网卡取 `info` 色相 +60°（蓝→紫），明度/饱和度
        // 沿用主题值，亮暗主题都与源站的主色蓝区分开。
        let nic_color = Hsla {
            h: (extended.colors.info.h + 60. / 360.).fract(),
            ..extended.colors.info
        };
        let color_of = |kind: SourceKind| -> Hsla {
            match kind {
                SourceKind::Origin | SourceKind::P2p => tokens.colors.primary,
                SourceKind::Cdn => extended.colors.success,
                SourceKind::Proxy => extended.colors.warning,
                SourceKind::Nic => nic_color,
            }
        };
        let arcs: Vec<(Hsla, f32)> = composition
            .slices
            .iter()
            .filter(|slice| slice.bytes > 0)
            .map(|slice| (color_of(slice.kind), slice.bytes as f32))
            .collect();
        // 单切片（100%）不留分隔缝，否则整环出现一道缺口。
        let pad_angle = if arcs.len() > 1 { 0.02 } else { 0. };
        // 环形图随文字缩放：中心的「已下载」与字节数需要装得下内圈（默认倍率 1，尺寸不变）。
        let donut_scale = f32::from(active_theme(cx).text_extent(1.));
        let donut = div()
            .relative()
            .flex_none()
            .size(active_theme(cx).text_extent(140.))
            .child(
                PieChart::new(arcs)
                    .value(|arc: &(Hsla, f32)| arc.1)
                    .color(|arc: &(Hsla, f32)| arc.0)
                    .outer_radius(66. * donut_scale)
                    .inner_radius(46. * donut_scale)
                    .pad_angle(pad_angle)
                    .interactive(false),
            )
            .child(
                v_flex()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_size(tokens.typography.xs.size)
                            .line_height(tokens.typography.xs.line_height)
                            .text_color(tokens.colors.muted_foreground)
                            .child(self.t(cx, "infoDownloaded")),
                    )
                    .child(
                        div()
                            .text_size(tokens.typography.sm.size)
                            .line_height(tokens.typography.sm.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .font_features(tabular_numbers())
                            .text_color(tokens.colors.foreground)
                            .child(SharedString::from(format_bytes(composition.downloaded))),
                    ),
            );

        let legend =
            v_flex()
                .flex_1()
                .min_w(active_theme(cx).text_extent(180.))
                .children(composition.slices.iter().map(|slice| {
                    h_flex()
                        .items_center()
                        .gap(tokens.spacing.sm)
                        .py(tokens.spacing.xs)
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .font_features(tabular_numbers())
                        .child(
                            div()
                                .flex_none()
                                .size(px(8.))
                                .rounded_full()
                                .bg(color_of(slice.kind)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(tokens.colors.foreground)
                                .truncate()
                                .child(self.t(cx, slice.kind.i18n_key())),
                        )
                        .child(div().text_color(tokens.colors.foreground).child(
                            SharedString::from(format_percent(composition.fraction(slice.bytes))),
                        ))
                        .child(
                            div()
                                .min_w(active_theme(cx).text_extent(64.))
                                .text_right()
                                .text_color(tokens.colors.muted_foreground)
                                .child(SharedString::from(format_bytes(slice.bytes))),
                        )
                }));

        let summary = if composition.p2p {
            self.t(cx, "sourcesP2pHint")
        } else if composition.accelerated_bytes > 0 {
            let percent = format_percent(composition.accelerated_share());
            SharedString::from(
                self.translator
                    .read(cx)
                    .text_with("sourcesAccelShare", &[("percent", &percent)]),
            )
        } else {
            self.t(cx, "sourcesNoAccel")
        };

        Some(
            v_flex()
                .flex_none()
                .w(active_theme(cx).text_extent(SOURCES_SECTION_WIDTH))
                .max_w_full()
                .gap(tokens.spacing.sm)
                .child(
                    div()
                        .text_size(extended.caption.size)
                        .line_height(extended.caption.line_height)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(extended.colors.text_tertiary)
                        .child(self.t(cx, "detailSourcesTitle")),
                )
                .child(
                    h_flex()
                        .flex_wrap()
                        .items_center()
                        .gap(tokens.spacing.md)
                        .child(donut)
                        .child(legend),
                )
                .child(
                    div()
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .text_color(tokens.colors.muted_foreground)
                        .child(summary),
                )
                .into_any_element(),
        )
    }

    fn render_seeding(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return div().into_any_element();
        };
        let uploaded_bytes = row.uploaded_bytes.max(0) as u64;
        let size_bytes = row.size_bytes;
        let seeding_status = row.seeding_status;
        drop(row);
        let seeding_time_secs = self
            .current_dto()
            .map(|dto| dto.seeding_time_secs)
            .unwrap_or(0);
        let status_label = self.t(cx, seeding_status_key(seeding_status));
        let ratio = if size_bytes > 0 {
            format!("{:.2}", uploaded_bytes as f64 / size_bytes as f64)
        } else {
            "—".to_owned()
        };
        let duration = format_duration(self.translator.read(cx), seeding_time_secs);
        // 做种页曲线画上传速度：上传一律用成功（绿）色，下载保持主色。
        let chart_points: Vec<(usize, f64)> = self
            .speed_history
            .iter()
            .enumerate()
            .map(|(index, (_, speed))| (index, *speed as f64))
            .collect();
        let has_chart = chart_points.len() > 1;

        v_flex()
            .child(detail_row(self.t(cx, "seedingStatus"), status_label, cx))
            .child(detail_row(
                self.t(cx, "uploadedTotal"),
                SharedString::from(format_bytes(uploaded_bytes)),
                cx,
            ))
            .child(detail_row(
                self.t(cx, "seedRatio"),
                SharedString::from(ratio),
                cx,
            ))
            .child(detail_row(self.t(cx, "seedTime"), duration, cx))
            .when(has_chart, |this| {
                this.child(
                    div().h(px(64.)).w_full().pt(tokens.spacing.sm).child(
                        AreaChart::new(chart_points)
                            .x(|point: &(usize, f64)| SharedString::from(point.0.to_string()))
                            .y(|point: &(usize, f64)| point.1)
                            .stroke(extended.colors.success)
                            .tooltip_content(|point: &(usize, f64), _, _| {
                                div().child(rate_text(point.1.max(0.) as u64))
                            })
                            .linear()
                            .x_axis(false)
                            .grid(false),
                    ),
                )
            })
            .child(
                div()
                    .h(extended.stroke.thin)
                    .my(tokens.spacing.md)
                    .bg(extended.colors.hairline),
            )
            .child(
                div()
                    .pb(tokens.spacing.md)
                    .text_size(extended.caption.size)
                    .line_height(extended.caption.line_height)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(extended.colors.text_tertiary)
                    .child(self.t(cx, "btSeedLimitsTitle")),
            )
            .child(
                form(cx)
                    .child(self.render_seed_limit_field(
                        cx,
                        0,
                        "btSeedRatioLimit",
                        &self.seed_ratio,
                        "btSeedRatioUnit",
                    ))
                    .child(self.render_seed_limit_field(
                        cx,
                        1,
                        "btSeedPostRatioLimit",
                        &self.seed_post_ratio,
                        "btSeedRatioUnit",
                    ))
                    .child(self.render_seed_limit_field(
                        cx,
                        2,
                        "btSeedTimeLimit",
                        &self.seed_time_limit,
                        "timeUnitMinutes",
                    ))
                    .child(self.render_seed_limit_field(
                        cx,
                        3,
                        "btSeedInactiveTimeLimit",
                        &self.seed_inactive_limit,
                        "timeUnitMinutes",
                    ))
                    .child(self.render_seed_upload_field(cx))
                    .child(
                        h_flex().justify_end().child(
                            Button::new("detail-save-seed-limits")
                                .primary()
                                .label(self.strings.confirm.clone())
                                .control(cx)
                                .on_click(cx.listener(|this, _, _, cx| this.save_seed_limits(cx))),
                        ),
                    ),
            )
            .into_any_element()
    }

    /// 做种限制字段：标签 → 模式下拉 + 数字输入（仅自定义模式可编辑）+ 数字右侧的单位。
    fn render_seed_limit_field(
        &self,
        cx: &mut Context<Self>,
        index: usize,
        label_key: &str,
        state: &Entity<InputState>,
        unit_key: &str,
    ) -> AnyElement {
        let mode = self.seed_modes[index];
        let options: Vec<(SeedMode, SharedString)> = SeedMode::ALL
            .into_iter()
            .map(|option| (option, self.t(cx, option.label_key())))
            .collect();
        let label = self.t(cx, mode.label_key());
        let this = cx.weak_entity();
        let dropdown = Button::new(SharedString::from(format!("detail-seed-mode-{index}")))
            .outline()
            .control(cx)
            .label(label)
            .dropdown_caret(true)
            .w_full()
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                options.iter().fold(menu, |menu, (option, label)| {
                    let option = *option;
                    let this = this.clone();
                    menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(option == mode)
                            .on_click(move |_, window, cx| {
                                let Ok(()) = this.update(cx, |this, cx| {
                                    this.set_seed_mode(index, option, window, cx);
                                }) else {
                                    // 视图已释放，结束这次回调而不再更新状态。
                                    return;
                                };
                            }),
                    )
                })
            });
        let extended = active_theme(cx).extended().clone();
        form_field(
            self.t(cx, label_key),
            // 详情面板很窄：每项独占一行，下拉占一半，数字框 + 单位占另一半，避免挤压重叠。
            h_flex()
                .w_full()
                .gap(px(8.))
                .child(div().flex_1().min_w_0().child(dropdown))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(6.))
                        .items_center()
                        .child(
                            div().flex_1().min_w_0().child(
                                NumberInput::new(state)
                                    .control(cx)
                                    .w_full()
                                    .disabled(mode != SeedMode::Custom),
                            ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(extended.caption.size)
                                .text_color(extended.colors.text_tertiary)
                                .child(self.t(cx, unit_key)),
                        ),
                ),
            None,
            cx,
        )
        .into_any_element()
    }

    /// 上传限速字段（KB/s）：始终可编辑的数字输入，0 = 不限速。
    fn render_seed_upload_field(&self, cx: &mut Context<Self>) -> AnyElement {
        form_field(
            self.t(cx, "btSeedUploadLimit"),
            NumberInput::new(&self.seed_upload_limit)
                .control(cx)
                .w_full(),
            Some(self.t(cx, "btSeedUploadLimitHint")),
            cx,
        )
        .into_any_element()
    }

    fn render_log(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let (oldest, newest, truncated) = self.activity.retained_range();
        let note = |text: SharedString, color: Hsla| {
            div()
                .text_size(tokens.typography.xs.size)
                .line_height(tokens.typography.xs.line_height)
                .text_color(color)
                .child(text)
        };
        let has_entries = self.activity.entries().next().is_some();
        let copied = self.log_copied != 0;
        let copy_label = self.t(
            cx,
            if copied {
                "detailLogCopied"
            } else {
                "detailCopyLog"
            },
        );
        let mut content = v_flex().gap(tokens.spacing.xs).child(
            h_flex()
                .items_start()
                .gap(tokens.spacing.sm)
                .pb(tokens.spacing.xs)
                .child(div().flex_1().min_w_0().child(note(
                    self.t(cx, "detailLogHint"),
                    tokens.colors.muted_foreground,
                )))
                .child(
                    Button::new("detail-copy-log")
                        .ghost()
                        .control(cx)
                        .icon(if copied {
                            FluxIcon::Check
                        } else {
                            FluxIcon::Copy
                        })
                        .label(copy_label)
                        .when(copied, |this| this.text_color(extended.colors.success))
                        .disabled(!has_entries)
                        .on_click(cx.listener(|this, _, _, cx| this.copy_log(cx))),
                ),
        );
        if truncated {
            content = content.child(note(
                self.t(cx, "detailActivityTruncated"),
                extended.colors.warning,
            ));
        }
        if self.activity.has_journal_gap() {
            content = content.child(note(
                self.t(cx, "detailActivityJournalGap"),
                extended.colors.warning,
            ));
        }
        if let (Some(oldest), Some(newest)) = (oldest, newest) {
            content = content.child(
                note(
                    SharedString::from(self.translator.read(cx).text_with(
                        "detailActivityRetainedRange",
                        &[
                            ("oldest", &format!("#{oldest}")),
                            ("newest", &format!("#{newest}")),
                        ],
                    )),
                    tokens.colors.muted_foreground,
                )
                .font_features(tabular_numbers()),
            );
        }
        if self.activity.failed() {
            content = content.child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .items_center()
                    .child(note(
                        self.t(cx, "detailActivityQueryFailed"),
                        tokens.colors.destructive,
                    ))
                    .child(
                        Button::new("detail-activity-retry")
                            .outline()
                            .control(cx)
                            .label(self.t(cx, "detailActivityRetry"))
                            .on_click(cx.listener(|this, _, _, cx| this.retry_activity(cx))),
                    ),
            );
        }
        if self.activity.is_loading() {
            content = content.child(note(
                self.t(cx, "detailActivityLoading"),
                tokens.colors.muted_foreground,
            ));
        }
        if self.activity.loaded() && self.activity.entries().next().is_none() {
            content = content.child(note(
                self.t(cx, "detailLogEmpty"),
                tokens.colors.muted_foreground,
            ));
        }
        let mut list = v_flex()
            .rounded(tokens.radius.sm)
            .border_1()
            .border_color(extended.colors.hairline)
            .overflow_hidden();
        let total = self.activity.entries().count();
        for (index, entry) in self.activity.entries().rev().enumerate() {
            let text = self.activity_line_text(entry, cx);
            list = list.child(
                v_flex()
                    .px(tokens.spacing.sm)
                    .py(tokens.spacing.xs)
                    .gap(px(2.))
                    .when(index + 1 < total, |this| {
                        this.border_b(extended.stroke.thin)
                            .border_color(extended.colors.hairline)
                    })
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .child(
                        div()
                            .w(active_theme(cx).text_extent(ACTIVITY_TIME_WIDTH))
                            .font_features(tabular_numbers())
                            .text_color(extended.colors.text_tertiary)
                            .child(format_activity_timestamp(entry.timestamp_ms)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_color(if entry.kind == "journal_overflow" {
                                extended.colors.warning
                            } else {
                                tokens.colors.foreground
                            })
                            .child(text),
                    ),
            );
        }
        if has_entries {
            content = content.child(list);
        }
        if self.activity.has_older() && !self.activity.is_loading() && !self.activity.failed() {
            content = content.child(
                div().pt(tokens.spacing.xs).child(
                    Button::new("detail-activity-load-older")
                        .ghost()
                        .control(cx)
                        .label(self.t(cx, "detailActivityLoadMore"))
                        .on_click(cx.listener(|this, _, _, cx| this.load_older_activity(cx))),
                ),
            );
        }
        content.into_any_element()
    }

    /// 单条活动的显示文本（界面与「复制全部」共用）。
    fn activity_line_text(&self, entry: &fluxdown_protocol::TaskActivityDto, cx: &App) -> String {
        let kind_key = activity_kind_key(&entry.kind);
        let label = self.t(cx, kind_key);
        let state = if entry.kind == "status" {
            entry
                .status
                .map(|status| self.strings.status_code_label(status))
        } else {
            None
        };
        if let Some(state) = state {
            format!("{label}: {state}")
        } else if kind_key == "detailActivityKindUnknown" {
            if entry.message.is_empty() {
                format!("{label} ({})", entry.kind)
            } else {
                format!("{label} ({}): {}", entry.kind, entry.message)
            }
        } else if entry.message.is_empty() {
            label.to_string()
        } else {
            format!("{label}: {}", entry.message)
        }
    }

    fn render_advanced(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.store.get(&RowKey::Local(self.task_id.clone())) else {
            return div().into_any_element();
        };
        let dto = self.current_dto();
        let referrer = SharedString::from(row.referrer.clone());
        let referrer_is_web = row.source_page_url().is_some();
        let proxy = dto
            .map(|dto| dto.proxy_url.clone())
            .filter(|value| !value.is_empty())
            .map(SharedString::from)
            .unwrap_or_else(|| self.t(cx, "detailFollowGlobal"));
        let ignore_tls = dto.is_some_and(|dto| dto.ignore_tls_errors);
        let route = dto
            .map(|dto| dto.auto_route.as_str())
            .filter(|route| !route.is_empty())
            .map(|route| route_label(route, self.translator.read(cx)));
        drop(row);
        let mut list = v_flex()
            .child(self.render_source_page_row(referrer, referrer_is_web, cx))
            .child(detail_row(self.t(cx, "taskProxy"), proxy, cx));
        if let Some(route) = route {
            list = list.child(detail_row(self.t(cx, "taskRoute"), route, cx));
        }
        list.child(detail_row(
            self.t(cx, "taskIgnoreTlsErrors"),
            SharedString::from(if ignore_tls { "✓" } else { "—" }),
            cx,
        ))
        .into_any_element()
    }
}

impl gpui::Render for TaskDetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let has_row = self
            .store
            .get(&RowKey::Local(self.task_id.clone()))
            .is_some();
        if !has_row {
            // 空状态：32px 三级文字图标 + sm MEDIUM 说明，居中。
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(tokens.spacing.sm)
                .bg(tokens.colors.surface)
                .child(
                    Icon::new(FluxIcon::FileText)
                        .size(px(EMPTY_ICON_SIZE))
                        .text_color(extended.colors.text_tertiary),
                )
                .child(
                    div()
                        .text_size(tokens.typography.sm.size)
                        .line_height(tokens.typography.sm.line_height)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tokens.colors.muted_foreground)
                        .child(self.t(cx, "selectTaskHint")),
                );
        }
        if self.tab == DetailTab::Seeding {
            self.sync_seed_inputs(window, cx);
        }
        if self.bt_tab_active() {
            self.bt_touch(cx);
        }
        if self.tab == DetailTab::Files {
            self.bt_files_touch(cx);
        }
        if self.ed2k_tab_active() {
            self.ed2k_touch(cx);
        }
        let body = match self.tab {
            DetailTab::General => self.render_general(cx),
            DetailTab::Files => self.render_bt_files(cx),
            DetailTab::Speed => self.render_speed(cx),
            DetailTab::Seeding => self.render_seeding(cx),
            DetailTab::Trackers => self.render_bt_trackers(window, cx),
            DetailTab::Peers => self.render_bt_peers(window, cx),
            DetailTab::Ed2kSources => self.render_ed2k_sources(window, cx),
            DetailTab::Ed2kNetwork => self.render_ed2k_network(window, cx),
            DetailTab::Log => self.render_log(cx),
            DetailTab::Advanced => self.render_advanced(cx),
            DetailTab::Plugin(index) => self.render_plugin_tab(index, window, cx),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .bg(tokens.colors.surface)
            .when(self.mode == DetailMode::Window, |this| {
                this.child(self.render_progress_head(window, cx))
            })
            .when_some(self.last_error.clone(), |this, error| {
                this.child(
                    div()
                        .px(tokens.spacing.md)
                        .pt(tokens.spacing.sm)
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .text_color(tokens.colors.destructive)
                        .child(error),
                )
            })
            .child(self.render_tab_bar(cx))
            .child(
                div()
                    .id("task-detail-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(tokens.spacing.md)
                    .child(body),
            )
    }
}

/// 详情键值表的一行：左列标签（xs、三级文字，固定宽度），右列值（sm、正文色、等宽数字）。
/// 标签沿用正文行高，使两列首行垂直居中对齐；值过长时在值列内换行而不挤压标签。
/// 任务详情（常规 / 做种 / 高级）与任务组概览共用，保证所有详情窗口同一种键值排版。
/// 标签通常是文字；「文件哈希」行把算法下拉放在标签位。
pub(crate) fn detail_row(label: impl IntoElement, value: impl IntoElement, cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    h_flex()
        .items_center()
        .gap(tokens.spacing.md)
        .min_h(theme.density().control)
        .py(tokens.spacing.xxs)
        .child(
            div()
                .flex_none()
                .w(theme.text_extent(DETAIL_LABEL_WIDTH))
                .text_size(tokens.typography.xs.size)
                .line_height(tokens.typography.sm.line_height)
                .text_color(theme.extended().colors.text_tertiary)
                .child(label),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .text_size(tokens.typography.sm.size)
                .line_height(tokens.typography.sm.line_height)
                .text_color(tokens.colors.foreground)
                .font_features(tabular_numbers())
                .child(value),
        )
}

fn push_bounded<T>(queue: &mut VecDeque<T>, item: T, capacity: usize) {
    queue.push_back(item);
    while queue.len() > capacity {
        queue.pop_front();
    }
}

fn format_activity_timestamp(timestamp_ms: i64) -> SharedString {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map_or_else(
            || SharedString::from("—"),
            |at| SharedString::from(at.format("%Y-%m-%d %H:%M:%S%.3f").to_string()),
        )
}

fn activity_kind_key(kind: &str) -> &'static str {
    match kind {
        "status" => "detailActivityKindStatus",
        "error" => "detailActivityKindError",
        "split" => "detailActivityKindSplit",
        "cdn_pool" => "detailActivityKindCdnPool",
        "cdn_kick" => "detailActivityKindCdnKick",
        "cdn_breaker" => "detailActivityKindCdnBreaker",
        "cdn_fallback" => "detailActivityKindCdnFallback",
        "cdn_summary" => "detailActivityKindCdnSummary",
        "nic_links" => "detailActivityKindNicLinks",
        "nic_off" => "detailActivityKindNicOff",
        "retry" => "detailActivityKindRetry",
        "journal_overflow" => "detailActivityKindJournalOverflow",
        "plugin" => "detailActivityKindPlugin",
        _ => "detailActivityKindUnknown",
    }
}

/// 做种限制的三态：跟随全局 / 不限制 / 自定义（对应线上哨兵 -2 / -1 或 0 / 正值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeedMode {
    Global,
    Unlimited,
    Custom,
}

impl SeedMode {
    const ALL: [Self; 3] = [Self::Global, Self::Unlimited, Self::Custom];

    /// 线上限制值 → 模式；引擎把 0 也视为不限制。
    fn of(value: i64) -> Self {
        if value == SEED_LIMIT_FOLLOW_GLOBAL {
            Self::Global
        } else if value > 0 {
            Self::Custom
        } else {
            Self::Unlimited
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Global => "btSeedLimitsModeGlobal",
            Self::Unlimited => "btSeedLimitsModeUnlimited",
            Self::Custom => "btSeedLimitsModeCustom",
        }
    }
}

/// 非自定义模式下数字框展示的 qBittorrent 默认值：总分享率、做种后分享率、做种时长（分钟）、
/// 不活跃时长（分钟）、上传限速（KB/s，0 = 不限）。
const SEED_DEFAULT_TEXTS: [&str; 5] = ["1.00", "1.00", "1440", "1440", "0"];

/// 千分比分享率 → 两位小数文本（1500 → "1.50"）。
fn format_seed_ratio(milli: i64) -> String {
    let hundredths = milli.saturating_add(5) / 10;
    format!("{}.{:02}", hundredths / 100, hundredths % 100)
}

/// 小数分享率文本 → 千分比；必须是有限正数且换算后至少 1。
fn parse_seed_ratio(text: &str) -> Option<i64> {
    let value: f64 = text.trim().parse().ok()?;
    let milli = (value * 1000.0).round();
    if !value.is_finite() || !(1.0..=1e12).contains(&milli) {
        return None;
    }
    Some(milli as i64)
}

/// 时长文本（分钟）→ 正整数。
fn parse_seed_minutes(text: &str) -> Option<i64> {
    text.trim().parse::<i64>().ok().filter(|value| *value > 0)
}

/// 上传限速文本（KB/s）→ 字节/秒；非负整数，空 = 0（不限）。
fn parse_seed_upload_limit(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    text.parse::<i64>()
        .ok()
        .filter(|kbps| *kbps >= 0)
        .map(|kbps| kbps.saturating_mul(1024))
}

/// 任务限制 → 表单状态：四个限制项的模式 + 五个数字框文本。自定义显示已存值，其余显示
/// qBittorrent 默认值；上传限速按 KB/s 向上取整显示（0 = 不限）。
fn seed_form_state(limits: &SeedLimits) -> ([SeedMode; 4], [String; 5]) {
    let values = [
        limits.ratio_limit_milli,
        limits.post_ratio_limit_milli,
        limits.seed_time_limit_minutes,
        limits.inactive_time_limit_minutes,
    ];
    let modes = values.map(SeedMode::of);
    let texts = std::array::from_fn(|index| {
        if index == 4 {
            return if limits.upload_limit_bps > 0 {
                (limits.upload_limit_bps.saturating_add(1023) / 1024).to_string()
            } else {
                SEED_DEFAULT_TEXTS[4].to_owned()
            };
        }
        match (modes[index], index) {
            (SeedMode::Custom, 0 | 1) => format_seed_ratio(values[index]),
            (SeedMode::Custom, _) => values[index].to_string(),
            _ => SEED_DEFAULT_TEXTS[index].to_owned(),
        }
    });
    (modes, texts)
}

/// 表单（四个模式 + 五个数字框文本）→ 保存的限制值。
/// 全局 → -2，不限制 → -1，自定义 → 解析值。模式与文本都未改动的项保持 `baseline` 原值
/// （含 0 视同不限制、上传限速不足 1 KB/s 的零头）；自定义文本非法的项同样保持原值。
fn seed_limits_from_form(
    modes: [SeedMode; 4],
    texts: [&str; 5],
    baseline: &SeedLimits,
) -> SeedLimits {
    let (base_modes, original) = seed_form_state(baseline);
    let base_values = [
        baseline.ratio_limit_milli,
        baseline.post_ratio_limit_milli,
        baseline.seed_time_limit_minutes,
        baseline.inactive_time_limit_minutes,
    ];
    let limit = |index: usize| -> i64 {
        let unchanged = base_values[index];
        match modes[index] {
            mode @ (SeedMode::Global | SeedMode::Unlimited) if mode == base_modes[index] => {
                unchanged
            }
            SeedMode::Global => SEED_LIMIT_FOLLOW_GLOBAL,
            SeedMode::Unlimited => -1,
            SeedMode::Custom => {
                let text = texts[index].trim();
                if base_modes[index] == SeedMode::Custom && text == original[index] {
                    unchanged
                } else {
                    let parsed = if index < 2 {
                        parse_seed_ratio(text)
                    } else {
                        parse_seed_minutes(text)
                    };
                    parsed.unwrap_or(unchanged)
                }
            }
        }
    };
    let upload_limit_bps = if texts[4].trim() == original[4] {
        baseline.upload_limit_bps
    } else {
        parse_seed_upload_limit(texts[4]).unwrap_or(baseline.upload_limit_bps)
    };
    SeedLimits {
        ratio_limit_milli: limit(0),
        post_ratio_limit_milli: limit(1),
        seed_time_limit_minutes: limit(2),
        inactive_time_limit_minutes: limit(3),
        upload_limit_bps,
    }
}

fn seeding_status_key(code: i32) -> &'static str {
    match code {
        1 => "seedingStatusSeeding",
        2 => "seedingStatusRatioReached",
        3 => "seedingStatusTimeReached",
        4 => "seedingStatusUserStopped",
        5 => "seedingStatusDeleted",
        6 => "seedingStatusSessionReleased",
        7 => "seedingStatusInactiveReached",
        8 => "seedingStatusQueued",
        _ => "seedingStatusNone",
    }
}

fn rate_text(bytes_per_second: u64) -> SharedString {
    SharedString::from(format!("{}/s", format_bytes(bytes_per_second)))
}

/// 曲线点：横坐标为该点结束时的下载用时（`m:ss` / `h:mm:ss`，悬浮提示标题），纵坐标为速率。
fn speed_chart_points(stats: &TaskSpeedHistoryDto) -> Vec<(SharedString, f64)> {
    let bucket = i64::from(stats.bucket_secs.max(1));
    let mut previous = 0_i64;
    stats
        .samples
        .iter()
        .zip(1_i64..)
        .map(|(speed, index)| {
            // 末点可能是未满桶：按累计用时截断，但保证横坐标严格递增。
            let elapsed = index
                .saturating_mul(bucket)
                .min(stats.active_secs)
                .max(previous + 1);
            previous = elapsed;
            (clock_text(elapsed), (*speed).max(0) as f64)
        })
        .collect()
}

fn clock_text(seconds: i64) -> SharedString {
    let (hours, minutes, secs) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    SharedString::from(if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    })
}

/// 速度页是否需要刷新：下载中按间隔轮询，离开下载态补拉一次拿到最终曲线。
fn speed_refresh_due(
    was_downloading: bool,
    downloading: bool,
    since_last_fetch: Option<Duration>,
) -> bool {
    (was_downloading && !downloading)
        || (downloading && since_last_fetch.is_none_or(|elapsed| elapsed >= SPEED_REFRESH_INTERVAL))
}

fn format_duration(translator: &Translator, total_seconds: i64) -> SharedString {
    let minutes = total_seconds.max(0) / 60;
    if minutes < 60 {
        SharedString::from(format!("{minutes} {}", translator.text("timeUnitMinutes")))
    } else {
        let hours = minutes / 60;
        let remaining = minutes % 60;
        SharedString::from(format!(
            "{hours} {} {remaining} {}",
            translator.text("timeUnitHours"),
            translator.text("timeUnitMinutes")
        ))
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};

    use super::{
        DetailTab, SEED_LIMIT_FOLLOW_GLOBAL, SeedLimits, SeedMode, format_activity_timestamp,
        format_seed_ratio, parse_seed_ratio, seed_form_state, seed_limits_from_form, visible_tabs,
    };

    #[test]
    fn speed_chart_points_label_elapsed_time_and_cap_partial_bucket() {
        let stats = fluxdown_protocol::TaskSpeedHistoryDto {
            bucket_secs: 2,
            samples: vec![100, 200, 300],
            // 末点为未满桶：只覆盖 1 秒。
            active_secs: 5,
            ..Default::default()
        };
        let labels: Vec<String> = super::speed_chart_points(&stats)
            .into_iter()
            .map(|(label, _)| label.to_string())
            .collect();
        assert_eq!(labels, ["0:02", "0:04", "0:05"]);

        let long = fluxdown_protocol::TaskSpeedHistoryDto {
            bucket_secs: 3600,
            samples: vec![1, 2],
            active_secs: 7265,
            ..Default::default()
        };
        let labels: Vec<String> = super::speed_chart_points(&long)
            .into_iter()
            .map(|(label, _)| label.to_string())
            .collect();
        assert_eq!(labels, ["1:00:00", "2:00:00"]);
    }

    #[test]
    fn speed_refresh_polls_while_downloading_and_once_after() {
        use std::time::Duration;

        use super::speed_refresh_due;

        let recent = Some(Duration::from_secs(1));
        let stale = Some(Duration::from_secs(2));
        assert!(speed_refresh_due(false, true, None));
        assert!(!speed_refresh_due(true, true, recent));
        assert!(speed_refresh_due(true, true, stale));
        assert!(speed_refresh_due(true, false, recent));
        assert!(!speed_refresh_due(false, false, stale));
    }

    #[test]
    fn plugin_tabs_follow_builtin_tabs() {
        assert_eq!(
            visible_tabs(None, 2),
            [
                DetailTab::General,
                DetailTab::Speed,
                DetailTab::Log,
                DetailTab::Advanced,
                DetailTab::Plugin(0),
                DetailTab::Plugin(1),
            ]
        );
        assert_eq!(
            visible_tabs(Some(super::TaskProtocol::Bt), 0),
            [
                DetailTab::General,
                DetailTab::Files,
                DetailTab::Speed,
                DetailTab::Seeding,
                DetailTab::Trackers,
                DetailTab::Peers,
                DetailTab::Log,
                DetailTab::Advanced,
            ]
        );
    }

    #[test]
    fn ed2k_tabs_only_on_ed2k_tasks() {
        assert_eq!(
            visible_tabs(Some(super::TaskProtocol::Ed2k), 0),
            [
                DetailTab::General,
                DetailTab::Speed,
                DetailTab::Ed2kSources,
                DetailTab::Ed2kNetwork,
                DetailTab::Log,
                DetailTab::Advanced,
            ]
        );
        assert!(
            !visible_tabs(Some(super::TaskProtocol::Http), 0).contains(&DetailTab::Ed2kSources)
        );
    }

    #[test]
    fn activity_uses_source_milliseconds_with_calendar_date() {
        let timestamp = Utc
            .with_ymd_and_hms(2026, 6, 10, 12, 30, 4)
            .single()
            .unwrap()
            .timestamp_millis()
            + 123;
        let label = format_activity_timestamp(timestamp);
        assert!(label.starts_with("2026-06-"));
        assert!(label.ends_with(".123"));
        assert_eq!(format_activity_timestamp(i64::MAX).as_ref(), "—");
    }

    #[test]
    fn ratio_text_is_decimal_with_two_places() {
        assert_eq!(format_seed_ratio(1500), "1.50");
        assert_eq!(format_seed_ratio(1000), "1.00");
        assert_eq!(format_seed_ratio(2005), "2.01");
        assert_eq!(format_seed_ratio(999), "1.00");
        assert_eq!(parse_seed_ratio("1.50"), Some(1500));
        assert_eq!(parse_seed_ratio(" 2 "), Some(2000));
        assert_eq!(parse_seed_ratio("0.0004"), None);
        assert_eq!(parse_seed_ratio("0"), None);
        assert_eq!(parse_seed_ratio("-1"), None);
        assert_eq!(parse_seed_ratio("abc"), None);
        assert_eq!(parse_seed_ratio("inf"), None);
    }

    #[test]
    fn form_shows_qbittorrent_defaults_unless_custom() {
        let (modes, texts) = seed_form_state(&SeedLimits::inherit_all());
        assert_eq!(modes, [SeedMode::Global; 4]);
        assert_eq!(
            texts,
            ["1.00", "1.00", "1440", "1440", "0"].map(String::from)
        );

        let limits = SeedLimits {
            ratio_limit_milli: 1500,
            post_ratio_limit_milli: -1,
            seed_time_limit_minutes: 0,
            inactive_time_limit_minutes: 90,
            upload_limit_bps: 1500,
        };
        let (modes, texts) = seed_form_state(&limits);
        assert_eq!(
            modes,
            [
                SeedMode::Custom,
                SeedMode::Unlimited,
                SeedMode::Unlimited,
                SeedMode::Custom
            ]
        );
        assert_eq!(texts, ["1.50", "1.00", "1440", "90", "2"].map(String::from));
    }

    #[test]
    fn untouched_form_round_trips_the_baseline() {
        for baseline in [
            SeedLimits::inherit_all(),
            SeedLimits {
                ratio_limit_milli: 1005,
                post_ratio_limit_milli: -1,
                seed_time_limit_minutes: 0,
                inactive_time_limit_minutes: 90,
                upload_limit_bps: 1500,
            },
        ] {
            let (modes, texts) = seed_form_state(&baseline);
            let refs = [
                texts[0].as_str(),
                texts[1].as_str(),
                texts[2].as_str(),
                texts[3].as_str(),
                texts[4].as_str(),
            ];
            assert_eq!(seed_limits_from_form(modes, refs, &baseline), baseline);
        }
    }

    #[test]
    fn modes_and_custom_values_map_to_limits() {
        let baseline = SeedLimits {
            ratio_limit_milli: 1500,
            post_ratio_limit_milli: 2000,
            seed_time_limit_minutes: 60,
            inactive_time_limit_minutes: 30,
            upload_limit_bps: 512 * 1024,
        };
        let saved = seed_limits_from_form(
            [
                SeedMode::Global,
                SeedMode::Custom,
                SeedMode::Unlimited,
                SeedMode::Custom,
            ],
            ["1.50", "2.5", "1440", "30", "1024"],
            &baseline,
        );
        assert_eq!(
            saved,
            SeedLimits {
                ratio_limit_milli: SEED_LIMIT_FOLLOW_GLOBAL,
                post_ratio_limit_milli: 2500,
                seed_time_limit_minutes: -1,
                inactive_time_limit_minutes: 30,
                upload_limit_bps: 1024 * 1024,
            }
        );
        // 上传限速清空 / 填 0 = 不限速。
        let (modes, _) = seed_form_state(&baseline);
        assert_eq!(
            seed_limits_from_form(modes, ["", "", "", "", ""], &baseline).upload_limit_bps,
            0
        );
        assert_eq!(
            seed_limits_from_form(modes, ["1.5", "2", "60", "30", "0"], &baseline).upload_limit_bps,
            0
        );
    }

    #[test]
    fn invalid_custom_input_keeps_the_baseline_value() {
        let baseline = SeedLimits {
            ratio_limit_milli: 1500,
            ..SeedLimits::inherit_all()
        };
        let modes = [SeedMode::Custom; 4];
        let saved = seed_limits_from_form(modes, ["0", "x", "-5", "", "-3"], &baseline);
        assert_eq!(saved, baseline);
    }
}
