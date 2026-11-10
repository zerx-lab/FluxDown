//! agent 进程装配、daemon 单会话与 UI Gateway 生命周期。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::local_service::AgentStartupFailureKind;
use fluxdown_protocol::{AgentSnapshot, ServiceEvent};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::api_host::AgentApiHost;
use crate::daemon_client::{DaemonClient, DaemonClientConfig, DaemonClientEvent};
use crate::event_hub::AgentEventHub;
use crate::gateway::{GatewayService, GatewayShell, load_or_create_bearer};
use crate::lifecycle::Lifecycle;
use crate::power::PowerService;
use crate::server_mode::{ServerHandle, ServerHandleParts, ServerRuntime, TokenSeed};
use crate::shell::{ShellHost, ShellServices, ShellState};
use crate::startup_failure::{StartupError, StartupReport};
use crate::state::{AgentState, StateError, StateStore};
use crate::supervisor::DaemonSupervisor;

pub type AgentResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// 在当前线程构建 tokio runtime 并运行 agent 直到退出；SIGTERM / Ctrl-C 只退出 agent。
pub fn run_blocking(host: ShellHost) -> AgentResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let cancel = CancellationToken::new();
        let signal_cancel = cancel.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            signal_cancel.cancel();
        });
        run(cancel, host).await
    })
}

#[cfg(unix)]
pub(crate) async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(terminate) => terminate,
        Err(error) => {
            tracing::warn!(error = %error, "could not register SIGTERM handler; falling back to Ctrl-C");
            if let Err(error) = tokio::signal::ctrl_c().await {
                tracing::error!(error = %error, "could not wait for shutdown signals");
                std::future::pending::<()>().await;
            }
            return;
        }
    };
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if let Err(error) = result {
                tracing::warn!(error = %error, "could not wait for Ctrl-C; waiting for SIGTERM");
                terminate.recv().await;
            }
        }
        _ = terminate.recv() => {}
    }
}

/// GUI 子系统进程没有控制台时 Ctrl-C 处理器可能注册失败：失败即永不触发，而不是立刻退出。
#[cfg(not(unix))]
pub(crate) async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(error = %error, "Ctrl-C unavailable; waiting for another agent shutdown source");
        std::future::pending::<()>().await;
    }
}

pub async fn run(cancel: CancellationToken, host: ShellHost) -> AgentResult {
    run_with(cancel, host, None).await
}

/// [`run`] 的完整形态：`server` 为 `Some` 时按 `--server` 契约装配（见 [`crate::server_mode`]）。
///
/// 桌面模式下，UI Gateway 开始服务之前的失败会写入启动失败记录（见 [`StartupReport`]），供
/// 拉起本进程的桌面界面展示具体原因。
pub(crate) async fn run_with(
    cancel: CancellationToken,
    host: ShellHost,
    server: Option<ServerRuntime>,
) -> AgentResult {
    let report = match server {
        Some(_) => None,
        None => match resolve_agent_data_dir() {
            Ok(agent_data_dir) => Some(StartupReport::new(&endpoint_dir(
                bearer_override().as_deref(),
                &agent_data_dir,
            ))),
            Err(error) => {
                tracing::warn!(error = %error, "agent data directory is unresolvable; startup failures cannot be reported to the desktop");
                None
            }
        },
    };
    let result = run_agent(cancel, host, server, report.as_ref()).await;
    if let Some(report) = &report {
        report.finish(&result).await;
    }
    result
}

async fn run_agent(
    cancel: CancellationToken,
    host: ShellHost,
    server: Option<ServerRuntime>,
    report: Option<&StartupReport>,
) -> AgentResult {
    let server_config = server.as_ref().map(|server| &server.config);
    let paths = AgentPaths::resolve()?;
    let store = match StateStore::open(paths.agent_data_dir.clone()).await {
        Ok(store) => Arc::new(store),
        Err(StateError::Locked) => {
            tracing::info!(
                data_dir = %paths.agent_data_dir.display(),
                "another fluxdown-agent owns the data directory"
            );
            return Ok(());
        }
        Err(error) => return Err(StartupError::new(AgentStartupFailureKind::DataDir, error).into()),
    };
    let mut state = store
        .load()
        .await
        .map_err(|error| StartupError::new(AgentStartupFailureKind::DataDir, error))?;
    initialize_device_identity(&mut state, &store, server_config, &paths.daemon_data_dir)
        .await
        .map_err(|error| StartupError::new(AgentStartupFailureKind::DataDir, error))?;

    // 先绑定 UI Gateway：实际端口进入状态与快照，后续 Doctor/兼容 API 都据此探测。
    // server 模式只由 `FLUXDOWN_BIND` 决定（允许非回环，不看 `lan_enabled`）。
    let override_bind = if server_config.is_none() {
        match std::env::var("FLUXDOWN_AGENT_BIND") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => {
                return Err(StartupError::new(AgentStartupFailureKind::GatewayBind, error).into());
            }
        }
    } else {
        None
    };
    let listener = bind_gateway_listener(
        &mut state,
        &store,
        server_config.map(|config| config.bind),
        override_bind.as_deref(),
    )
    .await?;
    let bound = listener.local_addr()?;
    // 本次运行的网关是否实际可被局域网访问：监听了非回环地址，或 server 模式（由
    // `FLUXDOWN_BIND` 决定、通常在容器 / 反代后面）。与持久化的 `lan_enabled`（下次启动才
    // 生效）区分开，界面据此决定是否展示配对码或提示「重启后生效」。
    state.gateway.lan_active = server_config.is_some() || !bound.ip().is_loopback();
    let bearer_override = bearer_override();
    let bearer = load_or_create_bearer(store.data_dir(), bearer_override.as_deref())
        .await
        .map_err(|error| StartupError::new(AgentStartupFailureKind::DataDir, error))?;
    let endpoint_dir = server
        .is_none()
        .then(|| endpoint_dir(bearer_override.as_deref(), store.data_dir()));
    if let Some(directory) = &endpoint_dir {
        store
            .save_gateway_endpoint(directory, bound)
            .await
            .map_err(|error| StartupError::new(AgentStartupFailureKind::DataDir, error))?;
    }
    tracing::info!(address = %bound, "fluxdown-agent gateway listening");

    // daemon：显式 `FLUXDOWN_DAEMON_URL` 时只连该地址并让 daemon 严格绑定它；否则 daemon 用
    // 默认端口、不可用时回退系统分配端口，并在 `daemon.token` 同目录发布端点，客户端每次连接
    // 前重读。token 由 daemon 首次启动时生成，客户端拿到之前不连接，Gateway 不必等它即可服务。
    let explicit_daemon_bind = paths
        .daemon_rpc_url
        .as_deref()
        .map(daemon_socket_address)
        .transpose()?;
    // 演示模式：把生效的演示 URL 交给 daemon（内置演示 URL 依赖本进程实际监听端口）。
    let daemon_env = server_config
        .and_then(|config| config.effective_demo_url(bound))
        .map(|url| vec![("FLUXDOWN_DEMO_URL".to_owned(), url)])
        .unwrap_or_default();
    let daemon_stderr_log =
        crate::log_export::agent_log_dir(&paths.agent_data_dir).join("fluxdownd.stderr.log");
    let supervisor = Arc::new(
        DaemonSupervisor::new(explicit_daemon_bind)
            .with_extra_env(daemon_env)
            .with_stderr_log(daemon_stderr_log.clone()),
    );
    let daemon_config = DaemonClientConfig::discovered(
        paths.daemon_rpc_url.clone(),
        paths.daemon_token_file.clone(),
        &paths.daemon_data_dir,
    );
    let (daemon, daemon_events) =
        DaemonClient::start(daemon_config.clone(), supervisor.clone(), cancel.clone())?;
    let daemon = Arc::new(daemon);

    // 不等 daemon 就绪就开 Gateway：首个快照先带偏好 / 外壳状态（`daemon_connected=false`），
    // 界面据此立即决定主题、语言与启动时是否只驻留托盘；daemon 连上后由投影任务替换
    // daemon 快照并发布 `DaemonConnectionChanged(true)`，与运行期断线重连同一路径。
    let update_target = crate::update::install::detect(server.is_some());
    let initial = AgentSnapshot {
        daemon_connected: false,
        session: state
            .credentials
            .as_ref()
            .and_then(|credentials| credentials.session.clone()),
        sync: state.sync.clone(),
        preferences: state.preferences.clone(),
        gateway: state.gateway.clone(),
        linked_devices: crate::link::public_devices(&state),
        remote_tasks: state.remote_tasks.clone(),
        shell: crate::shell::shell_status(host.availability, &state.preferences),
        update: crate::update::initial_status(&update_target),
        cloud_notify: crate::cloud_notify::initial_state(state.cloud_notify),
        ..AgentSnapshot::default()
    };
    let events = AgentEventHub::new(initial);
    let lifecycle = Arc::new(
        Lifecycle::new(
            cancel.clone(),
            daemon.clone(),
            supervisor.clone(),
            paths.daemon_data_dir.clone(),
        )
        .with_shutdown_config(daemon_config.clone()),
    );
    if let Some(server) = &server {
        // SIGTERM / SIGINT → 与 `system.shutdown` 同一条完全退出路径（先关停 daemon）。
        let quit = server.quit.clone();
        let lifecycle = lifecycle.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = quit.cancelled() => lifecycle.request_quit(),
                () = cancel.cancelled() => {}
            }
        });
    }
    // 桌面宿主：自身程序文件消失（App Translocation 副本回收、AppImage 卸载、App 被删除）后
    // 完全退出，避免无界面的幽灵实例占住端口。
    let bundle_watch_task = server
        .is_none()
        .then(|| crate::bundle_watch::spawn(lifecycle.clone(), cancel.clone()));
    let shell = ShellState::new(host.availability, daemon.clone(), events.clone());
    let power = Arc::new(PowerService::new(events.clone()));
    let power_task = tokio::spawn(power.clone().run(cancel.clone()));
    let (notice_tx, notice_rx) = crate::cloud_notify::notice_channel();
    // 提示音只在桌面宿主播放：headless server 没有「本机用户」，不发声。
    let (sound_tx, sound_task) =
        crate::sound::start(events.clone(), cancel.clone(), server.is_none());
    // 失败通知同样只在桌面宿主发：headless server 没有「本机用户」，也没有对应设置项。
    let (failure_tx, failure_rx) = if server.is_none() {
        let (tx, rx) = crate::failure_notice::channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let event_task = spawn_daemon_projection(
        daemon_events,
        events.clone(),
        notice_tx,
        sound_tx,
        failure_tx,
        cancel.clone(),
    );
    let notifier = Arc::new(crate::notification::Notifier::new(
        paths.agent_data_dir.clone(),
    ));
    let effects =
        crate::background_effects::BackgroundEffects::new(events.clone(), notifier.clone())
            .with_completion_sound(server.is_none());
    let effects = match failure_rx {
        Some(failure_rx) => effects.with_failure_notices(failure_rx),
        None => effects,
    };
    let effects_task = tokio::spawn(effects.run(cancel.clone()));

    let shared_state = Arc::new(tokio::sync::Mutex::new(state));
    // 局域网直连（L1）：先建服务，legacy 迁移完成后由 readiness 任务 `start`；
    // 桌面默认只监听回环时 `bound` 让它自动关闭广播 / 回连地址自报。
    let link = crate::link::LinkService::new(crate::link::LinkServiceParts {
        events: events.clone(),
        state: shared_state.clone(),
        store: store.clone(),
        tasks: Arc::new(crate::link::DaemonTaskCreator::new(daemon.clone())),
        bound,
        server_mode: server.is_some(),
        network: crate::link::LinkNetworkOptions::from_env(),
    });
    let link_task = tokio::spawn(link.clone().run(cancel.clone()));
    let analytics_task = tokio::spawn(
        crate::analytics::AnalyticsWorker::new(shared_state.clone(), store.clone())
            .run(cancel.clone()),
    );
    let (api_config, api_switches, api_token) = {
        let mut state = shared_state.lock().await;
        // 局域网监听且 takeover 或 aria2 开启时，空 token 等于对同网段匿名开放：启动即补齐。
        // server 模式的空密钥表示尚未完成首次设置，由兼容 API 自身拒绝，不能自动补。
        if server.is_none() && crate::gateway::ensure_exposed_auth_token(&mut state) {
            store.save(&state).await?;
        }
        // legacy Gateway 设置（含用户令牌）尚未从 daemon 迁入时，空令牌意味着兼容 API 不鉴权：
        // 迁移完成前兼容面全部关闭，由 readiness 任务按迁移后的状态开启。
        let migrated = state.gateway_migration_revision.is_some();
        let switches = Arc::new(fluxdown_api::server::ApiRuntimeSwitches::new(
            migrated && state.gateway.takeover_enabled,
            migrated && state.gateway.jsonrpc_enabled,
            migrated && state.gateway.api_enabled,
            migrated && state.gateway.mcp_enabled,
            state.gateway.cors_enabled,
        ));
        let mut config = compatibility_api_config(&state).with_runtime_switches(switches.clone());
        // 监听地址在启动时按 `lan_enabled` 固定（运行期切换下次启动生效），Host 校验与之同步；
        // server 模式可监听非回环地址，LAN 模式本就对外开放，均不强制。
        config.enforce_loopback_host = server.is_none() && !state.gateway.lan_enabled;
        config.require_token = server.is_some();
        let token = config.token.clone();
        (config, switches, token)
    };
    let (ready_tx, ready_rx) = tokio::sync::watch::channel(false);
    let bootstrap = server_config.map(|config| ServerBootstrap {
        seed: TokenSeed {
            preset: config.token.clone(),
            force: config.token_force,
        },
        ready: ready_tx,
    });
    let readiness_task = tokio::spawn(await_daemon_ready(DaemonReadiness {
        daemon: daemon.clone(),
        state: shared_state.clone(),
        store: store.clone(),
        events: events.clone(),
        api_switches: api_switches.clone(),
        api_token: api_token.clone(),
        cancel: cancel.clone(),
        server: bootstrap,
        link: link.clone(),
    }));
    let shared_state_for_server = shared_state.clone();
    let CloudServices {
        auth,
        api: cloud_api,
        sync,
        remote,
        notify,
        sync_task,
        remote_task,
        device_meta_task,
        cdn_task,
        bt_guard_task,
        notify_task,
    } = start_cloud_services(
        fluxcloud_base_url(
            std::env::var("FLUXCLOUD_BASE_URL").ok(),
            option_env!("FLUXCLOUD_BASE_URL"),
        ),
        &daemon,
        &events,
        &shared_state,
        &store,
        notice_rx,
        &cancel,
    )
    .await?;
    let capture = Arc::new(crate::capture::CaptureService::new(
        daemon.clone(),
        events.clone(),
        shell.clone(),
    ));
    let blobs = Arc::new(crate::capture::DaemonBlobClient::new(&daemon_config)?);
    // 已装插件的市场撤回：低频触发 daemon 拉索引（桌面与 server 宿主；embedded 不接入）。
    crate::plugin_revocation::spawn(
        daemon.clone(),
        Arc::new(crate::private_plugin::PrivatePluginService::new(
            Arc::new(cloud_api.clone()),
            daemon.clone(),
            blobs.clone(),
        )),
        cancel.clone(),
    );
    let nmh_task = if server.is_some() {
        // server 模式没有浏览器扩展中继：不启动 NMH IPC 也不注册 native messaging host；
        // 占位任务永不完成，退出时 abort。
        tokio::spawn(std::future::pending::<Result<(), std::io::Error>>())
    } else {
        // NMH 需要实时速度（扩展弹窗）：自持一个任务事件泵，与网关的泵互不依赖。
        let task_events = crate::task_events::TaskEventHub::spawn(&events);
        let nmh = crate::nmh::NmhService::new(daemon.clone(), capture.clone())
            .with_task_events(task_events)
            .with_sniff_rules(Arc::new(crate::sniff_rules::SniffRuleStore::new(
                store.data_dir().join("sniff-rules"),
                crate::egress::Egress::daemon(daemon.clone()),
            )));
        let nmh_cancel = cancel.clone();
        let (endpoint_ready, endpoint_live) = tokio::sync::oneshot::channel();
        // 浏览器扩展靠 NMH 注册找到中继：端点开始监听后再按归属规则自愈，并存安装的中继
        // 要实测能连到本 agent 才保留，仍可用时不与它互相覆盖。
        tokio::spawn(async move {
            match crate::nmh::registry::auto_register(endpoint_live.await.is_ok()).await {
                Ok(crate::nmh::registry::AutoRegisterOutcome::UpToDate) => {
                    tracing::debug!("NMH registration up to date");
                }
                Ok(crate::nmh::registry::AutoRegisterOutcome::Registered(relay)) => {
                    tracing::info!(relay = %relay.display(), "NMH registration repaired");
                }
                Err(error) => tracing::warn!(error = %error, "NMH auto-registration failed"),
            }
        });
        tokio::spawn(async move {
            let outcome = nmh.run(nmh_cancel.clone(), endpoint_ready).await;
            if let Err(error) = &outcome {
                tracing::warn!(error = %error, "NMH IPC service stopped; browser relay unavailable");
            } else if !nmh_cancel.is_cancelled() {
                tracing::warn!("NMH IPC service stopped unexpectedly; browser relay unavailable");
            }
            Ok(())
        })
    };
    let diagnostics = crate::diagnostics::DiagnosticsService::new(
        daemon.clone(),
        daemon_config.clone(),
        events.clone(),
        shared_state.clone(),
        store.clone(),
        api_switches.clone(),
        api_token.clone(),
    )
    .with_daemon_startup(supervisor.clone(), daemon_stderr_log);
    // 开机自启与系统通知只属于桌面宿主；headless 没有登录会话可检查。
    let diagnostics = Arc::new(if server.is_some() {
        diagnostics
    } else {
        diagnostics.with_desktop_checks(notifier.clone())
    });
    let update = Arc::new(crate::update::UpdateService::new(
        crate::update::UpdateParts {
            events: events.clone(),
            data_dir: store.data_dir().to_path_buf(),
            target: update_target,
            egress: crate::egress::Egress::daemon(daemon.clone()),
            request_restart: {
                let lifecycle = lifecycle.clone();
                Box::new(move || lifecycle.request_restart())
            },
        },
    ));
    update.start(cancel.clone(), true);
    let cloud = Arc::new(cloud_api);
    let gateway = GatewayService::new(
        daemon.clone(),
        events.clone(),
        auth,
        cloud,
        sync.clone(),
        remote,
        notify,
        capture.clone(),
        blobs.clone(),
        diagnostics.clone(),
        update,
        shared_state,
        store.clone(),
        api_switches,
        api_token.clone(),
        GatewayShell {
            shell: shell.clone(),
            power: power.clone(),
            lifecycle: lifecycle.clone(),
            notifier: notifier.clone(),
        },
    )
    .with_server_mode(server.is_some())
    .with_link(link.clone());
    let gateway_service = Arc::new(gateway);
    let shell_task = tokio::spawn(crate::shell::run_controller(
        ShellServices {
            state: shell.clone(),
            lifecycle,
            power,
            daemon: daemon.clone(),
            events: events.clone(),
            gateway: gateway_service.clone(),
            agent_data_dir: paths.agent_data_dir.clone(),
        },
        host,
        cancel.clone(),
    ));
    #[cfg(feature = "desktop")]
    if server.is_none() {
        // 应用图标：启动对账 + 跟随偏好变化（托盘由外壳控制器读同一偏好）；旧 Flutter 选择在此一次性迁移。
        tokio::spawn(
            crate::app_icon::AppIconService::new(
                &paths.agent_data_dir,
                paths.daemon_data_dir.clone(),
                sync,
                events.clone(),
                notifier,
                shell,
            )
            .run(cancel.clone()),
        );
    }
    #[cfg(feature = "desktop")]
    if server.is_none() {
        crate::clipboard_watch::spawn(events.clone(), gateway_service.clone(), cancel.clone());
    }
    if server.is_none() {
        match tokio::task::spawn_blocking(crate::platform::migrate_legacy_autostart).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "could not migrate legacy autostart entry");
            }
            Err(error) if error.is_cancelled() => {
                tracing::debug!("legacy autostart migration task cancelled during shutdown");
            }
            Err(error) => {
                tracing::error!(error = %error, "legacy autostart migration task panicked");
            }
        }
    }
    let server_handle = match server_config {
        Some(config) => {
            crate::server_mode::log_web_ui(config);
            Some(Arc::new(ServerHandle::new(ServerHandleParts {
                token: api_token.clone(),
                state: shared_state_for_server,
                store: store.clone(),
                events: events.clone(),
                ready: ready_rx,
                diagnostics,
                blobs: blobs.clone(),
                daemon: daemon_config.clone(),
                webroot: config.webroot.clone(),
                demo: config.effective_demo_url(bound).is_some(),
                allow_local_platform: config.allow_local_platform,
                link: Some(link.clone()),
            })?))
        }
        None => None,
    };
    let api_host = Arc::new(AgentApiHost::new(
        daemon,
        events,
        capture,
        blobs.clone(),
        link.clone(),
        server_config.and_then(|config| config.language.clone()),
    ));
    // 启动环节到此全部完成：之后的失败属于运行期，删除上一次的启动失败记录。
    if let Some(report) = report {
        report.mark_serving().await;
    }
    // NMH 只服务浏览器扩展：其端点故障只记日志，不拖垮 agent；正常退出也不是故障。
    let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = crate::gateway::serve(
        listener,
        gateway_service,
        api_host,
        api_config,
        bearer,
        cancel.clone(),
        server_handle,
        endpoint_dir,
    )
    .await
    .map_err(Into::into);
    cancel.cancel();
    // 所有后台任务都收尾：保留 Gateway / readiness 的首错，同时记录其他故障。
    let mut result = result;
    match readiness_task.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            if result.is_ok() {
                result = Err(error);
            } else {
                tracing::error!(error = %error, "daemon readiness failed during shutdown");
            }
        }
        Err(error) if error.is_cancelled() => {
            tracing::debug!("daemon readiness task cancelled during shutdown");
        }
        Err(error) => {
            tracing::error!(error = %error, "daemon readiness task panicked");
            if result.is_ok() {
                result = Err(error.into());
            }
        }
    }
    for (task, handle) in [
        ("daemon event projection", event_task),
        ("CDN", cdn_task),
        ("BT guard", bt_guard_task),
        ("cloud notify", notify_task),
        ("cloud sync", sync_task),
        ("remote tasks", remote_task),
        ("device link", link_task),
        ("device metadata", device_meta_task),
        ("background effects", effects_task),
        ("sound", sound_task),
        ("analytics", analytics_task),
        ("power", power_task),
        ("shell", shell_task),
    ] {
        match handle.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {
                tracing::debug!(task, "agent background task cancelled during shutdown");
            }
            Err(error) => {
                tracing::error!(task, error = %error, "agent background task panicked");
                if result.is_ok() {
                    result = Err(error.into());
                }
            }
        }
    }
    if let Some(handle) = bundle_watch_task {
        match handle.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {
                tracing::debug!("bundle watch task cancelled during shutdown");
            }
            Err(error) => {
                tracing::error!(error = %error, "bundle watch task panicked");
                if result.is_ok() {
                    result = Err(error.into());
                }
            }
        }
    }
    if server.is_some() {
        // server 模式的 NMH 占位任务永不自行结束。
        nmh_task.abort();
    }
    match nmh_task.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "NMH IPC service stopped; browser relay unavailable");
        }
        Err(error) if error.is_cancelled() => {
            tracing::debug!("NMH task cancelled during shutdown");
        }
        Err(error) => {
            tracing::error!(error = %error, "NMH task panicked");
            if result.is_ok() {
                result = Err(error.into());
            }
        }
    }
    result
}

/// 后台等待 daemon 的依赖集合。
struct DaemonReadiness {
    daemon: Arc<DaemonClient>,
    state: Arc<tokio::sync::Mutex<AgentState>>,
    store: Arc<StateStore>,
    events: AgentEventHub,
    api_switches: Arc<fluxdown_api::server::ApiRuntimeSwitches>,
    api_token: fluxdown_api::auth::TokenCell,
    cancel: CancellationToken,
    server: Option<ServerBootstrap>,
    link: Arc<crate::link::LinkService>,
}

/// server 模式在 daemon 就绪后的一次性引导：预置密钥策略 + 就绪信号。
struct ServerBootstrap {
    seed: TokenSeed,
    ready: tokio::sync::watch::Sender<bool>,
}

/// Gateway 已先行服务；这里等 daemon 首次就绪并完成一次性 legacy 迁移。迁移途中 daemon 断开
/// （崩溃、被重启、正在完全退出）是可重试的：迁移幂等，等 daemon 重连后重做；其它失败即取消
/// agent，语义与原先「就绪前阻塞启动」一致。迁移可能改写兼容 API 开关与令牌，完成后同步到运行期。
async fn await_daemon_ready(readiness: DaemonReadiness) -> AgentResult {
    let DaemonReadiness {
        daemon,
        state,
        store,
        events,
        api_switches,
        api_token,
        cancel,
        server,
        link,
    } = readiness;
    let work = async {
        // 慢盘 / NAS 冷启动可能远超 30s：daemon 客户端自己持续重连，这里持续等待并周期性
        // 报告状态；只有 agent 退出（信号 / daemon 致命错误 → cancel）才结束等待。
        let started = std::time::Instant::now();
        let fresh = state.lock().await.gateway_migration_revision.is_none();
        loop {
            while let Err(error) = daemon.wait_ready(DAEMON_READY_POLL).await {
                tracing::warn!(
                    error = ?error,
                    waited_secs = started.elapsed().as_secs(),
                    "fluxdownd is not ready yet; still waiting"
                );
            }
            match crate::link::migrate_legacy_state(&daemon, &state, &store, &events).await {
                Ok(()) => break,
                Err(crate::link::LinkMigrationError::Daemon(error)) if error.retryable => {
                    tracing::warn!(
                        code = ?error.code,
                        "fluxdownd became unavailable during the legacy migration; retrying once it reconnects"
                    );
                    tokio::time::sleep(MIGRATION_RETRY_DELAY).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        // 迁移可能写入 daemon 时代的身份与名册：互联必须在它之后才加载 / 生成身份。
        // 失败只影响局域网互联（`agent.link.*` 报 Unavailable），不拖垮下载与云功能。
        if let Err(error) = link.start().await {
            tracing::warn!(error = %error, "device link could not start");
        }
        if let Some(server) = &server {
            let mut state = state.lock().await;
            if crate::server_mode::apply_bootstrap(&mut state, fresh, &server.seed) {
                store.save(&state).await?;
                events.publish(fluxdown_protocol::AgentEvent::GatewayChanged(
                    state.gateway.clone(),
                ));
            }
        } else {
            // 迁移可能带入局域网监听与接管 / aria2 开关：局域网暴露面必须有 token。
            let mut state = state.lock().await;
            if crate::gateway::ensure_exposed_auth_token(&mut state) {
                store.save(&state).await?;
                events.publish(fluxdown_protocol::AgentEvent::GatewayChanged(
                    state.gateway.clone(),
                ));
            }
        }
        let state = state.lock().await;
        api_switches.update(
            state.gateway.takeover_enabled,
            state.gateway.jsonrpc_enabled,
            state.gateway.api_enabled,
            state.gateway.mcp_enabled,
            state.gateway.cors_enabled,
        );
        api_token.set(state.gateway_user_token.clone());
        // 令牌与开关都已就位：放行首次设置 / 状态接口（避免 setup 在迁移前落定、被迁移覆盖）。
        if let Some(server) = &server
            && server.ready.send(true).is_err()
        {
            // Gateway 已退出时没有 readiness 接收者，无须把正常关停当成引导失败。
            tracing::debug!("server readiness receiver closed during shutdown");
        }
        Ok(())
    };
    // agent 退出（信号 / 托盘退出 / daemon 致命错误）时不再等待，避免拖住关停。
    let outcome: AgentResult = tokio::select! {
        _ = cancel.cancelled() => Ok(()),
        outcome = work => outcome,
    };
    if let Err(error) = &outcome {
        tracing::error!(error = %error, "fluxdown-agent could not reach fluxdownd");
        cancel.cancel();
    }
    outcome
}

pub(crate) fn spawn_daemon_projection(
    mut daemon_events: tokio::sync::mpsc::Receiver<DaemonClientEvent>,
    events: AgentEventHub,
    notices: crate::cloud_notify::NoticeSender,
    sounds: Option<crate::sound::SoundSender>,
    failures: Option<crate::failure_notice::FailureSender>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                event = daemon_events.recv() => {
                    let Some(event) = event else { break; };
                    match event {
                        DaemonClientEvent::Snapshot(snapshot) => {
                            let fluxdown_protocol::SnapshotBody::Daemon(snapshot) = snapshot.body else { continue };
                            events.replace_daemon_snapshot(*snapshot);
                            events.publish(
                                fluxdown_protocol::AgentEvent::DaemonConnectionChanged(true),
                            );
                        }
                        DaemonClientEvent::Event(frame) => {
                            if let ServiceEvent::Daemon(event) = frame.event {
                                crate::cloud_notify::route_daemon_event(&events, &notices, sounds.as_ref(), failures.as_ref(), event);
                            }
                        }
                        DaemonClientEvent::Stale => {
                            events.publish(fluxdown_protocol::AgentEvent::DaemonConnectionChanged(
                                false,
                            ));
                            tracing::warn!("daemon connection is stale; commands are read-only until snapshot replacement");
                        }
                        DaemonClientEvent::Fatal(error) => {
                            tracing::error!(code = ?error.code, "fatal daemon connection error");
                            cancel.cancel();
                            events.publish(fluxdown_protocol::AgentEvent::DaemonConnectionChanged(
                                false,
                            ));
                            break;
                        }
                    }
                }
            }
        }
    })
}

/// 每轮等待 daemon 就绪的时长；超时只记录状态并继续等待。
const DAEMON_READY_POLL: Duration = Duration::from_secs(30);
/// legacy 迁移因 daemon 断开而失败后，重新等待就绪前的间隔（避免 daemon 仍回可重试错误时空转）。
const MIGRATION_RETRY_DELAY: Duration = Duration::from_secs(1);

/// FluxCloud 服务地址：运行期环境变量 > 构建期注入 > 本地默认。空串（CI 未配置 secret 时传入）视为未设置。
pub(crate) fn fluxcloud_base_url(runtime: Option<String>, build_time: Option<&str>) -> String {
    runtime
        .filter(|url| !url.trim().is_empty())
        .or_else(|| {
            build_time
                .filter(|url| !url.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "http://127.0.0.1:8720".to_owned())
}

/// FluxCloud 客户端与依赖它的后台服务（云认证、配置同步、远程任务、设备元数据、CDN）。
pub(crate) struct CloudServices {
    pub(crate) auth: Arc<crate::cloud::CloudAuthService>,
    pub(crate) api: crate::cloud::CloudApi,
    pub(crate) sync: Arc<crate::sync::SyncService>,
    pub(crate) remote: Arc<crate::remote::RemoteTaskService>,
    pub(crate) notify: Arc<crate::cloud_notify::CloudNotifyService>,
    pub(crate) sync_task: tokio::task::JoinHandle<()>,
    pub(crate) remote_task: tokio::task::JoinHandle<()>,
    pub(crate) device_meta_task: tokio::task::JoinHandle<()>,
    pub(crate) cdn_task: tokio::task::JoinHandle<()>,
    pub(crate) bt_guard_task: tokio::task::JoinHandle<()>,
    pub(crate) notify_task: tokio::task::JoinHandle<()>,
}

/// 装配并启动云相关服务；桌面 / server 与嵌入式宿主共用，差别只在 `base_url` 的来源。
pub(crate) async fn start_cloud_services(
    base_url: String,
    daemon: &Arc<DaemonClient>,
    events: &AgentEventHub,
    state: &Arc<tokio::sync::Mutex<AgentState>>,
    store: &Arc<StateStore>,
    notices: crate::cloud_notify::NoticeReceiver,
    cancel: &CancellationToken,
) -> Result<CloudServices, crate::cloud::CloudError> {
    let cloud_client = crate::cloud::CloudClient::new(base_url, state.clone(), store.clone())?
        .with_events(events.clone());
    cloud_client.restore_endpoint_override().await;
    let auth = Arc::new(crate::cloud::CloudAuthService::new(
        cloud_client.clone(),
        events.clone(),
    ));
    let api = crate::cloud::CloudApi::new(cloud_client);
    let sync = Arc::new(crate::sync::SyncService::new(
        api.clone(),
        daemon.clone(),
        events.clone(),
        state.clone(),
        store.clone(),
    ));
    let sync_task = tokio::spawn(sync.clone().run(cancel.clone()));
    let notify = Arc::new(crate::cloud_notify::CloudNotifyService::new(
        api.clone(),
        events.clone(),
        state.clone(),
        store.clone(),
        cancel.clone(),
    ));
    let notify_task = tokio::spawn(notify.clone().run(notices));
    let remote = crate::remote::RemoteTaskService::new(
        api.clone(),
        daemon.clone(),
        events.clone(),
        state.clone(),
        store.clone(),
    )
    .with_notify_feed(notify.feed());
    let remote = Arc::new(remote);
    let remote_task = tokio::spawn(remote.clone().run(cancel.clone()));
    let device_meta_task = tokio::spawn(
        Arc::new(crate::device_meta::DeviceMetaService::new(
            api.clone(),
            events.clone(),
        ))
        .run(cancel.clone()),
    );
    let cdn_task = tokio::spawn(
        crate::cdn_worker::CdnWorker::new(api.clone(), daemon.as_ref().clone(), events.clone())
            .run(cancel.clone()),
    );
    let bt_guard_task = tokio::spawn(
        crate::bt_guard::BtGuardWorker::new(api.clone(), daemon, events.clone(), store.data_dir())
            .run(cancel.clone()),
    );
    Ok(CloudServices {
        auth,
        api,
        sync,
        remote,
        notify,
        sync_task,
        remote_task,
        device_meta_task,
        cdn_task,
        bt_guard_task,
        notify_task,
    })
}

async fn initialize_device_identity(
    state: &mut AgentState,
    store: &StateStore,
    server: Option<&crate::server_mode::ServerConfig>,
    engine_data_dir: &Path,
) -> Result<(), crate::state::StateError> {
    let mut changed = false;
    let first_run = state.device_id.is_empty();
    if first_run {
        // 沿用 Flutter 时代的 `cloud_device_id`（同一台机器迁移到 GPUI 后云端仍视为同一设备，
        // 不多占设备名额）；找不到才生成新的。headless 实例不继承同机桌面安装版的身份。
        match crate::device_identity::load_flutter_identity(engine_data_dir, server.is_none()).await
        {
            Some(legacy) => {
                state.device_id = legacy.device_id;
                if let Some(name) = legacy.device_name {
                    state.device_name = name;
                }
            }
            None => state.device_id = uuid::Uuid::new_v4().to_string(),
        }
        // server 模式的兼容 API 分组在 daemon 迁移完成后由 `server_mode::apply_bootstrap` 决定。
        if server.is_none() {
            state.gateway.takeover_enabled = true;
            state.gateway.jsonrpc_enabled = true;
        }
        changed = true;
    }
    finalize_device_identity(state, store, std::env::consts::OS, None, changed).await
}

/// 设备身份的共同收尾（桌面 / server / 嵌入式共用）：设备名、平台名、凭证与账号归属自检。
///
/// `host_device_name` 是宿主给出的系统设备名（移动端经 UniFFI 传入；桌面 / server 为 `None`，
/// 改为探测主机名）。设备名缺失、非法或仍是占位名 `FluxDown` 时才重新解析，用户改过的名字不动。
pub(crate) async fn finalize_device_identity(
    state: &mut AgentState,
    store: &StateStore,
    platform: &str,
    host_device_name: Option<&str>,
    mut changed: bool,
) -> Result<(), crate::state::StateError> {
    let valid_name = {
        let length = state.device_name.trim().chars().count();
        (1..=64).contains(&length)
    };
    if !valid_name || crate::device_identity::is_placeholder_device_name(&state.device_name) {
        let name = crate::device_identity::resolve_device_name(host_device_name).await;
        if name != state.device_name {
            state.device_name = name;
            changed = true;
        }
    }
    if state.platform.is_empty() {
        state.platform = platform.to_owned();
        changed = true;
    }
    if state.credentials.as_ref().is_some_and(|credentials| {
        credentials.access_token.is_empty()
            || credentials.refresh_token.is_empty()
            || credentials.session.is_none()
    }) {
        state.credentials = None;
        // 凭证被判定无效：账号维度状态（同步水位 / 脏键 / 远程任务）随之隔离。
        state.bind_account(None);
        changed = true;
    }
    // 升级前的状态没有 `account_uid`：已登录时归属当前会话账号。
    let before = state.account_uid.clone();
    state.adopt_session_account();
    changed |= state.account_uid != before;
    if changed {
        store.save(state).await?;
    }
    Ok(())
}

fn compatibility_api_config(state: &AgentState) -> fluxdown_api::server::ApiServerConfig {
    let config = HashMap::from([
        ("local_server_enabled".to_owned(), "true".to_owned()),
        (
            "local_server_takeover_enabled".to_owned(),
            state.gateway.takeover_enabled.to_string(),
        ),
        (
            "local_server_jsonrpc_enabled".to_owned(),
            state.gateway.jsonrpc_enabled.to_string(),
        ),
        (
            "local_server_api_enabled".to_owned(),
            state.gateway.api_enabled.to_string(),
        ),
        (
            "local_server_mcp_enabled".to_owned(),
            state.gateway.mcp_enabled.to_string(),
        ),
        (
            "local_server_lan_enabled".to_owned(),
            state.gateway.lan_enabled.to_string(),
        ),
        (
            "local_server_cors_allow_all".to_owned(),
            state.gateway.cors_enabled.to_string(),
        ),
        (
            "local_server_token".to_owned(),
            state.gateway_user_token.clone(),
        ),
        (
            "local_server_port".to_owned(),
            state.gateway.port.to_string(),
        ),
    ]);
    fluxdown_api::server::ApiServerConfig::from_config_map(&config, fluxdown_protocol::APP_VERSION)
}

/// 发布实际绑定端口；固定 server / env 地址禁止运行期端口修改。
///
/// 桌面默认配置（非 server、无 `FLUXDOWN_AGENT_BIND`）下，已保存端口被占用或落在系统保留区间
/// （Windows Hyper-V / WSL / Docker 的动态排除端口，绑定返回 WSAEACCES）时回退到系统分配的
/// 端口：实际端口写回状态并经 `gateway-endpoint.json` 发布，桌面据此连接，兼容 API 与油猴脚本
/// 使用设置页显示的生效端口。显式地址严格失败，错误带上地址。
async fn bind_gateway_listener(
    state: &mut AgentState,
    store: &StateStore,
    server_bind: Option<std::net::SocketAddr>,
    override_bind: Option<&str>,
) -> Result<TcpListener, StartupError> {
    let address = match server_bind {
        Some(bind) => bind,
        None => gateway_bind_address(&state.gateway, override_bind)
            .map_err(|error| StartupError::new(AgentStartupFailureKind::GatewayBind, error))?,
    };
    let editable = server_bind.is_none() && override_bind.is_none();
    let listener = bind_with_fallback(address, editable)
        .await
        .map_err(|error| StartupError::new(AgentStartupFailureKind::GatewayBind, error))?;
    let port = listener
        .local_addr()
        .map_err(|error| StartupError::new(AgentStartupFailureKind::GatewayBind, error))?
        .port();
    let previous = state.gateway.clone();
    state.gateway.port = port;
    state.gateway.port_editable = editable;
    if state.gateway != previous
        && let Err(error) = store.save(state).await
    {
        state.gateway = previous;
        return Err(StartupError::new(AgentStartupFailureKind::DataDir, error));
    }
    Ok(listener)
}

/// 绑定 `address`；`fallback` 时地址被占用或被系统保留，改绑同一接口上系统分配的端口。
async fn bind_with_fallback(
    address: std::net::SocketAddr,
    fallback: bool,
) -> std::io::Result<TcpListener> {
    let error = match bind_gateway_socket(address) {
        Ok(listener) => return Ok(listener),
        Err(error) => error,
    };
    let unavailable = matches!(
        error.kind(),
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
    );
    if !(fallback && unavailable) {
        return Err(std::io::Error::new(
            error.kind(),
            format!("could not bind the UI gateway on {address}: {error}"),
        ));
    }
    tracing::warn!(
        %address,
        error = %error,
        "UI gateway port is in use or reserved by the system; falling back to an OS-assigned port"
    );
    bind_gateway_socket(std::net::SocketAddr::new(address.ip(), 0)).map_err(|fallback_error| {
        std::io::Error::new(
            fallback_error.kind(),
            format!(
                "could not bind the UI gateway on {address} ({error}) nor on an OS-assigned port: {fallback_error}"
            ),
        )
    })
}

/// 绑定 UI Gateway 监听 socket。IPv6 通配地址（`[::]`）显式关闭 `IPV6_V6ONLY`，同一 socket
/// 同时接 IPv4 与 IPv6（Windows 默认只接 IPv6，Linux 可被 `bindv6only` 改掉，不能靠系统默认）；
/// 本机不支持 IPv6 时退回同端口的 `0.0.0.0`。占用 / 权限错误原样返回，IPv4 同样会失败。
pub(crate) fn bind_gateway_socket(address: std::net::SocketAddr) -> std::io::Result<TcpListener> {
    let std::net::IpAddr::V6(ip) = address.ip() else {
        return bind_socket(address, false);
    };
    if !ip.is_unspecified() {
        return bind_socket(address, false);
    }
    let error = match bind_socket(address, true) {
        Ok(listener) => return Ok(listener),
        Err(error) => error,
    };
    if matches!(
        error.kind(),
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
    ) {
        return Err(error);
    }
    let ipv4 = std::net::SocketAddr::new(std::net::Ipv4Addr::UNSPECIFIED.into(), address.port());
    tracing::warn!(
        %address,
        error = %error,
        fallback = %ipv4,
        "IPv6 is unavailable; the UI gateway listens on IPv4 only"
    );
    bind_socket(ipv4, false).map_err(|fallback_error| {
        std::io::Error::new(
            fallback_error.kind(),
            format!("{address}: {error}; {ipv4}: {fallback_error}"),
        )
    })
}

/// 与 `TcpListener::bind` 同语义（Unix `SO_REUSEADDR`、backlog 1024），`dual_stack` 时关闭
/// `IPV6_V6ONLY`。
fn bind_socket(address: std::net::SocketAddr, dual_stack: bool) -> std::io::Result<TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    if dual_stack {
        socket.set_only_v6(false)?;
    }
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    socket.listen(1024)?;
    TcpListener::from_std(socket.into())
}

/// UI Gateway 监听地址：`FLUXDOWN_AGENT_BIND` 覆盖时必须是回环；否则按持久化的
/// `lan_enabled` 选择接口，直接使用已生效端口；旧状态的零端口兼容默认值。
fn gateway_bind_address(
    gateway: &fluxdown_protocol::GatewayStatusDto,
    override_bind: Option<&str>,
) -> Result<std::net::SocketAddr, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(value) = override_bind {
        let bind = value.parse::<std::net::SocketAddr>()?;
        if !bind.ip().is_loopback() {
            return Err("FLUXDOWN_AGENT_BIND must be loopback".into());
        }
        return Ok(bind);
    }
    let ip = if gateway.lan_enabled {
        std::net::Ipv4Addr::UNSPECIFIED
    } else {
        std::net::Ipv4Addr::LOCALHOST
    };
    let port = gateway.port;
    let port = if port == 0 {
        DEFAULT_GATEWAY_PORT
    } else {
        port
    };
    Ok(std::net::SocketAddr::new(ip.into(), port))
}

const DEFAULT_GATEWAY_PORT: u16 = 17800;

struct AgentPaths {
    agent_data_dir: PathBuf,
    /// daemon 的 engine 数据目录（`daemon.lock` 进程租约所在）。
    daemon_data_dir: PathBuf,
    daemon_token_file: PathBuf,
    /// 显式 `FLUXDOWN_DAEMON_URL`；未设置时按 daemon 发布的端点发现。
    daemon_rpc_url: Option<String>,
}

impl AgentPaths {
    fn resolve() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let agent_data_dir = resolve_agent_data_dir()?;
        // daemon 的 bearer 与进程租约落在 engine 数据目录（`fluxdown_engine::data_dir::resolve_data_dir`），
        // 与 agent 自己的 ProjectDirs 根不同；未显式指定时必须按同一规则推导，否则永远等不到 token。
        let daemon_data_dir = std::env::var_os("FLUXDOWN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(engine_data_dir);
        let daemon_token_file = std::env::var_os("FLUXDOWN_DAEMON_TOKEN_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| daemon_data_dir.join("daemon.token"));
        let daemon_rpc_url = std::env::var("FLUXDOWN_DAEMON_URL")
            .ok()
            .filter(|url| !url.trim().is_empty());
        Ok(Self {
            agent_data_dir,
            daemon_data_dir,
            daemon_token_file,
            daemon_rpc_url,
        })
    }
}

/// `FLUXDOWN_AGENT_TOKEN_FILE`：agent bearer 文件的显式位置。
fn bearer_override() -> Option<PathBuf> {
    std::env::var_os("FLUXDOWN_AGENT_TOKEN_FILE").map(PathBuf::from)
}

/// 桌面端点（`gateway-endpoint.json`）与启动失败记录所在目录：bearer 同目录。
fn endpoint_dir(bearer_override: Option<&Path>, agent_data_dir: &Path) -> PathBuf {
    match bearer_override {
        Some(path) => path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
        None => agent_data_dir.to_path_buf(),
    }
}

/// agent 数据目录：`FLUXDOWN_AGENT_DATA_DIR`，否则 `<FLUXDOWN_DATA_DIR 或 ProjectDirs 数据目录>/agent`。
/// 日志初始化与运行期装配共用，避免两处目录漂移。
pub fn resolve_agent_data_dir() -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(dir) = std::env::var_os("FLUXDOWN_AGENT_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let project_root = || -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
        Ok(directories::ProjectDirs::from("dev", "zerx", "FluxDown")
            .ok_or("could not resolve application data directory")?
            .data_dir()
            .to_owned())
    };
    if let Some(root) = std::env::var_os("FLUXDOWN_DATA_DIR") {
        return Ok(PathBuf::from(root).join("agent"));
    }
    #[cfg(target_os = "windows")]
    if let Some(portable) = portable_data_dir() {
        // 便携版与引擎 data_dir 同判定；旧版本把状态写在 ProjectDirs，首启迁移一次。
        let target = portable.join("agent");
        match project_root() {
            Ok(root) => migrate_legacy_agent_state(&root.join("agent"), &target),
            Err(error) => {
                tracing::warn!(error = %error, "could not locate legacy agent state for portable migration");
            }
        }
        return Ok(target);
    }
    Ok(project_root()?.join("agent"))
}

/// `<exe>/portable` 标记存在时返回 `<exe>/portable_data`。
#[cfg(target_os = "windows")]
fn portable_data_dir() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))?;
    exe_dir
        .join("portable")
        .exists()
        .then(|| exe_dir.join("portable_data"))
}

/// 目标无状态文件而旧位置有时复制 `agent-state.json`（登录态 / 设备身份）；失败不阻断启动。
#[cfg(target_os = "windows")]
fn migrate_legacy_agent_state(legacy: &Path, target: &Path) {
    let name = "agent-state.json";
    let (from, to) = (legacy.join(name), target.join(name));
    if to.exists() || !from.is_file() {
        return;
    }
    if let Err(error) = std::fs::create_dir_all(target) {
        tracing::warn!(path = %target.display(), error = %error, "could not create portable agent state directory");
        return;
    }
    if let Err(error) = std::fs::copy(&from, &to) {
        tracing::warn!(source = %from.display(), target = %to.display(), error = %error, "could not migrate legacy agent state");
    }
}

/// 镜像 `fluxdown_engine::data_dir::resolve_data_dir_inner` 的默认目录（agent 不依赖 engine）。
/// Windows NMH 清单目录（`nmh::registry`）也从这里派生。
pub(crate) fn engine_data_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
                PathBuf::from(home).join(".local").join("share")
            });
        base.join("fluxdown")
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("fluxdown")
    }
    #[cfg(target_os = "windows")]
    {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        if exe_dir.join("portable").exists() {
            return exe_dir.join("portable_data");
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("FluxDown");
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("FluxDown");
        }
        exe_dir
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        PathBuf::from(".")
    }
}

pub(crate) fn daemon_socket_address(
    url: &str,
) -> Result<std::net::SocketAddr, Box<dyn std::error::Error + Send + Sync>> {
    let url = reqwest::Url::parse(url)?;
    let host = url.host_str().ok_or("daemon URL has no host")?;
    let ip = host.parse::<std::net::IpAddr>()?;
    if !ip.is_loopback() {
        return Err("daemon URL must be loopback".into());
    }
    Ok(std::net::SocketAddr::new(ip, url.port().unwrap_or(80)))
}

#[cfg(test)]
mod tests {
    use super::{bind_gateway_listener, bind_gateway_socket, gateway_bind_address};
    use crate::state::AgentState;

    #[test]
    fn lan_flag_selects_interface_without_env_override() {
        let mut gateway = fluxdown_protocol::GatewayStatusDto::default();
        let loopback = gateway_bind_address(&gateway, None).expect("loopback bind");
        assert_eq!(loopback.ip(), std::net::Ipv4Addr::LOCALHOST);
        gateway.lan_enabled = true;
        let lan = gateway_bind_address(&gateway, None).expect("lan bind");
        assert_eq!(lan.ip(), std::net::Ipv4Addr::UNSPECIFIED);
    }

    #[test]
    fn env_override_wins_but_must_stay_loopback() {
        let gateway = fluxdown_protocol::GatewayStatusDto {
            lan_enabled: true,
            ..Default::default()
        };
        let bound = gateway_bind_address(&gateway, Some("127.0.0.1:0")).expect("override bind");
        assert_eq!(bound.to_string(), "127.0.0.1:0");
        assert!(gateway_bind_address(&gateway, Some("0.0.0.0:17800")).is_err());
        assert!(gateway_bind_address(&gateway, Some("not-an-address")).is_err());
    }

    #[test]
    fn legacy_zero_port_falls_back_and_saved_port_is_used() {
        let mut gateway = fluxdown_protocol::GatewayStatusDto {
            port: 0,
            ..Default::default()
        };
        assert_eq!(
            gateway_bind_address(&gateway, None)
                .expect("legacy bind")
                .port(),
            17800
        );
        gateway.port = u16::MAX;
        assert_eq!(
            gateway_bind_address(&gateway, None)
                .expect("saved bind")
                .port(),
            u16::MAX
        );
    }

    fn gateway_test_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "fluxdown_agent_{label}_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[tokio::test]
    async fn gateway_startup_binds_the_saved_active_port() {
        let dir = gateway_test_dir("restart_port");
        let store = crate::state::StateStore::open(dir.clone())
            .await
            .expect("open store");
        let mut state = AgentState::default();
        let first = bind_gateway_listener(&mut state, &store, None, Some("127.0.0.1:0"))
            .await
            .expect("first bind");
        let requested = first.local_addr().expect("first address").port();
        drop(first);
        state.gateway.port = requested;
        store.save(&state).await.expect("save active port");
        let mut restarted = store.load().await.expect("reload before startup");
        let listener = bind_gateway_listener(&mut restarted, &store, None, None)
            .await
            .expect("start saved listener");
        assert_eq!(
            listener.local_addr().expect("restarted address").port(),
            requested
        );
        assert_eq!(restarted.gateway.port, requested);
        assert!(restarted.gateway.port_editable);
        let persisted = store.load().await.expect("reload bound status");
        assert_eq!(persisted.gateway, restarted.gateway);
        drop(listener);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove restart port test directory");
        }
    }

    /// 桌面默认配置下保存的端口被占用（或被系统保留）：回退到系统分配的端口，实际端口写回状态，
    /// 仍可在设置中修改。
    #[tokio::test]
    async fn occupied_saved_port_falls_back_to_an_os_assigned_port() {
        let dir = gateway_test_dir("fallback_port_bind");
        let store = crate::state::StateStore::open(dir.clone())
            .await
            .expect("open store");
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve conflicting port");
        let occupied_port = occupied.local_addr().expect("occupied address").port();
        let mut state = AgentState::default();
        state.gateway.port = occupied_port;
        store.save(&state).await.expect("save active port");
        let listener = bind_gateway_listener(&mut state, &store, None, None)
            .await
            .expect("fallback bind");
        let bound = listener.local_addr().expect("fallback address");
        assert!(bound.ip().is_loopback());
        assert_ne!(bound.port(), occupied_port);
        assert_eq!(state.gateway.port, bound.port());
        assert!(state.gateway.port_editable);
        assert_eq!(
            store.load().await.expect("reload fallback bind").gateway,
            state.gateway
        );
        drop(listener);
        drop(occupied);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove fallback bind test directory");
        }
    }

    /// 显式地址（server 模式 `FLUXDOWN_BIND`、`FLUXDOWN_AGENT_BIND`）被占用时严格失败：错误带上
    /// 地址，已生效端口不变。
    #[tokio::test]
    async fn explicit_bindings_fail_strictly_and_preserve_the_active_port() {
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve conflicting port");
        let address = occupied.local_addr().expect("occupied address");
        let text = address.to_string();
        for server_mode in [false, true] {
            let dir = gateway_test_dir("strict_port_bind");
            let store = crate::state::StateStore::open(dir.clone())
                .await
                .expect("open store");
            let mut state = AgentState::default();
            state.gateway.port = 17_800;
            store.save(&state).await.expect("save active port");
            let before = state.gateway.clone();
            let server_bind = server_mode.then_some(address);
            let override_bind = (!server_mode).then_some(text.as_str());
            let Err(error) =
                bind_gateway_listener(&mut state, &store, server_bind, override_bind).await
            else {
                panic!("an occupied explicit address must not fall back");
            };
            assert!(error.to_string().contains(&text), "{error}");
            assert_eq!(state.gateway, before);
            assert_eq!(
                store.load().await.expect("reload failed bind").gateway,
                before
            );
            drop(store);
            if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!(path = %dir.display(), error = %error, "remove strict bind test directory");
            }
        }
        drop(occupied);
    }

    /// server 默认 `[::]`：同一监听同时接 IPv4 与 IPv6（飞牛 FN Connect 经 `[::1]` 转发、IPv6
    /// DDNS 直连）；本机无 IPv6 时退回 `0.0.0.0`，IPv4 仍可用。
    #[tokio::test]
    async fn ipv6_wildcard_binding_accepts_ipv4_and_ipv6_clients() {
        let listener =
            bind_gateway_socket("[::]:0".parse().expect("wildcard")).expect("dual-stack bind");
        let bound = listener.local_addr().expect("bound address");
        let ipv4 = std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), bound.port());
        tokio::net::TcpStream::connect(ipv4)
            .await
            .expect("IPv4 client reaches the IPv6 wildcard listener");
        if bound.is_ipv6() {
            let ipv6 =
                std::net::SocketAddr::new(std::net::Ipv6Addr::LOCALHOST.into(), bound.port());
            tokio::net::TcpStream::connect(ipv6)
                .await
                .expect("IPv6 client reaches the wildcard listener");
        } else {
            assert!(bound.ip().is_unspecified(), "{bound}");
        }
    }

    #[tokio::test]
    async fn server_and_environment_bindings_publish_non_editable_actual_ports() {
        for server_mode in [false, true] {
            let dir = gateway_test_dir("fixed_port_bind");
            let store = crate::state::StateStore::open(dir.clone())
                .await
                .expect("open store");
            let mut state = AgentState::default();
            let server_bind = server_mode.then(|| "127.0.0.1:0".parse().expect("server bind"));
            let override_bind = (!server_mode).then_some("127.0.0.1:0");
            let listener = bind_gateway_listener(&mut state, &store, server_bind, override_bind)
                .await
                .expect("fixed listener");
            assert_eq!(
                state.gateway.port,
                listener.local_addr().expect("bound address").port()
            );
            assert!(!state.gateway.port_editable);
            assert_eq!(
                store.load().await.expect("reload fixed status").gateway,
                state.gateway
            );
            drop(listener);
            drop(store);
            if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!(path = %dir.display(), error = %error, "remove fixed bind test directory");
            }
        }
    }

    #[tokio::test]
    async fn host_device_name_replaces_missing_or_placeholder_names_but_not_user_names() {
        let dir = gateway_test_dir("device_name");
        let store = crate::state::StateStore::open(dir.clone())
            .await
            .expect("open store");
        for (current, expected) in [
            ("", "Pixel 9 Pro"),
            ("FluxDown", "Pixel 9 Pro"),
            ("书房手机", "书房手机"),
        ] {
            let mut state = AgentState {
                device_id: "device-1".to_owned(),
                device_name: current.to_owned(),
                platform: "android".to_owned(),
                ..AgentState::default()
            };
            store.save(&state).await.expect("seed identity");
            super::finalize_device_identity(
                &mut state,
                &store,
                "android",
                Some("  Pixel 9 Pro\n"),
                false,
            )
            .await
            .expect("finalize identity");
            assert_eq!(state.device_name, expected, "starting from {current:?}");
            assert_eq!(
                store.load().await.expect("reload identity").device_name,
                expected,
                "persisted from {current:?}"
            );
        }
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove device name test directory");
        }
    }
}
