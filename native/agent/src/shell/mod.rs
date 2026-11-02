//! agent 系统外壳：托盘可用性、关闭全部 UI 后的驻留策略、按需拉起桌面程序。
//!
//! 驻留语义（`close_to_tray` 偏好，默认开启）：
//! - 托盘可用且开启：关闭全部 UI 只退出界面，agent + daemon 继续运行，托盘常驻；
//! - 托盘不可用或关闭：后台随界面一起退出（界面显式调用 `system.shutdown`；界面异常退出时
//!   由这里的闲置检测兜底）；
//! - headless 构建（未启用 `desktop` feature）没有托盘，后台恒驻留。
//!
//! 托盘本身跑在宿主线程（[`host`]，`desktop` feature），这里只通过 [`TrayPort`] 推送
//! [`TrayModel`] 并消费 [`TrayAction`]。

#[cfg(feature = "desktop")]
pub mod host;
mod prompt_launch;
mod show_window;
#[cfg(feature = "desktop")]
mod tray_model;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fluxdown_protocol::shortcuts::GlobalShortcutsStatusDto;
use fluxdown_protocol::{
    AgentEvent, AgentPreferencesDto, AgentSnapshot, DaemonEvent, ServiceEvent, ShellStatusDto,
    TrayUnavailableReason, method,
};
use serde_json::Value;
use tokio::sync::{Notify, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::daemon_client::DaemonClient;
use crate::event_hub::AgentEventHub;
use crate::lifecycle::Lifecycle;
use crate::power::PowerService;
use prompt_launch::UiLaunches;
use show_window::ShowWindowGuard;

/// 非驻留模式下没有 UI、也没有任何待处理工作多久后完全退出。
const IDLE_EXIT_GRACE: Duration = Duration::from_secs(15);
const CLOSE_TO_TRAY_KEY: &str = "close_to_tray";
/// 官方 UI 的下载提示窗口开关（设备本地偏好，缺省开启）。
const PROGRESS_WINDOW_PREF: &str = "desktop.progress_window";
const COMPLETION_WINDOW_PREF: &str = "desktop.completion_window";

/// 托盘可用性，在 runtime 启动前由宿主确定，进程生命周期内不变。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAvailability {
    Available,
    Unavailable(TrayUnavailableReason),
}

/// 托盘 / 系统事件回流到 runtime 的动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ShowWindow,
    ShowSettings,
    /// 全局快捷键「新建下载」：拉起桌面程序并要求打开新建下载窗口。
    NewDownload,
    PauseAll,
    ResumeAll,
    CancelShutdown,
    Quit,
    /// 完全退出后重新拉起桌面程序（桌面再拉起 agent 与 daemon）。
    Restart,
    /// 系统把链接 / `.torrent` 交给了 agent（macOS 上仅当 agent 被 Launch Services 视为
    /// 处理程序时出现，如未打包的开发布局；打包后这些事件由外层桌面程序接收）。
    OpenUrls(Vec<String>),
    /// 系统注销 / 关机：执行完全退出。
    SessionEnd,
}

/// 托盘的完整展示状态；宿主按值比较后只应用变化部分。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayModel {
    pub visible: bool,
    pub show_window: String,
    pub settings: String,
    pub pause_all: String,
    pub resume_all: String,
    /// `Some` 时显示「取消完成后关机」菜单项。
    pub cancel_shutdown: Option<String>,
    pub restart: String,
    pub quit: String,
    pub tooltip: String,
    /// 托盘图标（跟随应用图标设置；macOS 恒为 [`TrayIconSource::Default`] 单色模板图）。
    pub icon: TrayIconSource,
}

/// 托盘图标来源：默认由宿主按平台 / 明暗选内置图；其余是已缩放好的 PNG。
///
/// 相等性只看 `key`（`bolt` / `custom:<revision>`）：同一个 key 对应同一张图，宿主据此判定是否要
/// 重新 `set_icon`，不比较 PNG 字节。
#[derive(Clone, Debug, Default)]
pub enum TrayIconSource {
    #[default]
    Default,
    Image {
        key: String,
        png: Arc<[u8]>,
    },
}

impl PartialEq for TrayIconSource {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Default, Self::Default) => true,
            (Self::Image { key: left, .. }, Self::Image { key: right, .. }) => left == right,
            _ => false,
        }
    }
}

impl Eq for TrayIconSource {}

/// runtime → 托盘宿主线程的推送端口。
pub trait TrayPort: Send + Sync {
    fn apply(&self, model: &TrayModel);
}

/// 宿主交给 runtime 的外壳能力。
pub struct ShellHost {
    pub availability: TrayAvailability,
    pub port: Option<Arc<dyn TrayPort>>,
    pub actions: Option<mpsc::UnboundedReceiver<TrayAction>>,
    /// 由开机自启拉起（`--autostart`）。
    pub autostart: bool,
    /// 宿主主线程上的全局热键后端承载（仅 macOS 事件循环提供；其它平台后端自带线程）。
    #[cfg(feature = "desktop")]
    pub(crate) hotkey_host: Option<Arc<dyn crate::hotkey::MainThreadHost>>,
}

impl ShellHost {
    /// 没有托盘宿主线程（headless 构建或无图形会话）。
    #[must_use]
    pub fn without_tray(reason: TrayUnavailableReason, autostart: bool) -> Self {
        Self {
            availability: TrayAvailability::Unavailable(reason),
            port: None,
            actions: None,
            autostart,
            #[cfg(feature = "desktop")]
            hotkey_host: None,
        }
    }
}

/// 由托盘可用性与偏好推导外壳状态（快照首帧与后续变化共用）。
#[must_use]
pub fn shell_status(
    availability: TrayAvailability,
    preferences: &AgentPreferencesDto,
) -> ShellStatusDto {
    let close_to_tray = preference_bool(preferences, CLOSE_TO_TRAY_KEY, true);
    match availability {
        TrayAvailability::Available => ShellStatusDto {
            tray_available: true,
            tray_unavailable_reason: None,
            resident: close_to_tray,
            global_shortcuts: GlobalShortcutsStatusDto::default(),
        },
        TrayAvailability::Unavailable(reason) => ShellStatusDto {
            tray_available: false,
            tray_unavailable_reason: Some(reason),
            resident: reason == TrayUnavailableReason::NotBuilt,
            global_shortcuts: GlobalShortcutsStatusDto::default(),
        },
    }
}

fn preference_bool(preferences: &AgentPreferencesDto, key: &str, default: bool) -> bool {
    preferences
        .values
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn task_windows_enabled(preferences: &AgentPreferencesDto) -> bool {
    preference_bool(preferences, PROGRESS_WINDOW_PREF, true)
        || preference_bool(preferences, COMPLETION_WINDOW_PREF, true)
}

fn has_desktop_session(availability: TrayAvailability) -> bool {
    !matches!(
        availability,
        TrayAvailability::Unavailable(
            TrayUnavailableReason::NotBuilt | TrayUnavailableReason::NoDisplay
        )
    )
}

/// gateway / capture / 控制器共享的外壳状态。
pub struct ShellState {
    availability: TrayAvailability,
    ui_clients: AtomicUsize,
    /// 已订阅事件且取得初始快照的官方 UI；hello 本身不足以安全接收瞬时任务意图。
    ready_ui_clients: AtomicUsize,
    resident: AtomicBool,
    tray_visible: AtomicBool,
    /// 为待确认交互 / 下载提示窗口拉起的桌面进程及待交付意图；回收线程在进程退出时更新。
    launches: Arc<Mutex<UiLaunches>>,
    /// 托盘「显示窗口」的单飞拉起；双击 / 成批点击只拉起一个桌面进程。
    show_window: Arc<Mutex<ShowWindowGuard>>,
    /// 本 agent 在当前 daemon 连接上是否已订阅选择请求。
    selection_subscribed: tokio::sync::Mutex<bool>,
    daemon: Arc<DaemonClient>,
    events: AgentEventHub,
    changed: Arc<Notify>,
    /// 「最后一个官方 UI 已断开」的代数，每次断开 +1；应用图标服务记录待办时的代数，等更大的代数再重试
    /// （不用 `Notify`：它会在无人等待时存下许可，让之后才出现的待办立刻误重试）。
    ui_left: tokio::sync::watch::Sender<u64>,
}

impl ShellState {
    #[must_use]
    pub fn new(
        availability: TrayAvailability,
        daemon: Arc<DaemonClient>,
        events: AgentEventHub,
    ) -> Arc<Self> {
        let status = events.inspect(|snapshot| snapshot.shell.clone());
        Arc::new(Self {
            availability,
            ui_clients: AtomicUsize::new(0),
            ready_ui_clients: AtomicUsize::new(0),
            resident: AtomicBool::new(status.resident),
            tray_visible: AtomicBool::new(status.tray_available && status.resident),
            launches: Arc::new(Mutex::new(UiLaunches::default())),
            show_window: Arc::new(Mutex::new(ShowWindowGuard::default())),
            selection_subscribed: tokio::sync::Mutex::new(false),
            daemon,
            events,
            changed: Arc::new(Notify::new()),
            ui_left: tokio::sync::watch::channel(0).0,
        })
    }

    #[must_use]
    pub fn resident(&self) -> bool {
        self.resident.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn ui_clients(&self) -> usize {
        self.ui_clients.load(Ordering::Acquire)
    }

    /// 当前「最后一个官方 UI 已断开」的代数。
    #[must_use]
    pub fn ui_left_generation(&self) -> u64 {
        *self.ui_left.borrow()
    }

    /// 等待代数严格大于 `seen`，返回新的代数。
    pub async fn wait_ui_left_after(&self, seen: u64) -> u64 {
        let mut receiver = self.ui_left.subscribe();
        let newer = receiver
            .wait_for(|generation| *generation > seen)
            .await
            .map(|generation| *generation);
        match newer {
            Ok(generation) => generation,
            // 发送端属于 `self`，不会先于它关闭；万一如此，等于永远没有新的断开。
            Err(_) => std::future::pending().await,
        }
    }

    /// 官方 UI（声明 `client.selections`）完成握手。
    pub async fn ui_connected(&self) {
        self.ui_clients.fetch_add(1, Ordering::AcqRel);
        self.show_window_guard().settled();
        self.reconcile_selection(false).await;
        self.changed.notify_one();
    }

    /// 初始快照成功应答后才重放：订阅早于快照，重放 sequence 严格晚于快照。
    /// 与捕获入队共用同一把锁，避免就绪切换间隙漏掉意图；每个 ID 单独发布，消费后即移除。
    pub fn ui_snapshot_ready(&self) {
        let mut launches = self.launches();
        self.ready_ui_clients.fetch_add(1, Ordering::AcqRel);
        if !self
            .events
            .inspect(|snapshot| task_windows_enabled(&snapshot.preferences))
        {
            launches.clear_tasks();
            return;
        }
        for task_id in launches.take_tasks() {
            self.events
                .publish(AgentEvent::CaptureTasksStarted(vec![task_id]));
        }
    }

    /// 官方 UI 断开。最后一个 UI 断开后留静默期给重连，之后若仍有待确认交互由控制循环拉起界面。
    pub async fn ui_disconnected(&self, was_ready: bool) {
        {
            let mut launches = self.launches();
            if was_ready {
                self.ready_ui_clients.fetch_sub(1, Ordering::AcqRel);
            }
            let previous = self
                .ui_clients
                .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    Some(count.saturating_sub(1))
                })
                .unwrap_or_else(|_| panic!("UI client decrement always supplies an updated count"));
            if previous <= 1 {
                launches.ui_left(Instant::now());
                self.ui_left.send_modify(|generation| *generation += 1);
            }
        }
        self.reconcile_selection(false).await;
        self.changed.notify_one();
    }

    /// 有 UI，或托盘驻留可以随时拉起 UI 时，才订阅 daemon 选择请求；否则 daemon 走默认值。
    async fn reconcile_selection(&self, force: bool) {
        let want = self.ui_clients() > 0 || self.tray_visible.load(Ordering::Acquire);
        let mut subscribed = self.selection_subscribed.lock().await;
        if want == *subscribed && !(force && want) {
            return;
        }
        // 未连接时不调用：首次连接前调用会等待 daemon 就绪，而这里在 UI 握手路径上，
        // 等待会拖住首个快照。连上后 `DaemonConnectionChanged(true)` 强制补订。
        if !self.daemon.is_connected() {
            *subscribed = false;
            return;
        }
        let method_name = if want {
            method::DAEMON_SELECTION_SUBSCRIBE
        } else {
            method::DAEMON_SELECTION_UNSUBSCRIBE
        };
        match self
            .daemon
            .call::<Value, Value>(method_name, Some(serde_json::json!({})))
            .await
        {
            Ok(_) => *subscribed = want,
            Err(error) => {
                tracing::debug!(code = ?error.code, method_name, "selection subscription deferred");
                // daemon 未连接时订阅随连接一起丢失：记为未订阅，重连后强制补订。
                *subscribed = false;
            }
        }
    }

    fn launches(&self) -> MutexGuard<'_, UiLaunches> {
        self.launches.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn show_window_guard(&self) -> MutexGuard<'_, ShowWindowGuard> {
        self.show_window
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// 待确认交互发生变化：确认拉起重新计数。
    fn prompts_changed(&self) {
        self.launches().prompts_changed();
    }

    /// 电平触发的确认拉起：有待确认交互、没有 UI 连接时，按 [`UiLaunches`] 的约束拉起界面。
    /// 驻留模式只开确认窗口，非驻留模式开主窗口（没有界面就没有人承载接下来的下载）。
    fn reconcile_prompt(&self, now: Instant) {
        if self.ui_clients() > 0 {
            return;
        }
        let tray_visible = self.tray_visible.load(Ordering::Acquire);
        if !self
            .events
            .inspect(|snapshot| needs_prompt_ui(snapshot, tray_visible))
        {
            return;
        }
        {
            let mut launches = self.launches();
            if !launches.prompt_due(now) {
                return;
            }
            launches.begin_prompt(now);
        }
        let args: &[&str] = if self.resident() { &["--capture"] } else { &[] };
        if let Err(error) = self.launch_tracked(args) {
            tracing::warn!(error = %error, "could not launch desktop for pending prompt");
        }
    }

    /// 单任务捕获需要进度或完成提示时登记意图；批量捕获不调用此入口。
    /// hello 后尚未取快照的 UI 仍需补投，已就绪 UI 则继续消费原始 CaptureTasksStarted。
    pub fn launch_for_task_windows(&self, task_id: &str) {
        if !has_desktop_session(self.availability) {
            return;
        }
        {
            let mut launches = self.launches();
            if !self
                .events
                .inspect(|snapshot| task_windows_enabled(&snapshot.preferences))
            {
                launches.clear_tasks();
                return;
            }
            if self.ready_ui_clients.load(Ordering::Acquire) > 0 {
                return;
            }
            launches.queue_task(task_id);
        }
        self.reconcile_task_windows(Instant::now());
        self.changed.notify_one();
    }

    /// 与确认窗口共享单飞进程、退出回调和静默期；剩余重试由已有控制循环节拍触发。
    fn reconcile_task_windows(&self, now: Instant) {
        if self.ui_clients() > 0 || !has_desktop_session(self.availability) {
            return;
        }
        let task_id = {
            let mut launches = self.launches();
            if !self
                .events
                .inspect(|snapshot| task_windows_enabled(&snapshot.preferences))
            {
                launches.clear_tasks();
                return;
            }
            let Some(task_id) = launches.begin_task(now) else {
                return;
            };
            task_id
        };
        // 首项保活兼容现有命令行；首个快照后也会重放，UI 必须幂等消费。
        let resident_args = ["--capture", "--progress-task", task_id.as_str()];
        let foreground_args = ["--progress-task", task_id.as_str()];
        let args = if self.resident() {
            resident_args.as_slice()
        } else {
            foreground_args.as_slice()
        };
        if let Err(error) = self.launch_tracked(args) {
            tracing::warn!(error = %error, "could not launch desktop for task windows");
        }
    }

    /// 拉起已在 [`UiLaunches`] 登记的桌面进程；进程退出（或没能启动）时释放登记并唤醒控制循环
    /// 重新判定。
    fn launch_tracked(&self, args: &[&str]) -> Result<(), crate::platform::PlatformError> {
        let launches = Arc::clone(&self.launches);
        let changed = Arc::clone(&self.changed);
        let launched = crate::platform::launch_desktop_observed(args, move || {
            launches
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .finished();
            changed.notify_one();
        });
        if launched.is_err() {
            self.launches().finished();
        }
        launched
    }

    /// 显示（或新开）主窗口。上一次拉起尚未落定（进程未退出、UI 未连上）或仍在双击间隔内时
    /// 忽略：已运行的界面会被那次拉起唤起。
    pub fn show_window(&self) {
        if !self.show_window_guard().try_begin(Instant::now()) {
            tracing::debug!("desktop launch for show-window already in flight; coalesced");
            return;
        }
        let guard = Arc::clone(&self.show_window);
        let launched = crate::platform::launch_desktop_observed(&[], move || {
            guard
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .settled();
        });
        if let Err(error) = launched {
            self.show_window_guard().settled();
            tracing::warn!(error = %error, "could not launch desktop");
        }
    }

    /// 偏好变化后重算驻留与托盘可见；有变化时发布 `ShellChanged` 并返回 `true`。
    async fn refresh(&self) -> bool {
        if !self
            .events
            .inspect(|snapshot| task_windows_enabled(&snapshot.preferences))
        {
            self.launches().clear_tasks();
        }
        let (current, next) = self.events.inspect(|snapshot| {
            let mut next = shell_status(self.availability, &snapshot.preferences);
            // 全局快捷键状态由热键控制器发布，这里只推导托盘 / 驻留，不得覆盖它。
            next.global_shortcuts = snapshot.shell.global_shortcuts.clone();
            (snapshot.shell.clone(), next)
        });
        self.resident.store(next.resident, Ordering::Release);
        self.tray_visible
            .store(next.tray_available && next.resident, Ordering::Release);
        if current == next {
            return false;
        }
        self.events.publish(AgentEvent::ShellChanged(next));
        self.reconcile_selection(false).await;
        true
    }
}

/// 外壳控制器依赖的服务。
pub struct ShellServices {
    pub state: Arc<ShellState>,
    pub lifecycle: Arc<Lifecycle>,
    pub power: Arc<PowerService>,
    pub daemon: Arc<DaemonClient>,
    pub events: AgentEventHub,
    pub gateway: Arc<crate::gateway::GatewayService>,
    /// agent 数据目录（`icons/` 下的应用图标文件）。
    pub agent_data_dir: std::path::PathBuf,
}

/// 外壳控制循环：偏好 → 驻留 / 托盘展示；托盘动作；待确认交互拉起界面；非驻留闲置退出。
pub async fn run_controller(
    services: ShellServices,
    mut host: ShellHost,
    cancel: CancellationToken,
) {
    let ShellServices {
        state,
        lifecycle,
        power,
        daemon,
        events,
        gateway,
        agent_data_dir,
    } = services;
    #[cfg(feature = "desktop")]
    let mut tray = tray_model::TrayPresenter::new(host.port.take(), agent_data_dir);
    #[cfg(not(feature = "desktop"))]
    let _ = (host.port.take(), agent_data_dir);
    let mut actions = host.actions.take();
    #[cfg(feature = "desktop")]
    let mut hotkeys = crate::hotkey::HotkeyController::start(
        has_desktop_session(host.availability),
        host.hotkey_host.take(),
    );
    #[cfg(not(feature = "desktop"))]
    let mut hotkeys = NoHotkeys;
    let (mut receiver, _) = events.subscribe_and_snapshot();

    state.refresh().await;
    state.reconcile_selection(true).await;
    if host.autostart {
        start_from_autostart(&state);
    }
    #[cfg(feature = "desktop")]
    {
        tray.update(&events);
        hotkeys.sync(&events);
    }

    let mut idle_since: Option<Instant> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            frame = receiver.recv() => match frame {
                Ok(frame) => handle_frame(&state, &frame.event).await,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // 断档期间可能错过待确认交互的变化：按变化处理，确认拉起重新计数。
                    state.prompts_changed();
                    state.reconcile_selection(true).await;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            action = receive_action(&mut actions), if actions.is_some() => match action {
                Some(action) => {
                    handle_action(action, &state, &lifecycle, &power, &daemon, &gateway).await;
                }
                None => actions = None,
            },
            _ = state.changed.notified() => {}
            action = hotkey_action(&mut hotkeys) => {
                if let Some(action) = action {
                    handle_action(action, &state, &lifecycle, &power, &daemon, &gateway).await;
                }
            }
            _ = tick.tick() => {
                if should_idle_exit(&state, &events, &mut idle_since, Instant::now()) {
                    tracing::info!("no UI and no pending work outside tray residency: quitting");
                    lifecycle.request_quit();
                }
            }
        }
        state.refresh().await;
        // 完全退出流程中不再拉起界面：桌面进程会把刚关停的后台重新拉起来。
        if !lifecycle.quit_requested() {
            state.reconcile_prompt(Instant::now());
            state.reconcile_task_windows(Instant::now());
        }
        #[cfg(feature = "desktop")]
        {
            tray.update(&events);
            hotkeys.sync(&events);
        }
    }
    state.launches().clear_tasks();
}

async fn receive_action(
    actions: &mut Option<mpsc::UnboundedReceiver<TrayAction>>,
) -> Option<TrayAction> {
    match actions {
        Some(actions) => actions.recv().await,
        None => std::future::pending().await,
    }
}

/// 下一个全局快捷键触发的外壳动作；后端状态回报返回 `None`（由控制循环尾部发布）。
#[cfg(feature = "desktop")]
async fn hotkey_action(hotkeys: &mut crate::hotkey::HotkeyController) -> Option<TrayAction> {
    match hotkeys.recv().await {
        crate::hotkey::HotkeyEvent::Fire(action) => Some(action),
        crate::hotkey::HotkeyEvent::Reported => None,
    }
}

/// headless 构建没有全局快捷键。
#[cfg(not(feature = "desktop"))]
struct NoHotkeys;

#[cfg(not(feature = "desktop"))]
async fn hotkey_action(_: &mut NoHotkeys) -> Option<TrayAction> {
    std::future::pending().await
}

async fn handle_frame(state: &ShellState, event: &ServiceEvent) {
    let ServiceEvent::Agent(event) = event else {
        return;
    };
    match event {
        AgentEvent::PendingCapturesChanged(_)
        | AgentEvent::Daemon(
            DaemonEvent::SelectionPending(_) | DaemonEvent::SelectionResolved { .. },
        ) => state.prompts_changed(),
        // daemon 重连后选择订阅随旧连接丢失，必须补订；替换的快照可能带来新的选择请求。
        AgentEvent::DaemonSnapshotReplaced(_) => {
            state.prompts_changed();
            state.reconcile_selection(true).await;
        }
        AgentEvent::DaemonConnectionChanged(true) => {
            state.reconcile_selection(true).await;
        }
        _ => {}
    }
}

async fn handle_action(
    action: TrayAction,
    state: &ShellState,
    lifecycle: &Arc<Lifecycle>,
    power: &PowerService,
    daemon: &DaemonClient,
    gateway: &crate::gateway::GatewayService,
) {
    match action {
        TrayAction::ShowWindow => state.show_window(),
        TrayAction::ShowSettings => {
            if let Err(error) = crate::platform::launch_desktop(&["--settings"]) {
                tracing::warn!(error = %error, "could not launch desktop settings");
            }
        }
        TrayAction::NewDownload => {
            if let Err(error) = crate::platform::launch_desktop(&["--new-download"]) {
                tracing::warn!(error = %error, "could not launch desktop new-download window");
            }
        }
        TrayAction::PauseAll => call_daemon(daemon, method::DAEMON_TASK_PAUSE_ALL).await,
        TrayAction::ResumeAll => call_daemon(daemon, method::DAEMON_TASK_RESUME_ALL).await,
        TrayAction::CancelShutdown => power.disarm(),
        TrayAction::Quit | TrayAction::SessionEnd => lifecycle.request_quit(),
        TrayAction::Restart => restart_agent(lifecycle),
        TrayAction::OpenUrls(urls) => {
            submit_opened_urls(gateway, urls).await;
            if !state.resident() && state.ui_clients() == 0 {
                state.show_window();
            }
        }
    }
}

async fn call_daemon(daemon: &DaemonClient, method_name: &str) {
    if let Err(error) = daemon.call::<Value, Value>(method_name, None).await {
        tracing::warn!(code = ?error.code, method_name, "tray daemon action failed");
    }
}

/// 托盘「重启」：登记非更新的重启计划后走完全退出（daemon 优雅关停、下载状态照常持久化），
/// agent 运行期结束后由 `main` 重新拉起桌面程序。已在退出流程中（含更新重启）时忽略，避免
/// 覆盖已登记的计划；拿不到任何可重启的程序时保持当前运行，不退出。
fn restart_agent(lifecycle: &Arc<Lifecycle>) {
    if lifecycle.quit_requested() {
        tracing::debug!("restart ignored: a full quit is already in progress");
        return;
    }
    let Some(plan) = crate::update::restart::desktop_relaunch() else {
        tracing::error!("restart unavailable: cannot locate a program to start; keeping running");
        return;
    };
    crate::update::restart::schedule(plan);
    lifecycle.request_restart();
}

/// 与桌面进程处理系统交来的链接一致：`.torrent` 文件与可捕获链接都静默建任务，并声明来源
/// 关联（[`OpenAssociation`]），已被用户关闭的关联由网关拦截。
///
/// [`OpenAssociation`]: fluxdown_protocol::capture_link::OpenAssociation
async fn submit_opened_urls(gateway: &crate::gateway::GatewayService, urls: Vec<String>) {
    use fluxdown_protocol::capture_link::{
        OpenAssociation, deep_link_file_name, is_capture_url, normalize_capture_url,
        torrent_file_path,
    };
    for url in urls {
        let result = if let Some(path) = torrent_file_path(&url) {
            gateway
                .dispatch_local(
                    method::AGENT_CAPTURE_SUBMIT_TORRENT_FILE,
                    serde_json::json!({
                        "path": path.display().to_string(),
                        "silent": true,
                        "association": OpenAssociation::Torrent,
                    }),
                )
                .await
        } else if is_capture_url(&url) {
            gateway
                .dispatch_local(
                    method::AGENT_CAPTURE_SUBMIT,
                    serde_json::json!({
                        "request": {
                            "url": normalize_capture_url(&url),
                            "filename": deep_link_file_name(&url).unwrap_or_default(),
                        },
                        "silent": true,
                        "association": OpenAssociation::of_url(&url),
                    }),
                )
                .await
        } else {
            continue;
        };
        if let Err(error) = result {
            tracing::warn!(code = ?error.code, "could not submit opened link");
        }
    }
}

/// 开机自启：托盘可见（可用且驻留）时只驻留托盘，用户随时可从托盘打开窗口；托盘不可见时
/// 没有其它入口，拉起最小化的主窗口。`start_minimized_to_tray` 只决定手动冷启动是否开窗。
fn start_from_autostart(state: &ShellState) {
    if state.tray_visible.load(Ordering::Acquire) {
        tracing::info!("started at login; staying in the tray");
        return;
    }
    if let Err(error) = crate::platform::launch_desktop(&["--minimized"]) {
        tracing::warn!(error = %error, "could not launch desktop at login");
    }
}

/// 非驻留（托盘构建但托盘不可用或已关闭 `close_to_tray`）时，没有 UI 且没有待处理工作
/// 持续 [`IDLE_EXIT_GRACE`] 即完全退出；有下载 / 捕获 / 选择 / 关机计划时一直等待。
fn should_idle_exit(
    state: &ShellState,
    events: &AgentEventHub,
    idle_since: &mut Option<Instant>,
    now: Instant,
) -> bool {
    let eligible = state.availability
        != TrayAvailability::Unavailable(TrayUnavailableReason::NotBuilt)
        && !state.resident()
        && state.ui_clients() == 0
        && events.inspect(snapshot_is_idle);
    if !eligible {
        *idle_since = None;
        return false;
    }
    let since = *idle_since.get_or_insert(now);
    now.saturating_duration_since(since) >= IDLE_EXIT_GRACE
}

fn snapshot_is_idle(snapshot: &AgentSnapshot) -> bool {
    let stats = &snapshot.daemon.runtime_stats;
    // 自动重试间隙的任务在任务表里是失败状态，只在 `retry_pending_tasks` 里可见。
    stats.active_tasks == 0
        && stats.pending_tasks == 0
        && stats.retry_pending_tasks == 0
        && snapshot.pending_captures.is_empty()
        && snapshot.daemon.pending_selections.is_empty()
        && snapshot.power.armed_delay_secs.is_none()
}

/// 是否有必须由界面承载的待确认交互：外部捕获恒需要；引擎选择只在托盘驻留、由 agent 代为订阅
/// 时才需要拉起界面（非驻留且无 UI 时 agent 不订阅，daemon 直接走默认值）。
fn needs_prompt_ui(snapshot: &AgentSnapshot, tray_visible: bool) -> bool {
    !snapshot.pending_captures.is_empty()
        || (tray_visible && !snapshot.daemon.pending_selections.is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn preferences(values: &[(&str, Value)]) -> AgentPreferencesDto {
        AgentPreferencesDto {
            revision: 1,
            values: values
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn either_task_window_preference_allows_launch() {
        assert!(task_windows_enabled(&preferences(&[])));
        assert!(task_windows_enabled(&preferences(&[
            (PROGRESS_WINDOW_PREF, Value::Bool(false)),
            (COMPLETION_WINDOW_PREF, Value::Bool(true)),
        ])));
        assert!(task_windows_enabled(&preferences(&[
            (PROGRESS_WINDOW_PREF, Value::Bool(true)),
            (COMPLETION_WINDOW_PREF, Value::Bool(false)),
        ])));
        assert!(!task_windows_enabled(&preferences(&[
            (PROGRESS_WINDOW_PREF, Value::Bool(false)),
            (COMPLETION_WINDOW_PREF, Value::Bool(false)),
        ])));
    }

    #[test]
    fn task_windows_require_a_desktop_session_not_a_working_tray() {
        assert!(has_desktop_session(TrayAvailability::Available));
        for reason in [
            TrayUnavailableReason::NoHost,
            TrayUnavailableReason::InitFailed,
        ] {
            assert!(has_desktop_session(TrayAvailability::Unavailable(reason)));
        }
        for reason in [
            TrayUnavailableReason::NotBuilt,
            TrayUnavailableReason::NoDisplay,
        ] {
            assert!(!has_desktop_session(TrayAvailability::Unavailable(reason)));
        }
    }

    #[tokio::test]
    async fn task_intents_replay_individually_after_snapshot_and_not_again_on_reconnect() {
        let events = AgentEventHub::new(AgentSnapshot {
            preferences: preferences(&[
                (PROGRESS_WINDOW_PREF, Value::Bool(false)),
                (COMPLETION_WINDOW_PREF, Value::Bool(true)),
            ]),
            ..AgentSnapshot::default()
        });
        let state = ShellState::new(
            TrayAvailability::Available,
            Arc::new(DaemonClient::disconnected()),
            events.clone(),
        );
        // 首项已经在单飞启动队列里；hello 尚不足以消费它。
        state.launches().queue_task("before-hello");
        state.ui_connected().await;
        let (mut receiver, _) = events.subscribe_and_snapshot();
        state.launch_for_task_windows("during-hello");
        // 握手后、快照前断线不能消费队列；下一条连接仍能取得全部意图。
        state.ui_disconnected(false).await;
        state.ui_connected().await;
        let baseline = events.snapshot();
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        state.ui_snapshot_ready();
        for (offset, task_id) in ["before-hello", "during-hello"].into_iter().enumerate() {
            let frame = receiver.try_recv().expect("replayed task intent");
            assert_eq!(frame.sequence, baseline.sequence + offset as u64 + 1);
            assert!(matches!(
                frame.event,
                ServiceEvent::Agent(AgentEvent::CaptureTasksStarted(ids)) if ids == [task_id]
            ));
        }
        state.launch_for_task_windows("already-ready");
        assert!(state.launches().take_tasks().is_empty());
        state.ui_disconnected(true).await;
        state.ui_connected().await;
        state.launch_for_task_windows("during-reconnect");
        state.ui_snapshot_ready();
        let frame = receiver.try_recv().expect("new reconnect intent");
        assert!(matches!(
            frame.event,
            ServiceEvent::Agent(AgentEvent::CaptureTasksStarted(ids)) if ids == ["during-reconnect"]
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        state.ui_disconnected(true).await;
    }

    #[tokio::test]
    async fn disabling_both_windows_clears_unshown_task_intents() {
        let events = AgentEventHub::new(AgentSnapshot {
            preferences: preferences(&[
                (PROGRESS_WINDOW_PREF, Value::Bool(false)),
                (COMPLETION_WINDOW_PREF, Value::Bool(false)),
            ]),
            ..AgentSnapshot::default()
        });
        let state = ShellState::new(
            TrayAvailability::Available,
            Arc::new(DaemonClient::disconnected()),
            events.clone(),
        );
        state.launches().queue_task("before-disabled");
        state.refresh().await;
        assert!(state.launches().take_tasks().is_empty());
        state.launch_for_task_windows("while-disabled");
        assert!(state.launches().take_tasks().is_empty());
        let (mut receiver, _) = events.subscribe_and_snapshot();
        state.ui_connected().await;
        state.ui_snapshot_ready();
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        state.ui_disconnected(true).await;
    }

    #[test]
    fn available_tray_follows_close_to_tray_defaulting_to_resident() {
        let status = shell_status(TrayAvailability::Available, &preferences(&[]));
        assert!(status.tray_available && status.resident);
        let status = shell_status(
            TrayAvailability::Available,
            &preferences(&[(CLOSE_TO_TRAY_KEY, Value::Bool(false))]),
        );
        assert!(status.tray_available && !status.resident);
    }

    #[test]
    fn unavailable_tray_quits_with_ui_except_headless_builds() {
        let on = preferences(&[(CLOSE_TO_TRAY_KEY, Value::Bool(true))]);
        let no_host = shell_status(
            TrayAvailability::Unavailable(TrayUnavailableReason::NoHost),
            &on,
        );
        assert!(!no_host.resident);
        assert_eq!(
            no_host.tray_unavailable_reason,
            Some(TrayUnavailableReason::NoHost)
        );
        let headless = shell_status(
            TrayAvailability::Unavailable(TrayUnavailableReason::NotBuilt),
            &preferences(&[(CLOSE_TO_TRAY_KEY, Value::Bool(false))]),
        );
        assert!(
            headless.resident,
            "headless agents stay resident regardless of prefs"
        );
    }

    #[test]
    fn idle_requires_no_tasks_captures_selections_or_shutdown_plan() {
        let mut snapshot = AgentSnapshot::default();
        assert!(snapshot_is_idle(&snapshot));
        snapshot.daemon.runtime_stats.pending_tasks = 1;
        assert!(!snapshot_is_idle(&snapshot));
        snapshot.daemon.runtime_stats.pending_tasks = 0;
        snapshot.daemon.runtime_stats.retry_pending_tasks = 1;
        assert!(!snapshot_is_idle(&snapshot));
        snapshot.daemon.runtime_stats.retry_pending_tasks = 0;
        snapshot.power.armed_delay_secs = Some(0);
        assert!(!snapshot_is_idle(&snapshot));
    }

    #[test]
    fn captures_always_need_ui_but_selections_only_with_visible_tray() {
        let mut snapshot = AgentSnapshot::default();
        assert!(!needs_prompt_ui(&snapshot, true));
        snapshot.daemon.pending_selections.push(
            serde_json::from_value(serde_json::json!({
                "requestId": "selection-1",
                "taskId": "task-1",
                "kind": { "type": "hls", "options": [] },
                "defaultChoice": { "kind": "hls", "index": 0 },
                "deadlineUnixMs": 0,
            }))
            .expect("selection request"),
        );
        assert!(needs_prompt_ui(&snapshot, true));
        assert!(!needs_prompt_ui(&snapshot, false));
        snapshot.daemon.pending_selections.clear();
        snapshot.pending_captures.push(
            serde_json::from_value(serde_json::json!({
                "transactionId": "capture-1",
                "url": "https://example.com/a.zip",
                "createdAtUnixMs": 0,
            }))
            .expect("pending capture"),
        );
        assert!(needs_prompt_ui(&snapshot, false));
    }

    #[tokio::test]
    async fn idle_exit_waits_for_grace_and_resets_on_ui() {
        let events = AgentEventHub::new(AgentSnapshot {
            shell: shell_status(
                TrayAvailability::Unavailable(TrayUnavailableReason::NoHost),
                &AgentPreferencesDto::default(),
            ),
            ..AgentSnapshot::default()
        });
        let state = ShellState::new(
            TrayAvailability::Unavailable(TrayUnavailableReason::NoHost),
            Arc::new(DaemonClient::disconnected()),
            events.clone(),
        );
        let start = Instant::now();
        let mut idle_since = None;
        assert!(!should_idle_exit(&state, &events, &mut idle_since, start));
        assert!(!should_idle_exit(
            &state,
            &events,
            &mut idle_since,
            start + IDLE_EXIT_GRACE - Duration::from_secs(1)
        ));
        state.ui_connected().await;
        assert!(!should_idle_exit(
            &state,
            &events,
            &mut idle_since,
            start + IDLE_EXIT_GRACE
        ));
        state.ui_disconnected(false).await;
        let reconnect = start + IDLE_EXIT_GRACE;
        assert!(!should_idle_exit(
            &state,
            &events,
            &mut idle_since,
            reconnect
        ));
        assert!(should_idle_exit(
            &state,
            &events,
            &mut idle_since,
            reconnect + IDLE_EXIT_GRACE
        ));
    }

    #[tokio::test]
    async fn resident_and_headless_agents_never_idle_exit() {
        for availability in [
            TrayAvailability::Available,
            TrayAvailability::Unavailable(TrayUnavailableReason::NotBuilt),
        ] {
            let events = AgentEventHub::new(AgentSnapshot {
                shell: shell_status(availability, &AgentPreferencesDto::default()),
                ..AgentSnapshot::default()
            });
            let state = ShellState::new(
                availability,
                Arc::new(DaemonClient::disconnected()),
                events.clone(),
            );
            let mut idle_since = None;
            let start = Instant::now();
            should_idle_exit(&state, &events, &mut idle_since, start);
            assert!(!should_idle_exit(
                &state,
                &events,
                &mut idle_since,
                start + IDLE_EXIT_GRACE * 4
            ));
        }
    }
}
