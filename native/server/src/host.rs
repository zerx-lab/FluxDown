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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use fluxdown_api::service::{ApiError, ApiHost, LiveSpeed, TaskEvent};
use fluxdown_api::types::{
    CreateTaskRequest, DownloadRequest, MarketEntryDto, PluginDto, QueueDto, TaskDto,
};
use fluxdown_engine::db::Db;
use fluxdown_engine::plugin::{MarketClient, PluginManager};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::actor::ActorCmd;
use crate::wire::WsServerMsg;
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
    /// 部署级默认 Web UI 语言（`FLUXDOWN_LANG`）。仅作 [`ApiHost::web_language`]
    /// 的回退值，不写库——设置页保存过的 `web_language` 永远优先。
    default_language: Option<String>,
    /// 插件管理器（Arc 共享）。`Engine::new` 在 `plugins` feature 下总会注入，
    /// `None` 仅为防御性处理（理论上不会在本 crate 的构建配置下出现）。
    plugin_manager: Option<Arc<PluginManager>>,
    /// 数据目录（与 `Engine::data_dir` 同源），供组件存在性探测
    /// （`plugin::dependencies::missing_components`）解析托管组件路径。
    data_dir: PathBuf,
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
        default_language: Option<String>,
        plugin_manager: Option<Arc<PluginManager>>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            db,
            cmd_tx,
            hub,
            demo_url,
            default_language,
            plugin_manager,
            data_dir,
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

    /// 插件管理器访问：`None` 时返回内部错误（防御性——`plugins` feature 下
    /// `Engine::new` 总会注入，实际请求不会走到这条分支）。
    fn plugin_manager(&self) -> Result<&Arc<PluginManager>, ApiError> {
        self.plugin_manager
            .as_ref()
            .ok_or_else(|| ApiError::Internal("plugin manager not available".to_string()))
    }

    /// 构造市场客户端。`ServerApiHost` 不持有 `Engine`，只持有 `Db` + 插件
    /// 管理器 `Arc`——直接复刻 `DownloadManager::market_client()` 的逻辑
    /// （读市场源配置 + 组装 [`MarketClient`]），语义一致。
    async fn market_client(&self) -> Result<MarketClient, ApiError> {
        let pm = self.plugin_manager()?.clone();
        let all = self.db.get_all_config().await.unwrap_or_default();
        let sources = MarketClient::source_config(&all);
        Ok(MarketClient::new(pm, self.db.clone(), sources))
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
    ///
    /// `url` 可能是批量入口（`/download/batch`）换行连接的多 URL —— 桌面端由
    /// 快速下载弹框按行拆分，headless 在此按行拆分逐条创建。单 URL 时全量
    /// 透传 method/body/audioUrl/filename；多 URL 只共享 cookies/referrer/
    /// headers/saveDir（method/body/audioUrl 是单请求语义，批量下无意义）。
    async fn submit_external(&self, req: DownloadRequest) -> Result<(), ApiError> {
        let urls: Vec<&str> = req
            .url
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if urls.is_empty() {
            return Err(ApiError::BadRequest("url is required".to_string()));
        }
        for url in &urls {
            demo_guard(self.demo_url.as_deref(), url)?;
        }
        let single = urls.len() == 1;
        for url in urls {
            let create = CreateTaskRequest {
                url: url.to_string(),
                file_name: if single {
                    req.filename.clone()
                } else {
                    String::new()
                },
                save_dir: req.save_dir.clone(),
                segments: 0,
                cookies: req.cookies.clone(),
                referrer: req.referrer.clone(),
                proxy_url: String::new(),
                user_agent: String::new(),
                queue_id: String::new(),
                checksum: String::new(),
                headers: req.headers.clone(),
                torrent_b64: None,
                method: if single { req.method.clone() } else { None },
                body: if single { req.body.clone() } else { None },
                audio_url: if single { req.audio_url.clone() } else { None },
                start_paused: false,
            };
            self.send_cmd(|ack| ActorCmd::CreateTask {
                req: Box::new(create),
                hint_file_size: if single {
                    req.file_size.unwrap_or(0)
                } else {
                    0
                },
                ack,
            })
            .await??;
        }
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

    /// Web UI 语言实时求值：设置页保存的 `web_language` 优先，未保存（或空白）
    /// 时回退 `FLUXDOWN_LANG`。每次请求现读 DB，语言变更无需重启即生效。
    async fn web_language(&self) -> Option<String> {
        match self.db.get_config("web_language").await {
            Ok(Some(v)) if !v.trim().is_empty() => Some(v),
            _ => self.default_language.clone(),
        }
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

    /// 列出全部已安装插件（含设置定义与当前值）。
    async fn list_plugins(&self) -> Result<Vec<PluginDto>, ApiError> {
        let pm = self.plugin_manager()?;
        Ok(pm.list().await.into_iter().map(PluginDto::from).collect())
    }

    /// 手动启用/禁用插件；成功后广播 `pluginsChanged` 通知客户端刷新列表。
    async fn set_plugin_enabled(&self, identity: &str, enabled: bool) -> Result<(), ApiError> {
        let pm = self.plugin_manager()?;
        pm.set_enabled(identity, enabled)
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        self.hub.broadcast(&WsServerMsg::PluginsChanged {});
        Ok(())
    }

    /// 卸载插件；成功后广播 `pluginsChanged`。
    async fn uninstall_plugin(&self, identity: &str) -> Result<(), ApiError> {
        let pm = self.plugin_manager()?;
        pm.uninstall(identity)
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        self.hub.broadcast(&WsServerMsg::PluginsChanged {});
        Ok(())
    }

    /// 批量更新插件设置（all-or-nothing）；成功后广播 `pluginsChanged`。
    async fn update_plugin_settings(
        &self,
        identity: &str,
        entries: HashMap<String, String>,
    ) -> Result<(), ApiError> {
        let pm = self.plugin_manager()?;
        let entries: Vec<(String, String)> = entries.into_iter().collect();
        pm.update_settings(identity, &entries)
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        self.hub.broadcast(&WsServerMsg::PluginsChanged {});
        Ok(())
    }

    /// 从 zip 字节安装插件；成功后广播 `pluginsChanged`。
    async fn install_plugin_zip(&self, bytes: Vec<u8>) -> Result<String, ApiError> {
        let pm = self.plugin_manager()?;
        let identity = pm
            .install_from_zip(bytes)
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        self.hub.broadcast(&WsServerMsg::PluginsChanged {});
        Ok(identity)
    }

    /// dev 模式安装（引用目录，不拷贝）；成功后广播 `pluginsChanged`。
    async fn install_plugin_dev(&self, dir_path: String) -> Result<String, ApiError> {
        let pm = self.plugin_manager()?;
        let identity = pm
            .install_dev(Path::new(&dir_path))
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        self.hub.broadcast(&WsServerMsg::PluginsChanged {});
        Ok(identity)
    }

    /// 任务级逃生舱：清该任务 resolver 绑定后按原始链接重跑
    /// （`clear_task_resolver` 本身不失败；existence/actor 状态检查交给
    /// 复用的 `continue_task`）。
    async fn ignore_plugin_retry(&self, task_id: &str) -> Result<(), ApiError> {
        let pm = self.plugin_manager()?;
        pm.clear_task_resolver(task_id).await;
        self.continue_task(task_id).await
    }

    /// 拉取去中心化插件市场索引（多源 failover + 防回滚校验）。
    async fn market_list(&self) -> Result<Vec<MarketEntryDto>, ApiError> {
        let client = self.market_client().await?;
        let idx = client
            .fetch_index()
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        Ok(idx.entries.into_iter().map(MarketEntryDto::from).collect())
    }

    /// 从市场安装某插件最新版；成功后广播 `pluginsChanged`。
    async fn market_install(&self, plugin_id: &str) -> Result<String, ApiError> {
        let client = self.market_client().await?;
        let identity = client
            .install_latest(plugin_id)
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        self.hub.broadcast(&WsServerMsg::PluginsChanged {});
        Ok(identity)
    }

    /// 按插件声明权限探测缺失的基础组件（安装成功后回填提醒载荷）。
    async fn plugin_missing_components(&self, identity: &str) -> Vec<String> {
        let Some(pm) = self.plugin_manager.as_ref() else {
            return Vec::new();
        };
        let perms = pm.permissions_of(identity).await;
        fluxdown_engine::plugin::dependencies::missing_components(&self.db, &self.data_dir, &perms)
            .await
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
    async fn web_language_prefers_saved_config_over_env_fallback() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let hub = Arc::new(WsHub::new(4));
        let host = ServerApiHost::new(
            db.clone(),
            cmd_tx,
            Arc::clone(&hub),
            None,
            Some("zh".to_string()),
            None,
            std::env::temp_dir(),
        );

        // 设置页未保存过语言 → 回退 FLUXDOWN_LANG
        assert_eq!(host.web_language().await.as_deref(), Some("zh"));

        // 设置页保存后 → 保存值实时优先（无需重启）
        db.set_config("web_language", "en").await.expect("set");
        assert_eq!(host.web_language().await.as_deref(), Some("en"));

        // 空白值视为未保存 → 仍回退
        db.set_config("web_language", "  ").await.expect("set");
        assert_eq!(host.web_language().await.as_deref(), Some("zh"));
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
        let host = ServerApiHost::new(
            db,
            cmd_tx,
            Arc::clone(&hub),
            None,
            None,
            None,
            std::env::temp_dir(),
        );

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
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
        });

        let ev = rx.recv().await.expect("task event");
        assert_eq!(ev.task_id, "t1");
        assert_eq!(ev.kind, fluxdown_api::service::TaskEventKind::Start);
    }
}
