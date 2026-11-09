use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    UserStartIntent,
    actions::{
        ClearFinished, ClearSelection, CopySelectedUrl, CycleDensity, CycleGroupBy, CycleSort,
        DeleteSelected, DeleteSelectedWithFiles, EnterMultiSelect, FocusSearch, KEY_CONTEXT,
        NewDownload, OpenQueueManager, OpenSelected, OpenSelectedInWindow, OpenTorrentFile,
        PauseAll, PauseSelected, RedownloadSelected, RenameSelected, ResumeAll, ResumeSelected,
        RevealSelected, SeedLocal, SelectAllTasks, SetRarestFirstSelected, SetSequentialSelected,
        ShowSelectedDetail, ToggleBoostSelected, ToggleDetailPanel, ToggleFirstLastPiecesSelected,
        TogglePauseSelected,
    },
    components::{
        task_table::{
            DownloadTableDelegate, SelectModifiers, SelectionSummary, TableFilter, ToolbarCommand,
            piece_picking_order_targets, piece_picking_toggle_target, spawn_midnight_refresh,
            task_menu_facts_for,
        },
        title_bar::{DownloadSearchBox, DownloadTitleBar, DownloadViewMenu},
    },
    controller::{DownloadsCommand, DownloadsController, DownloadsPort, ResumeTargets},
    deletion::{
        DELETE_CHUNK_SIZE, DeletionProgress, DeletionReport, DeletionRequest, run_deletion,
    },
    model::{
        DownloadFilter, DownloadStatusFilter, RowKey, SidebarSection, SidebarSelection,
        StatusFolderMotion, TaskState,
        file_rescan::{RescanDecision, RescanThrottle},
        group_tree::{GroupMeta, member_dir},
        refresh_gate::{RefreshGate, RefreshPlan, RefreshTrigger},
        view_prefs::{DetailPlacement, VIEW_PREFS_KEY, ViewPrefs},
    },
    pages::new_download::{NewDownloadContext, build_new_download_context},
    pages::seed_local::open_seed_local_dialog,
    pages::task_detail::TaskDetailView,
    strings::{DownloadStrings, error_text},
    submission::{
        CancelToken, NewDownloadSubmission, SubmissionHooks, SubmissionProgress, SubmitNotice,
        run_submission,
    },
};
use fluxdown_ui_components::{ControlExt as _, FluxIcon, SidebarChange, SidebarState};
use fluxdown_ui_i18n::Translator;
use gpui::{
    AnyView, AnyWindowHandle, App, AppContext as _, ClickEvent, ClipboardItem, Context, Entity,
    ExternalPaths, FocusHandle, FontWeight, InteractiveElement as _, IntoElement, ParentElement,
    PathPromptOptions, Render, SharedString, Styled, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Icon, ResizableState, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, h_resizable,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    progress::Progress,
    resizable_panel,
    table::{TableEvent, TableState},
    v_flex, v_resizable,
};

#[path = "downloads_group_panel.rs"]
mod group_panel;

pub(crate) const SIDEBAR_MOTION_DURATION: Duration = Duration::from_millis(200);
const PREFS_DEBOUNCE: Duration = Duration::from_millis(300);
/// 搜索框输入防抖。
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// 「在独立窗口打开」一次最多开的窗口数。
pub const MAX_TASK_WINDOWS_PER_ACTION: usize = 8;

/// 宿主可直接触发的下载页命令（见 [`DownloadView::run_page_command`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageCommand {
    OpenTorrentFile,
    SeedLocal,
    PauseAll,
    ResumeAll,
    ClearFinished,
    SelectAll,
    FocusSearch,
    CycleDensity,
    CycleGroupBy,
    CycleSort,
    ToggleDetailPanel,
}

/// 宿主注入的「新建下载」入口：由 app 打开独立对话框窗口。
/// 表单初值由下载页在点击瞬间算好传入，打开方不得再回读 `DownloadView`
///（此时实体正处于 update 中）。
pub type NewDownloadOpener = Rc<dyn Fn(NewDownloadContext, &mut Window, &mut App)>;
/// 以 id 打开一个窗口 / 编辑器（任务窗口、组窗口、分类编辑）。
pub type IdOpener = Rc<dyn Fn(String, &mut Window, &mut App)>;
/// 无参数窗口入口（队列管理）。
pub type PlainOpener = Rc<dyn Fn(&mut Window, &mut App)>;
/// 分类编辑入口：`Some(id)` 编辑现有分类，`None` 新建。
pub type CategoryEditorOpener = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;
/// 单任务交互式开始的生命周期。新建成功直接通知；已有任务须在执行命令前通知 Requested。
pub type UserStartHook = Rc<dyn Fn(UserStartIntent, &mut App)>;
/// 执行插件命令：`(identity, command, task_ids)`。
pub type PluginCommandRunner = Rc<dyn Fn(String, String, Vec<String>, &mut Window, &mut App)>;
/// 构造插件详情 tab 视图：`(tab, task_id)`；宿主按 tab 的 `(identity, view)` 定位贡献，
/// 不依赖下标。
pub type PluginTabViewFactory =
    Rc<dyn Fn(&PluginTaskTab, String, &mut Window, &mut App) -> AnyView>;

/// 插件贡献的任务右键命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginTaskCommand {
    pub identity: String,
    pub command: String,
    pub title: SharedString,
}

/// 插件贡献的任务详情 tab。相等性包含插件 identity 与视图 id：换了插件的同标题 tab 也必须
/// 判为不同，详情面板才会作废旧视图缓存。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginTaskTab {
    pub identity: String,
    /// 贡献该 tab 的视图 id（插件内唯一）。
    pub view: String,
    pub title: SharedString,
    /// 内容修订号：变化即旧视图作废、须按工厂重建（插件包代码被替换时由 app 递增）。
    pub revision: u64,
}

/// app 注入的跨窗口 / 跨能力入口；未注入的入口对应按钮无动作。
#[derive(Clone, Default)]
pub struct DownloadHostActions {
    pub open_new_download: Option<NewDownloadOpener>,
    pub open_task_window: Option<IdOpener>,
    pub open_group_window: Option<IdOpener>,
    pub open_queue_manager: Option<PlainOpener>,
    /// 侧栏「设备」区标题上的「添加设备」入口；`None` 时不显示按钮。
    pub open_add_device: Option<PlainOpener>,
    /// 侧栏独立直连设备右键「重命名」：参数为设备指纹。
    pub rename_linked_device: Option<IdOpener>,
    /// 侧栏独立直连设备右键「解除配对」（含确认）：参数为设备指纹。
    pub unpair_linked_device: Option<IdOpener>,
    /// 任务表「待确认」角标点击：打开 / 置前聚合的「文件已存在」窗口。
    pub open_file_conflicts: Option<PlainOpener>,
    /// `Some(id)` 编辑现有分类，`None` 新建。
    pub open_category_editor: Option<CategoryEditorOpener>,
    /// 完成后关机的只读状态投影（`None` = Resident 未装配）。
    pub shutdown_status: Option<crate::model::shutdown::SharedShutdownStatus>,
    /// 状态栏发起关机请求的端口。
    pub shutdown: Option<crate::model::shutdown::ShutdownPort>,
    /// 单任务交互式开始回调（批量操作不回调）。
    pub on_user_started: Option<UserStartHook>,
    /// 点击插件任务右键命令：`(插件 identity, 命令 id, 任务 id 列表)`。
    pub run_plugin_command: Option<PluginCommandRunner>,
    /// 取插件详情 tab 内容：`(tab, 任务 id)`；同一 `(tab 下标, 任务)` 的视图由详情面板缓存，
    /// tab 列表变化（含插件 identity / 视图 id 变化）时缓存整体作废。
    pub plugin_tab_view: Option<PluginTabViewFactory>,
}

/// 一个进行中的大批量导入：进度只在分块边界更新，取消不回滚已创建的任务。
struct ImportJob {
    id: u64,
    progress: SubmissionProgress,
    cancel: CancelToken,
    cancelling: bool,
}

/// 一个进行中的大批量删除：进度只在分块边界更新，取消不回滚已删除的任务。
struct DeleteJob {
    id: u64,
    progress: DeletionProgress,
    cancel: CancelToken,
    cancelling: bool,
}

/// 进度条行的内容（导入 / 删除共用 [`DownloadView::render_progress_strip`]）。
struct ProgressStrip {
    job_id: u64,
    progress_id: &'static str,
    cancel_id: &'static str,
    label: SharedString,
    done: usize,
    total: usize,
    cancelling: bool,
}

/// 下载能力的顶层页面。
pub struct DownloadView {
    pub(crate) controller: DownloadsController,
    /// 详情面板 / 弹出任务窗口按需构造 `TaskDetailView` 用（`DownloadsController`
    /// 不暴露内部 port）。
    pub(crate) port: Arc<dyn DownloadsPort>,
    /// 状态栏新增文案的实时查表源（现有渲染热路径用 `strings` 缓存字段）。
    pub(crate) translator: Entity<Translator>,
    pub(crate) strings: DownloadStrings,
    pub(crate) selected_item: SidebarSelection,
    pub(crate) expanded_status: Option<DownloadStatusFilter>,
    pub(crate) folder_motion: StatusFolderMotion,
    pub(crate) folder_motion_started_at: Option<Instant>,
    pub(crate) section_expanded: HashMap<SidebarSection, bool>,
    pub(crate) section_motion_from: HashMap<SidebarSection, f32>,
    pub(crate) section_motion_started_at: HashMap<SidebarSection, Instant>,
    pub(crate) sidebar: Entity<SidebarState>,
    pub(crate) table_state: Entity<TableState<DownloadTableDelegate>>,
    pub(crate) host: DownloadHostActions,
    /// 插件贡献的任务详情 tab（下标即 `DetailTab::Plugin`）。
    pub(crate) plugin_tabs: Vec<PluginTaskTab>,
    /// 插件贡献的任务右键命令（下标即 `MenuEntry::Plugin`）。
    pub(crate) plugin_commands: Vec<PluginTaskCommand>,
    pub(crate) last_error: Option<SharedString>,
    /// 根元素 focus handle：右键菜单 action_context 分派目标，`escape`
    /// 清空搜索框后也交回给它。
    pub(crate) focus_handle: FocusHandle,
    /// 顶栏搜索框状态（输入框渲染在 [`DownloadSearchBox`] 里）。
    pub(crate) search_input: Entity<InputState>,
    /// 搜索框被布局隐藏时，经快捷键临时展开（`Esc` 收起）。
    search_revealed: bool,
    /// 已下发给搜索框的 placeholder（语言切换后需重下发）。
    search_placeholder: SharedString,
    /// 搜索防抖代数。
    search_generation: Rc<Cell<u64>>,
    prefs_generation: Rc<Cell<u64>>,
    /// 已从偏好加载过视图设置。
    prefs_loaded: bool,
    /// 最近一次应用 / 写出的视图偏好持久化值：未变化的偏好回流不重载。
    applied_view_prefs: Option<serde_json::Value>,
    /// 停靠详情面板（`None` = 尚未选中过任何任务）。
    pub(crate) detail: Option<Entity<TaskDetailView>>,
    /// 停靠面板正在显示概览的任务组（经组行聚焦；优先于 `detail`）。
    pub(crate) detail_group: Option<String>,
    /// 详情面板 / 主内容拆分的独立 resizable 状态。
    pub(crate) detail_resizable_state: Entity<ResizableState>,
    /// 上次渲染时的选中投影；表格选中变化时与之比较，变了才重绘本页（浮动选择条）。
    pub(crate) selection_summary: Cell<SelectionSummary>,
    /// 文件跟踪重扫节流（主窗口获焦触发）。
    file_rescan: RescanThrottle,
    /// 事件驱动刷新的合并闸门（≤30Hz、同批事件一次刷新）。
    refresh_gate: RefreshGate,
    /// 上次刷新时存储的行布局计数；与当前不同说明行 ID 可能已指向别的任务。
    refreshed_structure: u64,
    /// 进行中的大批量导入（每个有独立进度条与取消入口）。
    imports: Vec<ImportJob>,
    next_import_id: u64,
    /// 进行中的大批量删除（超过一块的才有进度条与取消入口）。
    deletions: Vec<DeleteJob>,
    next_delete_id: u64,
    /// 宿主的导入闸门被「全部暂停」或结果不确定的提交挂起：只有「全部开始」能解除，
    /// 所以挂起期间该按钮必须可点，即使没有可恢复的任务。
    import_suspended: bool,
    /// 下载页所在窗口：删除结束的异步通知经它推送。
    window: AnyWindowHandle,
}

impl DownloadView {
    /// 创建下载页面，并订阅共享翻译状态。
    pub fn new(
        translator: Entity<Translator>,
        port: Arc<dyn DownloadsPort>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let strings = DownloadStrings::from_translator(translator.read(cx));
        let controller = DownloadsController::new(Arc::clone(&port));
        crate::components::file_icon::install_port(&port, cx);
        let store = Rc::clone(controller.store());
        let focus_handle = cx.focus_handle();
        let table_state = cx.new(|cx| {
            TableState::new(
                DownloadTableDelegate::new(strings.clone(), store),
                window,
                cx,
            )
            .row_selectable(false)
            .col_selectable(false)
        });
        spawn_midnight_refresh(&table_state, cx);
        let weak_self = cx.weak_entity();
        table_state.update(cx, |table, _| {
            table.delegate_mut().set_host(weak_self);
            table
                .delegate_mut()
                .set_action_context(focus_handle.clone());
        });
        let strings_placeholder = strings.search_tasks_placeholder.clone();
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(strings.search_tasks_placeholder.clone())
        });
        cx.subscribe(&search_input, Self::handle_search_changed)
            .detach();

        cx.observe(&translator, |this, translator, cx| {
            this.set_strings(DownloadStrings::from_translator(translator.read(cx)), cx);
        })
        .detach();
        cx.subscribe_in(&table_state, window, Self::handle_table_event)
            .detach();
        // 选中变化只通知表格实体；浮动选择条依赖它，按投影差异重绘本页，
        // 避免表格滚动 / 悬停等高频 notify 带着整页重绘。停靠详情面板打开时跟随聚焦的
        // 任务组（组概览）或单选任务。
        cx.observe_in(&table_state, window, |this, table_state, window, cx| {
            let (selection, follow_group, follow) = {
                let delegate = table_state.read(cx).delegate();
                let detail_open = delegate.prefs().detail_open;
                let follow_group = detail_open
                    .then(|| delegate.focused_group().map(str::to_owned))
                    .flatten();
                let follow = (detail_open && follow_group.is_none())
                    .then(|| delegate.single_selected_key().filter(|key| key.is_local()))
                    .flatten()
                    .cloned();
                (delegate.selection_summary(), follow_group, follow)
            };
            if this.selection_summary.get() != selection {
                this.selection_summary.set(selection);
                cx.notify();
            }
            if let Some(group_id) = follow_group {
                this.bind_group_detail(group_id, cx);
            } else if let Some(key) = follow {
                this.bind_detail(key, window, cx);
            }
        })
        .detach();
        // 文件跟踪：主窗口获焦时用户可能刚在文件管理器里删除 / 移走了已完成任务的文件。
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.request_file_rescan(cx);
            }
        })
        .detach();
        let sidebar = cx.new(|cx| SidebarState::new(px(200.), px(176.)..px(300.), cx));
        cx.observe(&sidebar, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&sidebar, |this, _, change: &SidebarChange, cx| {
            this.table_state.update(cx, |table, _| {
                let prefs = table.delegate_mut().prefs_mut();
                prefs.sidebar_width = f32::from(change.width);
                prefs.sidebar_collapsed = change.collapsed;
            });
            this.schedule_persist_prefs(cx);
            cx.notify();
        })
        .detach();

        Self {
            controller,
            port,
            translator: translator.clone(),
            strings,
            selected_item: SidebarSelection::Download(DownloadFilter::ALL),
            expanded_status: Some(DownloadStatusFilter::All),
            folder_motion: StatusFolderMotion::settled(Some(DownloadStatusFilter::All)),
            folder_motion_started_at: None,
            section_expanded: SidebarSection::ALL
                .into_iter()
                .map(|section| (section, true))
                .collect(),
            section_motion_from: HashMap::new(),
            section_motion_started_at: HashMap::new(),
            sidebar,
            table_state,
            host: DownloadHostActions::default(),
            plugin_tabs: Vec::new(),
            plugin_commands: Vec::new(),
            last_error: None,
            focus_handle,
            search_input,
            search_revealed: false,
            search_placeholder: strings_placeholder,
            search_generation: Rc::new(Cell::new(0)),
            prefs_generation: Rc::new(Cell::new(0)),
            prefs_loaded: false,
            applied_view_prefs: None,
            detail: None,
            detail_group: None,
            detail_resizable_state: cx.new(|_| ResizableState::default()),
            selection_summary: Cell::new(SelectionSummary::default()),
            file_rescan: RescanThrottle::default(),
            refresh_gate: RefreshGate::default(),
            refreshed_structure: u64::MAX,
            imports: Vec::new(),
            next_import_id: 0,
            deletions: Vec::new(),
            next_delete_id: 0,
            import_suspended: false,
            window: window.window_handle(),
        }
    }

    /// 供 shell 顶栏与页面共享的侧栏状态；布局修改沿用下载页偏好持久化。
    pub fn sidebar_state(&self) -> Entity<SidebarState> {
        self.sidebar.clone()
    }

    /// 注入宿主入口（新建下载 / 任务窗口 / 队列管理…）。
    pub fn set_host_actions(&mut self, host: DownloadHostActions) {
        self.host = host;
    }

    /// 注入插件贡献点：详情 tab 与任务右键命令（顺序即下标，宿主回调按下标定位）。
    pub fn set_plugin_contributions(
        &mut self,
        mut tabs: Vec<PluginTaskTab>,
        mut commands: Vec<PluginTaskCommand>,
        cx: &mut Context<Self>,
    ) {
        tabs.truncate(usize::from(u16::MAX));
        commands.truncate(usize::from(u16::MAX));
        if self.plugin_tabs == tabs && self.plugin_commands == commands {
            return;
        }
        self.plugin_tabs = tabs.clone();
        self.plugin_commands = commands.clone();
        self.table_state.update(cx, |table, cx| {
            table.delegate_mut().set_plugin_commands(commands);
            cx.notify();
        });
        if let Some(detail) = self.detail.clone() {
            detail.update(cx, |detail, cx| detail.set_plugin_tabs(tabs, cx));
        }
        cx.notify();
    }

    /// 当前选中的本机任务 id（已排序；远程任务不含）。命令面板的插件命令据此取目标任务。
    pub fn selected_local_task_ids(&self, cx: &App) -> Vec<String> {
        self.table_state
            .read(cx)
            .delegate()
            .selected_keys()
            .iter()
            .filter(|key| key.is_local())
            .map(|key| key.task_id().to_owned())
            .collect()
    }

    /// 右键菜单点击插件命令：按菜单构建时捕获的 `(identity, 命令 id)` 定位，把选中任务 id
    /// 交给宿主回调。命令已不在当前贡献里（插件被禁用 / 卸载 / 熔断）时不执行。
    pub(crate) fn run_plugin_command(
        &self,
        identity: &str,
        command: &str,
        task_ids: Vec<String>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(run) = self.host.run_plugin_command.clone() else {
            return;
        };
        if !self
            .plugin_commands
            .iter()
            .any(|entry| entry.identity == identity && entry.command == command)
        {
            return;
        }
        run(
            identity.to_owned(),
            command.to_owned(),
            task_ids,
            window,
            cx,
        );
    }

    /// 宿主同步导入闸门挂起状态（见 `import_suspended`）。
    pub fn set_import_suspended(&mut self, suspended: bool, cx: &mut Context<Self>) {
        if self.import_suspended != suspended {
            self.import_suspended = suspended;
            cx.notify();
        }
    }

    /// 状态栏「全部暂停」不可用的原因（`None` = 可点）：没有等待 / 下载中的本地任务，
    /// 且本页没有进行中的导入（全部暂停同时挂起导入闸门）。
    pub(crate) fn pause_all_blocker(&self) -> Option<SharedString> {
        // 任务表分页载入期间「有没有可暂停任务」无从判断：显示加载态而不是「没有任务」。
        if self.controller.tables_loading() {
            return Some(self.strings.tasks_loading_title.clone());
        }
        let available = self.controller.has_pausable_tasks() || !self.imports.is_empty();
        (!available).then(|| self.strings.pause_all_unavailable.clone())
    }

    /// 状态栏「全部开始」不可用的原因（`None` = 可点）。导入闸门挂起时恒可点；
    /// 暂停任务全在已停止队列时单独说明——全部开始不会动它们，需先启动队列。
    pub(crate) fn resume_all_blocker(&self) -> Option<SharedString> {
        if self.import_suspended {
            return None;
        }
        if self.controller.tables_loading() {
            return Some(self.strings.tasks_loading_title.clone());
        }
        match self.controller.resume_targets() {
            ResumeTargets::Some => None,
            ResumeTargets::OnlyInStoppedQueues => {
                Some(self.strings.resume_all_queue_stopped.clone())
            }
            ResumeTargets::None => Some(self.strings.resume_all_unavailable.clone()),
        }
    }

    /// 创建挂到 shell 统一顶栏的下载页插槽（「新建」按钮）。
    pub fn new_title_bar(&self, cx: &mut Context<Self>) -> Entity<DownloadTitleBar> {
        let view = cx.weak_entity();
        let translator = self.translator.clone();
        cx.new(|_| DownloadTitleBar::new(view, translator))
    }

    /// 创建顶栏搜索框（chrome 布局条目，由宿主注册为 shell 控件）。
    pub fn new_search_box(&self, cx: &mut Context<Self>) -> Entity<DownloadSearchBox> {
        let view = cx.weak_entity();
        let search_input = self.search_input.clone();
        cx.new(|cx| DownloadSearchBox::new(view, search_input, cx))
    }

    /// 创建「视图」选项菜单（chrome 布局条目，由宿主注册为 shell 控件）。
    pub fn new_view_menu(&self, cx: &mut Context<Self>) -> Entity<DownloadViewMenu> {
        let view = cx.entity();
        let translator = self.translator.clone();
        let table_state = self.table_state.clone();
        cx.new(|cx| DownloadViewMenu::new(&view, translator, table_state, cx))
    }

    /// 「新建下载」表单的环境快照：保存目录 / 默认队列 / 线程数初值与队列候选；
    /// 队列、「下载到」设备优先侧栏当前筛选（规则见 [`build_new_download_context`]）。
    #[must_use]
    pub fn new_download_context(&self) -> NewDownloadContext {
        let controller = &self.controller;
        let selected_queue = match &self.selected_item {
            SidebarSelection::Queue(queue_id) => Some(queue_id.as_str()),
            _ => None,
        };
        let selected_device = match &self.selected_item {
            SidebarSelection::Device(id) => Some(id.as_str()),
            _ => None,
        };
        build_new_download_context(
            controller.config(),
            &controller.runtime_stats().save_dir,
            controller.preferences(),
            controller.queues(),
            selected_queue,
            selected_device,
            controller.other_devices(),
        )
    }

    /// 打开「新建下载」窗口（可预填链接）。
    pub(crate) fn open_new_download_with(
        &self,
        initial_urls: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(opener) = self.host.open_new_download.clone() else {
            return;
        };
        let mut context = self.new_download_context();
        context.initial_urls = initial_urls;
        opener(context, window, cx);
    }

    pub(crate) fn open_new_download(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_download_with(Vec::new(), window, cx);
    }

    /// 侧栏独立直连设备：打开「新建下载」并把「下载到」预选为该设备。
    pub(crate) fn open_new_download_to(
        &self,
        target: crate::model::devices::DispatchTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(opener) = self.host.open_new_download.clone() else {
            return;
        };
        let mut context = self.new_download_context();
        context.target = target;
        opener(context, window, cx);
    }

    /// 按表单提交创建任务 / 确认外部捕获 / 下发到其他设备；对话框确认后由宿主调用。
    ///
    /// 命令逐条执行，任一失败即在页面横幅提示（按错误 `reason` 给出原因）；同时把本次
    /// 保存目录 / 下载目标记入本机偏好（尽力而为）。返回的 task 在全部完成后给出汇总提示。
    pub fn create_download(
        &mut self,
        submission: NewDownloadSubmission,
        cx: &mut Context<Self>,
    ) -> gpui::Task<SubmitNotice> {
        let starts_immediately = submission.starts_immediately();
        let port = Arc::clone(&self.port);
        let on_user_started = self.host.on_user_started.clone();
        let timer = cx.background_executor().clone();
        let cancel = CancelToken::default();
        // 超过一块的导入才显示进度条（小批量保持原来的静默路径）。
        let job = submission.exceeds_one_chunk().then(|| {
            let id = self.next_import_id;
            self.next_import_id += 1;
            self.imports.push(ImportJob {
                id,
                progress: SubmissionProgress {
                    total: submission.task_count(),
                    ..SubmissionProgress::default()
                },
                cancel: cancel.clone(),
                cancelling: false,
            });
            cx.notify();
            id
        });
        cx.spawn(async move |this, cx| {
            let progress_view = this.clone();
            let mut progress_cx = cx.clone();
            let hooks = SubmissionHooks::new(move |delay| timer.timer(delay))
                .with_cancel(cancel)
                .with_progress(move |progress: &SubmissionProgress| {
                    let Some(id) = job else {
                        return;
                    };
                    let progress = *progress;
                    // 每块一次更新；页面已释放则不再有可更新的界面。
                    let Ok(()) = progress_view.update(&mut progress_cx, |this, cx| {
                        this.update_import(id, progress, cx);
                    }) else {
                        return;
                    };
                });
            let report =
                run_submission(submission, move |command| port.execute(command), hooks).await;
            if starts_immediately
                && let ([task_id], Some(hook)) = (report.created_task_ids(), on_user_started)
            {
                cx.update(|cx| hook(UserStartIntent::Created(task_id.clone()), cx));
            }
            let fallback = SubmitNotice {
                ok: !report.failed(),
                message: String::new(),
            };
            this.update(cx, |this, cx| {
                if let Some(id) = job {
                    this.imports.retain(|import| import.id != id);
                }
                let notice = report.notice(this.translator.read(cx));
                this.last_error = report
                    .failed()
                    .then(|| SharedString::from(notice.message.clone()));
                cx.notify();
                notice
            })
            .unwrap_or(fallback)
        })
    }

    fn update_import(&mut self, id: u64, progress: SubmissionProgress, cx: &mut Context<Self>) {
        if let Some(import) = self.imports.iter_mut().find(|import| import.id == id) {
            import.progress = progress;
            cx.notify();
        }
    }

    /// 取消一个进行中的导入：已创建的任务保留，在途分块给短暂收尾时间后放弃。
    fn cancel_import(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(import) = self.imports.iter_mut().find(|import| import.id == id) {
            import.cancelling = true;
            import.cancel.cancel();
            cx.notify();
        }
    }

    /// 进行中导入的进度条：进度文案（重试等待时换成重试提示）+ 取消按钮，每个导入一条。
    fn render_import_strips(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        self.imports
            .iter()
            .map(|import| {
                let progress = import.progress;
                let label = if import.cancelling {
                    self.strings.import_cancelling.clone()
                } else if let Some(retry) = progress.retry {
                    self.strings.format_import_retrying(
                        retry.delay.as_secs(),
                        retry.attempt,
                        retry.max,
                    )
                } else {
                    self.strings
                        .format_import_progress(progress.done, progress.total)
                };
                self.render_progress_strip(
                    ProgressStrip {
                        job_id: import.id,
                        progress_id: "import-progress",
                        cancel_id: "import-cancel",
                        label,
                        done: progress.done,
                        total: progress.total,
                        cancelling: import.cancelling,
                    },
                    Self::cancel_import,
                    cx,
                )
            })
            .collect()
    }

    /// 本地多任务删除：按块下发（见 [`crate::deletion`]），行立即变淡并移出选区；超过一块时
    /// 显示进度条与取消入口。结束时按 [`DeletionReport::notice`] 反馈：成功 / 取消用窗口通知，
    /// 有失败写入页面错误条（与其余命令的失败提示一致）。
    ///
    /// 通知需要窗口：下载页创建时记下所在窗口句柄，异步结束后经句柄推送，这样所有删除入口
    /// （菜单 / 快捷键 / 选择条 / 清除已完成）都不必为此改成传 `Window`。
    pub(crate) fn start_deletion(&mut self, request: DeletionRequest, cx: &mut Context<Self>) {
        // 本地服务已断开：不提交（与 `DownloadsController::execute` 在断连时的拒绝一致）。
        if self.controller.is_stale() {
            self.last_error = Some(self.strings.disconnected.clone());
            cx.notify();
            return;
        }
        let DeletionRequest {
            task_ids,
            delete_files,
        } = request;
        // 已在另一批删除中的任务不重复提交。
        let task_ids = self.table_state.update(cx, |table, cx| {
            let fresh = table.delegate_mut().begin_delete(task_ids);
            cx.notify();
            fresh
        });
        let total = task_ids.len();
        if total == 0 {
            return;
        }
        let id = self.next_delete_id;
        self.next_delete_id += 1;
        let cancel = CancelToken::default();
        // 超过一块才显示进度条（小批量只有行变淡 + 结束提示）。
        let with_strip = total > DELETE_CHUNK_SIZE;
        if with_strip {
            self.deletions.push(DeleteJob {
                id,
                progress: DeletionProgress { total, done: 0 },
                cancel: cancel.clone(),
                cancelling: false,
            });
        }
        cx.notify();
        let port = Arc::clone(&self.port);
        let timer = cx.background_executor().clone();
        let window = self.window;
        cx.spawn(async move |this, cx| {
            let progress_view = this.clone();
            let mut progress_cx = cx.clone();
            let report = run_deletion(
                &task_ids,
                delete_files,
                &move |command| port.execute(command),
                &move |delay| timer.timer(delay),
                &cancel,
                &mut |progress: &DeletionProgress| {
                    if !with_strip {
                        return;
                    }
                    let progress = *progress;
                    // 每块一次更新；页面已释放则不再有可更新的界面。
                    let Ok(()) = progress_view.update(&mut progress_cx, |this, cx| {
                        this.update_delete(id, progress, cx);
                    }) else {
                        return;
                    };
                },
            )
            .await;
            let Ok(notification) = this.update(cx, |this, cx| {
                this.finish_deletion(id, &task_ids, &report, cx)
            }) else {
                // 下载页已释放，没有可更新的界面，也不再推送通知。
                return;
            };
            if let Some(notification) = notification
                && let Err(error) = window.update(cx, |_, window, cx| {
                    window.push_notification(notification, cx)
                })
            {
                eprintln!("main window closed before the deletion notice: {error:#}");
            }
        })
        .detach();
    }

    fn update_delete(&mut self, id: u64, progress: DeletionProgress, cx: &mut Context<Self>) {
        if let Some(job) = self.deletions.iter_mut().find(|job| job.id == id) {
            job.progress = progress;
            cx.notify();
        }
    }

    /// 取消一个进行中的删除：已删除的任务不回滚，在途分块给短暂收尾时间后放弃。
    fn cancel_delete(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(job) = self.deletions.iter_mut().find(|job| job.id == id) {
            job.cancelling = true;
            job.cancel.cancel();
            cx.notify();
        }
    }

    /// 一批删除结束：释放变淡的行、撤下进度条，并给出结果。成功 / 取消返回待推送的窗口通知，
    /// 有失败则写入错误条。
    fn finish_deletion(
        &mut self,
        id: u64,
        task_ids: &[String],
        report: &DeletionReport,
        cx: &mut Context<Self>,
    ) -> Option<Notification> {
        self.deletions.retain(|job| job.id != id);
        self.table_state.update(cx, |table, cx| {
            table.delegate_mut().finish_delete(task_ids);
            cx.notify();
        });
        let notice = {
            let store = self.controller.store();
            let tally = report.tally(|task_id| store.find_local(task_id).is_none());
            report.notice(tally, &self.strings, self.translator.read(cx))
        };
        cx.notify();
        if !notice.ok {
            self.last_error = Some(SharedString::from(notice.message));
            return None;
        }
        self.last_error = None;
        // 单任务删除沿用既有的静默路径；多任务才弹成功 / 取消提示。
        (task_ids.len() > 1).then(|| {
            if report.cancelled() {
                Notification::info(notice.message)
            } else {
                Notification::success(notice.message)
            }
        })
    }

    /// 进行中删除的进度条（与导入共用样式）：进度文案 + 取消按钮，每批一条。
    fn render_delete_strips(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        self.deletions
            .iter()
            .map(|job| {
                let label = if job.cancelling {
                    self.strings.import_cancelling.clone()
                } else {
                    self.strings
                        .format_delete_progress(job.progress.done, job.progress.total)
                };
                self.render_progress_strip(
                    ProgressStrip {
                        job_id: job.id,
                        progress_id: "delete-progress",
                        cancel_id: "delete-cancel",
                        label,
                        done: job.progress.done,
                        total: job.progress.total,
                        cancelling: job.cancelling,
                    },
                    Self::cancel_delete,
                    cx,
                )
            })
            .collect()
    }

    /// 进度条行：文案 + 进度条 + 取消按钮（导入与删除共用）。
    fn render_progress_strip(
        &self,
        strip: ProgressStrip,
        on_cancel: fn(&mut Self, u64, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = fluxdown_ui_theme::active_theme(cx);
        let tokens = theme.tokens();
        let spacing = tokens.spacing;
        let text_size = tokens.typography.xs.size;
        let line_height = tokens.typography.xs.line_height;
        let muted = tokens.colors.muted_foreground;
        let primary = tokens.colors.primary;
        let percent = if strip.total == 0 {
            0.
        } else {
            (strip.done as f32 / strip.total as f32 * 100.).clamp(0., 100.)
        };
        let id = strip.job_id;
        h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap(spacing.md)
            .pl(spacing.md)
            .pr(spacing.xs)
            .py(spacing.xs)
            .text_size(text_size)
            .line_height(line_height)
            .text_color(muted)
            .child(div().flex_none().child(strip.label))
            .child(
                div().flex_1().min_w_0().child(
                    Progress::new((strip.progress_id, id as usize))
                        .w_full()
                        .color(primary)
                        .value(percent),
                ),
            )
            .child(
                Button::new((strip.cancel_id, id as usize))
                    .ghost()
                    .control(cx)
                    .label(self.strings.cancel.clone())
                    .disabled(strip.cancelling)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        on_cancel(this, id, cx);
                    })),
            )
            .into_any_element()
    }

    /// 角标点击入口：交给宿主打开 / 置前「文件已存在」窗口。
    pub(crate) fn open_file_conflicts(&self, window: &mut Window, cx: &mut App) {
        if let Some(open) = self.host.open_file_conflicts.as_ref() {
            open(window, cx);
        }
    }

    pub fn replace_snapshot(
        &mut self,
        snapshot: &fluxdown_protocol::AgentSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.controller.replace_snapshot(snapshot);
        if let Some(detail) = self.detail.clone() {
            detail.update(cx, |detail, cx| detail.replace_snapshot(snapshot, cx));
        }
        self.reconcile_sidebar_selection();
        self.last_error = (!snapshot.daemon_connected).then(|| self.strings.disconnected.clone());
        self.load_view_prefs(cx);
        self.refresh_from_store(cx);
    }

    pub fn apply_event(&mut self, event: &fluxdown_protocol::ServiceEvent, cx: &mut Context<Self>) {
        let table_changed = self.controller.apply_event(event);
        if let Some(detail) = self.detail.clone() {
            detail.update(cx, |detail, cx| detail.apply_event(event, cx));
        }
        match event {
            fluxdown_protocol::ServiceEvent::Agent(
                fluxdown_protocol::AgentEvent::PreferencesChanged(_),
            ) => self.load_view_prefs(cx),
            // agent 先于 daemon 就绪时首个快照即为未连接；连接态以事件为准，横幅随之出现 / 消失。
            fluxdown_protocol::ServiceEvent::Agent(
                fluxdown_protocol::AgentEvent::DaemonConnectionChanged(connected),
            ) => {
                self.last_error = (!connected).then(|| self.strings.disconnected.clone());
            }
            _ => {}
        }
        if table_changed {
            if matches!(
                event,
                fluxdown_protocol::ServiceEvent::Agent(
                    fluxdown_protocol::AgentEvent::Daemon(
                        fluxdown_protocol::DaemonEvent::QueuesChanged(_)
                            | fluxdown_protocol::DaemonEvent::SnapshotReplaced(_)
                    ) | fluxdown_protocol::AgentEvent::DaemonSnapshotReplaced(_)
                        | fluxdown_protocol::AgentEvent::CloudDevicesChanged(_)
                        | fluxdown_protocol::AgentEvent::LinkedDevicesChanged(_)
                        | fluxdown_protocol::AgentEvent::SessionChanged(_)
                        | fluxdown_protocol::AgentEvent::PreferencesChanged(_)
                )
            ) {
                self.reconcile_sidebar_selection();
            }
            self.schedule_table_refresh(cx);
        } else {
            cx.notify();
        }
    }

    /// 队列删除 / 设备消失（登出、被移除、解除配对）/ daemon 快照更替后，不能继续筛选
    /// 已不存在的队列或设备。
    fn reconcile_sidebar_selection(&mut self) {
        match &self.selected_item {
            SidebarSelection::Queue(queue_id)
                if !self
                    .controller
                    .queues()
                    .iter()
                    .any(|queue| &queue.queue_id == queue_id) =>
            {
                self.selected_item = SidebarSelection::Download(DownloadFilter::ALL);
            }
            SidebarSelection::Download(DownloadFilter {
                status,
                category: Some(category),
            }) if !self.controller.categories().rules().is_empty()
                && !self
                    .controller
                    .categories()
                    .visible()
                    .any(|rule| rule.dto.id == *category) =>
            {
                self.selected_item = SidebarSelection::Download(DownloadFilter::status(*status));
            }
            SidebarSelection::Device(id)
                if id != SidebarSelection::LOCAL_DEVICE
                    && id != SidebarSelection::ALL_DEVICES
                    && !self
                        .controller
                        .other_devices()
                        .iter()
                        .any(|device| !device.is_standalone_link() && device.id == *id) =>
            {
                self.selected_item = SidebarSelection::Download(DownloadFilter::ALL);
            }
            _ => {}
        }
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.last_error = Some(self.strings.disconnected.clone());
        self.controller.mark_stale();
        if let Some(detail) = self.detail.clone() {
            detail.update(cx, |detail, cx| detail.mark_stale(cx));
        }
        cx.notify();
    }

    /// 「隐藏任务来源」偏好 → 表格副标题与停靠详情副标题。独立于 [`Self::load_view_prefs`] 的
    /// 视图偏好去重：它是单独的偏好键，变化后只重画，不触发视图重算。
    fn sync_hide_source(&mut self, cx: &mut Context<Self>) {
        self.sync_detail_hide_source(cx);
        let hide = self.controller.hide_task_source();
        self.table_state.update(cx, |table, cx| {
            if table.delegate_mut().set_hide_source(hide) {
                cx.notify();
            }
        });
    }

    /// 只把「隐藏任务来源」偏好同步给停靠详情（切换 / 新建详情面板时用，表格不受影响）。
    fn sync_detail_hide_source(&mut self, cx: &mut Context<Self>) {
        let hide = self.controller.hide_task_source();
        if let Some(detail) = self.detail.clone() {
            detail.update(cx, |detail, cx| detail.set_hide_source(hide, cx));
        }
    }

    /// 偏好 → 表格视图设置；无偏好时用默认视图。
    ///
    /// 任何偏好键变化都会触发这里（`PreferencesChanged` 不区分键），只有视图偏好本身的持久化值
    /// 变了才重载：否则别的键（如命令面板使用记录）回流时，会用旧持久化值覆盖 300ms 防抖
    /// 窗口内尚未写回的本地改动，表现为切换密度 / 分组 / 详情面板「无效」。
    fn load_view_prefs(&mut self, cx: &mut Context<Self>) {
        self.sync_hide_source(cx);
        let prefs = match self.controller.preference(VIEW_PREFS_KEY) {
            Some(value) if self.applied_view_prefs.as_ref() == Some(value) => return,
            Some(value) => {
                self.applied_view_prefs = Some(value.clone());
                ViewPrefs::from_value(value)
            }
            None if self.prefs_loaded => return,
            None => ViewPrefs::default(),
        };
        self.prefs_loaded = true;
        self.table_state.update(cx, |table, cx| {
            table.delegate_mut().set_prefs(prefs);
            cx.notify();
        });
    }

    /// 队列名 / 任务组 / 分类 / 设备名同步进表格代理（只在变化时触发重算）。
    fn sync_delegate_context(&mut self, cx: &mut Context<Self>) {
        let queues: Vec<(String, String)> = self
            .controller
            .queues()
            .iter()
            .map(|queue| (queue.queue_id.clone(), queue.name.clone()))
            .collect();
        let groups: HashMap<String, GroupMeta> = self
            .controller
            .groups()
            .iter()
            .map(|group| (group.group_id.clone(), GroupMeta::from_dto(group)))
            .collect();
        let devices: HashMap<String, String> = self
            .controller
            .other_devices()
            .into_iter()
            .map(|device| (device.id, device.label))
            .collect();
        let categories = Rc::clone(self.controller.categories());
        if let Some(detail) = self.detail.clone() {
            detail.update(cx, |detail, cx| detail.set_queue_names(queues.clone(), cx));
        }
        self.table_state.update(cx, |table, _| {
            let delegate = table.delegate_mut();
            delegate.set_queue_names(queues);
            delegate.set_groups(groups);
            delegate.set_device_names(devices);
            delegate.set_categories(categories);
        });
    }

    /// 停靠详情面板：把当前任务的最新 DTO 推给面板刷新（事件 / 快照后调用）。
    fn sync_detail_panel(&mut self, cx: &mut Context<Self>) {
        if self.controller.is_stale() {
            return;
        }
        let Some(detail) = self.detail.clone() else {
            return;
        };
        let task_id = detail.read(cx).task_id().to_owned();
        let dto = self.controller.task_dto(&task_id).cloned();
        let runtime = self.controller.task_runtime(&task_id).cloned();
        detail.update(cx, |detail, cx| {
            detail.sync(dto, cx);
            detail.sync_runtime(runtime, cx);
        });
    }

    pub(crate) fn refresh_tasks(&mut self, cx: &mut Context<Self>) {
        let filter = TableFilter::from(&self.selected_item);
        self.table_state.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.set_filter(filter);
            delegate.refresh_view();
            // 行变化只需重绘（行数每帧从代理读取）；只有列配置变了才重建
            // `col_groups`，否则进度节拍会覆盖拖拽中的列宽。
            if delegate.take_columns_dirty() {
                table.refresh(cx);
            }
            // 行从共享的 `TaskStore` 读取：表格必须显式 notify，retained 渲染才会重画行。
            cx.notify();
        });
        cx.notify();
    }

    /// 任务 / 上下文变化后立即把存储同步进表格、侧栏与详情面板。
    fn refresh_from_store(&mut self, cx: &mut Context<Self>) {
        self.sync_delegate_context(cx);
        self.refresh_tasks(cx);
        self.sync_detail_panel(cx);
        self.refreshed_structure = self.controller.store().structure_generation();
        self.refresh_gate.flushed(Instant::now());
    }

    /// 事件使表格数据过期：按 [`RefreshGate`] 合并成一次刷新（同批事件之后、≤30Hz），
    /// 而不是每条事件都全量重算可见行。
    fn schedule_table_refresh(&mut self, cx: &mut Context<Self>) {
        let structural = self.controller.store().structure_generation() != self.refreshed_structure;
        match self.refresh_gate.mark(structural, Instant::now()) {
            RefreshPlan::Nothing => {}
            RefreshPlan::Deferred => {
                let this = cx.weak_entity();
                cx.defer(move |cx| {
                    let Ok(()) = this.update(cx, |this, cx| {
                        this.run_scheduled_refresh(RefreshTrigger::Deferred, cx);
                    }) else {
                        // 下载页已释放，结束回调，不再提交请求或刷新状态。
                        return;
                    };
                });
            }
            RefreshPlan::After(delay) => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;

                    let Ok(()) = this.update(cx, |this, cx| {
                        this.run_scheduled_refresh(RefreshTrigger::Timer, cx);
                    }) else {
                        // 下载页已释放，结束回调，不再提交请求或刷新状态。
                        return;
                    };
                })
                .detach();
            }
        }
    }

    fn run_scheduled_refresh(&mut self, trigger: RefreshTrigger, cx: &mut Context<Self>) {
        if self.refresh_gate.take(trigger, Instant::now()) {
            self.refresh_from_store(cx);
        }
    }

    pub(crate) fn select_sidebar_item(
        &mut self,
        selection: SidebarSelection,
        cx: &mut Context<Self>,
    ) {
        if self.selected_item == selection {
            return;
        }
        self.selected_item = selection;
        self.refresh_tasks(cx);
    }

    fn set_strings(&mut self, strings: DownloadStrings, cx: &mut Context<Self>) {
        self.strings = strings.clone();
        self.table_state.update(cx, |table, cx| {
            table.delegate_mut().set_strings(strings);
            table.delegate_mut().refresh_view();
            table.refresh(cx);
            cx.notify();
        });
        cx.notify();
    }

    fn handle_table_event(
        &mut self,
        table_state: &Entity<TableState<DownloadTableDelegate>>,
        event: &TableEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TableEvent::ColumnWidthsChanged(widths) => {
                table_state.update(cx, |table, _| {
                    table.delegate_mut().sync_column_widths(widths);
                });
                self.schedule_persist_prefs(cx);
            }
            TableEvent::MoveColumn(..) => {
                self.schedule_persist_prefs(cx);
            }
            _ => {}
        }
    }

    /// 打开停靠详情面板并切换到该任务（双击无法直接打开文件的行、右键「详情」）。
    pub(crate) fn open_detail_for(
        &mut self,
        key: RowKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !key.is_local() {
            return;
        }
        self.bind_detail(key, window, cx);
        if !self.table_state.read(cx).delegate().prefs().detail_open {
            self.mutate_prefs(|prefs| prefs.detail_open = true, cx);
        }
    }

    /// 行尾「详情」按钮：像普通单击一样只选中该行（停靠面板跟随单选，先选中才不会被
    /// 跟随逻辑切回旧选中项），再打开详情面板。
    pub(crate) fn show_row_detail(
        &mut self,
        key: RowKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.table_state.update(cx, |table, cx| {
            table
                .delegate_mut()
                .select_task(key.clone(), SelectModifiers::default());
            cx.notify();
        });
        self.open_detail_for(key, window, cx);
    }

    /// 双击任务行：已完成且文件仍在下载目录 → 用系统默认程序打开；其余（含文件已被删除
    /// / 移走的已完成任务）→ 停靠详情面板查看。文件已被标记丢失时顺带重扫，文件移回后
    /// 标记自愈。远程任务没有本机文件与详情，双击无动作。
    pub(crate) fn activate_row(
        &mut self,
        key: RowKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !key.is_local() {
            return;
        }
        let Some((openable, missing)) = self
            .controller
            .store()
            .get(&key)
            .map(|row| (row.has_local_file(), row.is_file_missing()))
        else {
            return;
        };
        if openable {
            self.execute_commands(
                vec![DownloadsCommand::OpenTask {
                    task_id: key.task_id().to_owned(),
                }],
                cx,
            );
            return;
        }
        if missing {
            self.rescan_files_now(cx);
        }
        self.open_detail_for(key, window, cx);
    }

    /// 可合并的文件跟踪重扫（获焦触发），见 [`RescanThrottle`]。
    fn request_file_rescan(&mut self, cx: &mut Context<Self>) {
        match self.file_rescan.request(Instant::now()) {
            RescanDecision::Now => self.send_file_rescan(cx),
            RescanDecision::After(delay) => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;

                    let Ok(()) = this.update(cx, |this, cx| {
                        this.file_rescan.trailing_fired(Instant::now());
                        this.send_file_rescan(cx);
                    }) else {
                        // 下载页已释放，结束回调，不再提交请求或刷新状态。
                        return;
                    };
                })
                .detach();
            }
            RescanDecision::Coalesced => {}
        }
    }

    /// 立即重扫：打开 / 拖出时发现文件已不在，行上的丢失标记要尽快跟上磁盘现状。
    pub(crate) fn rescan_files_now(&mut self, cx: &mut Context<Self>) {
        self.file_rescan.record_immediate(Instant::now());
        self.send_file_rescan(cx);
    }

    /// 结果经 `fileMissingChanged` 事件回流；失败（daemon 断开）不打扰用户，daemon 自身
    /// 的定时扫描兜底。不走 `execute_commands`，以免清掉页面横幅上的真实错误。
    fn send_file_rescan(&self, cx: &mut Context<Self>) {
        let future = self.controller.execute(DownloadsCommand::RescanFiles);
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = future.await {
                    // 后台定时扫描仍会兜底，不覆盖页面现有业务错误。
                    eprintln!(
                        "download file rescan request failed: {:?} ({:?})",
                        error.code, error.reason
                    );
                }
            })
            .detach();
    }

    /// 让停靠详情面板承载该任务（面板视图按需创建）；已是该任务时不做事。
    fn bind_detail(&mut self, key: RowKey, window: &mut Window, cx: &mut Context<Self>) {
        if !key.is_local() {
            return;
        }
        if self.detail_group.take().is_some() {
            cx.notify();
        }
        let task_id = key.task_id().to_owned();
        match self.detail.clone() {
            Some(detail) if detail.read(cx).task_id() == task_id => return,
            Some(detail) => {
                detail.update(cx, |detail, cx| detail.set_task(task_id, cx));
            }
            None => {
                let plugin_tabs = self.plugin_tabs.clone();
                let store = Rc::clone(self.controller.store());
                let host = self.host.clone();
                let translator = self.translator.clone();
                let port = Arc::clone(&self.port);
                let detail = cx.new(|cx| {
                    TaskDetailView::new_docked(translator, task_id, store, port, host, window, cx)
                });
                if !plugin_tabs.is_empty() {
                    detail.update(cx, |detail, cx| detail.set_plugin_tabs(plugin_tabs, cx));
                }
                if self.controller.is_stale() {
                    detail.update(cx, |detail, cx| detail.mark_stale(cx));
                }
                self.detail = Some(detail);
            }
        }
        self.sync_detail_hide_source(cx);
        self.sync_detail_panel(cx);
        cx.notify();
    }

    /// 让停靠面板显示该任务组的概览；已是该组时不做事。
    fn bind_group_detail(&mut self, group_id: String, cx: &mut Context<Self>) {
        if self.detail_group.as_deref() != Some(group_id.as_str()) {
            self.detail_group = Some(group_id);
            cx.notify();
        }
    }

    /// 组行「详情」按钮：选中并聚焦该组，打开停靠面板显示组概览。
    pub(crate) fn show_group_detail(
        &mut self,
        group_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.table_state.update(cx, |table, cx| {
            table
                .delegate_mut()
                .select_group(&group_id, SelectModifiers::default());
            cx.notify();
        });
        self.bind_group_detail(group_id, cx);
        if !self.table_state.read(cx).delegate().prefs().detail_open {
            self.mutate_prefs(|prefs| prefs.detail_open = true, cx);
        }
    }

    /// 右键「详情」：在停靠面板中查看选区锚点（或第一个本地选中）任务。
    fn on_show_selected_detail(
        &mut self,
        _: &ShowSelectedDetail,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let candidate = self.table_state.read(cx).delegate().detail_candidate();
        if let Some(key) = candidate {
            self.open_detail_for(key, window, cx);
        }
    }

    fn on_toggle_detail_panel(
        &mut self,
        _: &ToggleDetailPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if Self::guard_letter_key(window, cx) {
            self.run_page_command(PageCommand::ToggleDetailPanel, window, cx);
        }
    }

    fn on_close_detail_panel(&mut self, cx: &mut Context<Self>) {
        self.detail = None;
        self.detail_group = None;
        self.mutate_prefs(|prefs| prefs.detail_open = false, cx);
    }

    fn on_toggle_detail_placement(&mut self, cx: &mut Context<Self>) {
        self.mutate_prefs(
            |prefs| prefs.detail_placement = prefs.detail_placement.toggled(),
            cx,
        );
    }

    /// 面板「弹出为窗口」：交给宿主开独立任务 / 任务组窗口后关闭面板。
    fn on_pop_out_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(group_id) = self.detail_group.clone() {
            self.open_group_in_window(group_id, window, cx);
            self.on_close_detail_panel(cx);
            return;
        }
        let Some(detail) = self.detail.clone() else {
            return;
        };
        let task_id = detail.read(cx).task_id().to_owned();
        if let Some(opener) = self.host.open_task_window.clone() {
            opener(task_id, window, cx);
        }
        self.on_close_detail_panel(cx);
    }

    /// 视图偏好写回：300ms 防抖后合并成一次 `SetLocalPreference`。
    pub(crate) fn schedule_persist_prefs(&mut self, cx: &mut Context<Self>) {
        let generation = self.prefs_generation.get().wrapping_add(1);
        self.prefs_generation.set(generation);
        let marker = Rc::clone(&self.prefs_generation);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREFS_DEBOUNCE).await;
            if marker.get() != generation {
                return;
            }

            let Ok(()) = this.update(cx, |this, cx| {
                let value = this.table_state.update(cx, |table, _| {
                    let delegate = table.delegate_mut();
                    let columns = delegate.column_prefs();
                    delegate.prefs_mut().columns = columns;
                    delegate.prefs().to_value()
                });
                // 自己写出的值回流时不再重载（见 `load_view_prefs`）。
                this.applied_view_prefs = Some(value.clone());
                let future = this
                    .controller
                    .execute(DownloadsCommand::SetLocalPreference {
                        key: VIEW_PREFS_KEY,
                        value,
                    });
                cx.spawn(async move |this, cx| {
                    if let Err(error) = future.await {
                        let Ok(()) = this.update(cx, |this, cx| {
                            if this.prefs_generation.get() == generation {
                                this.applied_view_prefs = None;
                            }
                            this.last_error = Some(SharedString::from(error_text(
                                this.translator.read(cx),
                                &error,
                            )));
                            cx.notify();
                        }) else {
                            // 下载页已关闭，停止回写偏好保存结果。
                            return;
                        };
                    }
                })
                .detach();
            }) else {
                // 下载页已释放，结束回调，不再提交请求或刷新状态。
                return;
            };
        })
        .detach();
    }

    pub(crate) fn mutate_prefs(
        &mut self,
        mutate: impl FnOnce(&mut ViewPrefs),
        cx: &mut Context<Self>,
    ) {
        self.table_state.update(cx, |table, cx| {
            mutate(table.delegate_mut().prefs_mut());
            table.delegate_mut().refresh_view();
            table.refresh(cx);
            cx.notify();
        });
        self.schedule_persist_prefs(cx);
        cx.notify();
    }

    /// 搜索框内容变化：150ms 防抖后写入表格代理并重算可见行。
    fn handle_search_changed(
        &mut self,
        _input: Entity<InputState>,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let generation = self.search_generation.get().wrapping_add(1);
        self.search_generation.set(generation);
        let marker = Rc::clone(&self.search_generation);
        let input = self.search_input.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            if marker.get() != generation {
                return;
            }

            let Ok(()) = this.update(cx, |this, cx| {
                let query = input.read(cx).value().to_string();
                this.table_state.update(cx, |table, cx| {
                    table.delegate_mut().set_query(&query);
                    if table.delegate_mut().refresh_view() {
                        table.refresh(cx);
                        // 空闲时没有别的重绘来源：不 notify 则 retained 表格停在旧结果。
                        cx.notify();
                    }
                });
            }) else {
                // 下载页已释放，结束回调，不再提交请求或刷新状态。
                return;
            };
        })
        .detach();
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.run_page_command(PageCommand::FocusSearch, window, cx);
    }

    /// 搜索框内 `escape`：清空查询并把焦点交回下载页根元素。
    pub(crate) fn on_search_escape(
        &mut self,
        _: &gpui_component::input::Escape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.table_state.update(cx, |table, cx| {
            table.delegate_mut().set_query("");
            if table.delegate_mut().refresh_view() {
                table.refresh(cx);
                cx.notify();
            }
        });
        self.search_revealed = false;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// 搜索框被布局隐藏时是否仍临时显示：快捷键唤出后、或仍有查询词时（避免筛选条件不可见）。
    #[must_use]
    pub fn search_revealed(&self, cx: &App) -> bool {
        self.search_revealed || !self.search_input.read(cx).value().is_empty()
    }

    /// 内联重命名对话框（单选、满足 [`TaskMenuFacts::can_rename`] 的任务；应用菜单不置灰，
    /// 入口须按同一事实源再判）；确认后写 `DownloadsCommand::Rename`。
    ///
    /// [`TaskMenuFacts::can_rename`]: crate::components::task_table::TaskMenuFacts::can_rename
    fn on_rename_selected(
        &mut self,
        _: &RenameSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let [key] = selected.as_slice() else {
            return;
        };
        if !task_menu_facts_for(self.controller.store(), key)
            .is_some_and(|facts| facts.can_rename())
        {
            return;
        }
        let task_id = key.task_id().to_owned();
        let current_name = self
            .controller
            .store()
            .get(key)
            .map(|row| row.name.clone())
            .unwrap_or_default();
        let title = self.strings.rename_task_title.clone();
        let field_label = self.strings.col_file_name.clone();
        let placeholder = self.strings.rename_task_placeholder.clone();
        let ok_label = self.strings.confirm.clone();
        let cancel_label = self.strings.cancel.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(current_name)
                .placeholder(placeholder)
        });
        let dialog_input = input.clone();
        let this = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let content_input = dialog_input.clone();
            let ok_input = dialog_input.clone();
            let this = this.clone();
            let task_id = task_id.clone();
            let field_label = field_label.clone();
            dialog
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .w(px(520.))
                .content(move |content, _, cx| {
                    content.child(fluxdown_ui_components::form(cx).child(
                        fluxdown_ui_components::form_field(
                            field_label.clone(),
                            Input::new(&content_input).control(cx).w_full(),
                            None,
                            cx,
                        ),
                    ))
                })
                .footer(fluxdown_ui_components::dialog_footer(
                    Some(cancel_label.clone()),
                    ok_label.clone(),
                    fluxdown_ui_components::DialogIntent::Confirm,
                    cx,
                ))
                .on_ok(move |_, _, cx| {
                    let file_name = ok_input.read(cx).value().trim().to_owned();
                    if file_name.is_empty() {
                        return false;
                    }
                    let task_id = task_id.clone();
                    // 页面释放后没有提交命令，不能让确认框报告成功。
                    this.update(cx, |this, cx| {
                        this.execute_commands(
                            vec![DownloadsCommand::Rename { task_id, file_name }],
                            cx,
                        );
                    })
                    .is_ok()
                })
        });
        input.update(cx, |state, cx| state.focus(window, cx));
    }

    /// 插件重试挂起确认（右键菜单经 [`DownloadTableDelegate`] 回调）。
    pub(crate) fn confirm_ignore_plugin_retry(
        &mut self,
        task_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.strings.ignore_plugin_retry_title.clone();
        let description = self.strings.ignore_plugin_retry_msg.clone();
        let ok_label = self.strings.ignore_plugin_retry.clone();
        let cancel_label = self.strings.cancel.clone();
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let this = this.clone();
            let task_id = task_id.clone();
            dialog
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .description(description.clone())
                .footer(fluxdown_ui_components::dialog_footer(
                    Some(cancel_label.clone()),
                    ok_label.clone(),
                    fluxdown_ui_components::DialogIntent::Confirm,
                    cx,
                ))
                .on_ok(move |_, _, cx| {
                    let task_id = task_id.clone();
                    // 页面释放后没有提交命令，不能让确认框报告成功。
                    this.update(cx, |this, cx| {
                        this.execute_commands(
                            vec![DownloadsCommand::IgnorePluginRetry { task_id }],
                            cx,
                        );
                    })
                    .is_ok()
                })
        });
    }

    /// 将当前选中的本地任务批量移动到目标队列（右键菜单「移动到队列」）。
    pub(crate) fn move_selected_to_queue(&mut self, queue_id: String, cx: &mut Context<Self>) {
        let commands: Vec<DownloadsCommand> = self
            .table_state
            .read(cx)
            .delegate()
            .selected_keys()
            .into_iter()
            .filter(RowKey::is_local)
            .map(|key| DownloadsCommand::MoveToQueue {
                task_id: key.task_id().to_owned(),
                queue_id: queue_id.clone(),
            })
            .collect();
        self.execute_commands(commands, cx);
    }

    pub(crate) fn group_pause_all(&mut self, group_id: String, cx: &mut Context<Self>) {
        self.execute_commands(vec![DownloadsCommand::GroupPause { group_id }], cx);
    }

    pub(crate) fn group_resume_all(&mut self, group_id: String, cx: &mut Context<Self>) {
        self.execute_commands(vec![DownloadsCommand::GroupResume { group_id }], cx);
    }

    /// 组内失败成员逐个恢复。
    pub(crate) fn group_retry_failed(&mut self, group_id: String, cx: &mut Context<Self>) {
        let commands: Vec<DownloadsCommand> = self
            .controller
            .store()
            .local()
            .iter()
            .filter(|task| task.group_id == group_id && task.state == TaskState::Failed)
            .map(|task| DownloadsCommand::Resume {
                task_id: task.key.task_id().to_owned(),
            })
            .collect();
        self.execute_commands(commands, cx);
    }

    /// 打开组文件夹：定位一个位于组根目录的成员（文件管理器即打开组根目录），没有则
    /// 取第一个成员。
    pub(crate) fn group_open_folder(&mut self, group_id: String, cx: &mut Context<Self>) {
        let root = self
            .controller
            .group_summaries()
            .iter()
            .find(|group| group.id == group_id)
            .map(|group| group.meta.save_dir.clone())
            .unwrap_or_default();
        let store = Rc::clone(self.controller.store());
        let local = store.local();
        let mut members = local.iter().filter(|task| task.group_id == group_id);
        let Some(first) = members.next() else {
            return;
        };
        let target = std::iter::once(first)
            .chain(members)
            .find(|task| member_dir(&root, &task.save_dir).is_empty())
            .unwrap_or(first);
        let task_id = target.key.task_id().to_owned();
        drop(local);
        self.execute_commands(vec![DownloadsCommand::RevealTask { task_id }], cx);
    }

    pub(crate) fn group_copy_source_link(
        &mut self,
        group_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(url) = self
            .controller
            .group_summaries()
            .iter()
            .find(|group| group.id == group_id)
            .map(|group| group.meta.source_url.clone())
            .filter(|url| !url.is_empty())
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(url));
        window.push_notification(Notification::success(self.strings.url_copied.clone()), cx);
    }

    pub(crate) fn group_delete(&mut self, group_id: String, cx: &mut Context<Self>) {
        self.execute_commands(
            vec![DownloadsCommand::GroupDelete {
                group_id,
                delete_files: false,
            }],
            cx,
        );
    }

    /// 「删除组及文件」二次确认。
    pub(crate) fn confirm_group_delete_with_files(
        &mut self,
        group_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self
            .controller
            .group_summaries()
            .iter()
            .find(|group| group.id == group_id)
            .map_or_else(|| group_id.clone(), |group| group.meta.name.clone());
        let title = self.strings.group_delete_with_files.clone();
        let description = self.strings.group_delete_with_files_description(&name);
        let ok_label = self.strings.delete.clone();
        let cancel_label = self.strings.cancel.clone();
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let this = this.clone();
            let group_id = group_id.clone();
            dialog
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .description(description.clone())
                .footer(fluxdown_ui_components::dialog_footer(
                    Some(cancel_label.clone()),
                    ok_label.clone(),
                    fluxdown_ui_components::DialogIntent::Destructive,
                    cx,
                ))
                .on_ok(move |_, _, cx| {
                    let group_id = group_id.clone();
                    // 页面释放后没有提交命令，不能让确认框报告成功。
                    this.update(cx, |this, cx| {
                        this.execute_commands(
                            vec![DownloadsCommand::GroupDelete {
                                group_id,
                                delete_files: true,
                            }],
                            cx,
                        );
                    })
                    .is_ok()
                })
        });
    }

    pub(crate) fn open_group_in_window(
        &mut self,
        group_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(opener) = self.host.open_group_window.clone() {
            opener(group_id, window, cx);
        }
    }

    /// 拖放导入：`.torrent` 按用户主动打开处理（走 BT 文件选择后建任务）；
    /// `.txt`/`.url`/`.list`（≤1MB）按行提取
    /// 受支持的链接后打开新建下载窗口预填（窗口已开则追加进表单）；其他文件类型提示不支持。
    fn on_paths_dropped(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut torrent_commands = Vec::new();
        let mut urls = Vec::new();
        let mut unsupported = false;
        for path in paths.paths() {
            let ext = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase);
            match ext.as_deref() {
                Some("torrent") => torrent_commands.push(DownloadsCommand::open_torrent_file(path)),
                Some("txt" | "url" | "list") => match read_drop_text_file(path) {
                    Some(text) => urls.extend(parse_drop_urls(&text)),
                    None => unsupported = true,
                },
                _ => unsupported = true,
            }
        }
        if !torrent_commands.is_empty() {
            self.execute_commands(torrent_commands, cx);
        }
        if !urls.is_empty() {
            self.open_new_download_with(urls, window, cx);
        }
        if unsupported {
            window.push_notification(self.strings.unsupported_drop_hint.clone(), cx);
        }
    }

    fn guard_letter_key(window: &mut Window, cx: &mut Context<Self>) -> bool {
        if window.has_focused_input(cx) {
            cx.propagate();
            return false;
        }
        true
    }

    fn on_select_all(&mut self, _: &SelectAllTasks, window: &mut Window, cx: &mut Context<Self>) {
        self.run_page_command(PageCommand::SelectAll, window, cx);
    }

    fn on_enter_multi_select(
        &mut self,
        _: &EnterMultiSelect,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.table_state.update(cx, |table, cx| {
            table.delegate_mut().enter_multi_select();
            cx.notify();
        });
    }

    fn on_clear_selection(&mut self, _: &ClearSelection, _: &mut Window, cx: &mut Context<Self>) {
        self.clear_table_selection(cx);
    }

    /// 清空表格选中（Esc 快捷键与浮动选择条「取消选择」共用）。
    pub(crate) fn clear_table_selection(&mut self, cx: &mut Context<Self>) {
        self.table_state.update(cx, |table, cx| {
            table.delegate_mut().clear_selection();
            cx.notify();
        });
    }

    fn on_pause_selected(&mut self, _: &PauseSelected, _: &mut Window, cx: &mut Context<Self>) {
        self.execute_toolbar(ToolbarCommand::Pause, cx);
    }

    fn on_resume_selected(&mut self, _: &ResumeSelected, _: &mut Window, cx: &mut Context<Self>) {
        self.execute_toolbar(ToolbarCommand::Resume, cx);
    }

    fn on_toggle_pause_selected(
        &mut self,
        _: &TogglePauseSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::guard_letter_key(window, cx) {
            return;
        }
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let store = self.controller.store();
        let any_active = selected.iter().any(|key| {
            store.get(key).is_some_and(|row| {
                row.is_seeding() || matches!(row.state, TaskState::Downloading | TaskState::Pending)
            })
        });
        let action = if any_active {
            ToolbarCommand::Pause
        } else {
            ToolbarCommand::Resume
        };
        self.execute_toolbar(action, cx);
    }

    fn on_delete_selected(&mut self, _: &DeleteSelected, _: &mut Window, cx: &mut Context<Self>) {
        self.execute_toolbar(ToolbarCommand::Delete, cx);
    }

    fn on_delete_selected_with_files(
        &mut self,
        _: &DeleteSelectedWithFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_selected_with_files(window, cx);
    }

    /// 对当前选中集合发起「删除任务和文件」（带二次确认）；空选择为 no-op。
    pub(crate) fn delete_selected_with_files(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        if selected.is_empty() {
            return;
        }
        self.confirm_delete_with_files(selected, window, cx);
    }

    /// 「删除任务及文件」二次确认。
    pub(crate) fn confirm_delete_with_files(
        &mut self,
        keys: Vec<RowKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.strings.delete_task_and_file.clone();
        let description = self
            .strings
            .delete_with_files_description(&keys, self.controller.store());
        let ok_label = self.strings.delete.clone();
        let cancel_label = self.strings.cancel.clone();
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let this = this.clone();
            let keys = keys.clone();
            dialog
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .description(description.clone())
                .footer(fluxdown_ui_components::dialog_footer(
                    Some(cancel_label.clone()),
                    ok_label.clone(),
                    fluxdown_ui_components::DialogIntent::Destructive,
                    cx,
                ))
                .on_ok(move |_, _, cx| {
                    // 页面释放后没有提交命令，不能让确认框报告成功。
                    this.update(cx, |this, cx| {
                        let commands = this.delete_commands(&keys, true);
                        this.execute_commands(commands, cx);
                    })
                    .is_ok()
                })
        });
    }

    fn on_pause_all(&mut self, _: &PauseAll, window: &mut Window, cx: &mut Context<Self>) {
        self.run_page_command(PageCommand::PauseAll, window, cx);
    }

    fn on_resume_all(&mut self, _: &ResumeAll, window: &mut Window, cx: &mut Context<Self>) {
        self.run_page_command(PageCommand::ResumeAll, window, cx);
    }

    fn on_open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        self.execute_toolbar(ToolbarCommand::Open, cx);
    }

    fn on_reveal_selected(&mut self, _: &RevealSelected, _: &mut Window, cx: &mut Context<Self>) {
        self.execute_toolbar(ToolbarCommand::Reveal, cx);
    }

    fn on_open_selected_in_window(
        &mut self,
        _: &OpenSelectedInWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(opener) = self.host.open_task_window.clone() else {
            return;
        };
        let selected: Vec<RowKey> = self
            .table_state
            .read(cx)
            .delegate()
            .selected_keys()
            .into_iter()
            .filter(RowKey::is_local)
            .collect();
        if selected.len() > MAX_TASK_WINDOWS_PER_ACTION {
            window.push_notification(self.strings.too_many_windows_hint.clone(), cx);
        }
        for key in selected.into_iter().take(MAX_TASK_WINDOWS_PER_ACTION) {
            opener(key.task_id().to_owned(), window, cx);
        }
    }

    fn on_copy_selected_url(
        &mut self,
        _: &CopySelectedUrl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let store = self.controller.store();
        let urls: Vec<String> = selected
            .iter()
            .filter_map(|key| store.get(key).map(|row| row.share_url().to_owned()))
            .collect();
        if !urls.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(urls.join("\n")));
            window.push_notification(Notification::success(self.strings.url_copied.clone()), cx);
        }
    }

    fn on_redownload_selected(
        &mut self,
        _: &RedownloadSelected,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let commands = self.redownload_commands(cx);
        self.execute_commands(commands, cx);
    }

    /// 选中集合中可重新下载的本地任务（完成 / 失败且非 `torrent-file://` 哨兵）。
    pub(crate) fn redownload_commands(&self, cx: &Context<Self>) -> Vec<DownloadsCommand> {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        selected
            .iter()
            .filter(|key| key.is_local())
            .filter_map(|key| {
                let task_id = key.task_id();
                let row = self.controller.store().get(key)?;
                if !matches!(row.state, TaskState::Completed | TaskState::Failed)
                    || row.url.starts_with("torrent-file://")
                {
                    return None;
                }
                let dto = self.controller.task_dto(task_id)?;
                let request = fluxdown_protocol::CreateTaskRequest {
                    url: if dto.origin_url.is_empty() {
                        dto.url.clone()
                    } else {
                        dto.origin_url.clone()
                    },
                    file_name: dto.file_name.clone(),
                    save_dir: dto.save_dir.clone(),
                    segments: 0,
                    cookies: String::new(),
                    referrer: dto.referrer.clone(),
                    proxy_url: dto.proxy_url.clone(),
                    user_agent: String::new(),
                    queue_id: dto.queue_id.clone(),
                    checksum: dto.checksum.clone(),
                    ignore_tls_errors: dto.ignore_tls_errors,
                    headers: None,
                    torrent_b64: None,
                    method: None,
                    body: None,
                    audio_url: None,
                    start_paused: false,
                    http_user: String::new(),
                    http_password: String::new(),
                    save_site_auth: false,
                };
                Some(DownloadsCommand::Redownload(
                    Box::new(request),
                    task_id.to_owned(),
                ))
            })
            .collect()
    }

    fn on_toggle_boost_selected(
        &mut self,
        _: &ToggleBoostSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let Some(key) = selected.iter().find(|key| key.is_local()) else {
            return;
        };
        // 只有未结束的任务能优先下载：已完成 / 失败直接提示，不下发命令。
        let ended_notice = match self.controller.store().get(key).map(|row| row.state) {
            Some(TaskState::Completed) => Some(self.strings.boost_task_completed.clone()),
            Some(TaskState::Failed) => Some(self.strings.boost_task_ended.clone()),
            _ => None,
        };
        if let Some(notice) = ended_notice {
            window.push_notification(Notification::warning(notice), cx);
            return;
        }
        self.execute_commands(
            vec![DownloadsCommand::ToggleBoost {
                task_id: key.task_id().to_owned(),
            }],
            cx,
        );
    }

    fn on_set_rarest_first_selected(
        &mut self,
        _: &SetRarestFirstSelected,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_piece_picking_order(false, cx);
    }

    fn on_set_sequential_selected(
        &mut self,
        _: &SetSequentialSelected,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_piece_picking_order(true, cx);
    }

    fn on_toggle_first_last_pieces_selected(
        &mut self,
        _: &ToggleFirstLastPiecesSelected,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let store = self.controller.store();
        let facts: Vec<_> = selected
            .iter()
            .filter_map(|key| task_menu_facts_for(store, key))
            .collect();
        let Some(value) = piece_picking_toggle_target(&facts) else {
            return;
        };
        let commands = selected
            .into_iter()
            .filter(|key| key.is_local())
            .map(|key| DownloadsCommand::SetPiecePicking {
                task_id: key.task_id().to_owned(),
                sequential: None,
                first_last_pieces_first: Some(value),
            })
            .collect();
        self.execute_commands(commands, cx);
    }

    /// 右键 / 应用菜单选片顺序：只对尚未处于目标顺序的本机未完成 BT 任务下发，
    /// 每任务一次，只带 `sequential`。全体已是目标值则不下发。
    fn set_piece_picking_order(&mut self, sequential: bool, cx: &mut Context<Self>) {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let store = self.controller.store();
        let rows: Vec<_> = selected
            .into_iter()
            .filter_map(|key| task_menu_facts_for(store, &key).map(|facts| (key, facts)))
            .collect();
        let facts: Vec<_> = rows.iter().map(|(_, facts)| *facts).collect();
        let targets = piece_picking_order_targets(&facts, sequential);
        if targets.is_empty() {
            return;
        }
        let commands = targets
            .into_iter()
            .filter_map(|index| rows.get(index))
            .filter(|(key, _)| key.is_local())
            .map(|(key, _)| DownloadsCommand::SetPiecePicking {
                task_id: key.task_id().to_owned(),
                sequential: Some(sequential),
                first_last_pieces_first: None,
            })
            .collect();
        self.execute_commands(commands, cx);
    }

    fn on_cycle_density(&mut self, _: &CycleDensity, window: &mut Window, cx: &mut Context<Self>) {
        if Self::guard_letter_key(window, cx) {
            self.run_page_command(PageCommand::CycleDensity, window, cx);
        }
    }

    fn on_cycle_group_by(&mut self, _: &CycleGroupBy, window: &mut Window, cx: &mut Context<Self>) {
        if Self::guard_letter_key(window, cx) {
            self.run_page_command(PageCommand::CycleGroupBy, window, cx);
        }
    }

    fn on_cycle_sort(&mut self, _: &CycleSort, window: &mut Window, cx: &mut Context<Self>) {
        if Self::guard_letter_key(window, cx) {
            self.run_page_command(PageCommand::CycleSort, window, cx);
        }
    }

    fn on_new_download(&mut self, _: &NewDownload, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_download(window, cx);
    }

    fn on_open_torrent_file(
        &mut self,
        _: &OpenTorrentFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_page_command(PageCommand::OpenTorrentFile, window, cx);
    }

    fn on_seed_local(&mut self, _: &SeedLocal, window: &mut Window, cx: &mut Context<Self>) {
        self.run_page_command(PageCommand::SeedLocal, window, cx);
    }

    fn open_torrent_file(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let commands: Vec<DownloadsCommand> = paths
                .into_iter()
                .filter(|path| {
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("torrent"))
                })
                .map(|path| DownloadsCommand::open_torrent_file(&path))
                .collect();

            let Ok(()) = this.update(cx, |this, cx| this.execute_commands(commands, cx)) else {
                // 下载页已释放，结束回调，不再提交请求或刷新状态。
                return;
            };
        })
        .detach();
    }

    fn on_open_queue_manager(
        &mut self,
        _: &OpenQueueManager,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(opener) = self.host.open_queue_manager.clone() {
            opener(window, cx);
        }
    }

    fn on_clear_finished(
        &mut self,
        _: &ClearFinished,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_page_command(PageCommand::ClearFinished, window, cx);
    }

    /// 宿主直接触发的页面命令（命令面板）。键盘 / 菜单动作与这里共用同一份实现，但宿主调用
    /// 不经焦点派发，也不受「输入框聚焦时字母键让给输入框」的限制——这些命令不是按键。
    pub fn run_page_command(
        &mut self,
        command: PageCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            PageCommand::OpenTorrentFile => self.open_torrent_file(cx),
            PageCommand::SeedLocal => open_seed_local_dialog(
                Arc::clone(&self.port),
                self.strings.clone(),
                self.translator.clone(),
                window,
                cx,
            ),
            PageCommand::PauseAll => self.execute_toolbar(ToolbarCommand::PauseAll, cx),
            PageCommand::ResumeAll => self.execute_toolbar(ToolbarCommand::ResumeAll, cx),
            PageCommand::ClearFinished => {
                // 任务表分页载入期间「已完成」范围不完整，不能据占位表清理。
                if self.controller.tables_loading() {
                    return;
                }
                let commands: Vec<DownloadsCommand> = self
                    .controller
                    .store()
                    .local()
                    .iter()
                    .filter(|row| row.state == TaskState::Completed)
                    .map(|row| DownloadsCommand::Delete {
                        task_id: row.key.task_id().to_owned(),
                        delete_files: false,
                    })
                    .collect();
                self.execute_commands(commands, cx);
            }
            PageCommand::SelectAll => self.table_state.update(cx, |table, cx| {
                table.delegate_mut().select_all_tasks();
                cx.notify();
            }),
            PageCommand::FocusSearch => {
                self.search_revealed = true;
                self.search_input
                    .update(cx, |input, cx| input.focus(window, cx));
                cx.notify();
            }
            PageCommand::CycleDensity => self.mutate_prefs(ViewPrefs::cycle_density, cx),
            PageCommand::CycleGroupBy => self.mutate_prefs(ViewPrefs::cycle_group_by, cx),
            PageCommand::CycleSort => self.mutate_prefs(ViewPrefs::cycle_sort, cx),
            PageCommand::ToggleDetailPanel => {
                self.mutate_prefs(|prefs| prefs.detail_open = !prefs.detail_open, cx);
            }
        }
    }

    /// 错误 / 断连提示：紧凑的 destructive 文字条，可关闭。
    fn render_error_strip(&self, error: SharedString, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = fluxdown_ui_theme::active_theme(cx);
        let tokens = theme.tokens();
        let spacing = tokens.spacing;
        let text_size = tokens.typography.xs.size;
        let line_height = tokens.typography.xs.line_height;
        let destructive = tokens.colors.destructive;
        let icon_size = theme.extended().icon.sm;
        let close_label = SharedString::from(self.translator.read(cx).text("close").to_owned());
        let dismiss = self.icon_action(
            "download-error-dismiss",
            close_label,
            Icon::new(FluxIcon::X).size(icon_size),
            false,
            |this, _, cx| {
                this.last_error = None;
                cx.notify();
            },
            cx,
        );
        h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap(spacing.sm)
            .pl(spacing.md)
            .pr(spacing.xs)
            .text_size(text_size)
            .line_height(line_height)
            .text_color(destructive)
            .child(div().flex_1().min_w_0().truncate().child(error))
            .child(dismiss)
            .into_any_element()
    }

    pub(crate) fn render_main(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let surface = fluxdown_ui_theme::active_theme(cx).tokens().colors.surface;
        let prefs = self.table_state.read(cx).delegate().prefs().clone();
        let error_strip = self
            .last_error
            .clone()
            .map(|error| self.render_error_strip(error, cx));
        let import_strips = self.render_import_strips(cx);
        let delete_strips = self.render_delete_strips(cx);
        // 选择条覆盖在表格容器顶部的表头上，不遮挡任务行。
        let table = self
            .render_table(cx)
            .children(self.render_selection_bar(cx));
        let content = v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .children(error_strip)
            .children(import_strips)
            .children(delete_strips)
            .child(v_flex().flex_1().min_w_0().min_h_0().child(table));

        let body: gpui::AnyElement = if prefs.detail_open {
            let panel = self.render_detail_panel(cx);
            let on_resize = cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                if let Some(size) = state.read(cx).sizes().get(1).copied() {
                    let size = f32::from(size);
                    if size > 0. {
                        this.mutate_prefs(|prefs| prefs.detail_size = size, cx);
                    }
                }
            });
            match prefs.detail_placement {
                DetailPlacement::Bottom => v_resizable("downloads-detail-split")
                    .with_state(&self.detail_resizable_state)
                    .on_resize(on_resize)
                    .child(resizable_panel().child(content))
                    .child(
                        resizable_panel()
                            .size(px(prefs.detail_size))
                            .flex_none()
                            .size_range(px(160.)..px(480.))
                            .child(panel),
                    )
                    .into_any_element(),
                DetailPlacement::Right => h_resizable("downloads-detail-split")
                    .with_state(&self.detail_resizable_state)
                    .on_resize(on_resize)
                    .child(resizable_panel().child(content))
                    .child(
                        resizable_panel()
                            .size(px(prefs.detail_size))
                            .flex_none()
                            .size_range(px(220.)..px(560.))
                            .child(panel),
                    )
                    .into_any_element(),
            }
        } else {
            content.into_any_element()
        };

        // 侧栏 | 内容的结构线由 resizable 把手绘制（主题已映射为 hairline），这里不再画边框。
        div()
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(surface)
            .child(body)
            .into_any_element()
    }

    fn render_detail_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let group_overview = self
            .detail_group
            .as_deref()
            .and_then(|group_id| self.render_group_overview(group_id, cx));
        let showing_group = group_overview.is_some();
        let theme = fluxdown_ui_theme::active_theme(cx);
        let tokens = theme.tokens().clone();
        let icon_size = theme.extended().icon.md;
        let hairline = theme.extended().colors.hairline;
        let stroke = theme.extended().stroke.thin;
        let toolbar_button = theme.density().toolbar_button;
        let tertiary = theme.extended().colors.text_tertiary;
        let placement = self
            .table_state
            .read(cx)
            .delegate()
            .prefs()
            .detail_placement;
        let translator = self.translator.read(cx);
        let title = translator.text("detail").to_owned();
        let hint = translator.text("selectTaskHint").to_owned();
        // 切换按钮指向「切换后」的位置。
        let (placement_icon, placement_label) = if placement == DetailPlacement::Bottom {
            (
                FluxIcon::PanelRight,
                translator.text("viewDetailRight").to_owned(),
            )
        } else {
            (
                FluxIcon::PanelBottom,
                translator.text("viewDetailBottom").to_owned(),
            )
        };
        let close_label = SharedString::from(translator.text("close").to_owned());
        let pop_out_label = self.strings.open_in_window.clone();
        let toggle_position = self.icon_action(
            "detail-panel-toggle-position",
            SharedString::from(placement_label),
            Icon::new(placement_icon).size(icon_size),
            false,
            |this, _, cx| this.on_toggle_detail_placement(cx),
            cx,
        );
        let pop_out = self.icon_action(
            "detail-panel-pop-out",
            pop_out_label,
            Icon::new(FluxIcon::AppWindow).size(icon_size),
            false,
            |this, window, cx| this.on_pop_out_detail(window, cx),
            cx,
        );
        let close = self.icon_action(
            "detail-panel-close",
            close_label,
            Icon::new(FluxIcon::X).size(icon_size),
            false,
            |this, _, cx| this.on_close_detail_panel(cx),
            cx,
        );

        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(tokens.colors.surface)
            .child(
                h_flex()
                    // 面板头：`density.toolbarButton` 的图标按钮上下各留 spacing.xxs。
                    .min_h(toolbar_button + tokens.spacing.xs)
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .pl(tokens.spacing.md)
                    .pr(tokens.spacing.xxs)
                    .border_b(stroke)
                    .border_color(hairline)
                    .child(
                        div()
                            .text_size(tokens.typography.sm.size)
                            .line_height(tokens.typography.sm.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        h_flex()
                            .gap(tokens.spacing.xxs)
                            .child(toggle_position)
                            .child(pop_out)
                            .child(close),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .when_some(group_overview, |this, overview| this.child(overview))
                    .when(!showing_group, |this| {
                        this.when_some(self.detail.clone(), |this, detail| this.child(detail))
                    })
                    .when(!showing_group && self.detail.is_none(), |this| {
                        this.child(
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .gap(tokens.spacing.sm)
                                .child(Icon::new(FluxIcon::File).size(px(32.)).text_color(tertiary))
                                .child(
                                    div()
                                        .text_size(tokens.typography.xs.size)
                                        .line_height(tokens.typography.xs.line_height)
                                        .text_color(tokens.colors.muted_foreground)
                                        .child(hint),
                                ),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// 下载页根焦点：宿主据此把动作派发进下载页（如标题栏菜单在下载页外发起的动作）。
impl gpui::Focusable for DownloadView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DownloadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 语言切换后同步搜索框 placeholder（InputState 只在构造时取一次）。
        if self.search_placeholder != self.strings.search_tasks_placeholder {
            self.search_placeholder = self.strings.search_tasks_placeholder.clone();
            let placeholder = self.search_placeholder.clone();
            self.search_input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx);
            });
        }
        v_flex()
            .key_context(KEY_CONTEXT)
            .size_full()
            .min_w_0()
            .min_h_0()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_enter_multi_select))
            .on_action(cx.listener(Self::on_clear_selection))
            .on_action(cx.listener(Self::on_pause_selected))
            .on_action(cx.listener(Self::on_resume_selected))
            .on_action(cx.listener(Self::on_toggle_pause_selected))
            .on_action(cx.listener(Self::on_delete_selected))
            .on_action(cx.listener(Self::on_delete_selected_with_files))
            .on_action(cx.listener(Self::on_pause_all))
            .on_action(cx.listener(Self::on_resume_all))
            .on_action(cx.listener(Self::on_open_selected))
            .on_action(cx.listener(Self::on_reveal_selected))
            .on_action(cx.listener(Self::on_copy_selected_files))
            .on_action(cx.listener(Self::on_open_selected_in_window))
            .on_action(cx.listener(Self::on_show_selected_detail))
            .on_action(cx.listener(Self::on_copy_selected_url))
            .on_action(cx.listener(Self::on_copy_magnet_selected))
            .on_action(cx.listener(Self::on_export_torrent_selected))
            .on_action(cx.listener(Self::on_redownload_selected))
            .on_action(cx.listener(Self::on_toggle_boost_selected))
            .on_action(cx.listener(Self::on_cycle_density))
            .on_action(cx.listener(Self::on_cycle_group_by))
            .on_action(cx.listener(Self::on_cycle_sort))
            .on_action(cx.listener(Self::on_new_download))
            .on_action(cx.listener(Self::on_open_torrent_file))
            .on_action(cx.listener(Self::on_seed_local))
            .on_action(cx.listener(Self::on_open_queue_manager))
            .on_action(cx.listener(Self::on_clear_finished))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_rename_selected))
            .on_action(cx.listener(Self::on_change_selected_url))
            .on_action(cx.listener(Self::on_change_selected_segments))
            .on_action(cx.listener(Self::on_verify_selected))
            .on_action(cx.listener(Self::on_set_rarest_first_selected))
            .on_action(cx.listener(Self::on_set_sequential_selected))
            .on_action(cx.listener(Self::on_toggle_first_last_pieces_selected))
            .on_action(cx.listener(Self::on_toggle_detail_panel))
            .on_drop(cx.listener(Self::on_paths_dropped))
            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                let tokens = fluxdown_ui_theme::active_theme(cx).tokens();
                style.bg(tokens.colors.accent.opacity(0.2))
            })
            .child(self.render_sidebar_layout(window, cx))
    }
}

/// 拖入 `.txt`/`.url`/`.list` 文件（≤1MB）；超限或读取失败返回 `None`。
fn read_drop_text_file(path: &std::path::Path) -> Option<String> {
    const MAX_BYTES: u64 = 1_048_576;
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// 纯函数：从拖入文本按行提取受支持协议的下载链接（`http(s)://`、
/// `ftp(s)://`、`magnet:`、`ed2k://`）。
pub(crate) fn parse_drop_urls(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| is_supported_drop_url(line))
        .map(str::to_owned)
        .collect()
}

fn is_supported_drop_url(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("ftps://")
        || lower.starts_with("magnet:")
        || lower.starts_with("ed2k://")
}

#[cfg(test)]
mod drop_tests {
    use super::parse_drop_urls;

    #[test]
    fn extracts_only_supported_url_lines() {
        let text = "  https://example.com/a.zip  \n\
             not a url\n\
             \n\
             magnet:?xt=urn:btih:abc\n\
             ftp://files.example.com/b.iso\n\
             ed2k://|file|c.bin|123|abcdef|/\n\
             javascript:alert(1)\n";
        assert_eq!(
            parse_drop_urls(text),
            vec![
                "https://example.com/a.zip".to_owned(),
                "magnet:?xt=urn:btih:abc".to_owned(),
                "ftp://files.example.com/b.iso".to_owned(),
                "ed2k://|file|c.bin|123|abcdef|/".to_owned(),
            ]
        );
    }

    #[test]
    fn empty_or_unsupported_text_yields_no_urls() {
        assert!(parse_drop_urls("").is_empty());
        assert!(parse_drop_urls("just some notes\nno links here").is_empty());
    }

    #[test]
    fn is_case_insensitive_for_scheme() {
        assert_eq!(
            parse_drop_urls("HTTPS://EXAMPLE.COM/file"),
            vec!["HTTPS://EXAMPLE.COM/file".to_owned()]
        );
    }
}
