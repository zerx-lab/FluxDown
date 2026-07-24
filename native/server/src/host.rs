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
use std::time::Duration;

use async_trait::async_trait;
use fluxdown_api::service::{ApiError, ApiHost, LiveSpeed, TaskEvent};
use fluxdown_api::types::{
    CreateGroupRequest, CreateTaskRequest, DownloadRequest, GroupDto, LinkAuth, LinkCodeResponse,
    LinkDeviceInfo, LinkDiscoveredPeer, LinkPairBeginResponse, LinkPairConfirmRequest,
    LinkPairHelloRequest, LinkPairHelloResponse, LinkPingInfo, LinkTaskRequest, MarketEntryDto,
    PluginDto, QueueDto, ResolvePreviewRequest, ResolvePreviewResponse, TaskDto,
};
use fluxdown_engine::db::Db;
use fluxdown_engine::download_manager::{CreateGroupSpec, GroupItemSpec};
use fluxdown_engine::link::{DiscoveredPeer, DiscoveryKind, LinkError, LinkManager, WireHello};
use fluxdown_engine::plugin::{MarketClient, PluginManager};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::actor::ActorCmd;
use crate::config::default_save_dir;
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
    /// 本地设备互联管理器（`None` = 未启用 / mDNS 关闭）。
    link: Option<Arc<LinkManager>>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Db,
        cmd_tx: mpsc::Sender<ActorCmd>,
        hub: Arc<WsHub>,
        demo_url: Option<String>,
        default_language: Option<String>,
        plugin_manager: Option<Arc<PluginManager>>,
        data_dir: PathBuf,
        link: Option<Arc<LinkManager>>,
    ) -> Self {
        Self {
            db,
            cmd_tx,
            hub,
            demo_url,
            default_language,
            plugin_manager,
            data_dir,
            link,
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

    /// 任务组存在性检查（写操作前置），不存在 → 404。
    async fn ensure_group_exists(&self, group_id: &str) -> Result<(), ApiError> {
        match self.db.load_group_by_id(group_id).await {
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
                ignore_tls_errors: false,
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

    // -- 任务组与前置预解析（Phase D：docs/multi-file-task-group-design.md）--

    /// 前置预解析：写操作经 `ActorCmd::ResolvePreview` + oneshot 回执；
    /// wire↔engine 转换（`ResolvePreviewOutcome` → `ResolvePreviewResponse`、
    /// `ManifestItemInfo` → `PreviewItemDto`）在此完成。
    async fn resolve_preview(
        &self,
        req: ResolvePreviewRequest,
    ) -> Result<ResolvePreviewResponse, ApiError> {
        let ResolvePreviewRequest {
            url,
            cookies,
            referrer,
            user_agent,
            extra_headers,
        } = req;
        let source_url = url.clone();
        let outcome = self
            .send_cmd(|ack| ActorCmd::ResolvePreview {
                url,
                cookies,
                referrer,
                user_agent,
                extra_headers,
                ack,
            })
            .await?;
        Ok(ResolvePreviewResponse {
            name: outcome.name,
            source_url,
            error: outcome.error,
            items: outcome
                .items
                .into_iter()
                .map(manifest_item_to_preview_dto)
                .collect(),
        })
    }

    /// 创建多文件任务组：wire→engine 转换（`CreateGroupRequest` →
    /// `CreateGroupSpec`）在此完成，`save_dir` 空值兜底与
    /// `ActorCmd::CreateTask` 分支同款（config 表 `default_save_dir` →
    /// 平台默认下载目录）。
    async fn create_task_group(&self, req: CreateGroupRequest) -> Result<String, ApiError> {
        demo_guard(self.demo_url.as_deref(), &req.source_url)?;
        let mut base_save_dir = req.save_dir;
        if base_save_dir.trim().is_empty() {
            base_save_dir = self
                .db
                .get_config("default_save_dir")
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
        }
        if base_save_dir.trim().is_empty() {
            base_save_dir = default_save_dir();
        }
        let spec = CreateGroupSpec {
            source_url: req.source_url,
            group_name: req.group_name,
            base_save_dir,
            queue_id: req.queue_id,
            segments: req.segments,
            cookies: req.cookies,
            referrer: req.referrer,
            user_agent: req.user_agent,
            proxy_url: req.proxy_url,
            extra_headers: req.extra_headers,
            ignore_tls_errors: req.ignore_tls_errors,
            start_paused: req.start_paused,
            items: req
                .items
                .into_iter()
                .map(|it| GroupItemSpec {
                    resolver_item: it.resolver_item,
                    file_name: it.file_name,
                    rel_path: it.rel_path,
                    size: it.size,
                })
                .collect(),
        };
        self.send_cmd(|ack| ActorCmd::CreateGroup {
            spec: Box::new(spec),
            ack,
        })
        .await?
    }

    /// 列出全部任务组：直查 `Db`（与 `list_tasks`/`list_queues` 同款读写分离）。
    async fn list_groups(&self) -> Result<Vec<GroupDto>, ApiError> {
        self.db
            .load_all_groups()
            .await
            .map(|groups| groups.into_iter().map(GroupDto::from).collect())
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn group_pause(&self, group_id: &str) -> Result<(), ApiError> {
        self.ensure_group_exists(group_id).await?;
        self.send_cmd(|ack| ActorCmd::GroupPause {
            group_id: group_id.to_string(),
            ack,
        })
        .await
    }

    async fn group_continue(&self, group_id: &str) -> Result<(), ApiError> {
        self.ensure_group_exists(group_id).await?;
        self.send_cmd(|ack| ActorCmd::GroupContinue {
            group_id: group_id.to_string(),
            ack,
        })
        .await
    }

    async fn group_delete(&self, group_id: &str, delete_files: bool) -> Result<(), ApiError> {
        self.ensure_group_exists(group_id).await?;
        self.send_cmd(|ack| ActorCmd::GroupDelete {
            group_id: group_id.to_string(),
            delete_files,
            ack,
        })
        .await
    }

    async fn link_ping_info(&self) -> Option<LinkPingInfo> {
        let link = self.link.as_ref()?;
        Some(LinkPingInfo {
            fingerprint: link.fingerprint().to_string(),
            name: link.self_name().to_string(),
            platform: link.self_platform().unwrap_or("").to_string(),
        })
    }

    async fn link_pair_hello(
        &self,
        req: LinkPairHelloRequest,
    ) -> Result<LinkPairHelloResponse, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        let wire = WireHello {
            code: req.code,
            initiator_eph_pub: req.initiator_eph_pub,
            initiator_id_pub: req.initiator_id_pub,
            initiator_sig: req.initiator_sig,
            name: req.name,
            platform: opt_str(req.platform),
            app_version: opt_str(req.app_version),
            initiator_addrs: req.initiator_addrs,
        };
        let resp = link.pair_hello_wire(wire).map_err(map_link_err)?;
        Ok(LinkPairHelloResponse {
            session_id: resp.session_id,
            responder_eph_pub: resp.responder_eph_pub,
            responder_id_pub: resp.responder_id_pub,
            responder_sig: resp.responder_sig,
            name: resp.name,
            platform: resp.platform.unwrap_or_default(),
            app_version: resp.app_version.unwrap_or_default(),
            sas: resp.sas,
        })
    }

    async fn link_pair_confirm(&self, req: LinkPairConfirmRequest) -> Result<(), ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        link.pair_confirm(&req.session_id, req.confirm)
            .await
            .map_err(map_link_err)?;
        Ok(())
    }

    async fn link_create_task(&self, auth: LinkAuth, body: Vec<u8>) -> Result<String, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        // 用收到的原始字节校验（HMAC 覆盖 body 摘要），再反序列化。
        link.authorize(
            "POST",
            "/api/v1/link/tasks",
            &auth.device,
            auth.ts,
            &auth.nonce,
            &body,
            &auth.tag,
        )
        .await
        .map_err(map_link_err)?;
        let req: LinkTaskRequest =
            serde_json::from_slice(&body).map_err(|e| ApiError::BadRequest(e.to_string()))?;
        let ctreq: CreateTaskRequest = serde_json::from_value(serde_json::json!({
            "url": req.url,
            "saveDir": req.save_dir,
            "fileName": req.file_name,
        }))
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        self.create_task(ctreq).await
    }

    async fn link_generate_code(&self) -> Result<LinkCodeResponse, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        Ok(LinkCodeResponse {
            code: link.generate_code(),
            ttl_seconds: 120,
        })
    }

    async fn link_discovery(&self, start: bool) -> Result<(), ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        if start {
            link.start_discovery().map_err(map_link_err)
        } else {
            link.stop_discovery();
            Ok(())
        }
    }

    async fn link_discovered(&self) -> Result<Vec<LinkDiscoveredPeer>, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        Ok(link
            .discovered_peers()
            .into_iter()
            .map(link_discovered_dto)
            .collect())
    }

    async fn link_probe(&self, host: &str, port: u16) -> Result<LinkDiscoveredPeer, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        link.probe(host, port)
            .await
            .map(link_discovered_dto)
            .map_err(map_link_err)
    }

    async fn link_pair_begin(
        &self,
        host: &str,
        port: u16,
        code: &str,
    ) -> Result<LinkPairBeginResponse, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        let result = link
            .begin_pairing(host, port, code)
            .await
            .map_err(map_link_err)?;
        Ok(LinkPairBeginResponse {
            token: result.token,
            sas: result.sas,
            peer_name: result.peer_name,
            peer_fingerprint: result.peer_fingerprint,
        })
    }

    async fn link_pair_finish(
        &self,
        token: &str,
        accept: bool,
    ) -> Result<Option<LinkDeviceInfo>, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        let Some(record) = link
            .confirm_pairing(token, accept)
            .await
            .map_err(map_link_err)?
        else {
            return Ok(None);
        };
        let online = link.is_online(&record.fingerprint).await;
        Ok(Some(LinkDeviceInfo {
            fingerprint: record.fingerprint,
            name: record.name,
            platform: record.platform,
            online,
            paired_at: record.paired_at,
            last_seen_at: record.last_seen_at,
        }))
    }

    /// 已配对设备列表：并发在线探测（每台走传输栈 `connect`；参考 hub
    /// `emit_link_devices` 的并发思路），整体限时兜底——个别设备长时间不可达
    /// 不应拖慢整批响应。
    async fn link_devices(&self) -> Result<Vec<LinkDeviceInfo>, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        let records = link.list_devices().await.map_err(map_link_err)?;
        let probe =
            futures_util::future::join_all(records.iter().map(|r| link.is_online(&r.fingerprint)));
        let online = tokio::time::timeout(Duration::from_secs(5), probe)
            .await
            .unwrap_or_else(|_| vec![false; records.len()]);
        Ok(records
            .into_iter()
            .zip(online)
            .map(|(r, on)| LinkDeviceInfo {
                fingerprint: r.fingerprint,
                name: r.name,
                platform: r.platform,
                online: on,
                paired_at: r.paired_at,
                last_seen_at: r.last_seen_at,
            })
            .collect())
    }

    async fn link_remove_device(&self, fingerprint: &str) -> Result<bool, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        link.remove_device(fingerprint).await.map_err(map_link_err)
    }

    async fn link_dispatch(
        &self,
        fingerprint: &str,
        url: &str,
        save_dir: Option<&str>,
        file_name: Option<&str>,
    ) -> Result<String, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        link.dispatch(fingerprint, url, save_dir, file_name)
            .await
            .map_err(map_link_err)
    }
}

/// 引擎 `link::DiscoveredPeer` → wire DTO（`kind` → `source` 小写字符串）。
fn link_discovered_dto(p: DiscoveredPeer) -> LinkDiscoveredPeer {
    LinkDiscoveredPeer {
        fingerprint: p.fingerprint,
        name: p.name,
        platform: p.platform,
        host: p.host,
        port: p.port,
        app_version: p.app_version,
        source: match p.kind {
            DiscoveryKind::Mdns => "mdns",
            DiscoveryKind::Manual => "manual",
        }
        .to_string(),
    }
}

/// 空串 → `None`，非空 → `Some`（wire DTO 的空 platform/version 归一为 Option）。
fn opt_str(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// [`LinkError`] → [`ApiError`] 映射（决定 HTTP 状态码）。
fn map_link_err(e: LinkError) -> ApiError {
    match e {
        LinkError::Unauthorized => ApiError::Unauthorized,
        LinkError::InvalidCode
        | LinkError::BadSignature
        | LinkError::BadPayload(_)
        | LinkError::SelfPairing
        | LinkError::SessionExpired => ApiError::BadRequest(e.to_string()),
        LinkError::Unreachable => ApiError::Unavailable,
        other => ApiError::Internal(other.to_string()),
    }
}

/// 把插件清单条目转换为 REST 预解析响应 DTO（`server` 侧 wire↔engine
/// 转换，见 [`ServerApiHost::resolve_preview`]）。
fn manifest_item_to_preview_dto(
    item: fluxdown_engine::model::ManifestItemInfo,
) -> fluxdown_api::types::PreviewItemDto {
    fluxdown_api::types::PreviewItemDto {
        id: item.id,
        name: item.name,
        path: item.path,
        size: item.size,
        variants: item
            .variants
            .into_iter()
            .map(|v| fluxdown_api::types::PreviewVariantDto {
                id: v.id,
                label: v.label,
                size: v.size,
            })
            .collect(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use fluxdown_api::types::GroupItemRequest;

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
            None,
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
            None,
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
        });

        let ev = rx.recv().await.expect("task event");
        assert_eq!(ev.task_id, "t1");
        assert_eq!(ev.kind, fluxdown_api::service::TaskEventKind::Start);
    }

    #[tokio::test]
    async fn create_task_group_rejects_when_demo_mode_blocks_source_url() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
        let hub = Arc::new(WsHub::new(4));
        let host = ServerApiHost::new(
            db,
            cmd_tx,
            hub,
            Some(DEMO.to_string()),
            None,
            None,
            std::env::temp_dir(),
            None,
        );
        let req = CreateGroupRequest {
            source_url: "https://evil.example/share".to_string(),
            group_name: "x".to_string(),
            save_dir: String::new(),
            queue_id: String::new(),
            segments: 0,
            cookies: String::new(),
            referrer: String::new(),
            user_agent: String::new(),
            proxy_url: String::new(),
            extra_headers: HashMap::new(),
            ignore_tls_errors: false,
            start_paused: false,
            items: vec![GroupItemRequest {
                resolver_item: "a".to_string(),
                file_name: "a.bin".to_string(),
                rel_path: String::new(),
                size: 0,
            }],
        };
        let result = host.create_task_group(req).await;
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
        // 演示模式在发送 ActorCmd 前已拒绝，actor 通道不应收到任何命令。
        assert!(cmd_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn group_actions_return_not_found_for_unknown_group() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let hub = Arc::new(WsHub::new(4));
        let host = ServerApiHost::new(
            db,
            cmd_tx,
            hub,
            None,
            None,
            None,
            std::env::temp_dir(),
            None,
        );
        assert!(matches!(
            host.group_pause("missing").await,
            Err(ApiError::NotFound)
        ));
        assert!(matches!(
            host.group_continue("missing").await,
            Err(ApiError::NotFound)
        ));
        assert!(matches!(
            host.group_delete("missing", false).await,
            Err(ApiError::NotFound)
        ));
    }

    #[tokio::test]
    async fn list_groups_reads_from_db_as_camel_case_dto() {
        let db = Db::connect("sqlite::memory:")
            .await
            .expect("connect mem db");
        db.insert_group("g1", "合集", "https://x/share", "/downloads/合集")
            .await
            .expect("insert group");
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let hub = Arc::new(WsHub::new(4));
        let host = ServerApiHost::new(
            db,
            cmd_tx,
            hub,
            None,
            None,
            None,
            std::env::temp_dir(),
            None,
        );
        let groups = host.list_groups().await.expect("list groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, "g1");
        assert_eq!(groups[0].name, "合集");
        assert_eq!(groups[0].save_dir, "/downloads/合集");
    }

    /// 端到端：两个 LinkManager 经**真实 HTTP**（api_router + ServerApiHost）完成
    /// 配对——覆盖 wire base64 编解码、handler、host 映射、pairing 密码学全链路。
    #[tokio::test]
    async fn link_pairing_end_to_end_over_http() {
        use std::sync::Arc;
        use tokio::net::TcpListener;

        async fn mem_db(tag: &str) -> Db {
            let url = format!(
                "sqlite:file:linktest_{tag}_{}?mode=memory&cache=shared",
                uuid::Uuid::new_v4().simple()
            );
            Db::connect(&url).await.expect("mem db")
        }
        fn info(name: &str) -> fluxdown_engine::link::SelfInfo {
            fluxdown_engine::link::SelfInfo {
                name: name.to_string(),
                platform: Some("linux".to_string()),
                app_version: None,
            }
        }

        // 响应方（被添加设备）+ 真实 HTTP 服务器。
        let db_r = mem_db("resp").await;
        let (tx_r, _rx_r) = mpsc::channel::<fluxdown_engine::link::LinkEngineEvent>(16);
        let responder = LinkManager::load(db_r.clone(), info("NAS"), 17800, tx_r)
            .await
            .expect("responder link");
        let code = responder.generate_code();

        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let host: Arc<dyn ApiHost> = Arc::new(ServerApiHost::new(
            db_r,
            cmd_tx,
            Arc::new(WsHub::new(4)),
            None,
            None,
            None,
            std::env::temp_dir(),
            Some(responder.clone()),
        ));
        let cfg = fluxdown_api::server::ApiServerConfig::from_config_map(
            &std::collections::HashMap::new(),
            "test",
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = fluxdown_api::server::api_router(host, cfg);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        // 发起方（添加设备）。
        let db_i = mem_db("init").await;
        let (tx_i, _rx_i) = mpsc::channel::<fluxdown_engine::link::LinkEngineEvent>(16);
        let initiator = LinkManager::load(db_i, info("Laptop"), 0, tx_i)
            .await
            .expect("initiator link");

        // begin（发 hello）→ confirm（发 confirm），全程真实 HTTP。
        let begin = initiator
            .begin_pairing("127.0.0.1", addr.port(), &code)
            .await
            .expect("begin pairing");
        assert_eq!(begin.peer_name, "NAS");
        assert!(!begin.sas.is_empty());
        let rec = initiator
            .confirm_pairing(&begin.token, true)
            .await
            .expect("confirm pairing")
            .expect("peer record");
        assert_eq!(rec.name, "NAS");

        // 双方名册均入册；链路密钥一致（ECDH 对称）。
        let init_devs = initiator.list_devices().await.expect("init devices");
        let resp_devs = responder.list_devices().await.expect("resp devices");
        assert_eq!(init_devs.len(), 1);
        assert_eq!(resp_devs.len(), 1);
        assert_eq!(init_devs[0].name, "NAS");
        assert_eq!(resp_devs[0].name, "Laptop");
        assert_eq!(init_devs[0].link_secret, resp_devs[0].link_secret);

        // 错误配对码经 HTTP 被拒。
        let bad = initiator
            .begin_pairing("127.0.0.1", addr.port(), "000000")
            .await;
        assert!(bad.is_err());
    }

    /// 端到端：两个 LinkManager 经**新管理面 HTTP handler**
    /// （`/api/v1/link/pair/begin`、`/pair/finish`、`/devices`、
    /// `DELETE /devices/{fingerprint}`）完成配对与名册管理——区别于
    /// [`link_pairing_end_to_end_over_http`] 直调引擎方法，本测试全程走
    /// routes.rs 常量 → server.rs handler → `ApiHost` 新方法 →
    /// `ServerApiHost` 实现的真实 HTTP 链路，且带 management token 鉴权。
    #[tokio::test]
    async fn link_management_plane_pair_and_devices_over_http() {
        use std::sync::Arc;
        use tokio::net::TcpListener;

        async fn mem_db(tag: &str) -> Db {
            let url = format!(
                "sqlite:file:linkmgmt_{tag}_{}?mode=memory&cache=shared",
                uuid::Uuid::new_v4().simple()
            );
            Db::connect(&url).await.expect("mem db")
        }
        fn info(name: &str) -> fluxdown_engine::link::SelfInfo {
            fluxdown_engine::link::SelfInfo {
                name: name.to_string(),
                platform: Some("linux".to_string()),
                app_version: None,
            }
        }
        /// 起一个带 management token 的真实 axum 服务器，返回监听地址。
        async fn spawn_management(
            link: Arc<LinkManager>,
            db: Db,
            mgmt_token: &str,
        ) -> std::net::SocketAddr {
            let (cmd_tx, _cmd_rx) = mpsc::channel(1);
            let host: Arc<dyn ApiHost> = Arc::new(ServerApiHost::new(
                db,
                cmd_tx,
                Arc::new(WsHub::new(4)),
                None,
                None,
                None,
                std::env::temp_dir(),
                Some(link),
            ));
            let mut map = HashMap::new();
            map.insert("local_server_api_enabled".to_string(), "true".to_string());
            map.insert("local_server_token".to_string(), mgmt_token.to_string());
            let cfg = fluxdown_api::server::ApiServerConfig::from_config_map(&map, "test");
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().expect("addr");
            let app = fluxdown_api::server::api_router(host, cfg);
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            addr
        }

        // 响应方：真实 HTTP 服务器，承载既有数据面端点（pair/hello、pair/confirm，
        // 无 token 鉴权）——发起方管理面 handler 内部会向它发真实 HTTP 请求。
        let db_r = mem_db("resp").await;
        let (tx_r, _rx_r) = mpsc::channel::<fluxdown_engine::link::LinkEngineEvent>(16);
        let responder = LinkManager::load(db_r.clone(), info("NAS"), 17800, tx_r)
            .await
            .expect("responder link");
        let code = responder.generate_code();
        let addr_r = spawn_management(responder, db_r, "resp-token").await;

        // 发起方：本测试实际驱动的对象——经其新管理面路由完成 begin/finish/
        // devices/delete，而非直调引擎方法。
        let db_i = mem_db("init").await;
        let (tx_i, _rx_i) = mpsc::channel::<fluxdown_engine::link::LinkEngineEvent>(16);
        let initiator = LinkManager::load(db_i.clone(), info("Laptop"), 0, tx_i)
            .await
            .expect("initiator link");
        let token = "mgmt-secret";
        let addr_i = spawn_management(initiator, db_i, token).await;

        let client = reqwest::Client::new();
        let base = format!("http://{addr_i}");

        // begin：管理面 handler → ApiHost::link_pair_begin → LinkManager::begin_pairing
        // （内部对响应方发真实 HTTP hello）。
        let begin: fluxdown_api::types::LinkPairBeginResponse = client
            .post(format!("{base}/api/v1/link/pair/begin"))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "host": "127.0.0.1",
                "port": addr_r.port(),
                "code": code,
            }))
            .send()
            .await
            .expect("begin request")
            .json()
            .await
            .expect("begin json");
        assert_eq!(begin.peer_name, "NAS");
        assert!(!begin.sas.is_empty());

        // finish：SAS 核对后确认配对。
        let finish: fluxdown_api::types::LinkPairFinishResponse = client
            .post(format!("{base}/api/v1/link/pair/finish"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "token": begin.token, "accept": true }))
            .send()
            .await
            .expect("finish request")
            .json()
            .await
            .expect("finish json");
        assert!(finish.paired);
        let device = finish.device.expect("paired device");
        assert_eq!(device.name, "NAS");
        let fingerprint = device.fingerprint;

        // devices：列表应含刚配对的一台，且在线（响应方服务器真实存活）。
        let devices: fluxdown_api::types::LinkDevicesResponse = client
            .get(format!("{base}/api/v1/link/devices"))
            .bearer_auth(token)
            .send()
            .await
            .expect("devices request")
            .json()
            .await
            .expect("devices json");
        assert_eq!(devices.devices.len(), 1);
        assert_eq!(devices.devices[0].fingerprint, fingerprint);
        assert!(devices.devices[0].online);

        // 未鉴权请求应被管理 API 门禁拒绝（复用既有 token 中间件）。
        let unauth = client
            .get(format!("{base}/api/v1/link/devices"))
            .send()
            .await
            .expect("unauth request");
        assert_eq!(unauth.status(), reqwest::StatusCode::UNAUTHORIZED);

        // 删除：解除配对后名册应清空。
        let del = client
            .delete(format!("{base}/api/v1/link/devices/{fingerprint}"))
            .bearer_auth(token)
            .send()
            .await
            .expect("delete request");
        assert_eq!(del.status(), reqwest::StatusCode::OK);
        let del_body: fluxdown_api::types::LinkOkResponse = del.json().await.expect("delete json");
        assert!(del_body.ok);

        let devices_after: fluxdown_api::types::LinkDevicesResponse = client
            .get(format!("{base}/api/v1/link/devices"))
            .bearer_auth(token)
            .send()
            .await
            .expect("devices after request")
            .json()
            .await
            .expect("devices after json");
        assert!(devices_after.devices.is_empty());

        // 删除不存在的设备 → 404。
        let del_missing = client
            .delete(format!("{base}/api/v1/link/devices/{fingerprint}"))
            .bearer_auth(token)
            .send()
            .await
            .expect("delete missing request");
        assert_eq!(del_missing.status(), reqwest::StatusCode::NOT_FOUND);
    }
}
