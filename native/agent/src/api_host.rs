//! engine-neutral `ApiHost`：读取 agent 投影，并把下载相关操作转发给 daemon JSON-RPC。
//!
//! 兼容 API（`/api/v1/*`、`/jsonrpc`、`/mcp`）与官方客户端共用同一个 daemon：这里不持有
//! 任何下载事实，只做 wire 适配（错误码映射、DTO 组装、aria2 通知事件源）。设备互联
//! （配对 / 发现 / 下发 / 数据面路由）委托给 [`LinkService`]，不在这里持有状态。

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use fluxdown_api::service::{ApiError, ApiHost, LiveSpeed, TaskEvent};
use fluxdown_protocol::method;
use fluxdown_protocol::{
    ApplicationErrorCode, BtGuardEntriesAddParams, BtGuardEntriesListDto, BtGuardEntriesListParams,
    BtGuardEntriesRemoveDto, BtGuardEntriesRemoveParams, BtGuardEntryDto, BtGuardEventsListDto,
    BtGuardEventsListParams, BtGuardStateDto, ChangeTaskUrlParams, CreateGroupRequest,
    CreateGroupResponse, CreateTaskRequest, DaemonConfigPatch, DaemonCreateTaskParams,
    DownloadRequest, GroupDto, InstallPluginDevRequest, InstalledPlugin, LinkAuth,
    LinkCodeResponse, LinkDeviceInfo, LinkDiscoveredPeer, LinkPairBeginResponse,
    LinkPairConfirmOutcome, LinkPairConfirmRequest, LinkPairHelloRequest, LinkPairHelloResponse,
    LinkPairRevealRequest, LinkPairRevealResponse, LinkPingInfo, MarketEntryDto,
    MarketInstallRequest, PluginAuthRequest, PluginAuthResponse, PluginDto, QueueDto,
    ResolvePreviewRequest, ResolvePreviewResponse, RpcErrorObject, RssItemActionRequest,
    RssItemDto, RssSourceDto, RssValidateRequest, RssValidateResponse, SiteAuthCredentialDto,
    SiteAuthEntryDto, SiteAuthGetParams, SiteAuthSaveRequest, TaskDto,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::capture::{BlobError, BlobKind, CaptureError, CaptureService, DaemonBlobClient};
use crate::daemon_client::DaemonClient;
use crate::event_hub::AgentEventHub;
use crate::link::LinkService;
use crate::task_events::TaskEventHub;

pub struct AgentApiHost {
    daemon: Arc<DaemonClient>,
    events: AgentEventHub,
    capture: Arc<CaptureService>,
    blobs: Arc<DaemonBlobClient>,
    link: Arc<LinkService>,
    task_events: TaskEventHub,
    /// `/ping` 的 `language` 回退值（server 模式的 `FLUXDOWN_LANG`，已归一化为 en/zh）。
    default_language: Option<String>,
    /// 最近一次安装返回的缺失基础组件。`ApiHost` 只返回 identity，路由层随后经
    /// [`ApiHost::plugin_missing_components`] 取提醒数据；daemon 只在安装响应里给出它。
    missing_components: Mutex<HashMap<String, Vec<String>>>,
}

impl AgentApiHost {
    /// 必须在 Tokio 运行时内调用（内部启动任务事件泵）。
    #[must_use]
    pub fn new(
        daemon: Arc<DaemonClient>,
        events: AgentEventHub,
        capture: Arc<CaptureService>,
        blobs: Arc<DaemonBlobClient>,
        link: Arc<LinkService>,
        default_language: Option<String>,
    ) -> Self {
        let task_events = TaskEventHub::spawn(&events);
        Self {
            daemon,
            events,
            capture,
            blobs,
            link,
            task_events,
            default_language,
            missing_components: Mutex::new(HashMap::new()),
        }
    }

    async fn rpc<P: Serialize, R: DeserializeOwned>(
        &self,
        method_name: &str,
        params: P,
    ) -> Result<R, ApiError> {
        self.daemon
            .call_detailed(method_name, Some(params))
            .await
            .map_err(object_error)
    }

    async fn unit(&self, method_name: &str, params: Value) -> Result<(), ApiError> {
        let _: Value = self.rpc(method_name, params).await?;
        Ok(())
    }

    /// 写操作前置的存在性检查：daemon 的部分操作对不存在的 ID 是幂等成功，
    /// 而兼容 API 契约要求 404。
    async fn ensure_task_exists(&self, task_id: &str) -> Result<(), ApiError> {
        let _: Value = self
            .rpc(method::DAEMON_TASK_GET, json!({ "id": task_id }))
            .await?;
        Ok(())
    }

    async fn ensure_group_exists(&self, group_id: &str) -> Result<(), ApiError> {
        let groups = self.list_groups().await?;
        if groups.iter().any(|group| group.group_id == group_id) {
            Ok(())
        } else {
            Err(ApiError::NotFound)
        }
    }

    fn remember_missing_components(&self, installed: InstalledPlugin) -> String {
        lock_or_recover(&self.missing_components)
            .insert(installed.identity.clone(), installed.missing_components);
        installed.identity
    }

    async fn installed(&self, method_name: &str, params: Value) -> Result<String, ApiError> {
        let installed: InstalledPlugin = self.rpc(method_name, params).await?;
        Ok(self.remember_missing_components(installed))
    }
}

#[async_trait]
impl ApiHost for AgentApiHost {
    async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError> {
        Ok(self
            .events
            .inspect(|snapshot| snapshot.daemon.tasks.clone()))
    }

    async fn get_task(&self, task_id: &str) -> Result<Option<TaskDto>, ApiError> {
        Ok(self.events.inspect(|snapshot| {
            snapshot
                .daemon
                .tasks
                .iter()
                .find(|task| task.task_id == task_id)
                .cloned()
        }))
    }

    async fn create_task(&self, request: CreateTaskRequest) -> Result<String, ApiError> {
        let result: Value = self
            .rpc(
                method::DAEMON_TASK_CREATE,
                DaemonCreateTaskParams {
                    request,
                    torrent_blob_id: None,
                    unattended: false,
                    hint_file_size: None,
                    name_hint: None,
                    category_route: None,
                },
            )
            .await?;
        result
            .get("taskId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Internal("daemon returned no taskId".to_owned()))
    }

    async fn delete_task(&self, task_id: &str, delete_files: bool) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.unit(
            method::DAEMON_TASK_DELETE,
            json!({ "taskId": task_id, "deleteFiles": delete_files }),
        )
        .await
    }

    async fn pause_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.unit(method::DAEMON_TASK_PAUSE, json!({ "taskId": task_id }))
            .await
    }

    async fn continue_task(&self, task_id: &str) -> Result<(), ApiError> {
        self.ensure_task_exists(task_id).await?;
        self.unit(method::DAEMON_TASK_RESUME, json!({ "taskId": task_id }))
            .await
    }

    async fn rename_task(&self, task_id: &str, file_name: &str) -> Result<(), ApiError> {
        self.daemon
            .call_detailed::<_, Value>(
                method::DAEMON_TASK_RENAME,
                Some(json!({ "taskId": task_id, "fileName": file_name })),
            )
            .await
            .map(|_| ())
            .map_err(|error| engine_code_error(error, RenameCodes::Rename))
    }

    async fn change_task_url(&self, task_id: &str, url: &str) -> Result<(), ApiError> {
        self.daemon
            .call_detailed::<_, Value>(
                method::DAEMON_TASK_CHANGE_URL,
                Some(ChangeTaskUrlParams {
                    task_id: task_id.to_owned(),
                    url: url.to_owned(),
                }),
            )
            .await
            .map(|_| ())
            .map_err(|error| engine_code_error(error, RenameCodes::ChangeUrl))
    }

    async fn pause_all(&self) -> Result<(), ApiError> {
        self.unit(method::DAEMON_TASK_PAUSE_ALL, json!({})).await
    }

    async fn continue_all(&self) -> Result<(), ApiError> {
        self.unit(method::DAEMON_TASK_RESUME_ALL, json!({})).await
    }

    async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
        Ok(self
            .events
            .inspect(|snapshot| snapshot.daemon.queues.clone()))
    }

    async fn submit_external(&self, request: DownloadRequest) -> Result<(), ApiError> {
        if request.url.lines().all(|line| line.trim().is_empty()) {
            return Err(ApiError::BadRequest("url is required".to_owned()));
        }
        self.capture
            .submit(request, crate::capture::CaptureOrigin::External)
            .await
            .map(|_| ())
            .map_err(capture_error)
    }

    async fn get_config(&self) -> Result<HashMap<String, String>, ApiError> {
        Ok(self.events.inspect(|snapshot| {
            snapshot
                .daemon
                .config
                .values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        }))
    }

    async fn web_language(&self) -> Option<String> {
        self.default_language.clone()
    }

    async fn apply_config(&self, changes: HashMap<String, String>) -> Result<(), ApiError> {
        let values: BTreeMap<String, String> = changes.into_iter().collect();
        let mut revision = self
            .events
            .inspect(|snapshot| snapshot.daemon.config.revision);
        // 投影可能落后于 daemon：遇到版本冲突时用 daemon 报告的当前版本重试一次。
        for attempt in 0..2 {
            let result = self
                .daemon
                .call_detailed::<_, Value>(
                    method::DAEMON_CONFIG_PATCH,
                    Some(DaemonConfigPatch {
                        expected_revision: revision,
                        values: values.clone(),
                    }),
                )
                .await;
            match result {
                Ok(_) => return Ok(()),
                Err(error) => match conflict_revision(&error) {
                    Some(current) if attempt == 0 => revision = current,
                    _ => return Err(object_error(error)),
                },
            }
        }
        Err(ApiError::Conflict("config revision conflict".to_owned()))
    }

    async fn list_site_auth(&self) -> Result<Vec<SiteAuthEntryDto>, ApiError> {
        self.rpc(method::DAEMON_SITE_AUTH_LIST, json!({})).await
    }

    async fn get_site_auth(&self, site: &str) -> Result<Option<SiteAuthCredentialDto>, ApiError> {
        self.rpc(
            method::DAEMON_SITE_AUTH_GET,
            SiteAuthGetParams {
                site: site.to_owned(),
            },
        )
        .await
    }

    async fn save_site_auth(
        &self,
        request: SiteAuthSaveRequest,
    ) -> Result<SiteAuthEntryDto, ApiError> {
        self.rpc(method::DAEMON_SITE_AUTH_SAVE, request).await
    }

    async fn delete_site_auth(&self, site: &str) -> Result<(), ApiError> {
        self.unit(method::DAEMON_SITE_AUTH_DELETE, json!({ "site": site }))
            .await
    }

    async fn live_speeds(&self) -> Result<HashMap<String, LiveSpeed>, ApiError> {
        Ok(self.task_events.live_speeds())
    }

    fn subscribe_task_events(&self) -> Option<broadcast::Receiver<TaskEvent>> {
        Some(self.task_events.subscribe())
    }

    // -- 插件系统 --

    async fn list_plugins(&self) -> Result<Vec<PluginDto>, ApiError> {
        self.rpc(method::DAEMON_PLUGIN_LIST, json!({})).await
    }

    async fn set_plugin_enabled(&self, identity: &str, enabled: bool) -> Result<(), ApiError> {
        self.unit(
            method::DAEMON_PLUGIN_SET_ENABLED,
            json!({ "identity": identity, "enabled": enabled }),
        )
        .await
    }

    async fn uninstall_plugin(&self, identity: &str) -> Result<(), ApiError> {
        self.unit(
            method::DAEMON_PLUGIN_UNINSTALL,
            json!({ "identity": identity }),
        )
        .await
    }

    async fn update_plugin_settings(
        &self,
        identity: &str,
        entries: HashMap<String, String>,
    ) -> Result<(), ApiError> {
        self.unit(
            method::DAEMON_PLUGIN_UPDATE_SETTINGS,
            json!({ "identity": identity, "entries": entries }),
        )
        .await
    }

    async fn plugin_auth(
        &self,
        request: PluginAuthRequest,
    ) -> Result<PluginAuthResponse, ApiError> {
        self.rpc(method::DAEMON_PLUGIN_AUTH, request).await
    }

    async fn install_plugin_zip(&self, bytes: Vec<u8>) -> Result<String, ApiError> {
        let blob_id = self
            .blobs
            .upload(BlobKind::Plugin, bytes)
            .await
            .map_err(blob_error)?;
        self.installed(method::DAEMON_PLUGIN_INSTALL, json!({ "blobId": blob_id }))
            .await
    }

    async fn install_plugin_dev(&self, dir_path: String) -> Result<String, ApiError> {
        let params = serde_json::to_value(InstallPluginDevRequest { dir_path })
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        self.installed(method::DAEMON_PLUGIN_INSTALL_DEV, params)
            .await
    }

    async fn ignore_plugin_retry(&self, task_id: &str) -> Result<(), ApiError> {
        self.unit(
            method::DAEMON_PLUGIN_IGNORE_RETRY,
            json!({ "taskId": task_id }),
        )
        .await
    }

    async fn market_list(&self) -> Result<Vec<MarketEntryDto>, ApiError> {
        self.rpc(method::DAEMON_PLUGIN_MARKET_LIST, json!({})).await
    }

    async fn market_install(&self, plugin_id: &str) -> Result<String, ApiError> {
        let params = serde_json::to_value(MarketInstallRequest {
            plugin_id: plugin_id.to_owned(),
            version: None,
        })
        .map_err(|error| ApiError::Internal(error.to_string()))?;
        self.installed(method::DAEMON_PLUGIN_MARKET_INSTALL, params)
            .await
    }

    async fn plugin_missing_components(&self, identity: &str) -> Vec<String> {
        lock_or_recover(&self.missing_components)
            .remove(identity)
            .unwrap_or_default()
    }

    // -- 任务组与前置预解析 --

    async fn resolve_preview(
        &self,
        request: ResolvePreviewRequest,
    ) -> Result<ResolvePreviewResponse, ApiError> {
        self.rpc(method::DAEMON_GROUP_RESOLVE_PREVIEW, request)
            .await
    }

    async fn create_task_group(&self, request: CreateGroupRequest) -> Result<String, ApiError> {
        let created: CreateGroupResponse = self.rpc(method::DAEMON_GROUP_CREATE, request).await?;
        Ok(created.group_id)
    }

    async fn list_groups(&self) -> Result<Vec<GroupDto>, ApiError> {
        Ok(self
            .events
            .inspect(|snapshot| snapshot.daemon.groups.clone()))
    }

    async fn group_pause(&self, group_id: &str) -> Result<(), ApiError> {
        self.ensure_group_exists(group_id).await?;
        self.unit(method::DAEMON_GROUP_PAUSE, json!({ "groupId": group_id }))
            .await
    }

    async fn group_continue(&self, group_id: &str) -> Result<(), ApiError> {
        self.ensure_group_exists(group_id).await?;
        self.unit(method::DAEMON_GROUP_RESUME, json!({ "groupId": group_id }))
            .await
    }

    async fn group_delete(&self, group_id: &str, delete_files: bool) -> Result<(), ApiError> {
        self.ensure_group_exists(group_id).await?;
        self.unit(
            method::DAEMON_GROUP_DELETE,
            json!({ "groupId": group_id, "deleteFiles": delete_files }),
        )
        .await
    }

    // -- RSS 订阅 --

    async fn list_rss_sources(&self) -> Result<Vec<RssSourceDto>, ApiError> {
        self.rpc(method::DAEMON_RSS_LIST_SOURCES, json!({})).await
    }

    async fn create_rss_source(&self, request: RssSourceDto) -> Result<String, ApiError> {
        let created: Value = self.rpc(method::DAEMON_RSS_CREATE_SOURCE, request).await?;
        created
            .get("sourceId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Internal("daemon returned no sourceId".to_owned()))
    }

    /// 路径段是权威 ID——请求体里的 `sourceId` 一律被它覆盖。
    async fn update_rss_source(
        &self,
        source_id: &str,
        mut request: RssSourceDto,
    ) -> Result<(), ApiError> {
        source_id.clone_into(&mut request.source_id);
        let _: Value = self.rpc(method::DAEMON_RSS_UPDATE_SOURCE, request).await?;
        Ok(())
    }

    async fn delete_rss_source(&self, source_id: &str) -> Result<(), ApiError> {
        self.unit(
            method::DAEMON_RSS_DELETE_SOURCE,
            json!({ "sourceId": source_id }),
        )
        .await
    }

    async fn refresh_rss_source(&self, source_id: &str) -> Result<(), ApiError> {
        self.unit(
            method::DAEMON_RSS_REFRESH_SOURCE,
            json!({ "sourceId": source_id }),
        )
        .await
    }

    async fn list_rss_items(&self, source_id: &str) -> Result<Vec<RssItemDto>, ApiError> {
        self.rpc(
            method::DAEMON_RSS_GET_ITEMS,
            json!({ "sourceId": source_id }),
        )
        .await
    }

    async fn rss_item_action(
        &self,
        source_id: &str,
        request: RssItemActionRequest,
    ) -> Result<(), ApiError> {
        self.unit(
            method::DAEMON_RSS_ITEM_ACTION,
            json!({
                "sourceId": source_id,
                "guid": request.guid,
                "action": request.action,
            }),
        )
        .await
    }

    async fn validate_rss_feed(
        &self,
        request: RssValidateRequest,
    ) -> Result<RssValidateResponse, ApiError> {
        self.rpc(method::DAEMON_RSS_VALIDATE, request).await
    }

    // -- BT 防吸血：只读与条目管理转发 daemon；云名单应用 / 证据弹出是 agent 内部方法，不在此暴露 --

    async fn bt_guard_state(&self) -> Result<BtGuardStateDto, ApiError> {
        self.rpc(method::DAEMON_BT_GUARD_STATE, json!({})).await
    }

    async fn bt_guard_entries_list(
        &self,
        params: BtGuardEntriesListParams,
    ) -> Result<BtGuardEntriesListDto, ApiError> {
        self.rpc(method::DAEMON_BT_GUARD_ENTRIES_LIST, params).await
    }

    async fn bt_guard_entries_add(
        &self,
        params: BtGuardEntriesAddParams,
    ) -> Result<BtGuardEntryDto, ApiError> {
        self.rpc(method::DAEMON_BT_GUARD_ENTRIES_ADD, params).await
    }

    /// daemon 的移除是批量且对不存在的 id 幂等；兼容 API 契约要求单个不存在的条目回 404。
    async fn bt_guard_entry_remove(&self, entry_id: i64) -> Result<(), ApiError> {
        let removed: BtGuardEntriesRemoveDto = self
            .rpc(
                method::DAEMON_BT_GUARD_ENTRIES_REMOVE,
                BtGuardEntriesRemoveParams {
                    ids: vec![entry_id],
                },
            )
            .await?;
        if removed.removed == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn bt_guard_events_list(
        &self,
        params: BtGuardEventsListParams,
    ) -> Result<BtGuardEventsListDto, ApiError> {
        self.rpc(method::DAEMON_BT_GUARD_EVENTS_LIST, params).await
    }

    // -- 设备互联（局域网直连 L1）：全部交给 `LinkService` --

    async fn link_ping_info(&self) -> Option<LinkPingInfo> {
        self.link.api_ping_info()
    }

    async fn link_pair_hello(
        &self,
        req: LinkPairHelloRequest,
        source: Option<IpAddr>,
    ) -> Result<LinkPairHelloResponse, ApiError> {
        self.link.api_pair_hello(req, source).await
    }

    async fn link_pair_reveal(
        &self,
        req: LinkPairRevealRequest,
    ) -> Result<LinkPairRevealResponse, ApiError> {
        self.link.api_pair_reveal(req).await
    }

    async fn link_pair_confirm(
        &self,
        req: LinkPairConfirmRequest,
    ) -> Result<LinkPairConfirmOutcome, ApiError> {
        self.link.api_pair_confirm(req).await
    }

    async fn link_approve_incoming(&self, session_id: &str, accept: bool) -> Result<(), ApiError> {
        self.link.api_approve(session_id, accept)
    }

    async fn link_create_task(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        self.link.api_create_task(auth, body).await
    }

    async fn link_peer_info(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        self.link.api_peer_info(auth, body).await
    }

    async fn link_unpair(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        self.link.api_unpair(auth, body).await
    }

    fn link_trusted_proxy(&self, peer: IpAddr) -> bool {
        self.link.trusted_proxy(peer)
    }

    async fn link_generate_code(&self) -> Result<LinkCodeResponse, ApiError> {
        self.link.api_generate_code()
    }

    async fn link_stop_advertising(&self) -> Result<(), ApiError> {
        self.link.api_stop_advertising()
    }

    async fn link_discovery(&self, start: bool) -> Result<(), ApiError> {
        self.link.api_discovery(start)
    }

    async fn link_discovered(&self) -> Result<Vec<LinkDiscoveredPeer>, ApiError> {
        self.link.api_discovered()
    }

    async fn link_probe(&self, host: &str, port: u16) -> Result<LinkDiscoveredPeer, ApiError> {
        self.link.api_probe(host, port).await
    }

    async fn link_pair_begin(
        &self,
        host: &str,
        port: u16,
        code: &str,
    ) -> Result<LinkPairBeginResponse, ApiError> {
        self.link.api_pair_begin(host, port, code).await
    }

    async fn link_pair_finish(
        &self,
        token: &str,
        accept: bool,
    ) -> Result<Option<LinkDeviceInfo>, ApiError> {
        self.link.api_pair_finish(token, accept).await
    }

    async fn link_devices(&self) -> Result<Vec<LinkDeviceInfo>, ApiError> {
        self.link.api_devices().await
    }

    async fn link_remove_device(&self, fingerprint: &str) -> Result<bool, ApiError> {
        self.link.api_remove_device(fingerprint).await
    }

    async fn link_dispatch(
        &self,
        fingerprint: &str,
        url: &str,
        save_dir: Option<&str>,
        file_name: Option<&str>,
    ) -> Result<String, ApiError> {
        self.link
            .api_dispatch(fingerprint, url, save_dir, file_name)
            .await
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// daemon JSON-RPC 错误 → 兼容 API 错误。业务消息原样保留，客户端据此做 i18n / 诊断。
pub(crate) fn object_error(error: RpcErrorObject) -> ApiError {
    let message = error.message;
    match error.data.map(|data| data.code) {
        Some(ApplicationErrorCode::Unauthorized) => ApiError::Unauthorized,
        Some(ApplicationErrorCode::NotFound) => ApiError::NotFound,
        Some(ApplicationErrorCode::Conflict) => ApiError::Conflict(message),
        Some(ApplicationErrorCode::InvalidArgument) => ApiError::BadRequest(message),
        Some(ApplicationErrorCode::Unavailable | ApplicationErrorCode::Timeout) => {
            ApiError::Unavailable
        }
        _ => ApiError::Internal(message),
    }
}

/// 配置版本冲突时 daemon 报告的当前版本。
fn conflict_revision(error: &RpcErrorObject) -> Option<u64> {
    let data = error.data.as_ref()?;
    (data.code == ApplicationErrorCode::Conflict)
        .then_some(data.revision)
        .flatten()
}

/// 引擎稳定错误码字符串的两套映射表（daemon 以 Internal 错误的 `message` 携带）。
#[derive(Clone, Copy)]
enum RenameCodes {
    Rename,
    ChangeUrl,
}

/// 按稳定错误码映射 HTTP 语义：`not-found`→404、非法输入→400、其余业务拒绝→409，
/// 除 `not-found` 外错误码字符串原样透传；未知消息（如 `db:` / `rename:` IO 错误）按 daemon
/// 的应用错误码回落。
fn engine_code_error(error: RpcErrorObject, codes: RenameCodes) -> ApiError {
    let (bad_request, conflicts): (&[&str], &[&str]) = match codes {
        RenameCodes::Rename => (
            &["invalid-name"],
            &["task-active", "bt-unsupported", "target-exists"],
        ),
        RenameCodes::ChangeUrl => (
            &["invalid-url"],
            &[
                "task-active",
                "task-completed",
                "bt-unsupported",
                "protocol-unsupported",
                "protocol-mismatch",
            ],
        ),
    };
    let message = error.message.as_str();
    if message == "not-found" {
        ApiError::NotFound
    } else if bad_request.contains(&message) {
        ApiError::BadRequest(error.message)
    } else if conflicts.contains(&message) {
        ApiError::Conflict(error.message)
    } else {
        object_error(error)
    }
}

fn capture_error(error: CaptureError) -> ApiError {
    match error {
        CaptureError::Daemon(data) => object_error(RpcErrorObject::application(
            "daemon capture create failed",
            data,
        )),
        CaptureError::Full => ApiError::Unavailable,
        other => ApiError::Internal(other.to_string()),
    }
}

fn blob_error(error: BlobError) -> ApiError {
    match error {
        BlobError::Status(400 | 413 | 415) => {
            ApiError::BadRequest(format!("plugin package rejected: {error}"))
        }
        other => ApiError::Internal(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_api::service::ApiError;
    use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, RpcErrorObject};

    use super::{RenameCodes, blob_error, conflict_revision, engine_code_error, object_error};
    use crate::capture::BlobError;

    fn internal(message: &str) -> RpcErrorObject {
        RpcErrorObject::application(
            message,
            RpcErrorData::new(ApplicationErrorCode::Internal, false),
        )
    }

    #[test]
    fn rename_error_codes_map_to_http_semantics_and_keep_the_code_string() {
        let map = |message: &str| engine_code_error(internal(message), RenameCodes::Rename);
        assert!(matches!(map("not-found"), ApiError::NotFound));
        assert!(matches!(map("invalid-name"), ApiError::BadRequest(m) if m == "invalid-name"));
        for code in ["task-active", "bt-unsupported", "target-exists"] {
            assert!(
                matches!(map(code), ApiError::Conflict(m) if m == code),
                "{code}"
            );
        }
        // 引擎 IO / DB 错误不属稳定码：按 Internal 透传原文。
        assert!(matches!(map("rename: denied"), ApiError::Internal(m) if m == "rename: denied"));
        // 换源专属码在重命名表里不算业务拒绝。
        assert!(matches!(map("task-completed"), ApiError::Internal(_)));
    }

    #[test]
    fn change_url_error_codes_map_to_http_semantics_and_keep_the_code_string() {
        let map = |message: &str| engine_code_error(internal(message), RenameCodes::ChangeUrl);
        assert!(matches!(map("not-found"), ApiError::NotFound));
        assert!(matches!(map("invalid-url"), ApiError::BadRequest(m) if m == "invalid-url"));
        for code in [
            "task-active",
            "task-completed",
            "bt-unsupported",
            "protocol-unsupported",
            "protocol-mismatch",
        ] {
            assert!(
                matches!(map(code), ApiError::Conflict(m) if m == code),
                "{code}"
            );
        }
        assert!(matches!(map("invalid-name"), ApiError::Internal(_)));
        assert!(matches!(
            map("thunder decode failed"),
            ApiError::Internal(_)
        ));
    }

    #[test]
    fn typed_daemon_errors_map_by_application_code() {
        let with = |code: ApplicationErrorCode| {
            object_error(RpcErrorObject::application(
                "detail",
                RpcErrorData::new(code, false),
            ))
        };
        assert!(matches!(
            with(ApplicationErrorCode::Unauthorized),
            ApiError::Unauthorized
        ));
        assert!(matches!(
            with(ApplicationErrorCode::NotFound),
            ApiError::NotFound
        ));
        assert!(
            matches!(with(ApplicationErrorCode::Conflict), ApiError::Conflict(m) if m == "detail")
        );
        assert!(
            matches!(with(ApplicationErrorCode::InvalidArgument), ApiError::BadRequest(m) if m == "detail")
        );
        assert!(matches!(
            with(ApplicationErrorCode::Unavailable),
            ApiError::Unavailable
        ));
        assert!(matches!(
            with(ApplicationErrorCode::Timeout),
            ApiError::Unavailable
        ));
        assert!(
            matches!(with(ApplicationErrorCode::Unsupported), ApiError::Internal(m) if m == "detail")
        );
        // 无应用错误详情（协议级错误）→ Internal。
        let bare = RpcErrorObject {
            code: -32601,
            message: "method not found".to_owned(),
            data: None,
        };
        assert!(matches!(object_error(bare), ApiError::Internal(m) if m == "method not found"));
    }

    #[test]
    fn conflict_revision_is_only_reported_for_conflict_errors() {
        let mut data = RpcErrorData::new(ApplicationErrorCode::Conflict, false);
        data.revision = Some(9);
        assert_eq!(
            conflict_revision(&RpcErrorObject::application("c", data.clone())),
            Some(9)
        );
        data.code = ApplicationErrorCode::InvalidArgument;
        assert_eq!(
            conflict_revision(&RpcErrorObject::application("c", data)),
            None
        );
        assert_eq!(conflict_revision(&internal("x")), None);
    }

    #[test]
    fn plugin_blob_rejections_are_client_errors_other_failures_are_internal() {
        assert!(matches!(
            blob_error(BlobError::Status(413)),
            ApiError::BadRequest(_)
        ));
        assert!(matches!(
            blob_error(BlobError::Status(400)),
            ApiError::BadRequest(_)
        ));
        assert!(matches!(
            blob_error(BlobError::Status(500)),
            ApiError::Internal(_)
        ));
        assert!(matches!(
            blob_error(BlobError::Decode),
            ApiError::Internal(_)
        ));
    }
}
