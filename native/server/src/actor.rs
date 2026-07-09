//! 下载 actor —— 单任务事件循环独占 `Engine`（`manager` 写方法 `&mut self`），
//! 所有写操作经 [`ActorCmd`] + oneshot 回执串行执行。
//!
//! 结构照抄 `hub/src/actors/download_actor.rs`，去掉 rinf 信号 /
//! Native Messaging / 更新器 / 文件关联等桌面 App 专属分支。

use std::collections::HashMap;
use std::time::Duration;

use fluxdown_api::types::CreateTaskRequest;
use fluxdown_engine::Engine;
use fluxdown_engine::bt_downloader::BtConfig;
use fluxdown_engine::db::Db;
use fluxdown_engine::download_manager::TaskDone;
use fluxdown_engine::log_info;
use fluxdown_engine::proxy_config::ProxyConfig;
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;

use crate::config::default_save_dir;

/// actor 写命令。每个变体携带 oneshot 回执：actor 完成后发送结果，
/// HTTP 层同步等待；接收端掉线（请求中止）时 `send` 失败直接忽略。
pub enum ActorCmd {
    /// 直接创建任务，回执新任务 ID；`None` = DB 插入失败。
    /// `req` 装箱：`CreateTaskRequest` 远大于其余变体。
    CreateTask {
        req: Box<CreateTaskRequest>,
        /// 文件大小提示（aria2/接管入口透传；REST 创建为 0）。
        hint_file_size: i64,
        ack: oneshot::Sender<Option<String>>,
    },
    PauseTask {
        task_id: String,
        ack: oneshot::Sender<()>,
    },
    ContinueTask {
        task_id: String,
        ack: oneshot::Sender<()>,
    },
    DeleteTask {
        task_id: String,
        delete_files: bool,
        ack: oneshot::Sender<()>,
    },
    PauseAll {
        ack: oneshot::Sender<()>,
    },
    ContinueAll {
        ack: oneshot::Sender<()>,
    },
    /// 配置键已写入 DB，按键名 live-apply 到引擎（镜像桌面 SaveConfig 分支）。
    ApplyConfig {
        keys: Vec<String>,
        ack: oneshot::Sender<()>,
    },
    CreateQueue {
        name: String,
        speed_limit_kbps: i64,
        max_concurrent: i32,
        default_save_dir: String,
        default_segments: i32,
        default_user_agent: String,
        ack: oneshot::Sender<()>,
    },
    UpdateQueue {
        queue_id: String,
        name: String,
        speed_limit_kbps: i64,
        max_concurrent: i32,
        default_save_dir: String,
        default_segments: i32,
        default_user_agent: String,
        ack: oneshot::Sender<()>,
    },
    DeleteQueue {
        queue_id: String,
        ack: oneshot::Sender<()>,
    },
    MoveToQueue {
        task_id: String,
        queue_id: String,
        ack: oneshot::Sender<()>,
    },
    /// Boost 优先下载（空 task_id = 取消 Boost）。
    Boost {
        task_id: String,
        ack: oneshot::Sender<()>,
    },
    TestProxy {
        proxy_type: String,
        host: String,
        port: String,
        username: String,
        password: String,
        ack: oneshot::Sender<Result<i64, String>>,
    },
}

/// actor 主循环。持有 `Engine` 直至进程退出。
pub async fn run_actor(
    mut engine: Engine,
    mut cmd_rx: mpsc::Receiver<ActorCmd>,
    mut done_rx: mpsc::Receiver<TaskDone>,
    mut retry_rx: mpsc::Receiver<String>,
) {
    // 启动预热：加载队列缓存（每队列限速/并发生效）+ 广播全量任务快照。
    engine.manager.load_queues().await;
    engine.manager.load_and_send_all_tasks().await;

    // 文件跟踪：headless 无窗口聚焦事件，用低频定时器周期性重扫已完成任务
    // 文件是否仍在。声明在 loop 外并消费首个立即就绪的 tick（启动扫描已由
    // load_and_send_all_tasks 覆盖）；MissedTickBehavior::Delay 防休眠唤醒后
    // 积压 tick 造成扫描风暴。
    let mut rescan_timer = tokio::time::interval(Duration::from_secs(300));
    rescan_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    rescan_timer.tick().await;

    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                handle_cmd(cmd, &mut engine).await;
            }
            Some(done) = done_rx.recv() => {
                engine.manager.on_task_done(&done).await;
            }
            Some(task_id) = retry_rx.recv() => {
                // 仅在任务仍处于 error 状态时自动恢复（用户可能已手动干预）。
                if engine.manager.is_task_in_error(&task_id).await {
                    log_info!("[server-actor] auto-retry: resuming task {}", task_id);
                    engine.manager.resume_task_auto(&task_id).await;
                }
            }
            _ = rescan_timer.tick() => {
                engine.manager.spawn_file_scan();
            }
            else => {
                log_info!("[server-actor] all channels closed, exiting");
                break;
            }
        }
    }
}

async fn handle_cmd(cmd: ActorCmd, engine: &mut Engine) {
    match cmd {
        ActorCmd::CreateTask {
            req,
            hint_file_size,
            ack,
        } => {
            let req = *req;
            // 空 save_dir → 全局默认目录（config 表）→ 平台默认下载目录。
            let mut save_dir = req.save_dir;
            if save_dir.trim().is_empty() {
                save_dir = engine
                    .db
                    .get_config("default_save_dir")
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
            }
            if save_dir.trim().is_empty() {
                save_dir = default_save_dir();
            }
            log_info!("[server-actor] create task: url={}", req.url);
            let task_id = engine
                .manager
                .create_task(
                    req.url,
                    save_dir,
                    req.file_name,
                    req.segments,
                    req.cookies,
                    req.referrer,
                    hint_file_size,
                    Vec::new(),
                    req.proxy_url,
                    req.user_agent,
                    req.queue_id,
                    req.checksum,
                    req.headers.unwrap_or_default(),
                    Vec::new(),
                    None,
                    None,
                    None,
                )
                .await;
            // 立即广播 tasksSnapshot，确保客户端在首个 taskProgress 之前
            // 已拿到正确的 queue_id。
            engine.manager.load_and_send_all_tasks().await;
            let _ = ack.send(task_id);
        }
        ActorCmd::PauseTask { task_id, ack } => {
            engine.manager.pause_task(&task_id).await;
            let _ = ack.send(());
        }
        ActorCmd::ContinueTask { task_id, ack } => {
            engine.manager.resume_task(&task_id).await;
            let _ = ack.send(());
        }
        ActorCmd::DeleteTask {
            task_id,
            delete_files,
            ack,
        } => {
            engine.manager.delete_task(&task_id, delete_files).await;
            // 删除没有对应的 TaskProgress 事件——主动广播快照，
            // 让其他 WS 客户端的列表同步移除该任务。
            engine.manager.load_and_send_all_tasks().await;
            let _ = ack.send(());
        }
        ActorCmd::PauseAll { ack } => {
            // pending(0) / downloading(1) / preparing(5) 均可暂停。
            let ids = task_ids_by_status(&engine.db, &[0, 1, 5]).await;
            engine.manager.batch_pause(&ids).await;
            let _ = ack.send(());
        }
        ActorCmd::ContinueAll { ack } => {
            // 仅恢复 paused(2)；error(4) 留给显式的单任务 continue。
            let ids = task_ids_by_status(&engine.db, &[2]).await;
            engine.manager.batch_resume(&ids).await;
            let _ = ack.send(());
        }
        ActorCmd::ApplyConfig { keys, ack } => {
            apply_config(engine, &keys).await;
            let _ = ack.send(());
        }
        ActorCmd::CreateQueue {
            name,
            speed_limit_kbps,
            max_concurrent,
            default_save_dir,
            default_segments,
            default_user_agent,
            ack,
        } => {
            engine
                .manager
                .create_queue(
                    name,
                    speed_limit_kbps,
                    max_concurrent,
                    default_save_dir,
                    default_segments,
                    default_user_agent,
                )
                .await;
            let _ = ack.send(());
        }
        ActorCmd::UpdateQueue {
            queue_id,
            name,
            speed_limit_kbps,
            max_concurrent,
            default_save_dir,
            default_segments,
            default_user_agent,
            ack,
        } => {
            engine
                .manager
                .update_queue(
                    queue_id,
                    name,
                    speed_limit_kbps,
                    max_concurrent,
                    default_save_dir,
                    default_segments,
                    default_user_agent,
                )
                .await;
            let _ = ack.send(());
        }
        ActorCmd::DeleteQueue { queue_id, ack } => {
            engine.manager.delete_queue(queue_id).await;
            let _ = ack.send(());
        }
        ActorCmd::MoveToQueue {
            task_id,
            queue_id,
            ack,
        } => {
            engine.manager.move_task_to_queue(task_id, queue_id).await;
            let _ = ack.send(());
        }
        ActorCmd::Boost { task_id, ack } => {
            engine.manager.set_priority_task(task_id).await;
            let _ = ack.send(());
        }
        ActorCmd::TestProxy {
            proxy_type,
            host,
            port,
            username,
            password,
            ack,
        } => {
            let result = engine
                .test_proxy_connection(&proxy_type, &host, &port, &username, &password)
                .await
                .map_err(|e| e.to_string());
            let _ = ack.send(result);
        }
    }
}

/// 把已持久化的配置键 live-apply 到引擎（镜像桌面 `SaveConfig` 分支的
/// 键 → setter 映射；`local_server_*` 是服务器自身配置，重启生效，跳过）。
async fn apply_config(engine: &mut Engine, keys: &[String]) {
    let all = engine.db.get_all_config().await.unwrap_or_default();
    // 代理/BT 全组重载各执行至多一次。
    let mut proxy_applied = false;
    let mut bt_applied = false;
    for key in keys {
        match key.as_str() {
            "max_concurrent_tasks" => {
                if let Some(v) = all.get(key).and_then(|v| v.parse::<usize>().ok()) {
                    log_info!("[server-actor] max_concurrent -> {}", v);
                    engine.manager.set_max_concurrent(v).await;
                }
            }
            "speed_limit_bytes" => {
                if let Some(v) = all.get(key).and_then(|v| v.parse::<u64>().ok()) {
                    log_info!("[server-actor] speed_limit -> {} B/s", v);
                    engine.manager.set_speed_limit(v);
                }
            }
            "default_save_dir" => {
                if let Some(v) = all.get(key) {
                    log_info!("[server-actor] default_save_dir -> {}", v);
                    engine.manager.set_default_save_dir(v.clone());
                }
            }
            "default_segments" => {
                if let Some(v) = all.get(key).and_then(|v| v.parse::<i32>().ok()) {
                    engine.manager.set_default_segments(v);
                }
            }
            "global_user_agent" => {
                if let Some(v) = all.get(key)
                    && let Err(e) = engine.manager.set_user_agent(v.clone())
                {
                    log_info!("[server-actor] failed to apply user_agent: {}", e);
                }
            }
            "max_auto_retries" => {
                if let Some(v) = all.get(key).and_then(|v| v.parse::<i32>().ok()) {
                    engine.manager.set_max_auto_retries(v);
                }
            }
            "auto_retry_delay_secs" => {
                if let Some(v) = all.get(key).and_then(|v| v.parse::<u64>().ok()) {
                    engine.manager.set_auto_retry_delay_secs(v);
                }
            }
            "proxy_mode" | "proxy_type" | "proxy_host" | "proxy_port" | "proxy_username"
            | "proxy_password" | "proxy_no_list"
                if !proxy_applied =>
            {
                proxy_applied = true;
                log_info!("[server-actor] proxy config changed, rebuilding client");
                let new_proxy = ProxyConfig::from_config_map(&all);
                if let Err(e) = engine.manager.set_proxy_config(new_proxy) {
                    log_info!("[server-actor] failed to apply proxy config: {}", e);
                }
            }
            "bt_enable_dht"
            | "bt_enable_upnp"
            | "bt_port_start"
            | "bt_port_end"
            | "bt_custom_trackers"
            | "bt_tracker_sub_enabled"
            | "bt_tracker_sub_urls"
                if !bt_applied =>
            {
                bt_applied = true;
                log_info!("[server-actor] BT config changed, invalidating session");
                engine.manager.set_bt_config(bt_config_from_map(&all));
                engine.manager.invalidate_bt_session().await;
            }
            // 服务器自身配置（token/端口/子开关）重启生效；其余键无运行时动作。
            _ => {}
        }
    }
}

/// 按状态码过滤任务 ID（全局暂停/恢复用）。
async fn task_ids_by_status(db: &Db, statuses: &[i32]) -> Vec<String> {
    match db.load_all_tasks().await {
        Ok(tasks) => tasks
            .into_iter()
            .filter(|t| statuses.contains(&t.status))
            .map(|t| t.task_id)
            .collect(),
        Err(e) => {
            log_info!("[server-actor] load_all_tasks error: {}", e);
            Vec::new()
        }
    }
}

/// 从 config 键值对构建 [`BtConfig`]（复制自 `download_actor.rs` 的私有
/// helper；订阅关闭时排除缓存的订阅 tracker）。
pub fn bt_config_from_map(cfg: &HashMap<String, String>) -> BtConfig {
    let sub_enabled = cfg
        .get("bt_tracker_sub_enabled")
        .map(|v| v == "true")
        .unwrap_or(true);
    BtConfig {
        enable_dht: cfg
            .get("bt_enable_dht")
            .map(|v| v == "true")
            .unwrap_or(true),
        enable_upnp: cfg
            .get("bt_enable_upnp")
            .map(|v| v == "true")
            .unwrap_or(true),
        port_start: cfg
            .get("bt_port_start")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(6881),
        port_end: cfg
            .get("bt_port_end")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(6891),
        custom_trackers: cfg.get("bt_custom_trackers").cloned().unwrap_or_default(),
        subscription_trackers: if sub_enabled {
            cfg.get("bt_tracker_sub_cache").cloned().unwrap_or_default()
        } else {
            String::new()
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn cfg_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn bt_config_from_map_routes_each_key_to_its_own_field() {
        let cfg = cfg_map(&[
            ("bt_enable_dht", "false"),
            ("bt_enable_upnp", "false"),
            ("bt_port_start", "6900"),
            ("bt_port_end", "6950"),
            ("bt_custom_trackers", "udp://tracker.example:80/announce"),
            ("bt_tracker_sub_enabled", "true"),
            ("bt_tracker_sub_cache", "udp://sub.example:80/announce"),
        ]);

        let bt = bt_config_from_map(&cfg);

        assert!(!bt.enable_dht, "bt_enable_dht=false must disable DHT");
        assert!(!bt.enable_upnp, "bt_enable_upnp=false must disable UPnP");
        assert_eq!(bt.port_start, 6900);
        assert_eq!(bt.port_end, 6950);
        assert_eq!(bt.custom_trackers, "udp://tracker.example:80/announce");
        assert_eq!(bt.subscription_trackers, "udp://sub.example:80/announce");
    }

    #[test]
    fn bt_config_from_map_clears_subscription_trackers_when_subscription_disabled() {
        // The cached subscription tracker list must never leak through once
        // the subscription feature itself is turned off, even though the
        // cache key is still present in the config map (e.g. the user
        // disabled the feature but the last-fetched list was never purged).
        let cfg = cfg_map(&[
            ("bt_tracker_sub_enabled", "false"),
            (
                "bt_tracker_sub_cache",
                "udp://stale-tracker.example:80/announce",
            ),
        ]);

        let bt = bt_config_from_map(&cfg);

        assert!(
            bt.subscription_trackers.is_empty(),
            "disabled subscription must yield empty subscription_trackers regardless of cache contents"
        );
    }

    #[test]
    fn bt_config_from_map_treats_non_true_strings_as_false() {
        // The boolean keys are parsed via an exact `v == "true"` match, not
        // a general truthy parse -- values like "1" or "True" must NOT be
        // treated as enabled.
        let cfg = cfg_map(&[("bt_enable_dht", "1"), ("bt_enable_upnp", "True")]);

        let bt = bt_config_from_map(&cfg);

        assert!(!bt.enable_dht);
        assert!(!bt.enable_upnp);
    }
}
