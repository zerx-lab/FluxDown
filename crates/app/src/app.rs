//! composition root：一个 agent 会话、一个窗口注册表、全局菜单与动作；各窗口按需装配。

use std::{
    collections::BTreeMap,
    env,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use fluxdown_protocol::{
    AgentEvent, DaemonEvent, DaemonRuntimeStatsDto, ServiceEvent, ShellStatusDto,
};
use fluxdown_ui_account::AccountHost;
use fluxdown_ui_downloads::DownloadView;
use fluxdown_ui_i18n::{I18nCatalog, I18nError, Translator, system_locale};
use fluxdown_ui_settings::{SettingsStore, SettingsView, SlotVisibility, component_locale};
use fluxdown_ui_shell::ShellView;
use gpui::{App, AppContext as _, Entity, Global, WeakEntity};
use tokio::sync::mpsc;

use crate::agent_client::{AgentClient, AgentClientConfig, AgentClientError};
use crate::assets::DesktopAssets;
use crate::instance_ipc::{self, ActivateMessage, ActivationRequest, Endpoint};
use crate::launch::{self, LaunchOptions};
use crate::service_bootstrap::ServiceBootstrap;
use crate::session::{AgentSession, SessionSignal, attach};
use crate::settings_port::AgentSettingsPort;
use crate::windows::WindowRegistry;

/// 事件泵单次批量上限。
const EVENT_BATCH: usize = 256;
/// 次实例等待刚启动主实例的 IPC 端点就绪、或等待旧主实例释放锁的最长时间。
const ACTIVATION_RETRY_TIMEOUT: Duration = Duration::from_secs(5);
const ACTIVATION_RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// `--after-update` / `--relaunch`：旧桌面进程随 agent 整体退出而退出，等它释放单实例锁的最长
/// 时间与轮询间隔。
const AFTER_UPDATE_LOCK_WAIT: Duration = Duration::from_secs(30);
const AFTER_UPDATE_LOCK_INTERVAL: Duration = Duration::from_millis(100);
/// agent 拉起的确认 / 进度界面（`--capture` / `--progress-task`）等旧主实例释放单实例锁的最长
/// 时间：agent 只在没有 UI 连接时拉起，正在退出的旧实例会在此期间释放；仍在运行（只是在重连
/// agent）的旧实例则超时后照常接收转发。
const AGENT_LAUNCH_LOCK_WAIT: Duration = Duration::from_secs(5);

/// 桌面入口完成后的进程语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunOutcome {
    Completed,
    NoPrimary,
}

/// 任何协调故障都必须阻止第二个 UI 启动。
#[derive(Debug, thiserror::Error)]
pub(crate) enum AppError {
    #[error(transparent)]
    I18n(#[from] I18nError),
    #[error("FluxDown desktop instance lock unavailable")]
    InstanceLock(#[source] std::io::Error),
    #[error("FluxDown desktop activation unavailable")]
    Activation(#[source] instance_ipc::SendError),
    #[error("FluxDown desktop activation listener unavailable")]
    ActivationListener(#[source] std::io::Error),
    #[error("FluxDown agent client could not start")]
    AgentClient(#[source] AgentClientError),
}

enum LaunchDisposition {
    Primary(launch::InstanceLock),
    Activated,
    NoPrimary,
}

/// 跨窗口共享的应用状态（composition root 独有）。
pub(crate) struct Desktop {
    pub translator: Entity<Translator>,
    pub session: Entity<AgentSession>,
    pub client: Arc<AgentClient>,
    pub settings_store: Entity<SettingsStore>,
    /// 账户 / 设备 / 局域网配对状态：设置页与「添加设备」对话框共享。
    pub account_host: Entity<AccountHost>,
    /// 设置侧栏「推介有奖」页的可见性（已登录 + 设备本地偏好），设置窗口与命令面板共用。
    pub referral_visibility: Entity<SlotVisibility>,
    /// 主窗口内的下载页（主窗口关闭后失效）。
    pub main_downloads: Option<WeakEntity<DownloadView>>,
    pub main_shell: Option<WeakEntity<ShellView>>,
    /// 设置窗口内的设置页（窗口关闭后失效）：命令面板据此定位设置项。
    pub settings_view: Option<WeakEntity<SettingsView>>,
    /// 最新运行时统计（关窗 / 退出提示用）。
    pub runtime_stats: DaemonRuntimeStatsDto,
    /// agent 托盘可用性与驻留策略：决定关闭主窗口是只退出界面还是完全退出。
    pub shell: ShellStatusDto,
    /// 已进入退出流程：窗口关闭回调不再重复触发退出。
    pub quitting: bool,
}

impl Global for Desktop {}

impl Desktop {
    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub fn global_mut(cx: &mut App) -> &mut Self {
        cx.global_mut::<Self>()
    }

    pub fn active_task_count(cx: &App) -> u32 {
        Self::global(cx).runtime_stats.active_tasks
    }

    /// 当前偏好：`SettingsStore` 的读视图（agent 快照 / 事件 + 本进程尚未回执的本地写入）。
    pub fn preferences(cx: &App) -> &BTreeMap<String, serde_json::Value> {
        Self::global(cx).settings_store.read(cx).preferences()
    }

    pub fn pref(cx: &App, key: &str) -> Option<serde_json::Value> {
        Self::preferences(cx).get(key).cloned()
    }

    /// 写偏好：界面状态由偏好派生，任何改动偏好派生状态的入口都只写这里，
    /// 由 [`observe_preferences`] 统一投影，避免「内存已改、偏好未写」被下一次快照回弹。
    pub fn set_pref(cx: &mut App, key: &str, value: serde_json::Value) {
        let store = Self::global(cx).settings_store.clone();
        store.update(cx, |store, cx| store.set_pref(key, value, cx));
    }
}

pub(crate) fn run() -> Result<RunOutcome, AppError> {
    let launch = LaunchOptions::from_args(env::args().skip(1));
    let token_path = agent_token_path();
    let instance_dir = launch::instance_dir(&token_path);
    let endpoint = Endpoint::for_instance_dir(&instance_dir);
    let message = ActivateMessage {
        urls: launch.urls.clone(),
        files: launch.torrent_files.clone(),
        activate: launch.activate_existing || !launch.capture_only,
        settings: launch.settings,
        new_download: launch.new_download,
        progress_task: launch.progress_task.clone(),
    };
    let lock_wait = if launch.after_update || launch.relaunch {
        Some(AFTER_UPDATE_LOCK_WAIT)
    } else if launch.agent_initiated() {
        Some(AGENT_LAUNCH_LOCK_WAIT)
    } else {
        None
    };
    let _instance_lock = match acquire_or_activate(
        &instance_dir,
        &endpoint,
        &message,
        launch.activate_existing,
        lock_wait,
    )? {
        LaunchDisposition::Primary(lock) => lock,
        LaunchDisposition::Activated => {
            log::info!("another desktop instance is primary; request forwarded, exiting");
            return Ok(RunOutcome::Completed);
        }
        LaunchDisposition::NoPrimary => {
            log::info!("--activate-existing without a primary instance; exiting");
            return Ok(RunOutcome::NoPrimary);
        }
    };
    let (activate_tx, mut activate_rx) = mpsc::channel::<ActivationRequest>(16);
    // Unix sockets can bind before Tokio starts, so parallel starters are queued immediately.
    #[cfg(unix)]
    let listener =
        instance_ipc::Listener::bind(endpoint.clone()).map_err(AppError::ActivationListener)?;
    let agent_config = AgentClientConfig {
        rpc_url: match env::var("FLUXDOWN_AGENT_URL") {
            Ok(url) => Some(url),
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(AppError::AgentClient(AgentClientError::Configuration(
                    "FLUXDOWN_AGENT_URL is not valid UTF-8".to_owned(),
                )));
            }
        },
        bearer_path: token_path,
    };

    let catalog = Arc::new(I18nCatalog::load_embedded()?);
    let translator = catalog.translator(&system_locale());
    let locale = component_locale(translator.locale()).to_owned();

    let bootstrap = Arc::new(ServiceBootstrap::new());
    let (agent_client, mut agent_events) =
        AgentClient::start(agent_config, bootstrap.clone()).map_err(AppError::AgentClient)?;
    #[cfg(unix)]
    agent_client.spawn_background(listener.listen(activate_tx));
    #[cfg(windows)]
    start_windows_listener(&agent_client, endpoint, activate_tx)
        .map_err(AppError::ActivationListener)?;
    let launch_submissions = submit_captures_detached(
        &agent_client,
        launch.urls.clone(),
        launch.torrent_files.clone(),
    );
    let open_urls_client = agent_client.clone();

    // GPUI 在非 macOS 默认关掉最后一个窗口即退出；必须让 lifecycle 等待在途提交。
    let application = desktop_application()
        .with_quit_mode(gpui::QuitMode::Explicit)
        .with_assets(DesktopAssets);
    application.on_open_urls(move |urls| {
        let files = urls
            .iter()
            .filter_map(|url| launch::torrent_path(url))
            .collect();
        let urls = urls
            .into_iter()
            .filter(|url| fluxdown_protocol::capture_link::is_capture_url(url))
            .collect();
        drop(submit_captures_detached(&open_urls_client, urls, files));
    });
    application.on_reopen(|cx| {
        if cx.has_global::<Desktop>() {
            crate::windows::main::reveal(cx);
        }
    });
    application.run(move |cx| {
        // gpui-shell 组件目录的初始化内含 `gpui_component::init`，故整体替代它并先于主题初始化；
        // 失败只让插件界面不可用（组件仍已初始化）。
        if let Err(error) = fluxdown_ui_plugin_ui::init(
            cx,
            Arc::new(crate::capability_ports::AgentPluginUiPort::new(
                agent_client.clone(),
            )),
            DesktopPaths::from_env().data_root.join("plugin-ui"),
        ) {
            log::error!("plugin desktop interfaces unavailable: {error}");
        }
        crate::logging::install_ui_watchdog(cx);
        crate::app_icon::install(cx);
        // 先定汉字字体偏好再装主题：首次安装即用对的主字体与回退链，不重装第二次。
        fluxdown_ui_theme::set_han_font_preference(han_font_preference(&translator), cx);
        fluxdown_ui_theme::init(cx);
        gpui_component::set_locale(&locale);
        let translator = cx.new(|_| translator);
        fluxdown_ui_plugin_ui::set_translator(cx, translator.clone());
        let session = cx.new(|cx| AgentSession::new(agent_client.clone(), cx));
        WindowRegistry::init(cx, agent_client.clone());

        // 设置存储跨窗口存活：窗口关闭后防抖中的写回仍完成，快照/事件持续进入。
        let settings_store =
            cx.new(|_| SettingsStore::new(Arc::new(AgentSettingsPort::new(agent_client.clone()))));
        attach(&session, &settings_store, cx);
        let account_host = crate::account_host::install(&translator, &session, &agent_client, cx);
        let quit_store = settings_store.clone();
        cx.on_app_quit(move |cx| {
            let calls = quit_store.update(cx, |store, _| store.drain_pending_calls());
            let shutdown = crate::lifecycle::shutdown_request_on_app_quit(cx);
            async move {
                for call in calls {
                    if let Err(error) = call.await {
                        // 退出写回尽力完成全部键，不能让首个失败阻断其余提交。
                        log::warn!("settings flush on desktop exit failed: {:?}", error.code);
                    }
                }
                if let Some(shutdown) = shutdown
                    && let Err(error) = shutdown.await
                {
                    log::warn!("shutdown request on desktop exit failed: {:?}", error.code);
                }
            }
        })
        .detach();

        crate::shortcuts::install(cx, &settings_store, &translator);
        crate::menus::install(cx, &translator);
        crate::menus::install_global_actions(cx);
        let referral_visibility = cx.new(|_| SlotVisibility::default());
        cx.set_global(Desktop {
            translator: translator.clone(),
            session: session.clone(),
            client: agent_client.clone(),
            settings_store,
            account_host,
            referral_visibility,
            main_downloads: None,
            main_shell: None,
            settings_view: None,
            runtime_stats: DaemonRuntimeStatsDto::default(),
            shell: ShellStatusDto::default(),
            quitting: false,
        });

        // 会话 → 运行时统计 / 外壳状态折叠进 Desktop。偏好不在此处理：`SettingsStore` 已订阅同一
        // 会话并叠加本地未回执编辑，外观与语言只从它投影（见 `observe_preferences`）。
        observe_preferences(cx);
        crate::account_host::bind_referral_visibility(cx);
        after_first_snapshot(cx, crate::legacy_themes::migrate);
        cx.subscribe(&session, |_, signal, cx| match signal {
            SessionSignal::Snapshot(snapshot) => {
                #[cfg(windows)]
                crate::app_icon::agent_link_changed(true, cx);
                if let Some(body) = crate::session::agent_body(snapshot) {
                    let stats = body.daemon.runtime_stats.clone();
                    let shell = body.shell.clone();
                    let desktop = Desktop::global_mut(cx);
                    desktop.runtime_stats = stats;
                    desktop.shell = shell;
                }
            }
            SessionSignal::Event(frame) => match &frame.event {
                ServiceEvent::Agent(AgentEvent::ShellChanged(shell)) => {
                    Desktop::global_mut(cx).shell = shell.clone();
                }
                ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::RuntimeStatsChanged(
                    stats,
                )))
                | ServiceEvent::Daemon(DaemonEvent::RuntimeStatsChanged(stats)) => {
                    Desktop::global_mut(cx).runtime_stats = stats.clone();
                }
                ServiceEvent::Agent(AgentEvent::DaemonSnapshotReplaced(snapshot))
                | ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(
                    snapshot,
                ))) => {
                    Desktop::global_mut(cx).runtime_stats = snapshot.runtime_stats.clone();
                }
                _ => {}
            },
            SessionSignal::Fatal(error) => {
                log::error!("fatal FluxDown agent error: {:?}", error.code);
            }
            SessionSignal::ServiceStopped => crate::lifecycle::service_stopped(cx),
            SessionSignal::Stale | SessionSignal::ServiceFailed(_) => {
                #[cfg(windows)]
                crate::app_icon::agent_link_changed(false, cx);
            }
        })
        .detach();

        // 事件泵：按帧批处理，一次 `update` 广播一批。
        let pump_session = session.clone();
        cx.spawn(async move |cx| {
            loop {
                let Some(first) = agent_events.recv().await else {
                    break;
                };
                let mut batch = vec![first];
                while batch.len() < EVENT_BATCH {
                    match agent_events.try_recv() {
                        Ok(event) => batch.push(event),
                        Err(_) => break,
                    }
                }
                cx.update(|cx| {
                    pump_session.update(cx, |session, cx| session.ingest(batch, cx));
                });
            }
        })
        .detach();

        // 单实例激活通道：链接交给 agent，激活时重建 / 恢复 / 聚焦主窗口。
        let activate_client = agent_client.clone();
        cx.spawn(async move |cx| {
            while let Some(request) = activate_rx.recv().await {
                let (message, acknowledgement) = request.into_parts();
                cx.update(|cx| {
                    if !message.urls.is_empty() || !message.files.is_empty() {
                        let submitted =
                            submit_captures_detached(&activate_client, message.urls, message.files);
                        let task = cx.spawn(async move |_| {
                            if submitted.await.is_err() {
                                log::debug!("activation submission owner released");
                            }
                        });
                        crate::lifecycle::keep_alive(cx, task).detach();
                    }
                    if message.settings {
                        crate::windows::settings::open(cx);
                    } else if message.new_download {
                        crate::windows::new_download::open_default(cx);
                    } else if message.activate {
                        crate::windows::main::reveal(cx);
                    }
                    // 窗口动作之后再判定：唤起时刚打开的主窗口应先就位，进度窗口按它是否在场决定。
                    if let Some(task_id) = message.progress_task {
                        crate::progress_windows::user_started_on_launch(task_id, cx);
                    }
                });
                if acknowledgement.send(()).is_err() {
                    log::trace!("activation acknowledgement receiver already closed");
                }
            }
        })
        .detach();

        // 常驻能力归 agent（托盘 / 剪贴板监听 / 完成后关机执行）；这里只装界面投影。
        crate::power::install(cx);
        // 引擎选择请求窗口 / 外部捕获确认（并入新建下载窗口）：跟随会话事件独立开关，
        // 不依赖主窗口存在。
        crate::windows::selection::install(cx);
        crate::windows::file_conflict::install(cx);
        crate::plugin_notices::install(cx);
        crate::plugin_contributions::install(cx);
        crate::service_notices::install(cx);
        if launch.after_update {
            after_session_settled(cx, crate::update_notices::show_installed);
        }
        crate::progress_windows::install(cx);
        if launch.capture_only
            && let Some(task_id) = launch.progress_task.clone()
        {
            // 须先于下方「无待确认即退出」登记：意图的界面保活会推迟那次退出。
            crate::progress_windows::user_started_on_launch(task_id, cx);
        }

        if launch.settings {
            after_session_settled(cx, crate::windows::settings::open);
        } else if launch.new_download {
            after_session_settled(cx, crate::windows::new_download::open_default);
        } else if launch.capture_only {
            // 由 agent 为待确认交互拉起：不开主窗口；确认窗口随快照 / 事件打开，全部关闭后由
            // 窗口注册表退出。启动链接提交完成后若首个快照里已无待确认项，直接退出。
            quit_when_nothing_to_confirm(launch_submissions, cx);
        } else if launch.minimized {
            // 开机自启且托盘不可见（agent 判定没有其它入口时才拉起）：会话就绪后开最小化主窗口，
            // 新窗口不抢焦点。
            after_session_settled(cx, open_main_minimized);
        } else if launch.is_plain() {
            // 普通启动：本进程冷启动了服务时按「启动时最小化到托盘」决定；agent 早已驻留时
            // 这是用户在打开应用，直接开窗。
            decide_plain_launch(bootstrap, cx);
        } else {
            // 等快照就绪后开主窗口，避免首帧闪连接态。
            after_session_settled(cx, crate::windows::main::reveal);
        }
        if !launch.capture_only
            && let Some(task_id) = launch.progress_task.clone()
        {
            // 非驻留拉起会同时打开主窗口：排在上面的开窗决定之后登记（同一快照信号里后执行），
            // 进度窗口才不会先弹出又被随后打开的主窗口盖住。
            after_session_settled(cx, move |cx| {
                crate::progress_windows::user_started_on_launch(task_id, cx);
            });
        }
        // 主窗口的首帧 / 快照订阅先注册，捕获确认后开，避免主窗口覆盖待确认窗口。
        // --capture 仍不创建主窗口；其空窗退出检查在异步回调里执行。
        crate::windows::new_download::install_captures(cx);
    });

    Ok(RunOutcome::Completed)
}
/// `lock_wait` 为 `Some`（`--after-update`、agent 拉起的确认 / 进度界面）时先轮询等待旧主实例
/// 释放锁，而不是把请求转发给正在退出的旧主实例（它不会再处理请求）；超时后回退到常规的转发 /
/// 重试路径。
fn acquire_or_activate(
    instance_dir: &Path,
    endpoint: &Endpoint,
    message: &ActivateMessage,
    activate_existing: bool,
    lock_wait: Option<Duration>,
) -> Result<LaunchDisposition, AppError> {
    if let Some(wait) = lock_wait {
        let deadline = Instant::now() + wait;
        loop {
            match launch::InstanceLock::try_acquire(instance_dir).map_err(AppError::InstanceLock)? {
                Some(_lock) if activate_existing => return Ok(LaunchDisposition::NoPrimary),
                Some(lock) => return Ok(LaunchDisposition::Primary(lock)),
                None if Instant::now() < deadline => {
                    std::thread::sleep(AFTER_UPDATE_LOCK_INTERVAL);
                }
                None => {
                    log::warn!(
                        "old desktop instance still holds the lock after update; forwarding"
                    );
                    break;
                }
            }
        }
    }
    let deadline = Instant::now() + ACTIVATION_RETRY_TIMEOUT;
    loop {
        match launch::InstanceLock::try_acquire(instance_dir).map_err(AppError::InstanceLock)? {
            Some(_lock) if activate_existing => return Ok(LaunchDisposition::NoPrimary),
            Some(lock) => return Ok(LaunchDisposition::Primary(lock)),
            None => match instance_ipc::send_to_primary(endpoint, message) {
                Ok(()) => return Ok(LaunchDisposition::Activated),
                Err(error) if error.is_retryable() && Instant::now() < deadline => {
                    std::thread::sleep(ACTIVATION_RETRY_INTERVAL);
                }
                Err(error) => return Err(AppError::Activation(error)),
            },
        }
    }
}

#[cfg(windows)]
fn start_windows_listener(
    client: &Arc<AgentClient>,
    endpoint: Endpoint,
    tx: mpsc::Sender<ActivationRequest>,
) -> Result<(), std::io::Error> {
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    client.spawn_background(async move {
        match instance_ipc::Listener::bind(endpoint) {
            Ok(listener) => {
                if ready_tx.send(Ok(())).is_err() {
                    // 启动方已结束等待，不留下无人拥有的激活监听任务。
                    log::debug!("activation listener startup receiver closed");
                    return;
                }
                listener.listen(tx).await;
            }
            Err(error) => {
                if ready_tx.send(Err(error)).is_err() {
                    log::debug!("activation listener startup receiver closed after bind failure");
                }
            }
        }
    });
    ready_rx
        .recv_timeout(ACTIVATION_RETRY_TIMEOUT)
        .map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("activation listener did not start: {error}"),
            )
        })?
}

/// 创建 GPUI 应用。macOS 关闭 GPUI 无障碍：`accesskit_macos::SubclassingAdapter`
/// 会动态改写窗口内容视图的类，窗口关闭释放适配器时又无条件改回原类，抹掉
/// Touch Bar（`_NSTouchBarFinder`）在该视图上注册的 KVO；下一帧移除观察者抛出
/// NSException，带 Touch Bar 的 Mac 关闭任意窗口即闪退（SIGILL）。
#[cfg(target_os = "macos")]
fn desktop_application() -> gpui::Application {
    gpui::Application::new_inaccessible(gpui_platform::current_platform(false))
}

#[cfg(not(target_os = "macos"))]
fn desktop_application() -> gpui::Application {
    gpui_platform::application()
}

fn open_main_minimized(cx: &mut App) {
    if let Some(handle) = crate::windows::main::open_unfocused(cx)
        && let Err(error) = handle.update(cx, |_, window, _| window.minimize_window())
    {
        log::debug!("view or window released before lifecycle update: {error:#}");
    }
}

/// 普通启动的开窗判定：会话就绪（快照、宽限到期或致命错误）后执行。
///
/// - agent 已在运行（本进程没拉起它）：与其他启动一样直接开窗。
/// - 本进程冷启动了 agent：会话对首个快照放宽宽限（见 `session::COLD_START_GRACE`），这里据
///   首个快照读偏好——agent 在 daemon 就绪前就已带偏好与外壳状态提供快照。偏好开启且托盘
///   可见时不开窗、只退出界面，托盘由 agent 驻留；致命错误或宽限到期仍开窗。
fn decide_plain_launch(bootstrap: Arc<ServiceBootstrap>, cx: &mut App) {
    after_session_settled(cx, move |cx| {
        let snapshot = Desktop::global(cx).session.read(cx).latest().cloned();
        let tray_only = bootstrap.spawned_agent()
            && snapshot
                .as_deref()
                .and_then(crate::session::agent_body)
                .is_some_and(launch::start_in_tray);
        if tray_only {
            crate::lifecycle::quit_ui(cx);
        } else {
            crate::windows::main::reveal(cx);
        }
    });
}

/// 会话已就绪（拿到快照或已确认离线）立即执行，否则等到就绪后执行一次。
fn after_session_settled(cx: &mut App, run: impl FnOnce(&mut App) + 'static) {
    let session = Desktop::global(cx).session.clone();
    if session.read(cx).is_settled() {
        run(cx);
        return;
    }
    let subscription = std::rc::Rc::new(std::cell::RefCell::new(None));
    let holder = std::rc::Rc::clone(&subscription);
    let mut run = Some(run);
    *subscription.borrow_mut() = Some(cx.subscribe(&session, move |_, signal, cx| {
        if matches!(
            signal,
            SessionSignal::Snapshot(_)
                | SessionSignal::Stale
                | SessionSignal::Fatal(_)
                | SessionSignal::ServiceFailed(_)
        ) && holder.borrow_mut().take().is_some()
            && let Some(run) = run.take()
        {
            run(cx);
        }
    }));
}

/// `--capture`：启动链接提交完、首个快照也已派发确认窗口后仍没有任何窗口（请求已被别处处理
/// 或已按默认值超时）时退出，避免留下无窗口、无托盘的界面进程。
fn quit_when_nothing_to_confirm(submissions: tokio::sync::oneshot::Receiver<()>, cx: &mut App) {
    cx.spawn(async move |cx| {
        if submissions.await.is_err() {
            // 后台提交任务随客户端关闭；仍继续执行既有无窗口退出判定。
            log::debug!("capture submission completion sender released");
        }
        cx.update(|cx| {
            after_first_snapshot(cx, |cx| {
                cx.defer(|cx| {
                    if WindowRegistry::open_count(cx) == 0 {
                        crate::lifecycle::quit_ui(cx);
                    }
                });
            });
        });
    })
    .detach();
}

/// 已有快照立即执行，否则等首个快照到达后执行一次。
fn after_first_snapshot(cx: &mut App, run: fn(&mut App)) {
    let session = Desktop::global(cx).session.clone();
    if session.read(cx).latest().is_some() {
        run(cx);
        return;
    }
    let subscription = std::rc::Rc::new(std::cell::RefCell::new(None));
    let holder = std::rc::Rc::clone(&subscription);
    *subscription.borrow_mut() = Some(cx.subscribe(&session, move |_, signal, cx| {
        if matches!(signal, SessionSignal::Snapshot(_)) {
            run(cx);
            holder.borrow_mut().take();
        }
    }));
}

/// 偏好 → 全局外观、活动栏与语言。唯一的投影入口：主题 / 语言等由偏好派生的全局状态只在这里
/// 修改，界面控件只写偏好（[`Desktop::set_pref`] / `SettingsStore::set_pref`）。偏好视图含本地
/// 未回执编辑，快照或无关键的事件回流都不会把刚做的改动回弹。
fn observe_preferences(cx: &mut App) {
    let store = Desktop::global(cx).settings_store.clone();
    let mut applied = BTreeMap::new();
    cx.observe(&store, move |store, cx| {
        let values = store.read(cx).preferences();
        if *values == applied {
            return;
        }
        applied.clone_from(values);
        apply_preferences(&applied, cx);
    })
    .detach();
}

fn apply_preferences(values: &BTreeMap<String, serde_json::Value>, cx: &mut App) {
    let translator = Desktop::global(cx).translator.clone();
    // 导入主题先于外观注册，`custom:<id>` 选择才能直接命中。
    for failure in fluxdown_ui_settings::sync_theme_library(values, cx) {
        log::warn!("failed to load imported theme: {failure}");
    }
    fluxdown_ui_theme::apply_appearance_preferences(values, cx);
    fluxdown_ui_icon_pack::apply_icon_pack_preference(values, cx);
    {
        let client = Arc::clone(&Desktop::global(cx).client);
        crate::app_icon::apply_preference(values, &client, cx);
    }
    apply_activity_bar_preferences(values, cx);
    if let Some(locale) = values
        .get("general.locale")
        .and_then(serde_json::Value::as_str)
    {
        let target = if locale == "system" {
            system_locale()
        } else {
            locale.to_owned()
        };
        let locale_changed = translator.update(cx, |translator, cx| {
            let changed = translator.set_locale(&target);
            if changed {
                gpui_component::set_locale(component_locale(translator.locale()));
                cx.notify();
            }
            changed
        });
        if locale_changed {
            // gpui-component 自带文案读进程级静态 locale，不在被追踪的状态里；retained 渲染
            // 下只有整窗刷新才能让没读 Translator 的库控件换成新语言。
            cx.refresh_windows();
        }
    }
    // 界面语言（含跟随系统）一变，汉字主字体与回退链随之更新；未变时为空操作。
    let preference = han_font_preference(translator.read(cx));
    fluxdown_ui_theme::set_han_font_preference(preference, cx);
}

/// 界面语言 → 汉字字体偏好：中文界面按界面语言的简 / 繁；非中文界面按系统语言推导回退链
/// 里优先的字形（文件名等用户数据多与系统语言同字形），推不出时取简体。
fn han_font_preference(translator: &Translator) -> fluxdown_ui_theme::HanFontPreference {
    use fluxdown_ui_theme::{HanFontPreference, HanScript};
    if let Some(script) = HanScript::from_locale(translator.locale()) {
        return HanFontPreference {
            script,
            chinese_ui: true,
        };
    }
    let mut system = translator.clone();
    system.set_locale(&system_locale());
    HanFontPreference {
        script: HanScript::from_locale(system.locale()).unwrap_or_default(),
        chinese_ui: false,
    }
}

/// 偏好快照/事件 → 活动栏可选入口可见性与 chrome 布局（条目与偏好键见 `activity` /
/// `chrome_layout`）。
fn apply_activity_bar_preferences(values: &BTreeMap<String, serde_json::Value>, cx: &mut App) {
    let Some(shell) = Desktop::global(cx).main_shell.clone() else {
        return;
    };
    if let Err(error) = shell.update(cx, |shell, cx| {
        crate::activity::apply_visibility(shell, values, cx);
        crate::chrome_layout::apply(shell, values, cx);
        crate::menus::apply_menu_bar_visibility(shell, values, cx);
    }) {
        log::debug!("view or window released before lifecycle update: {error:#}");
    }
}

/// 系统交来的外部链接 / `.torrent` 文件 → agent 捕获入口的 RPC 列表。声明来源关联
/// （[`OpenAssociation`]）：用户已在设置中关闭的关联由 agent 拦截，不建任务。
///
/// [`OpenAssociation`]: fluxdown_protocol::capture_link::OpenAssociation
fn capture_calls(
    client: &Arc<AgentClient>,
    urls: Vec<String>,
    files: Vec<std::path::PathBuf>,
) -> Vec<(
    &'static str,
    crate::agent_client::AgentFuture<serde_json::Value>,
)> {
    use fluxdown_protocol::capture_link::{
        OpenAssociation, deep_link_file_name, normalize_capture_url,
    };
    let mut calls = Vec::with_capacity(urls.len() + files.len());
    for url in urls {
        // 关联按原始 scheme 判定：`fluxdown:` 深链解码出的 `magnet:` 不受 magnet 开关约束。
        let association = OpenAssociation::of_url(&url);
        let filename = deep_link_file_name(&url).unwrap_or_default();
        let url = normalize_capture_url(&url);
        let future = client.call::<serde_json::Value, serde_json::Value>(
            fluxdown_protocol::method::AGENT_CAPTURE_SUBMIT,
            Some(serde_json::json!({
                "request": { "url": url, "filename": filename },
                "silent": true,
                "association": association,
            })),
        );
        calls.push(("link", future));
    }
    for file in files {
        let path = file.display().to_string();
        let future = client.call::<serde_json::Value, serde_json::Value>(
            fluxdown_protocol::method::AGENT_CAPTURE_SUBMIT_TORRENT_FILE,
            Some(serde_json::json!({
                "path": path,
                "silent": true,
                "association": OpenAssociation::Torrent,
            })),
        );
        calls.push(("torrent file", future));
    }
    calls
}

/// 主实例：不阻塞 UI 线程，在后台把链接交给 agent；返回的接收端在全部提交结束后完成。
pub(crate) fn submit_captures_detached(
    client: &Arc<AgentClient>,
    urls: Vec<String>,
    files: Vec<std::path::PathBuf>,
) -> tokio::sync::oneshot::Receiver<()> {
    let (done, finished) = tokio::sync::oneshot::channel();
    if urls.is_empty() && files.is_empty() {
        if done.send(()).is_err() {
            log::trace!("capture submission completion receiver closed");
        }
        return finished;
    }
    let calls = capture_calls(client, urls, files);
    let failures = client.clone();
    client.spawn_background(async move {
        for (kind, future) in calls {
            if let Err(error) = future.await {
                log::warn!("failed to submit captured {kind}: {:?}", error.code);
                failures
                    .submissions
                    .capture_failure
                    .send_replace(Some(error));
            }
        }
        if done.send(()).is_err() {
            log::trace!("capture submission completion receiver closed");
        }
    });
    finished
}

/// 桌面侧推导的 agent 路径；镜像 `native/agent/src/runtime.rs` 的 `resolve_agent_data_dir`。
/// 桌面端不依赖 agent crate，只解析路径；旧状态的便携迁移由 agent 独占处理。
#[derive(Debug, PartialEq, Eq)]
struct DesktopPaths {
    /// 数据根：`FLUXDOWN_DATA_DIR`，否则（Windows 便携版）`<exe>/portable_data`，否则 ProjectDirs 数据目录。
    data_root: std::path::PathBuf,
    /// `FLUXDOWN_AGENT_DATA_DIR`，否则 `<数据根>/agent`。
    agent_data_dir: std::path::PathBuf,
    /// `FLUXDOWN_AGENT_TOKEN_FILE`，否则 `<agent 数据目录>/agent.token`。
    agent_token: std::path::PathBuf,
}

impl DesktopPaths {
    fn from_env() -> Self {
        Self::resolve(
            |name| env::var_os(name),
            portable_data_dir(),
            directories::ProjectDirs::from("dev", "zerx", "FluxDown")
                .map(|project| project.data_dir().to_owned()),
        )
    }

    fn resolve(
        lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
        portable_data_dir: Option<std::path::PathBuf>,
        project_data_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let data_root = lookup("FLUXDOWN_DATA_DIR")
            .map(std::path::PathBuf::from)
            .or(portable_data_dir)
            .or(project_data_dir)
            .unwrap_or_default();
        let agent_data_dir = lookup("FLUXDOWN_AGENT_DATA_DIR")
            .map_or_else(|| data_root.join("agent"), std::path::PathBuf::from);
        let agent_token = lookup("FLUXDOWN_AGENT_TOKEN_FILE").map_or_else(
            || agent_data_dir.join("agent.token"),
            std::path::PathBuf::from,
        );
        Self {
            data_root,
            agent_data_dir,
            agent_token,
        }
    }
}

/// Windows 便携版：`<exe>/portable` 标记存在时数据在 `<exe>/portable_data`（与 agent 同判定）。
#[cfg(windows)]
fn portable_data_dir() -> Option<std::path::PathBuf> {
    portable_data_dir_for_exe(&env::current_exe().ok()?)
}

/// 标记使用 `exists()`，与 `native/agent/src/runtime.rs` 的 `portable_data_dir` 一致。
#[cfg(any(windows, test))]
fn portable_data_dir_for_exe(exe: &Path) -> Option<std::path::PathBuf> {
    let exe_dir = exe.parent()?;
    exe_dir
        .join("portable")
        .exists()
        .then(|| exe_dir.join("portable_data"))
}

#[cfg(not(windows))]
fn portable_data_dir() -> Option<std::path::PathBuf> {
    None
}

/// 桌面数据根目录（旧版导入主题文件所在，见 `legacy_themes`）。
pub(crate) fn app_data_dir() -> std::path::PathBuf {
    DesktopPaths::from_env().data_root
}

/// agent 数据目录；诊断日志写在其下 `logs/`。
pub(crate) fn agent_data_dir() -> std::path::PathBuf {
    DesktopPaths::from_env().agent_data_dir
}

fn agent_token_path() -> std::path::PathBuf {
    DesktopPaths::from_env().agent_token
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("fluxdown-app-{label}-{}", std::process::id()))
    }

    #[test]
    fn paths_follow_the_agent_resolution_rules() {
        use std::path::PathBuf;

        let project = Some(PathBuf::from("/project"));
        let resolve = |vars: &[(&str, &str)]| {
            let vars: Vec<(String, String)> = vars
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect();
            DesktopPaths::resolve(
                |name| {
                    vars.iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value.into())
                },
                None,
                project.clone(),
            )
        };

        let defaults = resolve(&[]);
        assert_eq!(
            defaults.agent_token,
            PathBuf::from("/project/agent/agent.token")
        );

        // 只设数据根：token 必须跟着 agent 落到 `<root>/agent`（曾经仍读 ProjectDirs）。
        let rooted = resolve(&[("FLUXDOWN_DATA_DIR", "/root")]);
        assert_eq!(rooted.data_root, PathBuf::from("/root"));
        assert_eq!(rooted.agent_data_dir, PathBuf::from("/root/agent"));
        assert_eq!(rooted.agent_token, PathBuf::from("/root/agent/agent.token"));

        let agent_dir = resolve(&[
            ("FLUXDOWN_DATA_DIR", "/root"),
            ("FLUXDOWN_AGENT_DATA_DIR", "/agent-state"),
        ]);
        assert_eq!(agent_dir.data_root, PathBuf::from("/root"));
        assert_eq!(
            agent_dir.agent_token,
            PathBuf::from("/agent-state/agent.token")
        );

        let token_file = resolve(&[
            ("FLUXDOWN_AGENT_DATA_DIR", "/agent-state"),
            ("FLUXDOWN_AGENT_TOKEN_FILE", "/secrets/token"),
        ]);
        assert_eq!(token_file.agent_data_dir, PathBuf::from("/agent-state"));
        assert_eq!(token_file.agent_token, PathBuf::from("/secrets/token"));
    }

    #[test]
    fn portable_data_precedes_project_dirs_but_not_env() {
        use std::path::PathBuf;

        let portable = Some(PathBuf::from("/exe/portable_data"));
        let project = Some(PathBuf::from("/project"));
        let paths = DesktopPaths::resolve(|_| None, portable.clone(), project.clone());
        assert_eq!(
            paths.agent_data_dir,
            PathBuf::from("/exe/portable_data/agent")
        );
        assert_eq!(
            paths.agent_token,
            PathBuf::from("/exe/portable_data/agent/agent.token")
        );
        let env_root = DesktopPaths::resolve(
            |name| (name == "FLUXDOWN_DATA_DIR").then(|| "/root".into()),
            portable.clone(),
            project.clone(),
        );
        assert_eq!(env_root.agent_data_dir, PathBuf::from("/root/agent"));
        assert_eq!(
            env_root.agent_token,
            PathBuf::from("/root/agent/agent.token")
        );

        let env_agent = DesktopPaths::resolve(
            |name| (name == "FLUXDOWN_AGENT_DATA_DIR").then(|| "/agent-state".into()),
            portable.clone(),
            project.clone(),
        );
        assert_eq!(env_agent.data_root, PathBuf::from("/exe/portable_data"));
        assert_eq!(env_agent.agent_data_dir, PathBuf::from("/agent-state"));
        assert_eq!(
            env_agent.agent_token,
            PathBuf::from("/agent-state/agent.token")
        );

        let env_all = DesktopPaths::resolve(
            |name| match name {
                "FLUXDOWN_DATA_DIR" => Some("/root".into()),
                "FLUXDOWN_AGENT_DATA_DIR" => Some("/agent-state".into()),
                "FLUXDOWN_AGENT_TOKEN_FILE" => Some("/secrets/token".into()),
                _ => None,
            },
            portable,
            project,
        );
        assert_eq!(env_all.data_root, PathBuf::from("/root"));
        assert_eq!(env_all.agent_data_dir, PathBuf::from("/agent-state"));
        assert_eq!(env_all.agent_token, PathBuf::from("/secrets/token"));
    }

    #[test]
    fn missing_portable_marker_keeps_project_paths() {
        let dir = test_dir("no-portable-marker");
        let portable = dir.join("portable_data");
        std::fs::create_dir_all(&portable).expect("create unmarked portable data directory");
        let project = dir.join("project");
        let paths = DesktopPaths::resolve(
            |_| None,
            portable_data_dir_for_exe(&dir.join("FluxDown.exe")),
            Some(project.clone()),
        );
        assert_eq!(paths.data_root, project);
        assert_eq!(paths.agent_data_dir, project.join("agent"));
        assert_eq!(paths.agent_token, project.join("agent/agent.token"));
        std::fs::remove_dir_all(dir).expect("remove unmarked fixture");
    }

    #[test]
    fn portable_marker_selects_agent_paths_for_files_and_directories() {
        let dir = test_dir("portable-marker");
        let agent_dir = dir.join("portable_data/agent");
        std::fs::create_dir_all(&agent_dir).expect("create portable agent directory");
        std::fs::write(agent_dir.join("agent.token"), "portable-token")
            .expect("write portable agent token");
        let marker = dir.join("portable");
        for directory_marker in [false, true] {
            if directory_marker {
                std::fs::create_dir(&marker).expect("create directory marker");
            } else {
                std::fs::write(&marker, "").expect("create file marker");
            }
            let paths = DesktopPaths::resolve(
                |_| None,
                portable_data_dir_for_exe(&dir.join("FluxDown.exe")),
                Some(dir.join("project")),
            );
            assert_eq!(paths.data_root, dir.join("portable_data"));
            assert_eq!(paths.agent_data_dir, agent_dir);
            assert_eq!(paths.agent_token, agent_dir.join("agent.token"));
            assert_eq!(
                std::fs::read_to_string(&paths.agent_token).expect("read portable agent token"),
                "portable-token"
            );
            if directory_marker {
                std::fs::remove_dir(&marker).expect("remove directory marker");
            } else {
                std::fs::remove_file(&marker).expect("remove file marker");
            }
        }
        std::fs::remove_dir_all(dir).expect("remove portable fixture");
    }

    #[test]
    fn activate_existing_never_claims_a_free_lock() {
        let dir = test_dir("activate-only");
        let endpoint = Endpoint::for_instance_dir(&dir);
        let outcome = acquire_or_activate(&dir, &endpoint, &ActivateMessage::default(), true, None)
            .expect("coordinate launch");
        assert!(matches!(outcome, LaunchDisposition::NoPrimary));
        if let Err(error) = std::fs::remove_dir_all(dir) {
            log::warn!("could not remove activate-only test fixture: {error}");
        }
    }

    #[test]
    fn retries_and_claims_lock_after_previous_primary_exits() {
        let dir = test_dir("takeover");
        let endpoint = Endpoint::for_instance_dir(&dir);
        let held = launch::InstanceLock::try_acquire(&dir)
            .expect("acquire initial lock")
            .expect("initial primary");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            drop(held);
        });
        let outcome =
            acquire_or_activate(&dir, &endpoint, &ActivateMessage::default(), false, None)
                .expect("take over after release");
        releaser.join().expect("release thread");
        match outcome {
            LaunchDisposition::Primary(lock) => drop(lock),
            LaunchDisposition::Activated | LaunchDisposition::NoPrimary => {
                panic!("expected primary takeover")
            }
        }
        if let Err(error) = std::fs::remove_dir_all(dir) {
            log::warn!("could not remove takeover test fixture: {error}");
        }
    }

    #[test]
    fn after_update_waits_for_the_old_primary_instead_of_forwarding() {
        let dir = test_dir("after-update");
        let endpoint = Endpoint::for_instance_dir(&dir);
        let held = launch::InstanceLock::try_acquire(&dir)
            .expect("acquire initial lock")
            .expect("initial primary");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        // 旧主实例没有激活监听：若转发则会以 Activation 错误失败。
        let outcome = acquire_or_activate(
            &dir,
            &endpoint,
            &ActivateMessage::default(),
            false,
            Some(Duration::from_secs(10)),
        )
        .expect("wait for old primary");
        releaser.join().expect("release thread");
        match outcome {
            LaunchDisposition::Primary(lock) => drop(lock),
            LaunchDisposition::Activated | LaunchDisposition::NoPrimary => {
                panic!("expected primary after waiting")
            }
        }
        if let Err(error) = std::fs::remove_dir_all(dir) {
            log::warn!("could not remove after-update test fixture: {error}");
        }
    }
}
