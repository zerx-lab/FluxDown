//! FluxDown headless 服务器 —— 复用 `fluxdown_engine` 下载引擎与
//! `fluxdown_api` HTTP 契约，外加 WebSocket 实时推送与 Web SPA 托管。
//!
//! 端到端：浏览器打开服务器地址 → token 登录 → 三栏任务界面管理下载 →
//! WS 实时进度 → 完成后经 `/api/v1/tasks/{id}/file` 流式取回。
//!
//! 运行：`cargo run -p fluxdown_server`（环境变量见 [`config`] 模块文档）。

mod actor;
mod analytics;
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

use crate::actor::{ActorCmd, bt_config_from_map, refresh_tracker_sub, run_actor};
use crate::config::{ServerConfig, default_save_dir, ensure_server_config};
use crate::host::ServerApiHost;
use crate::routes_ext::{ServerState, extra_router};
use crate::ws_hub::{EngineEventSink, WsHostSelection, WsHub};

/// 服务器版本。发布流水线在编译期经 `FLUXDOWN_SERVER_VERSION` 注入 git tag
/// 版本号；本地开发构建（`cargo run` 未注入）时固定显示 `dev`，
/// 而非 crate 版本号（crate 版本不随发布演进，直接显示无意义）。
/// web 端据此跳过更新检测（`dev` 视为无版本，永不判定"有新版本"）。
pub(crate) const SERVER_VERSION: &str = {
    let injected = match option_env!("FLUXDOWN_SERVER_VERSION") {
        Some(v) => v,
        None => "",
    };
    if injected.is_empty() { "dev" } else { injected }
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_cfg = ServerConfig::from_env();

    // 数据目录：显式覆盖或平台自动探测（与桌面一致的解析器）。先解析、再初始化
    // 日志——日志须落到同一数据目录（`FLUXDOWN_DATA_DIR`，Docker 下为挂载卷
    // `/data`），而非平台默认的 HOME 路径，才能持久化并让「关于」页正确显示。
    let data_dir =
        fluxdown_engine::data_dir::resolve_data_dir(server_cfg.data_dir_override.as_deref())?;
    fluxdown_engine::logger::init_with_dir(&data_dir);
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

    // FLUXDOWN_LANG 是部署级默认语言：不写库，仅作设置页未保存过语言时的
    // 回退值（ServerApiHost::web_language 实时求值）——手动更改永远优先。
    if let Some(lang) = &server_cfg.language {
        log_info!("[server] default web language (FLUXDOWN_LANG): {}", lang);
    }

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
    // Auto 模式最大连接数上限。老库无此 key → 默认 16。
    engine.manager.set_auto_max_connections(
        all_cfg
            .get("auto_max_connections")
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(16),
    );
    // Multi-CDN 并发下载开关（实验性，P0）。老库无此 key → 默认关闭。
    engine.manager.set_cdn_multi_enabled(
        all_cfg
            .get("cdn_multi_enabled")
            .is_some_and(|s| s == "1" || s == "true"),
    );
    // 单任务最多钉定的 CDN 节点数，0..=8；0 = 自动档（按文件大小/并发推导）。
    // 老库无此 key → 默认 0。
    engine.manager.set_cdn_max_nodes(
        all_cfg
            .get("cdn_max_nodes")
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0)
            .clamp(0, 8),
    );
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
    // 下载完成后是否采用服务器 Last-Modified 作为文件修改时间（默认关闭）。
    if let Some(v) = all_cfg.get("use_server_time") {
        engine.manager.set_use_server_time(v == "true");
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
    let resolve_rx = engine
        .manager
        .take_resolve_rx()
        .ok_or("take_resolve_rx returned None (already taken)")?;
    let plugin_retry_rx = engine
        .manager
        .take_plugin_retry_rx()
        .ok_or("take_plugin_retry_rx returned None (already taken)")?;

    let db_handle = engine.db.clone();
    let selector_handle = engine.selector.clone();
    let plugin_manager = engine.manager.plugin_manager();
    // 组件 API（ffmpeg 探测/安装）不走 actor，直接持 Db + data_dir；取自
    // engine 而非局部 data_dir，保持与引擎内部解析结果一致（含 override）。
    let engine_data_dir = engine.data_dir.clone();

    // actor 独占 engine；HTTP 层经 cmd_tx 写入。
    let (cmd_tx, cmd_rx) = mpsc::channel::<ActorCmd>(64);
    tokio::spawn(run_actor(
        engine,
        cmd_rx,
        done_rx,
        retry_rx,
        resolve_rx,
        plugin_retry_rx,
    ));

    // 本地设备互联（P2P 局域网配对 + mDNS 发现 + 直连传输）。
    // 广播端口 = FLUXDOWN_BIND 的端口；FLUXDOWN_MDNS=off 时不主动广播（仍可手动配对）。
    let link_mgr = {
        let api_port = server_cfg
            .bind
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(17800);
        let self_name = std::env::var("FLUXDOWN_LINK_NAME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "FluxDown Server".to_string());
        let self_info = fluxdown_engine::link::SelfInfo {
            name: self_name,
            platform: Some("server".to_string()),
            app_version: Some(SERVER_VERSION.to_string()),
        };
        let (link_tx, mut link_rx) = mpsc::channel::<fluxdown_engine::link::LinkEngineEvent>(64);
        match fluxdown_engine::link::LinkManager::load(
            db_handle.clone(),
            self_info,
            api_port,
            link_tx,
        )
        .await
        {
            Ok(mgr) => {
                log_info!(
                    "[server] device link ready, fingerprint={}",
                    mgr.fingerprint()
                );
                // 事件仅记日志（headless 无交互 UI；配对成功/错误可在日志追踪）。
                tokio::spawn(async move {
                    while let Some(ev) = link_rx.recv().await {
                        match ev {
                            fluxdown_engine::link::LinkEngineEvent::Paired(r) => {
                                log_info!(
                                    "[server] paired device: {} ({})",
                                    r.name,
                                    r.short_fingerprint()
                                );
                            }
                            fluxdown_engine::link::LinkEngineEvent::Error(m) => {
                                log_info!("[server] link error: {}", m);
                            }
                            _ => {}
                        }
                    }
                });
                let mdns_on = std::env::var("FLUXDOWN_MDNS")
                    .map(|v| {
                        !matches!(
                            v.trim().to_ascii_lowercase().as_str(),
                            "0" | "false" | "off" | "no"
                        )
                    })
                    .unwrap_or(true);
                if mdns_on {
                    mgr.start_advertising();
                    log_info!("[server] mDNS advertising on port {}", api_port);
                }
                Some(mgr)
            }
            Err(e) => {
                log_info!("[server] device link init failed: {}", e);
                None
            }
        }
    };

    // 匿名统计（首次部署/每日活跃；不含任何下载任务信息）。
    // FLUXDOWN_ANALYTICS=off 或未配置 App-Key 时内部自行退出。
    tokio::spawn(analytics::run(db_handle.clone(), SERVER_VERSION));

    // Tracker 订阅启动自动刷新：启用且缓存超过刷新周期未更新时，后台拉取一次
    // （镜像桌面 download_actor 的启动自刷新；不阻塞 serve）。
    {
        let sub_enabled = all_cfg
            .get("bt_tracker_sub_enabled")
            .map(|v| v == "true")
            .unwrap_or(true);
        let updated_at = all_cfg
            .get("bt_tracker_sub_updated_at")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if sub_enabled
            && now.saturating_sub(updated_at)
                > fluxdown_engine::tracker_subscription::REFRESH_INTERVAL_SECS
        {
            log_info!(
                "[server] tracker subscription stale (updated_at={}), auto-refreshing",
                updated_at
            );
            let db = db_handle.clone();
            let tx = cmd_tx.clone();
            tokio::spawn(async move {
                refresh_tracker_sub(&db, &tx).await;
            });
        }
    }

    // 路由：核心（fluxdown_api 复用）+ 扩展（本 crate）+ SPA 静态托管。
    let api_cfg = ApiServerConfig::from_config_map(&all_cfg, SERVER_VERSION);
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
        server_cfg.language.clone(),
        plugin_manager,
        engine_data_dir.clone(),
        link_mgr,
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
        version: SERVER_VERSION.to_string(),
        demo_url: server_cfg.demo_url.clone(),
        data_dir: engine_data_dir,
        ffmpeg_installing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ytdlp_installing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
