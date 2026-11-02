//! `HostSession`：一台主机的 FFI 会话对象。`next_signal` 拉取已接受的信号，其余方法把
//! `HostSession.kt` 的命令映射到协议 RPC（`daemon.*`）。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::method::{
    CAPABILITY_CLIENT_SELECTIONS, DAEMON_CONFIG_PATCH, DAEMON_QUEUE_BOOST, DAEMON_QUEUE_MOVE_TASK,
    DAEMON_RSS_LIST_SOURCES, DAEMON_RSS_REFRESH_SOURCE, DAEMON_RSS_UPDATE_SOURCE,
    DAEMON_SELECTION_RESOLVE, DAEMON_TASK_CHANGE_URL, DAEMON_TASK_CREATE, DAEMON_TASK_CREATE_MANY,
    DAEMON_TASK_DELETE, DAEMON_TASK_DELETE_MANY, DAEMON_TASK_PAUSE, DAEMON_TASK_PAUSE_ALL,
    DAEMON_TASK_PAUSE_MANY, DAEMON_TASK_RENAME, DAEMON_TASK_RESCAN, DAEMON_TASK_RESUME,
    DAEMON_TASK_RESUME_ALL, DAEMON_TASK_RESUME_MANY, SYSTEM_HELLO,
};
use fluxdown_protocol::{
    APP_VERSION, CategoryRouteDto, ClientHello, CreateTaskRequest, CreatedTask, DaemonConfigPatch,
    DaemonCreateTaskParams, DaemonCreateTasksParams, DaemonCreateTasksResult, MIN_PROTOCOL_VERSION,
    PROTOCOL_VERSION, RequestId, RpcRequest, RssSourceDto, ServiceRole,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::driver::{Driver, LinkState, Timing};
use crate::dto::{CreateTaskRequestDto, HostSignalDto, SelectionOutcomeDto};
use crate::error::FluxError;
use crate::import::{ImportBatchDto, ImportHandle};
use crate::link::{Caller, Connector};

/// 向 UI 的信号队列容量。
const SIGNAL_QUEUE: usize = 1024;
/// 连接恢复期间命令最长排队时间（离线宽限内的命令排队，之后失败）。
const COMMAND_WAIT: Duration = Duration::from_secs(10);

/// 客户端名：与 Android 应用对应；iOS 构建自报 `fluxdown-ios`。
const CLIENT_NAME: &str = if cfg!(target_os = "ios") {
    "fluxdown-ios"
} else {
    "fluxdown-android"
};

/// `system.hello` 首帧：声明 `client.selections`，agent 据此把引擎选择请求推给本 UI。
pub(crate) fn client_hello() -> Result<RpcRequest, FluxError> {
    let hello = ClientHello {
        client_name: CLIENT_NAME.to_owned(),
        client_version: APP_VERSION.to_owned(),
        min_protocol_version: MIN_PROTOCOL_VERSION,
        max_protocol_version: PROTOCOL_VERSION,
        requested_role: ServiceRole::Agent,
        capabilities: vec![CAPABILITY_CLIENT_SELECTIONS.to_owned()],
    };
    let params = serde_json::to_value(hello)
        .map_err(|error| FluxError::internal(format!("encode hello failed: {error}")))?;
    Ok(RpcRequest::new(
        RequestId::Integer(1),
        SYSTEM_HELLO,
        Some(params),
    ))
}

/// 一台主机的会话。由 [`crate::FluxCore`] 创建。
#[derive(uniffi::Object)]
pub struct HostSession {
    signals: Mutex<mpsc::Receiver<HostSignalDto>>,
    link: watch::Receiver<LinkState>,
    cancel: CancellationToken,
    /// 导入等后台任务的执行器（与会话的驱动同一个 runtime）。
    runtime: Handle,
    /// 主机当前分类路由（驱动随快照 / 偏好事件更新），建任务时按它路由。
    routes: watch::Receiver<Option<CategoryRouteDto>>,
}

/// 启动驱动并等待首次同步：连接 / 握手 / 鉴权 / 快照任一失败都作为错误返回，
/// 成功时首个信号（`Snapshot`）已排入队列。
pub(crate) async fn open_session(
    runtime: &Handle,
    connector: Arc<dyn Connector>,
    cancel: CancellationToken,
    timing: Timing,
) -> Result<(Arc<HostSession>, JoinHandle<()>), FluxError> {
    let (signals_tx, signals_rx) = mpsc::channel(SIGNAL_QUEUE);
    let (link_tx, link_rx) = watch::channel(LinkState::Connecting);
    let (first_tx, first_rx) = oneshot::channel();
    let (routes_tx, routes_rx) = watch::channel(None);
    let driver = Driver {
        connector,
        hello: client_hello()?,
        signals: signals_tx,
        link: link_tx,
        cancel: cancel.clone(),
        first: Some(first_tx),
        timing,
        routes: routes_tx,
    };
    let task = runtime.spawn(driver.run());
    match first_rx.await {
        Ok(Ok(())) => Ok((
            Arc::new(HostSession {
                signals: Mutex::new(signals_rx),
                link: link_rx,
                cancel,
                runtime: runtime.clone(),
                routes: routes_rx,
            }),
            task,
        )),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(FluxError::Closed),
    }
}

/// 等待连接可用后取得调用端；重连期间排队（最长 [`COMMAND_WAIT`]），超时失败。
pub(crate) async fn ready_caller(
    link: &watch::Receiver<LinkState>,
    cancel: &CancellationToken,
) -> Result<Arc<dyn Caller>, FluxError> {
    if cancel.is_cancelled() {
        return Err(FluxError::Closed);
    }
    let mut link = link.clone();
    let waited = timeout(
        COMMAND_WAIT,
        link.wait_for(|state| !matches!(state, LinkState::Connecting)),
    )
    .await;
    match waited {
        Err(_) => Err(FluxError::transport("host is unreachable")),
        Ok(Err(_)) => Err(FluxError::Closed),
        Ok(Ok(state)) => match &*state {
            LinkState::Ready(caller) => Ok(Arc::clone(caller)),
            LinkState::Closed | LinkState::Connecting => Err(FluxError::Closed),
        },
    }
}

/// 批量导入的发送端：经当前连接调用 `daemon.task.createMany`（本机进程内 / 远端 `/rpc` 同一路径）。
pub(crate) struct SessionSender {
    link: watch::Receiver<LinkState>,
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl crate::import::ChunkSender for SessionSender {
    async fn send(
        &self,
        params: &DaemonCreateTasksParams,
    ) -> Result<DaemonCreateTasksResult, FluxError> {
        let caller = ready_caller(&self.link, &self.cancel).await?;
        let value = caller
            .call(
                DAEMON_TASK_CREATE_MANY,
                Some(encode(params, "create many")?),
            )
            .await?;
        decode(value, "create many result")
    }
}

impl Drop for HostSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl HostSession {
    /// 等待连接可用后取得调用端；重连期间排队，超时失败。
    async fn ready_caller(&self) -> Result<Arc<dyn Caller>, FluxError> {
        ready_caller(&self.link, &self.cancel).await
    }

    /// 批量导入用的发送端：只拿连接状态与取消信号，不持有会话本身（导入任务不延长会话寿命）。
    pub(crate) fn chunk_sender(&self) -> SessionSender {
        SessionSender {
            link: self.link.clone(),
            cancel: self.cancel.clone(),
        }
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, FluxError> {
        self.ready_caller().await?.call(method, Some(params)).await
    }

    async fn rpc_bare(&self, method: &str) -> Result<Value, FluxError> {
        self.ready_caller().await?.call(method, None).await
    }

    async fn unit(&self, method: &str, params: Value) -> Result<(), FluxError> {
        self.rpc(method, params).await.map(drop)
    }

    async fn unit_bare(&self, method: &str) -> Result<(), FluxError> {
        self.rpc_bare(method).await.map(drop)
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: Value, what: &str) -> Result<T, FluxError> {
    serde_json::from_value(value)
        .map_err(|error| FluxError::internal(format!("undecodable {what}: {error}")))
}

fn encode<T: Serialize>(value: &T, what: &str) -> Result<Value, FluxError> {
    serde_json::to_value(value)
        .map_err(|error| FluxError::internal(format!("encode {what} failed: {error}")))
}

/// 仅 agent 内部使用的方法：UI 通用通道不放行（云名单整层替换会绕过验签后的信任边界，
/// 证据队列只能由已登录的 agent 上报流程弹出）。
const AGENT_ONLY_METHODS: &[&str] = &[
    fluxdown_protocol::method::DAEMON_BT_GUARD_CLOUD_APPLY,
    fluxdown_protocol::method::DAEMON_BT_GUARD_EVIDENCE_DRAIN,
];

/// 通用调用的前置校验：只放行 `daemon.*` / `agent.*`（`system.*` 属会话层，不对外开放；
/// [`AGENT_ONLY_METHODS`] 同样拒绝），参数必须是合法 JSON；JSON `null` 等同于无参数。
fn prepare_call(method: &str, params_json: Option<&str>) -> Result<Option<Value>, FluxError> {
    let namespaced = ["daemon.", "agent."].iter().any(|prefix| {
        method
            .strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty())
    });
    if !namespaced || AGENT_ONLY_METHODS.contains(&method) {
        return Err(FluxError::invalid_argument(format!(
            "method {method:?} is not allowed: only daemon.* and agent.* can be called"
        )));
    }
    let Some(text) = params_json else {
        return Ok(None);
    };
    let value = serde_json::from_str::<Value>(text).map_err(|error| {
        FluxError::invalid_argument(format!("params of {method} are not valid JSON: {error}"))
    })?;
    Ok((!value.is_null()).then_some(value))
}

/// 把表单请求映射为 `daemon.task.create` 参数：`offered_save_dir` 为 `Some` 且 `route`
/// 存在、提交目录仍跟随系统给出的目录时，`saveDir` 取路由基准目录并带上 `categoryRoute`；
/// 其余情况原样提交、不路由。
pub(crate) fn routed_create_params(
    request: CreateTaskRequestDto,
    route: Option<&CategoryRouteDto>,
) -> DaemonCreateTaskParams {
    let unattended = request.unattended;
    let (request, category_route) = routed_request(request, route);
    DaemonCreateTaskParams {
        request,
        torrent_blob_id: None,
        // 交互式：声明了 client.selections，需要选择时请求会推到本 UI；免打扰由调用方声明。
        unattended,
        hint_file_size: None,
        name_hint: None,
        category_route,
    }
}

/// [`routed_create_params`] 的请求部分，批量导入复用：返回协议请求与要随之发送的路由。
pub(crate) fn routed_request(
    request: CreateTaskRequestDto,
    route: Option<&CategoryRouteDto>,
) -> (CreateTaskRequest, Option<CategoryRouteDto>) {
    let offered = request.offered_save_dir.clone();
    let mut request = CreateTaskRequest::from(request);
    let routed = match (offered, route) {
        (Some(offered), Some(route)) if route.follows(&request.save_dir, &offered) => {
            request.save_dir = route.base_dir(&request.save_dir, &offered);
            Some(route.clone())
        }
        _ => None,
    };
    (request, routed)
}

/// 会话路由下命中的分类保存目录；无路由 / 无命中 / 命中分类未配置目录时为 `None`。
fn category_save_dir_for(
    route: Option<&CategoryRouteDto>,
    file_name: &str,
    url: &str,
) -> Option<String> {
    fluxdown_agent::route_save_dir(route?, file_name, url).map(str::to_owned)
}

#[uniffi::export(async_runtime = "tokio")]
impl HostSession {
    /// 拉取式信号流：首个为 `Snapshot`；`None` = 会话已永久关闭（`Fatal` 之后、
    /// `disconnect` 之后）。
    pub async fn next_signal(&self) -> Option<HostSignalDto> {
        let mut signals = self.signals.lock().await;
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => None,
            signal = signals.recv() => signal,
        }
    }

    /// 通用 RPC 通道：调用协议里任意 `daemon.*` / `agent.*` 方法。`params_json` 与返回值都是
    /// 协议 serde wire 的 JSON 文本（camelCase；结果为空时返回 `"null"`）。连接恢复期间的排队 /
    /// 超时语义与其它命令一致；方法前缀或参数 JSON 不合法返回 `InvalidArgument`。
    pub async fn call(
        &self,
        method: String,
        params_json: Option<String>,
    ) -> Result<String, FluxError> {
        let params = prepare_call(&method, params_json.as_deref())?;
        let value = self.ready_caller().await?.call(&method, params).await?;
        serde_json::to_string(&value)
            .map_err(|error| FluxError::internal(format!("encode {method} result failed: {error}")))
    }

    // daemon.task.*

    pub async fn create_task(&self, request: CreateTaskRequestDto) -> Result<String, FluxError> {
        let route = self.routes.borrow().clone();
        let params = routed_create_params(request, route.as_ref());
        let value = self
            .rpc(DAEMON_TASK_CREATE, encode(&params, "create task")?)
            .await?;
        Ok(decode::<CreatedTask>(value, "create task result")?.task_id)
    }

    pub async fn pause(&self, task_id: String) -> Result<(), FluxError> {
        self.unit(DAEMON_TASK_PAUSE, json!({ "taskId": task_id }))
            .await
    }

    pub async fn resume(&self, task_id: String) -> Result<(), FluxError> {
        self.unit(DAEMON_TASK_RESUME, json!({ "taskId": task_id }))
            .await
    }

    pub async fn delete(&self, task_id: String, delete_files: bool) -> Result<(), FluxError> {
        self.unit(
            DAEMON_TASK_DELETE,
            json!({ "taskId": task_id, "deleteFiles": delete_files }),
        )
        .await
    }

    pub async fn pause_many(&self, task_ids: Vec<String>) -> Result<(), FluxError> {
        self.unit(DAEMON_TASK_PAUSE_MANY, json!({ "taskIds": task_ids }))
            .await
    }

    pub async fn resume_many(&self, task_ids: Vec<String>) -> Result<(), FluxError> {
        self.unit(DAEMON_TASK_RESUME_MANY, json!({ "taskIds": task_ids }))
            .await
    }

    pub async fn delete_many(
        &self,
        task_ids: Vec<String>,
        delete_files: bool,
    ) -> Result<(), FluxError> {
        self.unit(
            DAEMON_TASK_DELETE_MANY,
            json!({ "taskIds": task_ids, "deleteFiles": delete_files }),
        )
        .await
    }

    pub async fn pause_all(&self) -> Result<(), FluxError> {
        self.unit_bare(DAEMON_TASK_PAUSE_ALL).await
    }

    pub async fn resume_all(&self) -> Result<(), FluxError> {
        self.unit_bare(DAEMON_TASK_RESUME_ALL).await
    }

    pub async fn rename(&self, task_id: String, file_name: String) -> Result<(), FluxError> {
        self.unit(
            DAEMON_TASK_RENAME,
            json!({ "taskId": task_id, "fileName": file_name }),
        )
        .await
    }

    pub async fn change_url(&self, task_id: String, url: String) -> Result<(), FluxError> {
        self.unit(
            DAEMON_TASK_CHANGE_URL,
            json!({ "taskId": task_id, "url": url }),
        )
        .await
    }

    pub async fn rescan(&self) -> Result<(), FluxError> {
        self.unit_bare(DAEMON_TASK_RESCAN).await
    }

    // daemon.queue.*

    /// `queue_id` 为空串 = 主队列。
    pub async fn move_to_queue(&self, task_id: String, queue_id: String) -> Result<(), FluxError> {
        self.unit(
            DAEMON_QUEUE_MOVE_TASK,
            json!({ "taskId": task_id, "queueId": queue_id }),
        )
        .await
    }

    pub async fn boost(&self, task_id: String) -> Result<(), FluxError> {
        self.unit(DAEMON_QUEUE_BOOST, json!({ "taskId": task_id }))
            .await
    }

    // daemon.selection.*

    pub async fn resolve_selection(
        &self,
        request_id: String,
        outcome: SelectionOutcomeDto,
    ) -> Result<(), FluxError> {
        let resolution = fluxdown_protocol::SelectionResolutionDto {
            request_id,
            outcome: outcome.into(),
        };
        self.unit(
            DAEMON_SELECTION_RESOLVE,
            encode(&resolution, "selection resolution")?,
        )
        .await
    }

    // daemon.config.*：乐观并发，`expected_revision` 不符时主机返回 `Conflict`。

    pub async fn patch_config(
        &self,
        expected_revision: u64,
        values: HashMap<String, String>,
    ) -> Result<(), FluxError> {
        let patch = DaemonConfigPatch {
            expected_revision,
            values: values.into_iter().collect::<BTreeMap<_, _>>(),
        };
        self.unit(DAEMON_CONFIG_PATCH, encode(&patch, "config patch")?)
            .await
    }

    // daemon.rss.*

    pub async fn refresh_rss_source(&self, source_id: String) -> Result<(), FluxError> {
        self.unit(DAEMON_RSS_REFRESH_SOURCE, json!({ "sourceId": source_id }))
            .await
    }

    /// `daemon.rss.updateSource` 要求完整订阅：读最新列表，仅改 `enabled` 再整体写回。
    pub async fn set_rss_source_enabled(
        &self,
        source_id: String,
        enabled: bool,
    ) -> Result<(), FluxError> {
        let caller = self.ready_caller().await?;
        let listed = caller.call(DAEMON_RSS_LIST_SOURCES, None).await?;
        let mut source = decode::<Vec<RssSourceDto>>(listed, "rss sources")?
            .into_iter()
            .find(|source| source.source_id == source_id)
            .ok_or_else(|| {
                FluxError::rpc(
                    crate::error::ErrorCodeDto::NotFound,
                    format!("rss source {source_id} not found"),
                )
            })?;
        source.enabled = enabled;
        caller
            .call(
                DAEMON_RSS_UPDATE_SOURCE,
                Some(encode(&source, "rss source")?),
            )
            .await
            .map(drop)
    }
}

#[uniffi::export]
impl HostSession {
    /// 结束会话：停止驱动与重连，`next_signal` 随后返回 `None`。本地主机保持运行
    /// （停止它用 `FluxCore::shutdown_local`）。
    pub fn disconnect(&self) {
        self.cancel.cancel();
    }

    /// 新建下载表单的分类目录预测：按已连接主机当前分类路由，返回 `file_name`（不含 `.` 时
    /// 回退 `url` 路径末段）命中的分类保存目录；无路由 / 无命中返回 `None`。与建任务时
    /// 的路由同一套匹配语义（agent `route_save_dir`）。
    pub fn category_save_dir(&self, file_name: String, url: String) -> Option<String> {
        category_save_dir_for(self.routes.borrow().as_ref(), &file_name, &url)
    }

    /// 批量导入（GPUI 同一套语义，见 `import.rs`）：按 256 条分块调用 `daemon.task.createMany`，
    /// 结果不确定时同一 `batchId` 退避重发，可取消。立即返回句柄；经句柄拉取进度与最终汇总。
    /// 本机主机与远端 `/rpc` 走同一路径。丢弃句柄不会取消导入。
    pub fn start_import(&self, batches: Vec<ImportBatchDto>) -> Arc<ImportHandle> {
        let route = self.routes.borrow().clone();
        crate::import::start(
            &self.runtime,
            Arc::new(self.chunk_sender()),
            batches,
            route.as_ref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::HostSession;
    use crate::dto::{CreateTaskRequestDto, SelectionOutcomeDto};
    use crate::error::{ErrorCodeDto, FluxError};
    use crate::testkit::{LinkHandle, Recorder, session_with_recorder};

    #[test]
    fn generic_call_rejects_agent_only_bt_guard_methods_but_allows_ui_ones() {
        use fluxdown_protocol::method;
        for blocked in [
            method::DAEMON_BT_GUARD_CLOUD_APPLY,
            method::DAEMON_BT_GUARD_EVIDENCE_DRAIN,
        ] {
            assert!(super::prepare_call(blocked, None).is_err(), "{blocked}");
        }
        for allowed in [
            method::DAEMON_BT_GUARD_STATE,
            method::DAEMON_BT_GUARD_ENTRIES_LIST,
            method::DAEMON_BT_GUARD_ENTRIES_ADD,
            method::DAEMON_BT_GUARD_ENTRIES_REMOVE,
            method::DAEMON_BT_GUARD_EVENTS_LIST,
            method::DAEMON_BT_GUARD_EXPORT,
            method::DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
        ] {
            assert!(super::prepare_call(allowed, None).is_ok(), "{allowed}");
        }
    }

    fn request(url: &str) -> CreateTaskRequestDto {
        CreateTaskRequestDto {
            url: url.to_owned(),
            file_name: String::new(),
            save_dir: String::new(),
            segments: 0,
            queue_id: "later".to_owned(),
            start_paused: true,
            cookies: String::new(),
            referrer: String::new(),
            user_agent: String::new(),
            proxy_url: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            headers: std::collections::HashMap::new(),
            http_user: "u".to_owned(),
            http_password: "p".to_owned(),
            save_site_auth: true,
            unattended: false,
            offered_save_dir: None,
        }
    }

    fn route() -> fluxdown_protocol::CategoryRouteDto {
        fluxdown_protocol::CategoryRouteDto {
            rules: vec![fluxdown_protocol::CategoryRuleDto {
                match_mode: "extension".to_owned(),
                extensions: vec!["jpg".to_owned()],
                regex_pattern: String::new(),
                url_pattern: String::new(),
                save_dir: "/pictures".to_owned(),
            }],
            other_dir: String::new(),
        }
    }

    fn routed(save_dir: &str, offered: Option<&str>) -> fluxdown_protocol::DaemonCreateTaskParams {
        let mut req = request("https://example.com/a.jpg");
        req.save_dir = save_dir.to_owned();
        req.offered_save_dir = offered.map(str::to_owned);
        super::routed_create_params(req, Some(&route()))
    }

    #[test]
    fn routed_create_params_routes_only_when_form_follows_system_dirs() {
        // 表单留空 / 保持初始目录 / 预填的分类目录：以初始目录为基准目录并带路由。
        for submitted in ["", "/default", " /pictures "] {
            let params = routed(submitted, Some("/default"));
            assert_eq!(params.request.save_dir, "/default", "{submitted:?}");
            assert_eq!(params.category_route, Some(route()), "{submitted:?}");
        }
        // 用户改选了别的目录：原样提交、不路由。
        let chosen = routed("/elsewhere", Some("/default"));
        assert_eq!(chosen.request.save_dir, "/elsewhere");
        assert_eq!(chosen.category_route, None);
    }

    #[test]
    fn routed_create_params_never_routes_without_offered_dir_or_route() {
        let plain = routed("", None);
        assert_eq!(plain.request.save_dir, "");
        assert_eq!(plain.category_route, None);

        let mut req = request("https://example.com/a.jpg");
        req.offered_save_dir = Some("/default".to_owned());
        let no_route = super::routed_create_params(req, None);
        assert_eq!(no_route.request.save_dir, "");
        assert_eq!(no_route.category_route, None);
    }

    #[test]
    fn category_save_dir_follows_the_route_and_is_none_without_one() {
        let route = route();
        assert_eq!(
            super::category_save_dir_for(Some(&route), "photo.JPG", "").as_deref(),
            Some("/pictures")
        );
        // 文件名不含 `.` 时回退 URL 路径末段。
        assert_eq!(
            super::category_save_dir_for(Some(&route), "", "https://x.test/a/b.jpg?x=1").as_deref(),
            Some("/pictures")
        );
        assert_eq!(
            super::category_save_dir_for(Some(&route), "setup.exe", ""),
            None
        );
        assert_eq!(super::category_save_dir_for(None, "photo.jpg", ""), None);
    }

    #[tokio::test]
    async fn session_without_custom_dirs_has_no_category_save_dir() {
        let (session, _recorder, _link) = ready().await;
        assert_eq!(
            session.category_save_dir("photo.jpg".to_owned(), String::new()),
            None
        );
    }

    #[test]
    fn routed_create_params_forwards_unattended() {
        let mut req = request("https://example.com/a.jpg");
        req.unattended = true;
        assert!(super::routed_create_params(req, None).unattended);
        assert!(!routed("", None).unattended);
    }

    async fn ready() -> (Arc<HostSession>, Arc<Recorder>, LinkHandle) {
        let (session, link) = session_with_recorder().await;
        let recorder = Arc::clone(&link.recorder);
        (session, recorder, link)
    }

    #[tokio::test]
    async fn task_commands_map_to_daemon_methods_and_params() {
        let (session, recorder, _link) = ready().await;
        recorder.reply("daemon.task.create", Ok(json!({ "taskId": "new-1" })));

        let id = session
            .create_task(request("https://example.com/a.zip"))
            .await
            .expect("create");
        assert_eq!(id, "new-1");
        session.pause("t".to_owned()).await.expect("pause");
        session.resume("t".to_owned()).await.expect("resume");
        session.delete("t".to_owned(), true).await.expect("delete");
        session
            .pause_many(vec!["a".to_owned(), "b".to_owned()])
            .await
            .expect("pause_many");
        session
            .resume_many(vec!["a".to_owned()])
            .await
            .expect("resume_many");
        session
            .delete_many(vec!["a".to_owned()], false)
            .await
            .expect("delete_many");
        session.pause_all().await.expect("pause_all");
        session.resume_all().await.expect("resume_all");
        session
            .rename("t".to_owned(), "n.bin".to_owned())
            .await
            .expect("rename");
        session
            .change_url("t".to_owned(), "https://x/y".to_owned())
            .await
            .expect("change_url");
        session.rescan().await.expect("rescan");
        session
            .move_to_queue("t".to_owned(), String::new())
            .await
            .expect("move");
        session.boost("t".to_owned()).await.expect("boost");

        let calls = recorder.calls();
        let create = &calls[0];
        assert_eq!(create.0, "daemon.task.create");
        assert_eq!(create.1["request"]["url"], "https://example.com/a.zip");
        assert_eq!(create.1["request"]["queueId"], "later");
        assert_eq!(create.1["request"]["startPaused"], true);
        assert_eq!(create.1["request"]["httpUser"], "u");
        assert_eq!(create.1["request"]["saveSiteAuth"], true);
        assert!(
            create.1["request"]
                .get("headers")
                .is_none_or(Value::is_null)
        );
        assert_eq!(create.1["unattended"], false);

        let rest: Vec<(&str, &Value)> = calls[1..]
            .iter()
            .map(|(method, params)| (method.as_str(), params))
            .collect();
        assert_eq!(rest[0], ("daemon.task.pause", &json!({ "taskId": "t" })));
        assert_eq!(rest[1], ("daemon.task.resume", &json!({ "taskId": "t" })));
        assert_eq!(
            rest[2],
            (
                "daemon.task.delete",
                &json!({ "taskId": "t", "deleteFiles": true })
            )
        );
        assert_eq!(
            rest[3],
            ("daemon.task.pauseMany", &json!({ "taskIds": ["a", "b"] }))
        );
        assert_eq!(
            rest[4],
            ("daemon.task.resumeMany", &json!({ "taskIds": ["a"] }))
        );
        assert_eq!(
            rest[5],
            (
                "daemon.task.deleteMany",
                &json!({ "taskIds": ["a"], "deleteFiles": false })
            )
        );
        assert_eq!(rest[6].0, "daemon.task.pauseAll");
        assert_eq!(rest[7].0, "daemon.task.resumeAll");
        assert_eq!(
            rest[8],
            (
                "daemon.task.rename",
                &json!({ "taskId": "t", "fileName": "n.bin" })
            )
        );
        assert_eq!(
            rest[9],
            (
                "daemon.task.changeUrl",
                &json!({ "taskId": "t", "url": "https://x/y" })
            )
        );
        assert_eq!(rest[10].0, "daemon.task.rescan");
        assert_eq!(
            rest[11],
            (
                "daemon.queue.moveTask",
                &json!({ "taskId": "t", "queueId": "" })
            )
        );
        assert_eq!(rest[12], ("daemon.queue.boost", &json!({ "taskId": "t" })));
    }

    #[tokio::test]
    async fn selection_and_config_commands_use_wire_shapes() {
        let (session, recorder, _link) = ready().await;
        session
            .resolve_selection(
                "r1".to_owned(),
                SelectionOutcomeDto::Bt {
                    indices: vec![0, 2],
                },
            )
            .await
            .expect("resolve");
        session
            .resolve_selection("r2".to_owned(), SelectionOutcomeDto::Cancelled)
            .await
            .expect("cancel");
        session
            .patch_config(
                7,
                [("max_concurrent".to_owned(), "3".to_owned())]
                    .into_iter()
                    .collect(),
            )
            .await
            .expect("patch");
        let calls = recorder.calls();
        assert_eq!(
            calls[0].1,
            json!({ "requestId": "r1", "outcome": { "kind": "bt", "indices": [0, 2] } })
        );
        assert_eq!(
            calls[1].1,
            json!({ "requestId": "r2", "outcome": { "kind": "cancelled" } })
        );
        assert_eq!(calls[2].0, "daemon.config.patch");
        assert_eq!(
            calls[2].1,
            json!({ "expectedRevision": 7, "values": { "max_concurrent": "3" } })
        );
    }

    #[tokio::test]
    async fn rss_enable_rewrites_the_full_source_with_flag_flipped() {
        let (session, recorder, _link) = ready().await;
        recorder.reply(
            "daemon.rss.listSources",
            Ok(json!([
                { "sourceId": "s1", "url": "https://feed/1", "name": "One", "enabled": true },
                { "sourceId": "s2", "url": "https://feed/2", "name": "Two", "enabled": true }
            ])),
        );
        session
            .set_rss_source_enabled("s2".to_owned(), false)
            .await
            .expect("toggle");
        let calls = recorder.calls();
        assert_eq!(calls[0].0, "daemon.rss.listSources");
        assert_eq!(calls[1].0, "daemon.rss.updateSource");
        assert_eq!(calls[1].1["sourceId"], "s2");
        assert_eq!(calls[1].1["enabled"], false);
        assert_eq!(calls[1].1["name"], "Two");

        let missing = session
            .set_rss_source_enabled("nope".to_owned(), true)
            .await
            .expect_err("unknown source");
        assert!(matches!(
            missing,
            FluxError::Rpc {
                code: ErrorCodeDto::NotFound,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn generic_call_passes_params_and_returns_result_json() {
        let (session, recorder, _link) = ready().await;
        recorder.reply(
            "daemon.rss.listSources",
            Ok(json!([{ "sourceId": "s1", "enabled": true }])),
        );
        recorder.reply("agent.gateway.get", Ok(Value::Null));

        let listed = session
            .call("daemon.rss.listSources".to_owned(), None)
            .await
            .expect("list");
        assert_eq!(
            serde_json::from_str::<Value>(&listed).expect("json"),
            json!([{ "sourceId": "s1", "enabled": true }])
        );
        let empty = session
            .call(
                "agent.gateway.get".to_owned(),
                Some(r#"{"verbose":true}"#.to_owned()),
            )
            .await
            .expect("agent call");
        assert_eq!(empty, "null");
        let null_params = session
            .call("daemon.queue.list".to_owned(), Some("null".to_owned()))
            .await
            .expect("null params");
        assert_eq!(null_params, r#"{"ok":true}"#);

        let calls = recorder.calls();
        assert_eq!(calls[0], ("daemon.rss.listSources".to_owned(), Value::Null));
        assert_eq!(
            calls[1],
            ("agent.gateway.get".to_owned(), json!({ "verbose": true }))
        );
        assert_eq!(calls[2], ("daemon.queue.list".to_owned(), Value::Null));
    }

    #[tokio::test]
    async fn generic_call_validates_before_touching_the_connection() {
        let (session, recorder, _link) = ready().await;
        for method in [
            "system.snapshot",
            "system.taskPage",
            "system.shutdown",
            "rss.listSources",
            "daemon.",
            "agent.",
            "",
            "xdaemon.task.list",
        ] {
            let error = session
                .call(method.to_owned(), None)
                .await
                .expect_err("method outside daemon./agent. is rejected");
            assert!(
                matches!(
                    error,
                    FluxError::Rpc {
                        code: ErrorCodeDto::InvalidArgument,
                        ..
                    }
                ),
                "{method:?}: {error:?}"
            );
        }
        let bad_json = session
            .call("daemon.task.get".to_owned(), Some("{not json".to_owned()))
            .await
            .expect_err("invalid params JSON");
        assert!(matches!(
            bad_json,
            FluxError::Rpc {
                code: ErrorCodeDto::InvalidArgument,
                ..
            }
        ));
        assert!(recorder.calls().is_empty(), "nothing reached the host");
    }

    #[tokio::test]
    async fn generic_call_surfaces_rpc_errors_and_closed_sessions() {
        let (session, recorder, _link) = ready().await;
        recorder.reply(
            "daemon.rss.createSource",
            Err(FluxError::Rpc {
                code: ErrorCodeDto::InvalidArgument,
                reason: None,
                retryable: false,
                detail: "bad url".to_owned(),
            }),
        );
        let error = session
            .call(
                "daemon.rss.createSource".to_owned(),
                Some(r#"{"url":"nope"}"#.to_owned()),
            )
            .await
            .expect_err("host error");
        assert!(matches!(
            error,
            FluxError::Rpc {
                code: ErrorCodeDto::InvalidArgument,
                ..
            }
        ));
        session.disconnect();
        assert!(matches!(
            session.call("daemon.task.list".to_owned(), None).await,
            Err(FluxError::Closed)
        ));
    }

    #[tokio::test]
    async fn rpc_errors_surface_as_flux_error_and_disconnect_closes() {
        let (session, recorder, _link) = ready().await;
        recorder.reply(
            "daemon.task.pause",
            Err(FluxError::Rpc {
                code: ErrorCodeDto::NotFound,
                reason: None,
                retryable: false,
                detail: "no such task".to_owned(),
            }),
        );
        let error = session.pause("x".to_owned()).await.expect_err("fails");
        assert!(matches!(
            error,
            FluxError::Rpc {
                code: ErrorCodeDto::NotFound,
                ..
            }
        ));
        session.disconnect();
        assert!(matches!(
            session.pause("x".to_owned()).await,
            Err(FluxError::Closed)
        ));
        assert!(session.next_signal().await.is_none());
    }

    #[tokio::test]
    async fn start_import_calls_create_many_through_the_session_with_item_wire() {
        use crate::import::{ImportBatchDto, ImportItemDto, ImportUpdateDto};

        let (session, recorder, _link) = ready().await;
        recorder.reply(
            "daemon.task.createMany",
            Ok(json!({ "results": [
                { "taskId": "t1" },
                { "error": { "code": "invalidArgument", "retryable": false, "field": "url" } },
            ] })),
        );
        let handle = session.start_import(vec![ImportBatchDto {
            common: request(""),
            items: vec![
                ImportItemDto {
                    url: "https://x/1".to_owned(),
                    file_name: "one.bin".to_owned(),
                    checksum: "sha256=ab".to_owned(),
                },
                ImportItemDto {
                    url: "bad".to_owned(),
                    ..ImportItemDto::default()
                },
            ],
        }]);
        let summary = loop {
            if let ImportUpdateDto::Finished { summary } =
                handle.next_update().await.expect("update before Finished")
            {
                break summary;
            }
        };
        assert_eq!((summary.succeeded, summary.failed), (1, 1));
        let error = summary.first_error.expect("first error");
        assert_eq!(error.code, ErrorCodeDto::InvalidArgument);
        assert_eq!(error.message, "url");
        assert_eq!(summary.unfinished, [1]);

        let calls = recorder.calls();
        assert_eq!(calls.len(), 1);
        let (method, params) = &calls[0];
        assert_eq!(method, "daemon.task.createMany");
        assert!(params["batchId"].as_str().is_some_and(|id| !id.is_empty()));
        assert_eq!(params["common"]["url"], "");
        assert_eq!(params["common"]["queueId"], "later");
        assert_eq!(params["common"]["startPaused"], true);
        assert_eq!(params["items"][0]["url"], "https://x/1");
        assert_eq!(params["items"][0]["fileName"], "one.bin");
        assert_eq!(params["items"][0]["checksum"], "sha256=ab");
        assert_eq!(params["items"][1]["url"], "bad");
    }
}
