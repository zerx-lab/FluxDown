//! daemon 进程装配、引擎启动顺序与控制面生命周期。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fluxdown_engine::db::{Db, DbError, EngineWriteGuard};
use fluxdown_engine::download_manager;
use fluxdown_engine::events::EventSink;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::selection::HostSelection;
use fluxdown_engine::{Engine, EngineConfig};
use fluxdown_protocol::{DaemonConfigSnapshot, DaemonSnapshot};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::actor::EngineReceivers;
#[cfg(feature = "plugins")]
use crate::actor::PluginEvent;
use crate::blob_store::BlobStore;
use crate::config::{DaemonConfig, bt_config_from_map};
use crate::event_hub::{DaemonEngineEventSink, DaemonEventHub};
use crate::http::{load_or_create_token, serve};
use crate::selection::DaemonSelection;
use crate::service::DaemonService;

/// 控制面绑定成功后报告给嵌入方的信息：真实监听地址（含临时端口）与控制 token。
#[derive(Clone, Debug)]
pub struct DaemonReady {
    pub addr: SocketAddr,
    pub token: String,
}

/// 控制面端点文件路径：与 daemon token 同目录。
fn endpoint_file_path(config: &DaemonConfig, data_dir: &Path) -> PathBuf {
    let dir = match config.token_file_override.as_deref() {
        Some(token) => match token.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => data_dir,
        },
        None => data_dir,
    };
    dir.join(fluxdown_protocol::local_service::DAEMON_ENDPOINT_FILE)
}

/// 删除端点文件；`NotFound` 忽略，其它错误只记录（后续原子写入会覆盖）。
fn remove_endpoint_file(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "failed to remove daemon endpoint file");
        }
    }
}

/// 退出时删除端点文件。
struct EndpointGuard(PathBuf);

impl Drop for EndpointGuard {
    fn drop(&mut self) {
        remove_endpoint_file(&self.0);
    }
}

/// 原子发布控制面真实地址（临时文件 + rename，权限同 daemon.token）。
async fn publish_endpoint_file(path: &Path, bound: SocketAddr) -> std::io::Result<()> {
    let endpoint = fluxdown_protocol::local_service::LocalEndpoint {
        rpc_url: format!("ws://{bound}/rpc"),
    };
    let mut bytes = serde_json::to_vec(&endpoint).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    let temp = crate::http::temporary_path(path);
    if let Err(error) = write_private_file(&temp, &bytes).await {
        if let Err(cleanup) = tokio::fs::remove_file(&temp).await
            && cleanup.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %temp.display(), error = %cleanup, "failed to remove temporary endpoint file");
        }
        return Err(error);
    }
    if let Err(error) = tokio::fs::rename(&temp, path).await {
        if let Err(cleanup) = tokio::fs::remove_file(&temp).await
            && cleanup.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %temp.display(), error = %cleanup, "failed to remove temporary endpoint file");
        }
        return Err(error);
    }
    Ok(())
}

async fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut options = tokio::fs::OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = options.open(path).await?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    drop(file);
    crate::http::set_private_file_permissions(path).await
}

/// 绑定控制面。`fallback` 为真且地址被占用 / 被系统保留（Windows WSAEACCES）时，
/// 回退到同一 IP 的系统分配端口。
async fn bind_control_plane(addr: SocketAddr, fallback: bool) -> std::io::Result<TcpListener> {
    let error = match TcpListener::bind(addr).await {
        Ok(listener) => return Ok(listener),
        Err(error) => error,
    };
    let fallback_addr = SocketAddr::new(addr.ip(), 0);
    if !(fallback
        && matches!(
            error.kind(),
            std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
        ))
    {
        return Err(std::io::Error::new(
            error.kind(),
            format!("could not bind daemon control plane {addr}: {error}"),
        ));
    }
    tracing::warn!(%addr, %error, "default daemon control port unavailable; falling back to a system-assigned loopback port");
    TcpListener::bind(fallback_addr).await.map_err(|second| {
        std::io::Error::new(
            second.kind(),
            format!(
                "could not bind daemon control plane {addr} ({error}) or {fallback_addr}: {second}"
            ),
        )
    })
}

/// 运行 daemon 直到收到取消信号。
pub async fn run(
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_with(DaemonConfig::from_env()?, cancel, None).await
}

/// 与 [`run`] 相同，但使用显式配置；控制面开始监听后通过 `ready` 报告真实地址与 token。
///
/// 进程租约已被其它 daemon 持有时：`ready` 为 `None`（独立进程）按原语义静默返回 `Ok(())`；
/// `ready` 为 `Some`（嵌入方）返回错误，让嵌入方能够暴露该失败而不是永远等不到 ready。
pub async fn run_with(
    process_config: DaemonConfig,
    cancel: CancellationToken,
    ready: Option<oneshot::Sender<DaemonReady>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let data_dir =
        fluxdown_engine::data_dir::resolve_data_dir(process_config.data_dir_override.as_deref())?;
    fluxdown_engine::logger::init_with_dir(&data_dir)?;
    if let Err(error) = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
    {
        tracing::debug!(%error, "daemon tracing subscriber initialization failed; retaining existing subscriber");
    }

    let _process_lease = match DaemonProcessLease::acquire(&data_dir)? {
        Some(lease) => lease,
        None => {
            tracing::info!(
                data_dir = %data_dir.display(),
                "another fluxdownd owns the daemon process lease"
            );
            if ready.is_some() {
                return Err(std::io::Error::other(format!(
                    "another fluxdownd owns the daemon process lease for {}",
                    data_dir.display()
                ))
                .into());
            }
            return Ok(());
        }
    };
    let endpoint_path = process_config
        .publish_endpoint
        .then(|| endpoint_file_path(&process_config, &data_dir));
    if let Some(path) = &endpoint_path {
        remove_endpoint_file(path);
    }
    // 声明在租约之后：先于租约释放，覆盖所有返回路径。
    let _endpoint_guard = endpoint_path.clone().map(EndpointGuard);
    if let Err(error) = crate::private_fs::secure_data_dir(&data_dir).await {
        tracing::warn!(error = %error, "failed to restrict data directory permissions");
    }

    let (boot_db, write_guard) = open_database(&process_config, &data_dir).await?;
    // `FLUXDOWN_SAVE_DIR` 只作为首次播种值：库中已有 `default_save_dir` 时以库为准。
    let default_save_dir = process_config
        .save_dir_seed
        .clone()
        .unwrap_or_else(fluxdown_engine::user_dirs::download_dir_or_cwd);
    let (all_config, snapshot) = bootstrap_database(&boot_db, &default_save_dir).await?;
    let events = DaemonEventHub::new(snapshot, 1024);
    let selections = DaemonSelection::new(events.clone());
    let sink: Arc<dyn EventSink> = Arc::new(DaemonEngineEventSink(events.clone()));
    let selector: Arc<dyn HostSelection> = Arc::new(selections.clone());

    let save_dir = configured_save_dir(&all_config, default_save_dir);
    let mut engine = Engine::from_db(
        EngineConfig {
            max_concurrent: usize_config(&all_config, "max_concurrent_tasks", 5),
            speed_limit_bps: u64_config(&all_config, "speed_limit_bytes", 0),
            upload_limit_bps: u64_config(&all_config, "upload_limit_bytes", 0),
            default_save_dir: save_dir.clone(),
            app_data_dir: data_dir.to_string_lossy().into_owned(),
            bt_config: bt_config_from_map(&all_config),
            proxy_config: ProxyConfig::from_config_map(&all_config),
            user_agent: all_config
                .get("global_user_agent")
                .cloned()
                .unwrap_or_default(),
            data_dir_override: Some(data_dir.clone()),
            database_url: process_config.database_url.clone(),
        },
        boot_db,
        write_guard,
        sink.clone(),
        selector,
    )
    .await?;
    apply_manager_settings(&mut engine, &all_config);
    // 引擎随后被 actor 独占：先取走租约句柄，供下面的心跳在引擎之外校验。
    let lease_guard = engine.write_guard();

    let activity_journal = engine.activity_journal();
    let progress_task = engine.manager.take_progress_rx().map(|progress| {
        tokio::spawn(download_manager::progress_reporter(
            progress,
            engine.db.clone(),
            engine.activity_sink.clone(),
        ))
    });
    let service_db = engine.db.clone();
    #[cfg(feature = "bt")]
    let service_bt_guard = engine.manager.bt_guard().clone();
    let service_data_dir = data_dir.clone();
    #[cfg(feature = "plugins")]
    let service_plugin_manager = engine.manager.plugin_manager();
    #[cfg(feature = "plugins")]
    let host_plugin_manager = service_plugin_manager.clone();
    let receivers = take_engine_receivers(&mut engine)?;
    let (actor, mut actor_task) = crate::actor::spawn_actor(
        engine,
        receivers,
        selections.clone(),
        events.clone(),
        cancel.clone(),
    );
    let maintenance_actor = actor.clone();
    let startup_config = all_config.clone();
    let startup_maintenance_task = tokio::spawn(async move {
        if config_enabled(&startup_config, "bt_enabled", true)
            && config_enabled(&startup_config, "bt_tracker_sub_enabled", true)
        {
            match maintenance_actor
                .execute(crate::actor::ActorOperation::RefreshTrackerSubscription)
                .await
            {
                Ok(_) => {}
                Err(crate::actor::ActorCallError::Unavailable) => {
                    tracing::debug!("daemon actor closed before startup tracker refresh");
                }
                Err(error) => tracing::warn!(%error, "startup tracker refresh failed"),
            }
        }
        if config_enabled(&startup_config, "ed2k_server_sub_enabled", true) {
            match maintenance_actor
                .execute(crate::actor::ActorOperation::RefreshEd2kServerSubscription)
                .await
            {
                Ok(_) => {}
                Err(crate::actor::ActorCallError::Unavailable) => {
                    tracing::debug!("daemon actor closed before startup ED2K server refresh");
                }
                Err(error) => tracing::warn!(%error, "startup ED2K server refresh failed"),
            }
        }
        if config_enabled(&startup_config, "ed2k_enable_kad", true) {
            match maintenance_actor
                .execute(crate::actor::ActorOperation::RefreshEd2kNodes)
                .await
            {
                Ok(_) => {}
                Err(crate::actor::ActorCallError::Unavailable) => {
                    tracing::debug!("daemon actor closed before startup ED2K nodes refresh");
                }
                Err(error) => tracing::warn!(%error, "startup ED2K nodes refresh failed"),
            }
        }
    });

    let token =
        load_or_create_token(&data_dir, process_config.token_file_override.as_deref()).await?;
    let blobs = Arc::new(BlobStore::open(data_dir.join("daemon-blobs")).await?);
    let hello = crate::service_hello(uuid::Uuid::new_v4().to_string(), runtime_capabilities(true));
    let service = DaemonService::new(
        hello,
        events,
        selections,
        blobs.clone(),
        actor.clone(),
        service_db,
        service_data_dir,
        #[cfg(feature = "plugins")]
        service_plugin_manager,
    )
    .with_demo_url(process_config.demo_url.clone());
    #[cfg(feature = "bt")]
    let service = service.with_bt_guard(service_bt_guard);
    let service = Arc::new(service);
    service
        .initialize_dynamic_projection()
        .await
        .map_err(|error| std::io::Error::other(error.message))?;
    // 常驻插件宿主：先装好 `flux.tasks` 出口，再启动 background 宿主。宿主启动不阻塞
    // 控制面就绪（单个插件的 main 初始化最长 30 s）。
    #[cfg(feature = "plugins")]
    if let Some(plugin_manager) = host_plugin_manager {
        plugin_manager.set_task_port(Arc::new(crate::plugin_port::DaemonPluginPort::new(
            Arc::downgrade(&service),
        )));
        tokio::spawn(async move {
            plugin_manager.start_background_hosts().await;
        });
    }
    let listener =
        bind_control_plane(process_config.bind_addr, process_config.bind_fallback).await?;
    let bound_addr = listener.local_addr()?;
    if let Some(path) = &endpoint_path {
        publish_endpoint_file(path, bound_addr).await?;
    }
    tracing::info!(address = %bound_addr, "fluxdownd control plane listening");
    if let Some(ready) = ready
        && ready
            .send(DaemonReady {
                addr: bound_addr,
                token: token.clone(),
            })
            .is_err()
    {
        tracing::debug!("daemon embedder dropped the ready receiver before startup completed");
    }

    let sweep_task = spawn_blob_sweeper(blobs.clone(), cancel.clone());
    let mut lease_task = spawn_lease_monitor(lease_guard, cancel.clone());
    let serve_fut = serve(listener, service, token, cancel.clone());
    tokio::pin!(serve_fut);
    // actor 任务一旦意外结束，daemon 只剩只读快照可用（写操作全部 Unavailable）；
    // 此时主动退出，交给上层 supervisor 重拉进程。
    let mut actor_finished = false;
    let mut lease_finished = false;
    let mut lease_lost = false;
    let mut result = tokio::select! {
        result = &mut serve_fut => result,
        joined = &mut actor_task => {
            actor_finished = true;
            let error = match joined {
                Ok(()) if cancel.is_cancelled() => None,
                Ok(()) => Some(std::io::Error::other("daemon actor stopped unexpectedly")),
                Err(error) if error.is_cancelled() && cancel.is_cancelled() => {
                    tracing::debug!("daemon actor task cancelled during shutdown");
                    None
                }
                Err(error) => {
                    tracing::error!(%error, "daemon actor task failed");
                    Some(std::io::Error::other(error))
                }
            };
            if let Some(error) = error {
                cancel.cancel();
                if let Err(serve_error) = serve_fut.await {
                    tracing::error!(%serve_error, "daemon control plane shutdown failed after actor exit");
                }
                Err(error)
            } else {
                serve_fut.await
            }
        }
        // 监控只会在取消或租约确认丢失时结束；取消不能掩盖 panic 或租约失败。
        lease = &mut lease_task => {
            lease_finished = true;
            let error = match lease {
                Ok(Ok(())) if cancel.is_cancelled() => None,
                Ok(Ok(())) => Some(std::io::Error::other("engine writer lease monitor stopped unexpectedly")),
                Ok(Err(error)) => {
                    tracing::error!(%error, "engine writer lease lost; shutting down");
                    Some(std::io::Error::other(error))
                }
                Err(error) if error.is_cancelled() && cancel.is_cancelled() => {
                    tracing::debug!("engine writer lease monitor cancelled during shutdown");
                    None
                }
                Err(error) => {
                    tracing::error!(%error, "engine writer lease monitor failed; shutting down");
                    Some(std::io::Error::other(error))
                }
            };
            if let Some(error) = error {
                lease_lost = true;
                cancel.cancel();
                if let Err(serve_error) = serve_fut.await {
                    tracing::error!(%serve_error, "daemon control plane shutdown failed after lease loss");
                }
                Err(error)
            } else {
                serve_fut.await
            }
        }
    };
    if lease_lost {
        // 租约已丢失，不再执行会写库的常规收尾；仍回收任务，区分主动取消和 panic。
        actor_task.abort();
        if let Some(progress_task) = &progress_task {
            progress_task.abort();
        }
        startup_maintenance_task.abort();
        sweep_task.abort();
        if !actor_finished {
            match actor_task.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {
                    tracing::debug!("daemon actor aborted after lease loss")
                }
                Err(error) => tracing::error!(%error, "daemon actor failed after lease loss"),
            }
        }
        if let Some(progress_task) = progress_task {
            match progress_task.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {
                    tracing::debug!("progress reporter aborted after lease loss")
                }
                Err(error) => tracing::error!(%error, "progress reporter failed after lease loss"),
            }
        }
        match startup_maintenance_task.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {
                tracing::debug!("startup maintenance aborted after lease loss")
            }
            Err(error) => tracing::error!(%error, "startup maintenance failed after lease loss"),
        }
        match sweep_task.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {
                tracing::debug!("blob sweeper aborted after lease loss")
            }
            Err(error) => tracing::error!(%error, "blob sweeper failed after lease loss"),
        }
        return result.map_err(Into::into);
    }
    if !actor_finished {
        match tokio::time::timeout(Duration::from_secs(10), actor.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(crate::actor::ActorCallError::Unavailable)) => {
                // actor 可能已响应外部取消而关闭；下面仍 join，不能把 panic 当正常关闭。
                tracing::debug!("daemon actor closed before shutdown acknowledgement");
            }
            Ok(Err(error)) => {
                tracing::error!(%error, "daemon actor shutdown failed");
                if result.is_ok() {
                    result = Err(std::io::Error::other(error));
                }
            }
            Err(error) => {
                tracing::error!(%error, "daemon actor shutdown acknowledgement timed out");
                if result.is_ok() {
                    result = Err(std::io::Error::new(std::io::ErrorKind::TimedOut, error));
                }
            }
        }
    }
    cancel.cancel();
    match sweep_task.await {
        Ok(()) => {}
        Err(error) if error.is_cancelled() => {
            tracing::debug!("daemon blob sweeper cancelled during shutdown")
        }
        Err(error) => {
            tracing::error!(%error, "daemon blob sweeper failed");
            if result.is_ok() {
                result = Err(std::io::Error::other(error));
            }
        }
    }
    if !lease_finished {
        match lease_task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!(%error, "engine writer lease monitor failed during shutdown");
                if result.is_ok() {
                    result = Err(std::io::Error::other(error));
                }
            }
            Err(error) if error.is_cancelled() => {
                tracing::debug!("engine writer lease monitor cancelled during shutdown")
            }
            Err(error) => {
                tracing::error!(%error, "engine writer lease monitor task failed during shutdown");
                if result.is_ok() {
                    result = Err(std::io::Error::other(error));
                }
            }
        }
    }
    if !actor_finished {
        match tokio::time::timeout(Duration::from_secs(10), &mut actor_task).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if error.is_cancelled() => {
                tracing::debug!("daemon actor cancelled during shutdown")
            }
            Ok(Err(error)) => {
                tracing::error!(%error, "daemon actor failed during shutdown");
                if result.is_ok() {
                    result = Err(std::io::Error::other(error));
                }
            }
            Err(error) => {
                tracing::error!(%error, "daemon actor shutdown timed out");
                if result.is_ok() {
                    result = Err(std::io::Error::new(std::io::ErrorKind::TimedOut, error));
                }
                actor_task.abort();
                match actor_task.await {
                    Ok(()) => {}
                    Err(error) if error.is_cancelled() => {
                        tracing::debug!("daemon actor aborted after shutdown timeout")
                    }
                    Err(error) => {
                        tracing::error!(%error, "daemon actor failed after shutdown timeout")
                    }
                }
            }
        }
    }
    if let Some(mut progress_task) = progress_task {
        match tokio::time::timeout(Duration::from_secs(10), &mut progress_task).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if error.is_cancelled() => {
                tracing::debug!("progress reporter cancelled before journal flush")
            }
            Ok(Err(error)) => {
                tracing::error!(%error, "progress reporter stopped before journal flush");
                if result.is_ok() {
                    result = Err(std::io::Error::other(error));
                }
            }
            Err(error) => {
                tracing::error!(%error, "progress reporter did not drain before journal flush");
                if result.is_ok() {
                    result = Err(std::io::Error::new(std::io::ErrorKind::TimedOut, error));
                }
                progress_task.abort();
                match progress_task.await {
                    Ok(()) => {}
                    Err(error) if error.is_cancelled() => {
                        tracing::debug!("progress reporter aborted after shutdown timeout")
                    }
                    Err(error) => {
                        tracing::error!(%error, "progress reporter failed after shutdown timeout")
                    }
                }
            }
        }
    }
    match tokio::time::timeout(Duration::from_secs(10), activity_journal.flush()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::error!(%error, "task activity journal final flush failed");
            if result.is_ok() {
                result = Err(std::io::Error::other(error));
            }
        }
        Err(error) => {
            tracing::error!(%error, "task activity journal final flush timed out");
            if result.is_ok() {
                result = Err(std::io::Error::new(std::io::ErrorKind::TimedOut, error));
            }
        }
    }
    if !startup_maintenance_task.is_finished() {
        startup_maintenance_task.abort();
    }
    match startup_maintenance_task.await {
        Ok(()) => {}
        Err(error) if error.is_cancelled() => {
            tracing::debug!("startup maintenance cancelled during shutdown")
        }
        Err(error) => {
            tracing::error!(%error, "startup maintenance task failed");
            if result.is_ok() {
                result = Err(std::io::Error::other(error));
            }
        }
    }
    if let Err(error) = blobs.cleanup_all().await {
        tracing::warn!(%error, "daemon temporary cleanup failed");
        if result.is_ok() {
            result = Err(std::io::Error::other(error));
        }
    }
    result.map_err(Into::into)
}

/// An OS-backed process lease that identifies an already-running fluxdownd for this data dir.
/// It is intentionally distinct from the engine writer lease: another host may legitimately hold
/// the latter, which remains a startup error for this daemon.
struct DaemonProcessLease {
    _file: File,
}

impl DaemonProcessLease {
    fn acquire(data_dir: &Path) -> Result<Option<Self>, std::io::Error> {
        std::fs::create_dir_all(data_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(data_dir.join("daemon.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}
async fn open_database(
    config: &DaemonConfig,
    data_dir: &std::path::Path,
) -> Result<(Db, EngineWriteGuard), fluxdown_engine::db::DbError> {
    match &config.database_url {
        Some(url) => Db::connect_exclusive(url, data_dir).await,
        None => Db::open_exclusive(data_dir).await,
    }
}

/// 播种默认配置与内置队列后读取配置与初始快照。
///
/// 初始快照只在此处从库读取一次，后续靠引擎事件增量更新；而引擎启动时不会
/// 广播队列列表，所以 `main` / `later` 必须在读快照**之前**播种，否则全新
/// 数据目录的首次启动会给客户端一个空队列列表（`default_queue_id` 同理）。
/// `Engine::from_db` 内的二次播种由 `builtin_queues_seeded` 守卫为空操作。
async fn bootstrap_database(
    db: &Db,
    default_save_dir: &str,
) -> Result<(HashMap<String, String>, DaemonSnapshot), fluxdown_engine::db::DbError> {
    db.init_default_config(default_save_dir).await?;
    db.seed_builtin_queues().await?;
    let config = db.get_all_config().await?;
    let snapshot = initial_snapshot(db, &config).await?;
    Ok((config, snapshot))
}

async fn initial_snapshot(
    db: &Db,
    config: &HashMap<String, String>,
) -> Result<DaemonSnapshot, fluxdown_engine::db::DbError> {
    Ok(DaemonSnapshot {
        task_runtime: Default::default(),
        tasks: db
            .load_all_tasks()
            .await?
            .into_iter()
            .map(fluxdown_engine_protocol::task_info_to_dto)
            .collect(),
        queues: db
            .load_all_queues()
            .await?
            .into_iter()
            .map(fluxdown_engine_protocol::queue_info_to_dto)
            .collect(),
        groups: db
            .load_all_groups()
            .await?
            .into_iter()
            .map(fluxdown_engine_protocol::group_info_to_dto)
            .collect(),
        config: DaemonConfigSnapshot {
            revision: config
                .get("daemon_config_revision")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0),
            values: crate::config::public_config_values(config),
        },
        rss_sources: db
            .load_all_rss_sources()
            .await?
            .into_iter()
            .map(fluxdown_engine_protocol::rss_source_info_to_dto)
            .collect(),
        ..DaemonSnapshot::default()
    })
}

fn take_engine_receivers(engine: &mut Engine) -> Result<EngineReceivers, std::io::Error> {
    let done = engine
        .manager
        .take_done_rx()
        .ok_or_else(|| std::io::Error::other("done receiver already taken"))?;
    let retry = engine
        .manager
        .take_retry_rx()
        .ok_or_else(|| std::io::Error::other("retry receiver already taken"))?;
    let missing_cleanup = engine
        .manager
        .take_missing_cleanup_rx()
        .ok_or_else(|| std::io::Error::other("missing cleanup receiver already taken"))?;
    let (plugin_tx, plugin) = tokio::sync::mpsc::unbounded_channel();
    #[cfg(feature = "plugins")]
    {
        let mut resolve = engine
            .manager
            .take_resolve_rx()
            .ok_or_else(|| std::io::Error::other("resolve receiver already taken"))?;
        let resolve_tx = plugin_tx.clone();
        tokio::spawn(async move {
            while let Some(outcome) = resolve.recv().await {
                if resolve_tx
                    .send(PluginEvent::Resolve(Box::new(outcome)))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut retry_events = engine
            .manager
            .take_plugin_retry_rx()
            .ok_or_else(|| std::io::Error::other("plugin retry receiver already taken"))?;
        let retry_tx = plugin_tx.clone();
        tokio::spawn(async move {
            while let Some((task_id, delay_ms)) = retry_events.recv().await {
                if retry_tx
                    .send(PluginEvent::Retry { task_id, delay_ms })
                    .is_err()
                {
                    break;
                }
            }
        });
    }
    drop(plugin_tx);
    Ok(EngineReceivers {
        done,
        retry,
        plugin,
        missing_cleanup,
    })
}

fn apply_manager_settings(engine: &mut Engine, config: &HashMap<String, String>) {
    engine
        .manager
        .set_default_segments(i32_config(config, "default_segments", 0));
    engine
        .manager
        .set_auto_max_connections(i32_config(config, "auto_max_connections", 16));
    engine
        .manager
        .set_cdn_multi_enabled(bool_config(config, "cdn_multi_enabled", false));
    engine
        .manager
        .set_cdn_max_nodes(i32_config(config, "cdn_max_nodes", 0).clamp(0, 8));
    engine
        .manager
        .set_multi_nic_enabled(bool_config(config, "multi_nic_enabled", false));
    engine
        .manager
        .set_max_auto_retries(i32_config(config, "max_auto_retries", 3));
    engine
        .manager
        .set_auto_retry_delay_secs(u64_config(config, "auto_retry_delay_secs", 2));
    engine
        .manager
        .set_use_server_time(bool_config(config, "use_server_time", false));
    engine.manager.set_file_exists_behavior(
        config
            .get("file_exists_behavior")
            .map(|value| download_manager::FileExistsBehavior::from_config_str(value))
            .unwrap_or(download_manager::FileExistsBehavior::Rename),
    );
    engine.manager.set_missing_file_auto_delete(
        config
            .get("file_missing_action")
            .is_some_and(|value| value == "delete"),
    );
    engine
        .manager
        .set_idle_file_scan(bool_config(config, "idle_file_scan", false));
    engine.manager.set_completed_record_retention_days(
        u32::try_from(i32_config(config, "completed_record_retention_days", 0).clamp(0, 3650))
            .unwrap_or(0),
    );
    #[cfg(feature = "bt")]
    engine
        .manager
        .set_bt_guard_config(crate::config::bt_guard_config_from_map(config));
}

fn configured_save_dir(config: &HashMap<String, String>, fallback: String) -> String {
    config
        .get("default_save_dir")
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or(fallback)
}

fn bool_config(config: &HashMap<String, String>, key: &str, fallback: bool) -> bool {
    config
        .get(key)
        .map_or(fallback, |value| value == "true" || value == "1")
}

fn i32_config(config: &HashMap<String, String>, key: &str, fallback: i32) -> i32 {
    config
        .get(key)
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(fallback)
}

fn u64_config(config: &HashMap<String, String>, key: &str, fallback: u64) -> u64 {
    config
        .get(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(fallback)
}

fn usize_config(config: &HashMap<String, String>, key: &str, fallback: usize) -> usize {
    config
        .get(key)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(fallback)
}

fn spawn_blob_sweeper(
    blobs: Arc<BlobStore>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    if let Err(error) = blobs.sweep().await {
                        tracing::warn!(error = %error, "daemon blob sweep failed");
                    }
                }
            }
        }
    })
}

/// 引擎写入者租约的心跳间隔：PostgreSQL advisory lock 的寿命等于持锁连接的寿命，
/// 连接被中间设备静默断开后必须在有限时间内发现。
const LEASE_CHECK_INTERVAL: Duration = Duration::from_secs(30);
/// 单次校验失败（多半是网络抖动）后的重试间隔与总尝试次数。
const LEASE_RETRY_DELAY: Duration = Duration::from_secs(3);
const LEASE_CHECK_ATTEMPTS: u32 = 3;

fn spawn_lease_monitor(
    guard: Arc<EngineWriteGuard>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<(), DbError>> {
    tokio::spawn(monitor_writer_lease(
        move || {
            let guard = guard.clone();
            async move { guard.verify_lease().await }
        },
        cancel,
        LEASE_CHECK_INTERVAL,
        LEASE_RETRY_DELAY,
        LEASE_CHECK_ATTEMPTS,
    ))
}

/// 每隔 `interval` 调用一次 `verify`，直到取消。
///
/// 租约被别的会话确认夺走（`WriterLeaseHeld`）立即返回错误；其它失败先按 `retry_delay`
/// 重试，连续 `attempts` 次都失败才视为租约已丢失——持锁连接已断时每次都会失败，
/// 而网络抖动不应该让整个 daemon 退出。
async fn monitor_writer_lease<F, Fut>(
    verify: F,
    cancel: CancellationToken,
    interval: Duration,
    retry_delay: Duration,
    attempts: u32,
) -> Result<(), DbError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<(), DbError>>,
{
    loop {
        tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            () = tokio::time::sleep(interval) => {}
        }
        let mut attempt = 1;
        loop {
            match verify().await {
                Ok(()) => break,
                Err(error @ DbError::WriterLeaseHeld(_)) => return Err(error),
                Err(error) if attempt >= attempts => return Err(error),
                Err(error) => {
                    tracing::warn!(attempt, error = %error, "engine writer lease check failed; retrying");
                    attempt += 1;
                    tokio::select! {
                        () = cancel.cancelled() => return Ok(()),
                        () = tokio::time::sleep(retry_delay) => {}
                    }
                }
            }
        }
    }
}

/// 只宣称已完成初始化的能力。
#[must_use]
pub fn runtime_capabilities(engine_initialized: bool) -> Vec<String> {
    if !engine_initialized {
        return Vec::new();
    }
    let capabilities = vec![
        fluxdown_protocol::method::CAPABILITY_DAEMON_TASKS.to_owned(),
        fluxdown_protocol::method::CAPABILITY_DAEMON_QUEUES.to_owned(),
        fluxdown_protocol::method::CAPABILITY_DAEMON_GROUPS.to_owned(),
        fluxdown_protocol::method::CAPABILITY_DAEMON_CONFIG.to_owned(),
        fluxdown_protocol::method::CAPABILITY_DAEMON_RSS.to_owned(),
        fluxdown_protocol::method::CAPABILITY_DAEMON_WEBHOOKS.to_owned(),
        fluxdown_protocol::method::CAPABILITY_DAEMON_SELECTIONS.to_owned(),
        fluxdown_protocol::method::CAPABILITY_DAEMON_FILES.to_owned(),
    ];
    #[cfg(feature = "plugins")]
    let capabilities = {
        let mut enabled = capabilities;
        enabled.push(fluxdown_protocol::method::CAPABILITY_DAEMON_PLUGINS.to_owned());
        enabled
    };
    #[cfg(feature = "components")]
    let capabilities = {
        let mut enabled = capabilities;
        enabled.push(fluxdown_protocol::method::CAPABILITY_DAEMON_COMPONENTS.to_owned());
        enabled
    };
    capabilities
}

fn config_enabled(config: &HashMap<String, String>, key: &str, default: bool) -> bool {
    config
        .get(key)
        .map(|value| matches!(value.as_str(), "true" | "1"))
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use fluxdown_engine::db::DbError;
    use tokio_util::sync::CancellationToken;

    use super::{
        EndpointGuard, bind_control_plane, bootstrap_database, endpoint_file_path,
        monitor_writer_lease, publish_endpoint_file,
    };

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fluxdown-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[tokio::test]
    async fn bind_falls_back_only_when_allowed() {
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("occupy port");
        let addr = occupied.local_addr().expect("occupied addr");

        let fallback = bind_control_plane(addr, true)
            .await
            .expect("fallback bind succeeds");
        let bound = fallback.local_addr().expect("fallback addr");
        assert_ne!(bound.port(), addr.port());
        assert!(bound.ip().is_loopback());

        let error = bind_control_plane(addr, false)
            .await
            .expect_err("strict bind must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        assert!(error.to_string().contains(&addr.to_string()), "{error}");
    }

    #[tokio::test]
    async fn endpoint_file_replaces_stale_content_and_guard_removes_it() {
        let dir = temp_dir("endpoint");
        let path = dir.join("daemon-endpoint.json");
        std::fs::write(&path, "stale").expect("write stale file");
        let bound: std::net::SocketAddr = "127.0.0.1:4321".parse().expect("addr");

        publish_endpoint_file(&path, bound).await.expect("publish");
        let endpoint: fluxdown_protocol::local_service::LocalEndpoint =
            serde_json::from_slice(&std::fs::read(&path).expect("read endpoint"))
                .expect("parse endpoint");
        assert_eq!(endpoint.rpc_url, "ws://127.0.0.1:4321/rpc");

        drop(EndpointGuard(path.clone()));
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn endpoint_file_sits_next_to_the_token() {
        let data = std::path::Path::new("data");
        let mut config = crate::config::DaemonConfig::embedded(data.to_path_buf(), None);
        assert_eq!(
            endpoint_file_path(&config, data),
            data.join("daemon-endpoint.json")
        );
        config.token_file_override = Some(std::path::PathBuf::from("tokens/daemon.token"));
        assert_eq!(
            endpoint_file_path(&config, data),
            std::path::Path::new("tokens").join("daemon-endpoint.json")
        );
        config.token_file_override = Some(std::path::PathBuf::from("daemon.token"));
        assert_eq!(
            endpoint_file_path(&config, data),
            data.join("daemon-endpoint.json")
        );
    }

    #[tokio::test]
    async fn fresh_database_snapshot_contains_builtin_queues() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_daemon_runtime_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create runtime test dir");
        let db = fluxdown_engine::db::Db::open(&dir)
            .await
            .expect("open runtime test db");

        let (_config, snapshot) = bootstrap_database(&db, "/tmp")
            .await
            .expect("bootstrap fresh db");

        let ids: Vec<&str> = snapshot
            .queues
            .iter()
            .map(|queue| queue.queue_id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                fluxdown_protocol::MAIN_QUEUE_ID,
                fluxdown_protocol::LATER_QUEUE_ID
            ]
        );
        assert_eq!(
            snapshot
                .config
                .values
                .get("default_queue_id")
                .map(String::as_str),
            Some(fluxdown_protocol::MAIN_QUEUE_ID)
        );
        drop(db);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            eprintln!("runtime test directory removal failed: {error}");
        }
    }

    /// 租约校验探针：按脚本依次返回结果，脚本耗尽后恒成功；同时记录被调用次数。
    fn scripted(
        script: Vec<Result<(), DbError>>,
    ) -> (
        impl Fn() -> std::future::Ready<Result<(), DbError>>,
        Arc<AtomicUsize>,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let script = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
            script,
        )));
        let counter = calls.clone();
        let verify = move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let next = script
                .lock()
                .ok()
                .and_then(|mut script| script.pop_front())
                .unwrap_or(Ok(()));
            std::future::ready(next)
        };
        (verify, calls)
    }

    fn transient() -> DbError {
        DbError::Io(std::io::Error::other("connection reset"))
    }

    const TICK: Duration = Duration::from_millis(5);

    #[tokio::test]
    async fn lease_monitor_keeps_running_until_cancelled_while_the_lease_holds() {
        let (verify, calls) = scripted(Vec::new());
        let cancel = CancellationToken::new();
        let monitor = tokio::spawn(monitor_writer_lease(verify, cancel.clone(), TICK, TICK, 3));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!monitor.is_finished());
        assert!(
            calls.load(Ordering::SeqCst) >= 2,
            "lease is checked periodically"
        );
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), monitor)
            .await
            .expect("monitor stops on cancel")
            .expect("monitor task");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn lease_held_by_another_session_is_fatal_without_retrying() {
        let (verify, calls) = scripted(vec![Err(DbError::WriterLeaseHeld("pg".to_owned()))]);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            monitor_writer_lease(verify, CancellationToken::new(), TICK, TICK, 3),
        )
        .await
        .expect("monitor reports the loss");
        assert!(matches!(result, Err(DbError::WriterLeaseHeld(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn transient_check_failures_recover_but_persistent_ones_are_fatal() {
        // 两次瞬时失败后恢复：不误杀。
        let (verify, calls) = scripted(vec![Err(transient()), Err(transient())]);
        let cancel = CancellationToken::new();
        let monitor = tokio::spawn(monitor_writer_lease(verify, cancel.clone(), TICK, TICK, 3));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(!monitor.is_finished(), "recovered monitor keeps running");
        assert!(calls.load(Ordering::SeqCst) >= 3);
        cancel.cancel();
        monitor
            .await
            .expect("lease monitor task")
            .expect("recovered lease monitor stops cleanly on cancel");

        // 连续 `attempts` 次都失败：连接已不可用，租约视为丢失。
        let (verify, calls) = scripted(vec![
            Err(transient()),
            Err(transient()),
            Err(transient()),
            Err(transient()),
        ]);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            monitor_writer_lease(verify, CancellationToken::new(), TICK, TICK, 3),
        )
        .await
        .expect("monitor gives up");
        assert!(matches!(result, Err(DbError::Io(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
