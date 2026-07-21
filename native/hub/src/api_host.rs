//! [`HubApiHost`] —— `fluxdown_api::service::ApiHost` 的桌面 App 实现。
//!
//! ## 读写分离
//!
//! - **读操作**（任务列表 / 单任务 / 队列列表 / 全局配置）直查 [`Db`]（`Clone`，
//!   Arc 背书），零 actor 往返。进度字段（`downloaded_bytes`）随下载引擎的
//!   5s 批量持久化刷新，对轮询式管理客户端足够。
//! - **写操作**（创建 / 暂停 / 恢复 / 删除 / 配置写入）打包为 [`ApiCommand`] +
//!   oneshot 回执，经 mpsc 送入 `download_actor` 的 `select!` 事件循环 ——
//!   engine 由 actor 独占（单线程串行），与 rinf 信号处理共享同一条路径，
//!   天然免数据竞争。
//! - **外部下载**（脚本接管 / aria2 兼容）复用既有 `native_msg_rx` 通道，
//!   走「确认弹框 → 创建」全链路，与浏览器扩展完全一致。
//! - **实时速率**（[`ApiHost::live_speeds`]）直查内存态 [`LiveSpeedMap`]，
//!   由 [`crate::rinf_sink::RinfEventSink`] 在 `EngineEvent::TaskProgress`
//!   时写入，两者共享同一个 `Arc`（构造点见 `download_actor::run`），
//!   不经 actor 往返。
//! - **任务事件订阅**([`ApiHost::subscribe_task_events`])返回内存态
//!   `broadcast::Sender<TaskEvent>` 的新 `Receiver`,同一个 `Sender` 由
//!   [`crate::rinf_sink::RinfEventSink`] 在状态迁移判定后发送(构造点同见
//!   `download_actor::run`),供 `/jsonrpc` 的 WS 层转译为
//!   `aria2.onDownloadXxx` 通知帧。

use std::collections::HashMap;
#[cfg(hub_plugins)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fluxdown_api::service::{ApiError, ApiHost, LiveSpeed, TaskEvent};
use fluxdown_api::types::{
    CreateGroupRequest, CreateTaskRequest, DownloadRequest, GroupDto, QueueDto,
    ResolvePreviewRequest, ResolvePreviewResponse, TaskDto,
};
#[cfg(hub_link)]
use fluxdown_api::types::{
    LinkAuth, LinkCodeResponse, LinkPairConfirmRequest, LinkPairHelloRequest,
    LinkPairHelloResponse, LinkPingInfo, LinkTaskRequest,
};
#[cfg(hub_plugins)]
use fluxdown_api::types::{MarketEntryDto, PluginDto};
use fluxdown_engine::db::Db;
use fluxdown_engine::download_manager::{CreateGroupSpec, GroupItemSpec, ResolvePreviewOutcome};
#[cfg(hub_link)]
use fluxdown_engine::link::{LinkError, WireHello};
#[cfg(hub_plugins)]
use fluxdown_engine::plugin::{MarketClient, PluginManager};
use tokio::sync::{broadcast, mpsc, oneshot};

/// 任务实时速率表：`task_id → LiveSpeed`。写端见 [`crate::rinf_sink::RinfEventSink`]；
/// 这里只是共享 `Arc` 的类型别名，读写双方各自加锁做「单次操作 + 立即
/// 释放」，不跨 `.await` 持锁。
pub type LiveSpeedMap = Arc<Mutex<HashMap<String, LiveSpeed>>>;

/// 取出锁内容；`Mutex` 中毒（某持锁线程 panic）时回退到内部值而非扩散
/// panic——这是内存态缓存，恢复正确性由后续事件覆盖写入保证，值得用
/// 回退换稳定性（同一模式见 `rinf_selection.rs`/`ws_hub.rs` 各自的
/// `lock_or_recover`）。
pub(crate) fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 写操作命令。由 `download_actor` 的 `api_cmd_rx` 分支消费。
///
/// 每个变体携带 oneshot 回执：actor 完成操作后发送结果，HTTP 层同步等待。
/// actor 退出（应用关闭）时 channel 断开，映射为 503。
pub enum ApiCommand {
    /// 直接创建任务（不弹确认框），回执新任务 ID；`None` = DB 插入失败。
    /// `req` 装箱：`CreateTaskRequest` 远大于其余变体（clippy::large_enum_variant）。
    CreateTask {
        req: Box<CreateTaskRequest>,
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
    /// 配置键已由 `HubApiHost::apply_config` 逐键写入 DB，按键名 live-apply
    /// 到引擎（镜像桌面 `SaveConfig` 信号分支的「键 → 引擎 setter」逻辑，
    /// 见 `download_actor::apply_config_key`）。
    ApplyConfig {
        keys: Vec<String>,
        ack: oneshot::Sender<()>,
    },
    /// 建组：wire→engine 转换（`CreateGroupRequest` → `CreateGroupSpec`，含
    /// `save_dir` 空值兜底）已在 [`HubApiHost::create_task_group`] 完成，回执
    /// 新组 ID；`None` = DB 插入失败或 `items` 为空。`spec` 装箱理由同
    /// [`ApiCommand::CreateTask`]。
    CreateGroup {
        spec: Box<CreateGroupSpec>,
        ack: oneshot::Sender<Option<String>>,
    },
    /// 暂停组内全部成员。
    GroupPause {
        group_id: String,
        ack: oneshot::Sender<()>,
    },
    /// 恢复组内全部成员。
    GroupContinue {
        group_id: String,
        ack: oneshot::Sender<()>,
    },
    /// 删除整组（批量删成员）。
    GroupDelete {
        group_id: String,
        delete_files: bool,
        ack: oneshot::Sender<()>,
    },
    /// 前置预解析：actor 内 `spawn_resolve_preview` off-actor 完成后由转发
    /// 任务回执，绝不在 actor 内 await 解析结果（最长 30s 会冻结事件循环）。
    ResolvePreview {
        url: String,
        cookies: String,
        referrer: String,
        user_agent: String,
        extra_headers: HashMap<String, String>,
        ack: oneshot::Sender<ResolvePreviewOutcome>,
    },
}

/// 桌面 App 的 API 宿主。构造后传给 `fluxdown_api::server::spawn_api_server`。
pub struct HubApiHost {
    db: Db,
    cmd_tx: mpsc::Sender<ApiCommand>,
    ext_tx: mpsc::Sender<Vec<DownloadRequest>>,
    /// 实时速率表,与注入 `RinfEventSink` 的是同一个 `Arc`。
    live_speeds: LiveSpeedMap,
    /// 任务生命周期事件广播源,与注入 `RinfEventSink` 的是同一个 `Sender`;
    /// `subscribe_task_events()` 经它开出新的 `Receiver`。
    task_events_tx: broadcast::Sender<TaskEvent>,
    #[cfg(hub_plugins)]
    /// 插件管理器,与 `download_actor::run` 内本循环持有的是同一个 `Arc`
    /// （见插件系统契约 hub 节 5）。`None` 理论上不应发生
    /// （`Engine::new` 恒注入），仅作防御性兜底。
    plugin_manager: Option<Arc<PluginManager>>,
    /// 数据目录（与 `Engine::data_dir` 同源），供组件存在性探测
    /// （`plugin::dependencies::missing_components`）解析托管组件路径。
    data_dir: PathBuf,
    /// 本地设备互联管理器（桌面 `hub_link`；`None` = mDNS 关闭）。
    #[cfg(hub_link)]
    link: Option<Arc<fluxdown_engine::link::LinkManager>>,
}

impl HubApiHost {
    /// `cmd_tx` → actor 的 `api_cmd_rx`;`ext_tx` → actor 的 `native_msg_rx`
    /// (与 NMH / 脚本接管共用的外部下载通道);`live_speeds` → 与
    /// `RinfEventSink` 共享的同一个实时速率表 `Arc`;`task_events_tx` → 与
    /// `RinfEventSink` 共享的同一个任务事件广播 `Sender`。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Db,
        cmd_tx: mpsc::Sender<ApiCommand>,
        ext_tx: mpsc::Sender<Vec<DownloadRequest>>,
        live_speeds: LiveSpeedMap,
        task_events_tx: broadcast::Sender<TaskEvent>,
        #[cfg(hub_plugins)] plugin_manager: Option<Arc<PluginManager>>,
        data_dir: PathBuf,
        #[cfg(hub_link)] link: Option<Arc<fluxdown_engine::link::LinkManager>>,
    ) -> Self {
        Self {
            db,
            cmd_tx,
            ext_tx,
            live_speeds,
            task_events_tx,
            #[cfg(hub_plugins)]
            plugin_manager,
            data_dir,
            #[cfg(hub_link)]
            link,
        }
    }

    /// 发送命令并等待回执。actor 侧断开 → 503。
    async fn send_cmd<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> ApiCommand,
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

    #[cfg(hub_plugins)]
    /// 构造市场客户端。`HubApiHost` 不持有 `Engine`，只持有 `Db` + 插件管理器
    /// `Arc`——直接复刻 `DownloadManager::market_client()` 的逻辑（读市场源
    /// 配置 + 组装 [`MarketClient`]），语义一致。
    async fn market_client(&self) -> Result<MarketClient, ApiError> {
        let pm = self.plugin_manager.clone().ok_or(ApiError::Unavailable)?;
        let all = self.db.get_all_config().await.unwrap_or_default();
        let sources = MarketClient::source_config(&all);
        Ok(MarketClient::new(pm, self.db.clone(), sources))
    }
}

#[async_trait]
impl ApiHost for HubApiHost {
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
        self.send_cmd(|ack| ApiCommand::CreateTask {
            req: Box::new(req),
            ack,
        })
        .await?
        .ok_or_else(|| ApiError::Internal("failed to persist task".to_string()))
    }

    async fn delete_task(&self, task_id: &str, delete_files: bool) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.send_cmd(|ack| ApiCommand::DeleteTask {
            task_id: task_id.to_string(),
            delete_files,
            ack,
        })
        .await
    }

    async fn pause_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.send_cmd(|ack| ApiCommand::PauseTask {
            task_id: task_id.to_string(),
            ack,
        })
        .await
    }

    async fn continue_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.send_cmd(|ack| ApiCommand::ContinueTask {
            task_id: task_id.to_string(),
            ack,
        })
        .await
    }

    async fn pause_all(&self) -> Result<(), ApiError> {
        self.send_cmd(|ack| ApiCommand::PauseAll { ack }).await
    }

    async fn continue_all(&self) -> Result<(), ApiError> {
        self.send_cmd(|ack| ApiCommand::ContinueAll { ack }).await
    }

    async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
        self.db
            .load_all_queues()
            .await
            .map(|qs| qs.into_iter().map(QueueDto::from).collect())
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn submit_external(&self, req: DownloadRequest) -> Result<(), ApiError> {
        self.ext_tx
            .send(vec![req])
            .await
            .map_err(|_| ApiError::Unavailable)
    }

    async fn get_config(&self) -> Result<HashMap<String, String>, ApiError> {
        self.db
            .get_all_config()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn apply_config(&self, changes: HashMap<String, String>) -> Result<(), ApiError> {
        // 先逐键持久化到 DB，全部成功后才触发引擎 live-apply。命令只携带
        // keys（不带值）：与 server 侧 `ActorCmd::ApplyConfig` 语义一致——
        // 接收端重新从 DB 整表读取，避免命令 payload 与 DB 状态不一致。
        for (key, value) in &changes {
            self.db
                .set_config(key, value)
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        let keys: Vec<String> = changes.into_keys().collect();
        self.send_cmd(|ack| ApiCommand::ApplyConfig { keys, ack })
            .await
    }

    async fn live_speeds(&self) -> Result<HashMap<String, LiveSpeed>, ApiError> {
        Ok(lock_or_recover(&self.live_speeds).clone())
    }

    fn subscribe_task_events(&self) -> Option<broadcast::Receiver<TaskEvent>> {
        Some(self.task_events_tx.subscribe())
    }

    #[cfg(hub_plugins)]
    async fn list_plugins(&self) -> Result<Vec<PluginDto>, ApiError> {
        let Some(pm) = &self.plugin_manager else {
            return Ok(Vec::new());
        };
        Ok(pm.list().await.into_iter().map(PluginDto::from).collect())
    }

    #[cfg(hub_plugins)]
    async fn set_plugin_enabled(&self, identity: &str, enabled: bool) -> Result<(), ApiError> {
        let pm = self.plugin_manager.as_ref().ok_or(ApiError::Unavailable)?;
        pm.set_enabled(identity, enabled)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    #[cfg(hub_plugins)]
    async fn uninstall_plugin(&self, identity: &str) -> Result<(), ApiError> {
        let pm = self.plugin_manager.as_ref().ok_or(ApiError::Unavailable)?;
        pm.uninstall(identity)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    #[cfg(hub_plugins)]
    async fn update_plugin_settings(
        &self,
        identity: &str,
        entries: HashMap<String, String>,
    ) -> Result<(), ApiError> {
        let pm = self.plugin_manager.as_ref().ok_or(ApiError::Unavailable)?;
        let entries: Vec<(String, String)> = entries.into_iter().collect();
        pm.update_settings(identity, &entries)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    #[cfg(hub_plugins)]
    async fn install_plugin_zip(&self, bytes: Vec<u8>) -> Result<String, ApiError> {
        let pm = self.plugin_manager.as_ref().ok_or(ApiError::Unavailable)?;
        pm.install_from_zip(bytes)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    #[cfg(hub_plugins)]
    async fn install_plugin_dev(&self, dir_path: String) -> Result<String, ApiError> {
        let pm = self.plugin_manager.as_ref().ok_or(ApiError::Unavailable)?;
        pm.install_dev(Path::new(&dir_path))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    #[cfg(hub_plugins)]
    /// 逃生舱：清该任务的 resolver 绑定，再经既有 `ContinueTask` 命令按原始
    /// 链接恢复(镜像 `download_actor` 的 `IgnorePluginRetry` 信号分支)。
    async fn ignore_plugin_retry(&self, task_id: &str) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        if let Some(pm) = &self.plugin_manager {
            pm.clear_task_resolver(task_id).await;
        }
        self.send_cmd(|ack| ApiCommand::ContinueTask {
            task_id: task_id.to_string(),
            ack,
        })
        .await
    }

    #[cfg(hub_plugins)]
    /// 拉取去中心化插件市场索引（多源 failover + 防回滚校验）。
    async fn market_list(&self) -> Result<Vec<MarketEntryDto>, ApiError> {
        let client = self.market_client().await?;
        let idx = client
            .fetch_index()
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        Ok(idx.entries.into_iter().map(MarketEntryDto::from).collect())
    }

    #[cfg(hub_plugins)]
    /// 从市场安装某插件最新版（下载 → content_hash 校验 → 安装），返回 identity。
    async fn market_install(&self, plugin_id: &str) -> Result<String, ApiError> {
        let client = self.market_client().await?;
        client
            .install_latest(plugin_id)
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))
    }

    #[cfg(hub_plugins)]
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

    /// 前置预解析：写操作经 `ApiCommand::ResolvePreview` + oneshot 回执；
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
            .send_cmd(|ack| ApiCommand::ResolvePreview {
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
    /// `CreateGroupSpec`）在此完成，`save_dir` 空值兜底与 `ApiCommand::CreateTask`
    /// 分支同款（config 表 `default_save_dir` → 平台默认下载目录）。
    async fn create_task_group(&self, req: CreateGroupRequest) -> Result<String, ApiError> {
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
            base_save_dir = crate::actors::download_actor::default_save_dir();
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
        self.send_cmd(|ack| ApiCommand::CreateGroup {
            spec: Box::new(spec),
            ack,
        })
        .await?
        .ok_or_else(|| ApiError::Internal("failed to persist group".to_string()))
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
        self.send_cmd(|ack| ApiCommand::GroupPause {
            group_id: group_id.to_string(),
            ack,
        })
        .await
    }

    async fn group_continue(&self, group_id: &str) -> Result<(), ApiError> {
        self.ensure_group_exists(group_id).await?;
        self.send_cmd(|ack| ApiCommand::GroupContinue {
            group_id: group_id.to_string(),
            ack,
        })
        .await
    }

    async fn group_delete(&self, group_id: &str, delete_files: bool) -> Result<(), ApiError> {
        self.ensure_group_exists(group_id).await?;
        self.send_cmd(|ack| ApiCommand::GroupDelete {
            group_id: group_id.to_string(),
            delete_files,
            ack,
        })
        .await
    }

    #[cfg(hub_link)]
    async fn link_ping_info(&self) -> Option<LinkPingInfo> {
        let link = self.link.as_ref()?;
        Some(LinkPingInfo {
            fingerprint: link.fingerprint().to_string(),
            name: link.self_name().to_string(),
            platform: link.self_platform().unwrap_or("").to_string(),
        })
    }

    #[cfg(hub_link)]
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
            platform: link_opt_str(req.platform),
            app_version: link_opt_str(req.app_version),
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

    #[cfg(hub_link)]
    async fn link_pair_confirm(&self, req: LinkPairConfirmRequest) -> Result<(), ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        link.pair_confirm(&req.session_id, req.confirm)
            .await
            .map_err(map_link_err)?;
        Ok(())
    }

    #[cfg(hub_link)]
    async fn link_create_task(&self, auth: LinkAuth, body: Vec<u8>) -> Result<String, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
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

    #[cfg(hub_link)]
    async fn link_generate_code(&self) -> Result<LinkCodeResponse, ApiError> {
        let link = self.link.as_ref().ok_or(ApiError::Unauthorized)?;
        Ok(LinkCodeResponse {
            code: link.generate_code(),
            ttl_seconds: 120,
        })
    }
}

/// 把插件清单条目转换为 REST 预解析响应 DTO（`hub` 侧 wire↔engine 转换，
/// 见 [`HubApiHost::resolve_preview`]）。
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

/// 空串 → `None`（wire DTO 的空 platform/version 归一为 Option）。
#[cfg(hub_link)]
fn link_opt_str(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// [`LinkError`] → [`ApiError`] 映射（决定 HTTP 状态码）。
#[cfg(hub_link)]
fn map_link_err(e: LinkError) -> ApiError {
    match e {
        LinkError::Unauthorized => ApiError::Unauthorized,
        LinkError::InvalidCode
        | LinkError::BadSignature
        | LinkError::BadPayload(_)
        | LinkError::SessionExpired => ApiError::BadRequest(e.to_string()),
        LinkError::Unreachable => ApiError::Unavailable,
        other => ApiError::Internal(other.to_string()),
    }
}
