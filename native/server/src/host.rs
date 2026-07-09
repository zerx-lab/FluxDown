//! [`ServerApiHost`] —— `fluxdown_api::service::ApiHost` 的 headless 实现。
//!
//! 读操作直查 [`Db`]（Clone）；写操作打包 [`ActorCmd`] + oneshot 经 mpsc
//! 进 actor 事件循环串行执行（照抄 `hub/src/api_host.rs` 的读写分离）。
//!
//! 与桌面唯一的语义差异：[`submit_external`](ApiHost::submit_external)
//! （脚本接管 / aria2 兼容入口）没有确认弹框可弹 —— headless 环境直接
//! 创建任务，透传 `file_size` 提示。
//!
//! 另新增 aria2 JSON-RPC 兼容层可选方法：[`ApiHost::get_config`] /
//! [`ApiHost::apply_config`] 直查/回写 config 表；[`ApiHost::live_speeds`]
//! 读取 [`WsHub`] 内 `EngineEventSink` 维护的实时速率缓存。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use fluxdown_api::service::{ApiError, ApiHost, LiveSpeed, TaskEvent};
use fluxdown_api::types::{CreateTaskRequest, DownloadRequest, QueueDto, TaskDto};
use fluxdown_engine::db::Db;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::actor::ActorCmd;
use crate::ws_hub::WsHub;

/// headless 服务器的 API 宿主。
#[derive(Clone)]
pub struct ServerApiHost {
    db: Db,
    cmd_tx: mpsc::Sender<ActorCmd>,
    /// WS 广播中枢：借其内维护的实时速率缓存实现 [`ApiHost::live_speeds`]。
    hub: Arc<WsHub>,
    /// 演示模式：`Some(url)` 时仅允许下载该 URL（`FLUXDOWN_DEMO_URL`）。
    demo_url: Option<String>,
}

/// 演示模式守卫：`demo_url` 已设置且请求 URL 与之不符（trim 后精确比较）
/// 时拒绝创建任务。所有任务创建入口（管理 API / 脚本接管 / aria2 兼容）
/// 都收敛到 [`ServerApiHost`]，在此拦截即覆盖全部路径。
pub fn demo_guard(demo_url: Option<&str>, url: &str) -> Result<(), ApiError> {
    match demo_url {
        Some(allowed) if url.trim() != allowed => Err(ApiError::BadRequest(
            "demo mode: only the designated demo file can be downloaded".to_string(),
        )),
        _ => Ok(()),
    }
}

impl ServerApiHost {
    pub fn new(
        db: Db,
        cmd_tx: mpsc::Sender<ActorCmd>,
        hub: Arc<WsHub>,
        demo_url: Option<String>,
    ) -> Self {
        Self {
            db,
            cmd_tx,
            hub,
            demo_url,
        }
    }

    /// 发送命令并等待回执。actor 侧断开 → 503。
    pub async fn send_cmd<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> ActorCmd,
    ) -> Result<T, ApiError> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(make(ack))
            .await
            .map_err(|_| ApiError::Unavailable)?;
        rx.await.map_err(|_| ApiError::Unavailable)
    }

    /// 任务存在性检查（写操作前置），不存在 → 404。
    async fn ensure_task_exists(&self, task_id: &str) -> Result<(), ApiError> {
        match self.db.load_task_by_id(task_id).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(ApiError::NotFound),
            Err(e) => Err(ApiError::Internal(e.to_string())),
        }
    }
}

#[async_trait]
impl ApiHost for ServerApiHost {
    async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError> {
        self.db
            .load_all_tasks()
            .await
            .map(|tasks| tasks.into_iter().map(TaskDto::from).collect())
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn get_task(&self, task_id: &str) -> Result<Option<TaskDto>, ApiError> {
        self.db
            .load_task_by_id(task_id)
            .await
            .map(|t| t.map(TaskDto::from))
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn create_task(&self, req: CreateTaskRequest) -> Result<String, ApiError> {
        demo_guard(self.demo_url.as_deref(), &req.url)?;
        self.send_cmd(|ack| ActorCmd::CreateTask {
            req: Box::new(req),
            hint_file_size: 0,
            ack,
        })
        .await?
    }

    async fn delete_task(&self, task_id: &str, delete_files: bool) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.send_cmd(|ack| ActorCmd::DeleteTask {
            task_id: task_id.to_string(),
            delete_files,
            ack,
        })
        .await
    }

    async fn pause_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.send_cmd(|ack| ActorCmd::PauseTask {
            task_id: task_id.to_string(),
            ack,
        })
        .await
    }

    async fn continue_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.send_cmd(|ack| ActorCmd::ContinueTask {
            task_id: task_id.to_string(),
            ack,
        })
        .await
    }

    async fn pause_all(&self) -> Result<(), ApiError> {
        self.send_cmd(|ack| ActorCmd::PauseAll { ack }).await
    }

    async fn continue_all(&self) -> Result<(), ApiError> {
        self.send_cmd(|ack| ActorCmd::ContinueAll { ack }).await
    }

    async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
        self.db
            .load_all_queues()
            .await
            .map(|qs| qs.into_iter().map(QueueDto::from).collect())
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    /// headless 无确认弹框：外部下载请求直接创建任务，透传 file_size 提示。
    async fn submit_external(&self, req: DownloadRequest) -> Result<(), ApiError> {
        demo_guard(self.demo_url.as_deref(), &req.url)?;
        let create = CreateTaskRequest {
            url: req.url,
            file_name: req.filename,
            save_dir: req.save_dir,
            segments: 0,
            cookies: req.cookies,
            referrer: req.referrer,
            proxy_url: String::new(),
            user_agent: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            headers: req.headers,
            torrent_b64: None,
        };
        self.send_cmd(|ack| ActorCmd::CreateTask {
            req: Box::new(create),
            hint_file_size: req.file_size.unwrap_or(0),
            ack,
        })
        .await??;
        Ok(())
    }

    /// aria2 `getGlobalOption` 兼容入口：直查配置表快照
    /// （FluxDown 原生 key，aria2 选项名翻译在 jsonrpc 层完成）。
    async fn get_config(&self) -> Result<HashMap<String, String>, ApiError> {
        self.db
            .get_all_config()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    /// aria2 `changeGlobalOption` 兼容入口：逐键持久化后 live-apply 到引擎
    /// （复用既有 `ActorCmd::ApplyConfig`，与 `/api/v1/config` REST 端点
    /// 走同一条路径，行为完全一致）。
    async fn apply_config(&self, changes: HashMap<String, String>) -> Result<(), ApiError> {
        let keys: Vec<String> = changes.keys().cloned().collect();
        for (key, value) in &changes {
            self.db
                .set_config(key, value)
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        self.send_cmd(|ack| ActorCmd::ApplyConfig { keys, ack })
            .await
    }

    /// aria2 `tellStatus`/`tellActive` 的 downloadSpeed 字段来源：读取
    /// `WsHub` 内 `EngineEventSink` 维护的实时速率缓存快照。
    async fn live_speeds(&self) -> Result<HashMap<String, LiveSpeed>, ApiError> {
        Ok(self.hub.live_speeds_snapshot())
    }

    /// aria2 `/jsonrpc` WS 通知源：订阅 [`WsHub`] 内 `EngineEventSink`
    /// 维护的任务生命周期事件广播（迁移规则见 `ws_hub` 模块文档）。
    fn subscribe_task_events(&self) -> Option<broadcast::Receiver<TaskEvent>> {
        Some(self.hub.subscribe_task_events())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const DEMO: &str = "https://example.com/demo.bin";

    #[test]
    fn demo_guard_disabled_allows_any_url() {
        assert!(demo_guard(None, "https://evil.example/anything.iso").is_ok());
    }

    #[test]
    fn demo_guard_allows_exact_demo_url_with_surrounding_whitespace() {
        assert!(demo_guard(Some(DEMO), DEMO).is_ok());
        // 客户端（多行输入框/脚本）常带换行或空格，trim 后仍应放行。
        assert!(demo_guard(Some(DEMO), &format!("  {DEMO}\n")).is_ok());
    }

    #[test]
    fn demo_guard_rejects_other_urls() {
        for url in [
            "https://evil.example/anything.iso",
            // 前缀伪装：demo URL 追加查询串/路径不得放行（精确比较）。
            &format!("{DEMO}?x=1"),
            &format!("{DEMO}/../secret"),
            // 大小写变体也不放行（URL path 大小写敏感，保守精确匹配）。
            "https://example.com/DEMO.bin",
            "",
        ] {
            assert!(
                demo_guard(Some(DEMO), url).is_err(),
                "should reject {url:?}"
            );
        }
    }

    #[test]
    fn demo_guard_rejects_empty_url_torrent_task_when_demo_enabled() {
        // BT/种子任务的 CreateTaskRequest.url 允许为空（内容在 torrent_b64
        // 里），但演示模式白名单必须严格——空 URL 与任何非法 URL 一样不在
        // 白名单内，必须拒绝，防止演示服务器被用来分发任意种子。
        assert!(demo_guard(Some(DEMO), "").is_err());
    }

    #[test]
    fn demo_guard_allows_empty_url_torrent_task_when_demo_disabled() {
        // 演示模式关闭时不做任何限制，种子任务的空 URL 必须放行
        // （否则正常部署下 BT 下载会被误拒）。
        assert!(demo_guard(None, "").is_ok());
    }

    #[tokio::test]
    async fn subscribe_task_events_delegates_to_the_shared_ws_hub() {
        use fluxdown_engine::events::{EngineEvent, EventSink};

        use crate::ws_hub::EngineEventSink;

        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let hub = Arc::new(WsHub::new(4));
        let host = ServerApiHost::new(db, cmd_tx, Arc::clone(&hub), None);

        let mut rx = host
            .subscribe_task_events()
            .expect("ServerApiHost must provide a task-event subscription");

        // 经同一个 WsHub 的 EngineEventSink 推一条状态迁移，确认 host 订阅的
        // 是同一条广播通道，而非另建了一个 WsHub 实例。
        EngineEventSink(hub).emit(EngineEvent::TaskProgress {
            task_id: "t1".to_string(),
            status: 1,
            downloaded_bytes: 0,
            total_bytes: 100,
            speed: 0,
            file_name: "f".to_string(),
            save_dir: "/tmp".to_string(),
            url: "http://x".to_string(),
            error_message: String::new(),
            upload_speed_bps: 0,
        });

        let ev = rx.recv().await.expect("task event");
        assert_eq!(ev.task_id, "t1");
        assert_eq!(ev.kind, fluxdown_api::service::TaskEventKind::Start);
    }
}
