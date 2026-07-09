//! B 模式：内嵌 [`fluxdown_engine::Engine`] 的一次性独立下载（`fluxdown add --local`）。
//!
//! 不连接运行中的 App/headless server，在本进程内构造引擎、创建任务、阻塞等待至
//! 终态后退出。与 A 模式共享同一数据目录/SQLite（安装模式），任务对 App/server 可见。
//!
//! 结构照 `native/engine/examples/headless_download.rs`：直接 `&mut Engine` 顺序调用，
//! 不搭 actor（一次性进程仅单一调用方）。HLS/BT 走 [`NoopSelection`]（HLS 取最高码率、
//! BT 下全部文件）；进度经 [`progress_reporter`] 排空并落 DB（不排空会因通道背压卡死）。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use fluxdown_cli::client::ClientError;
use fluxdown_cli::exit::ExitCode;
use fluxdown_engine::bt_downloader::BtConfig;
use fluxdown_engine::data_dir::resolve_data_dir;
use fluxdown_engine::db::Db;
use fluxdown_engine::download_manager::progress_reporter;
use fluxdown_engine::events::EventSink;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

use crate::AddArgs;

/// 在本进程内嵌引擎完成 `add --local` 下载：收集 URL → 解析数据目录/落盘目录 →
/// 构造 [`Engine`] → 排空进度通道 → 逐 URL `create_task` → 等待全部终态或 Ctrl-C →
/// 按终态 DB 状态映射 aria2 退出码。返回的 [`ClientError`] 携带退出码，由 `run()` 打印。
pub async fn run_add_local(args: AddArgs, json: bool) -> Result<(), ClientError> {
    // 1) 收集 URL（复用 A 模式约定：命令行 URL + -i 文件；空列表 → BadRequest）。
    let mut urls = args.urls.clone();
    if let Some(f) = &args.input_file {
        match crate::read_url_file(f) {
            Ok(mut extra) => urls.append(&mut extra),
            Err(e) => return Err(ClientError::new(e, ExitCode::BadRequest)),
        }
    }
    if urls.is_empty() {
        return Err(ClientError::new(
            "no URLs given (pass URLs or -i <file>)",
            ExitCode::BadRequest,
        ));
    }

    // 2) 数据目录 + 落盘目录（优先级：-d > 共享库 default_save_dir 配置 > 当前工作目录）。
    let data_dir =
        resolve_data_dir(None).map_err(|e| ClientError::new(e.to_string(), ExitCode::Unknown))?;
    let save_dir = resolve_save_dir(args.dir.clone(), &data_dir).await?;

    // 3) 构造引擎（NoopSink/NoopSelection：无 UI 交互，HLS 取最高码率、BT 全下）。
    let sink: Arc<dyn EventSink> = Arc::new(NoopSink);
    let cfg = EngineConfig {
        max_concurrent: 4,
        speed_limit_bps: 0,
        default_save_dir: save_dir.clone(),
        app_data_dir: data_dir.to_string_lossy().into_owned(),
        bt_config: BtConfig::default(),
        proxy_config: ProxyConfig::default(),
        user_agent: args.user_agent.clone().unwrap_or_default(),
        data_dir_override: None,
        database_url: None,
    };
    let mut engine = Engine::new(cfg, sink.clone(), Arc::new(NoopSelection))
        .await
        .map_err(|e| ClientError::new(e.to_string(), ExitCode::Unknown))?;

    // 排空进度通道：段协调器每 ~200ms 阻塞 send，通道满（容量 8192）会卡死下载。
    // 复刻 server/hub 的 progress_reporter 接线（顺带把 downloaded_bytes 落 DB）。
    if let Some(prx) = engine.manager.take_progress_rx() {
        tokio::spawn(progress_reporter(prx, engine.db.clone(), sink.clone()));
    }
    let mut done_rx = engine
        .manager
        .take_done_rx()
        .ok_or_else(|| ClientError::new("engine done channel unavailable", ExitCode::Unknown))?;

    // 4) 逐 URL 建任务；仅成功者（create_task 返 Some）进入等待集合。
    let mut created_ids: Vec<String> = Vec::with_capacity(urls.len());
    let mut first_err: Option<ClientError> = None;
    for url in &urls {
        let id = engine
            .manager
            .create_task(
                url.clone(),
                save_dir.clone(),
                args.out.clone().unwrap_or_default(),
                args.segments.unwrap_or(0),
                args.cookies.clone().unwrap_or_default(),
                args.referrer.clone().unwrap_or_default(),
                0,
                Vec::new(),
                args.proxy.clone().unwrap_or_default(),
                args.user_agent.clone().unwrap_or_default(),
                args.queue.clone().unwrap_or_default(),
                args.checksum.clone().unwrap_or_default(),
                HashMap::new(),
                Vec::new(),
                None,
                None,
                None,
            )
            .await;
        match id {
            Some(id) => created_ids.push(id),
            None => {
                eprintln!("fluxdown: failed to create task for {url}");
                if first_err.is_none() {
                    first_err = Some(ClientError::new(
                        format!("failed to create task for {url}"),
                        ExitCode::Unknown,
                    ));
                }
            }
        }
    }
    if created_ids.is_empty() {
        return Err(
            first_err.unwrap_or_else(|| ClientError::new("no task created", ExitCode::Unknown))
        );
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&created_ids).unwrap_or_default()
        );
    } else {
        for id in &created_ids {
            println!("added {id}");
        }
    }

    // 5) 等待全部终态或 Ctrl-C（无墙钟超时：跑到完成/中断，引擎内部超时处理卡顿）。
    let mut remaining: HashSet<String> = created_ids.iter().cloned().collect();
    while !remaining.is_empty() {
        tokio::select! {
            maybe_done = done_rx.recv() => {
                let Some(done) = maybe_done else { break };
                engine.manager.on_task_done(&done).await; // 内部 drain_queue 推进排队任务
                remaining.remove(&done.task_id);
            }
            _ = tokio::signal::ctrl_c() => {
                for id in &remaining {
                    engine.manager.pause_task(id).await; // pause 保留续传语义（非 cancel）
                }
                let _ = engine.db.wal_checkpoint().await; // pause 路径不自动 checkpoint，补一次
                return Err(ClientError::new(
                    "interrupted; downloads unfinished",
                    ExitCode::Unfinished,
                ));
            }
        }
    }

    // 6) 按终态 DB 状态映射退出码；取首个非成功（若创建阶段已有错误则沿用）。
    for id in &created_ids {
        let task = engine
            .db
            .load_task_by_id(id)
            .await
            .map_err(|e| ClientError::new(e.to_string(), ExitCode::Unknown))?;
        if let Some(t) = task
            && t.status == 4
            && first_err.is_none()
        {
            first_err = Some(ClientError::new(
                t.error_message.clone(),
                classify_error(&t.error_message),
            ));
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// 解析落盘目录：`-d` > 共享库 `default_save_dir` 配置（与 App/server 一致）> 当前工作目录。
///
/// 引擎对空 `save_dir` 无回退（直接 `PathBuf::from`），故 CLI 侧显式解析一个非空目录。
async fn resolve_save_dir(dir_arg: Option<String>, data_dir: &Path) -> Result<String, ClientError> {
    let mut save_dir = dir_arg.unwrap_or_default();
    if save_dir.trim().is_empty() {
        // boot_db 仅用于读共享库配置（与 server 引导同法；双池 SQLite WAL 安全）。
        let boot_db = Db::open(data_dir)
            .await
            .map_err(|e| ClientError::new(e.to_string(), ExitCode::Unknown))?;
        save_dir = boot_db
            .get_config("default_save_dir")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
    }
    if save_dir.trim().is_empty() {
        save_dir = std::env::current_dir()
            .map_err(|e| {
                ClientError::new(
                    format!("cannot resolve current dir: {e}"),
                    ExitCode::Unknown,
                )
            })?
            .to_string_lossy()
            .into_owned();
    }
    Ok(save_dir)
}

/// status=4 的最佳努力退出码。引擎 `error_message` 是非稳定的人类可读串，故只识别
/// 最鲁棒的超时信号，其余归 [`ExitCode::Unknown`]（脚本靠非零判失败即可；细分待
/// 引擎暴露结构化错误枚举）。
fn classify_error(msg: &str) -> ExitCode {
    let m = msg.to_ascii_lowercase();
    if m.contains("timed out") || m.contains("timeout") {
        ExitCode::Timeout
    } else {
        ExitCode::Unknown
    }
}
