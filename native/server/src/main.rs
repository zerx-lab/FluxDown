//! FluxDown headless 服务器 —— 复用 `fluxdown_engine` 下载引擎与
//! `fluxdown_api` HTTP 契约，外加 WebSocket 实时推送与 Web SPA 托管。
//!
//! 端到端：浏览器打开服务器地址 → token 登录 → 三栏任务界面管理下载 →
//! WS 实时进度 → 完成后经 `/api/v1/tasks/{id}/file` 流式取回。
//!
//! 运行：`cargo run -p fluxdown_server`（环境变量见 [`config`] 模块文档）。

mod actor;
mod config;
mod demo;
mod host;
mod routes_ext;
mod wire;
mod ws_hub;

use std::sync::Arc;

use axum::Router;
use fluxdown_api::server::{ApiServerConfig, api_router};
use fluxdown_engine::db::Db;
use fluxdown_engine::download_manager::{self};
use fluxdown_engine::events::EventSink;
use fluxdown_engine::log_info;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::selection::HostSelection;
use fluxdown_engine::{Engine, EngineConfig};
use tokio::sync::mpsc;
use tower_http::services::{ServeDir, ServeFile};

use crate::actor::{ActorCmd, bt_config_from_map, run_actor};
use crate::config::{ServerConfig, default_save_dir, ensure_server_config};
use crate::host::ServerApiHost;
use crate::routes_ext::{ServerState, extra_router};
use crate::ws_hub::{EngineEventSink, WsHostSelection, WsHub};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    fluxdown_engine::logger::init();
    let server_cfg = ServerConfig::from_env();

    // 数据目录：显式覆盖或平台自动探测（与桌面一致的解析器）。
    let data_dir =
        fluxdown_engine::data_dir::resolve_data_dir(server_cfg.data_dir_override.as_deref())?;
    log_info!("[server] data dir: {}", data_dir.display());
    if let Some(url) = &server_cfg.database_url {
        // 打印时掩掉凭证段，避免密码进日志。
        let masked = url.split('@').next_back().unwrap_or(url);
        log_info!("[server] database: {} (external)", masked);
    }

    // 引导连接：读初始配置 + 首次运行初始化（Engine::new 内部会再建一个池，
    // 与桌面 App 的双开模式一致——SQLite WAL / pg 均安全）。
    let boot_db = match &server_cfg.database_url {
        Some(url) => Db::connect(url).await?,
        None => Db::open(&data_dir).await?,
    };
    boot_db.init_default_config(&default_save_dir()).await?;
    let token = ensure_server_config(&boot_db).await?;

    let all_cfg = boot_db.get_all_config().await.unwrap_or_default();
    let max_concurrent = all_cfg
        .get("max_concurrent_tasks")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5);
    let speed_limit_bps = all_cfg
        .get("speed_limit_bytes")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let save_dir = {
        let dir = all_cfg.get("default_save_dir").cloned().unwrap_or_default();
        if dir.trim().is_empty() {
            default_save_dir()
        } else {
            dir
        }
    };
    let default_segments = all_cfg
        .get("default_segments")
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(0);

    // WS 中枢：任务进度/快照广播 + HLS/BT 选择 + aria2 生命周期事件源
    // （ServerApiHost::subscribe_task_events 经下方 hub.clone() 共享同一个
    // 实例；EngineEventSink 按 ws_hub 模块文档的统一规则把状态迁移映射为
    // TaskEvent 广播）。
    let hub = Arc::new(WsHub::new(1024));
    let sink: Arc<dyn EventSink> = Arc::new(EngineEventSink(hub.clone()));
    let selector: Arc<dyn HostSelection> = Arc::new(WsHostSelection(hub.clone()));

    let mut engine = Engine::new(
        EngineConfig {
            max_concurrent,
            speed_limit_bps,
            default_save_dir: save_dir.clone(),
            app_data_dir: data_dir.to_string_lossy().into_owned(),
            bt_config: bt_config_from_map(&all_cfg),
            proxy_config: ProxyConfig::from_config_map(&all_cfg),
            user_agent: all_cfg
                .get("global_user_agent")
                .cloned()
                .unwrap_or_default(),
            data_dir_override: Some(data_dir.clone()),
            database_url: server_cfg.database_url.clone(),
        },
        sink.clone(),
        selector.clone(),
    )
    .await?;

    engine.manager.set_default_segments(default_segments);
    if let Some(v) = all_cfg
        .get("max_auto_retries")
        .and_then(|s| s.parse::<i32>().ok())
    {
        engine.manager.set_max_auto_retries(v);
    }
    if let Some(v) = all_cfg
        .get("auto_retry_delay_secs")
        .and_then(|s| s.parse::<u64>().ok())
    {
        engine.manager.set_auto_retry_delay_secs(v);
    }

    // 进度上报旁路：progress_rx 独立消费（不 spawn 则无任何进度事件）。
    if let Some(rx) = engine.manager.take_progress_rx() {
        tokio::spawn(download_manager::progress_reporter(
            rx,
            engine.db.clone(),
            sink.clone(),
        ));
    }
    let done_rx = engine
        .manager
        .take_done_rx()
        .ok_or("take_done_rx returned None (already taken)")?;
    let retry_rx = engine
        .manager
        .take_retry_rx()
        .ok_or("take_retry_rx returned None (already taken)")?;

    let db_handle = engine.db.clone();
    let selector_handle = engine.selector.clone();

    // actor 独占 engine；HTTP 层经 cmd_tx 写入。
    let (cmd_tx, cmd_rx) = mpsc::channel::<ActorCmd>(64);
    tokio::spawn(run_actor(engine, cmd_rx, done_rx, retry_rx));

    // 路由：核心（fluxdown_api 复用）+ 扩展（本 crate）+ SPA 静态托管。
    let api_cfg = ApiServerConfig::from_config_map(&all_cfg, env!("CARGO_PKG_VERSION"));
    let api_cfg = ApiServerConfig {
        token: token.clone(),
        management_enabled: true,
        ..api_cfg
    };
    let host: Arc<dyn fluxdown_api::service::ApiHost> = Arc::new(ServerApiHost::new(
        db_handle.clone(),
        cmd_tx.clone(),
        hub.clone(),
        server_cfg.demo_url.clone(),
    ));
    if let Some(url) = &server_cfg.demo_url {
        log_info!("[server] demo mode enabled, allowed url: {}", url);
        eprintln!("演示模式已开启：仅允许下载 {url}");
    }
    let state = ServerState {
        db: db_handle,
        cmd_tx,
        hub,
        selector: selector_handle,
        token,
        version: env!("CARGO_PKG_VERSION").to_string(),
        demo_url: server_cfg.demo_url.clone(),
    };
    let spa = ServeDir::new(&server_cfg.webroot)
        .fallback(ServeFile::new(server_cfg.webroot.join("index.html")));
    let mut app: Router = api_router(host, api_cfg).merge(extra_router(state));
    if server_cfg.demo_url.is_some() {
        // 内置演示下载源（无鉴权，生成字节流）；仅演示模式挂载。
        app = app.merge(demo::demo_router());
    }
    let app = app.fallback_service(spa);

    let listener = tokio::net::TcpListener::bind(&server_cfg.bind).await?;
    log_info!("[server] listening on {}", server_cfg.bind);
    eprintln!("FluxDown Server listening on http://{}", server_cfg.bind);
    eprintln!("  Web UI:    http://{}/", server_cfg.bind);
    eprintln!("  API docs:  http://{}/api/v1/docs", server_cfg.bind);

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            log_info!("[server] ctrl-c received, shutting down");
        })
        .await?;
    Ok(())
}
