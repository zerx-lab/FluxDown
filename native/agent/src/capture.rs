//! 浏览器/NMH 捕获确认事务：内存有界、单次消费且不持久化敏感请求上下文。
//!
//! Cookie / 请求头 / 请求体只留在事务里；官方 UI 只拿到 [`PendingCaptureDto`] 摘要，
//! 确认时提交表单产出的 [`CreateTaskRequest`]，由 [`merge_confirmed_request`] 以捕获原请求为底合并。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use base64::Engine;
use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, ApplicationErrorCode, CategoryRouteDto, CreateGroupRequest,
    CreateGroupResponse, CreateTaskRequest, DaemonCreateTaskParams, DownloadRequest,
    PendingCaptureDto, ResolvePreviewRequest, ResolvePreviewResponse, RpcErrorData,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::daemon_client::DaemonClient;
use crate::event_hub::AgentEventHub;
use crate::shell::ShellState;

/// 不小于 NMH 单批上限（1000），否则合法的整批多选会被确认队列整体拒绝。
const CAPTURE_CAPACITY: usize = 1000;
/// 已结束事务结局的保留数量；超出时丢弃最旧记录，仅供扩展轮询 `capture_status`。
const CAPTURE_OUTCOME_CAPACITY: usize = 1024;

/// 「免打扰下载」：外部接管请求不弹确认，直接按默认设置建任务（设置页 `download.rs`）。
pub(crate) const SILENT_DOWNLOAD_PREF: &str = "download.silent_download";
/// 免打扰子开关：静默建的任务跳过 BT 文件 / HLS·DASH 画质 / 插件变体二次选择。设备本地偏好。
pub(crate) const SILENT_SKIP_SELECTION_PREF: &str = "download.silent_skip_selection";
/// 「跟随上次保存位置」与其记录（官方 UI 新建下载写入）。
const REMEMBER_LAST_SAVE_DIR_PREF: &str = "download.remember_last_save_dir";
const LAST_SAVE_DIR_PREF: &str = "download.last_save_dir";

/// 确认事务的终态，供浏览器扩展判断被拒 / 失败后的回退。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureOutcome {
    Accepted,
    Rejected,
    Failed,
}

impl CaptureOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
}

/// 捕获来源，决定是否需要用户确认。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureOrigin {
    /// 本机用户显式交来的链接（系统打开链接、拖入）：直接建任务。
    Direct,
    /// 浏览器扩展 / 用户脚本 / NMH 等外部接管：按「免打扰下载」偏好静默或确认。
    External,
    /// 需要用户过目的探测结果（剪贴板监听）：恒入确认队列。
    Prompt,
}

/// 本机 `.torrent` 建任务的附加选项；默认 = 静默全选、daemon 默认队列、立即开始。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TorrentCreateOptions {
    /// true：全选文件不弹选择框；false：由 daemon 发 BT 文件选择请求。
    pub unattended: bool,
    /// 目标队列（`None` = daemon 默认队列）。
    pub queue_id: Option<String>,
    pub start_paused: bool,
}

/// 外部接管的静默策略；由偏好现算，改动即生效。
#[derive(Clone, Debug, Eq, PartialEq)]
struct ExternalPolicy {
    silent: bool,
    skip_selection: bool,
    /// 「跟随上次保存位置」开启且有记录时的目录；捕获方未指定目录时使用。
    remembered_save_dir: Option<String>,
}

impl ExternalPolicy {
    fn from_preferences(preferences: &BTreeMap<String, Value>) -> Self {
        let flag = |key: &str| {
            preferences
                .get(key)
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        let remembered_save_dir = flag(REMEMBER_LAST_SAVE_DIR_PREF)
            .then(|| preferences.get(LAST_SAVE_DIR_PREF).and_then(Value::as_str))
            .flatten()
            .map(str::trim)
            .filter(|dir| !dir.is_empty())
            .map(str::to_owned);
        Self {
            silent: flag(SILENT_DOWNLOAD_PREF),
            skip_selection: flag(SILENT_SKIP_SELECTION_PREF),
            remembered_save_dir,
        }
    }
}

struct CaptureTransaction {
    public: PendingCaptureDto,
    request: DownloadRequest,
    /// 捕获方未指定保存目录时的分类路由（入队时按当时的偏好算定）。
    category: Option<CategoryPlan>,
    /// 按需分配；owned guard 跨 daemon 调用存活，避免重入或旧 resolve 重复建任务。
    group_creation: Option<Arc<Mutex<()>>>,
}

/// 捕获方未指定保存目录时交给 daemon 的分类路由：引擎按初始名定初始目录，下载完成、最终
/// 文件名确定后（插件改名、按响应推断、HLS 转封装）再复核，见引擎 `category_route.rs`。
#[derive(Clone, Debug)]
struct CategoryPlan {
    route: CategoryRouteDto,
    /// 按捕获名命中的分类目录（确认表单据此预填）；未命中为空串。
    prefill: String,
}

impl CategoryPlan {
    fn of(request: &DownloadRequest, route: Option<&CategoryRouteDto>) -> Option<Self> {
        let route = route.filter(|_| request.save_dir.trim().is_empty())?;
        let prefill = crate::category_dir::route_save_dir(route, &request.filename, &request.url)
            .unwrap_or_default()
            .to_owned();
        Some(Self {
            route: route.clone(),
            prefill,
        })
    }

    /// 确认表单提交的目录仍是系统给出的（空 / 表单默认目录 `offered` / 任一分类目录）时
    /// 继续按分类路由；用户改选了别的目录即以用户选择为准。认任一分类目录而不只是本条的
    /// 预填：多条捕获并入同一张表单时，目录框只预填第一条命中的分类目录。
    fn follows(&self, submitted: &str, offered: &str) -> bool {
        self.route.follows(submitted, offered)
    }

    /// 交给 daemon 路由：基准目录（路由未命中时的落点）= 「跟随上次保存位置」记录，否则
    /// daemon 默认。
    fn apply(self, spec: &mut CaptureCreate, base: Option<&str>) {
        spec.request.save_dir = base.unwrap_or_default().to_owned();
        spec.category_route = Some(self.route);
    }
}

/// 官方新建下载表单在捕获方未给目录时显示的默认目录（`crates/downloads`
/// `build_new_download_context` 同序）：跟随上次 → 设置项 `default_save_dir` → daemon 运行时目录。
fn offered_save_dir(snapshot: &AgentSnapshot, policy: &ExternalPolicy) -> String {
    if let Some(dir) = &policy.remembered_save_dir {
        return dir.clone();
    }
    match snapshot
        .daemon
        .config
        .values
        .get("default_save_dir")
        .map(|dir| dir.trim())
    {
        Some(dir) if !dir.is_empty() => dir.to_owned(),
        _ => snapshot.daemon.runtime_stats.save_dir.trim().to_owned(),
    }
}

pub struct CaptureService {
    daemon: Arc<DaemonClient>,
    events: AgentEventHub,
    pending: Mutex<VecDeque<CaptureTransaction>>,
    /// 静默建成单个任务且没有就绪 UI 时按需拉起官方 UI 承载进度 / 完成提示（待确认捕获的
    /// 拉起由外壳控制循环按快照电平触发，见 `shell::prompt_launch`）。
    shell: Arc<ShellState>,
    /// 最近的事务结局（最新在队尾）；std 锁只在内存操作期间短持有，不跨 await。
    outcomes: std::sync::Mutex<VecDeque<(String, CaptureOutcome)>>,
}

impl CaptureService {
    #[must_use]
    pub fn new(daemon: Arc<DaemonClient>, events: AgentEventHub, shell: Arc<ShellState>) -> Self {
        Self {
            daemon,
            events,
            pending: Mutex::new(VecDeque::new()),
            shell,
            outcomes: std::sync::Mutex::new(VecDeque::new()),
        }
    }

    fn record_outcome(&self, id: &str, outcome: CaptureOutcome) {
        let mut outcomes = self
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if outcomes.len() >= CAPTURE_OUTCOME_CAPACITY {
            outcomes.pop_front();
        }
        outcomes.push_back((id.to_owned(), outcome));
    }

    /// 按事务 ID 查询状态：待确认为 `pending`，已结束取最新结局，查不到为 `unknown`。
    pub async fn statuses(&self, ids: &[String]) -> serde_json::Map<String, Value> {
        let pending = self.pending.lock().await;
        let outcomes = self
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ids.iter()
            .map(|id| {
                let status = if pending
                    .iter()
                    .any(|transaction| transaction.public.transaction_id == *id)
                {
                    "pending"
                } else {
                    outcomes
                        .iter()
                        .rev()
                        .find(|(outcome_id, _)| outcome_id == id)
                        .map_or("unknown", |(_, outcome)| outcome.as_str())
                };
                (id.clone(), Value::from(status))
            })
            .collect()
    }

    /// 按来源分流：`Direct` 与开启免打扰的 `External` 直接提交 daemon，其余排入确认队列（外壳
    /// 控制循环据快照在没有 UI 时拉起官方 UI）。批量请求（换行连接的多 URL）先拆成逐条请求。
    ///
    /// 保存目录优先级：捕获方指定 > 分类目录 > （仅静默）跟随上次 > daemon 默认（原生移动端 `SilentCapture` 同序）。
    /// 捕获方未指定目录时分类目录由 daemon 按初始名与下载完成时的最终文件名确定（[`CategoryPlan`]）；
    /// 确认路径把按捕获名命中的分类目录写进待确认摘要，官方表单据此预填。
    pub async fn submit(
        &self,
        request: DownloadRequest,
        origin: CaptureOrigin,
    ) -> Result<Value, CaptureError> {
        self.submit_many(vec![request], origin).await
    }

    /// 多个请求作为一个整体按 [`submit`](Self::submit) 的规则分流：确认队列容量按整批检查，
    /// 放不下时整批拒绝且不留下任何半入队的事务，入队后只发布一次队列变更。
    pub async fn submit_many(
        &self,
        requests: Vec<DownloadRequest>,
        origin: CaptureOrigin,
    ) -> Result<Value, CaptureError> {
        let requests = requests
            .into_iter()
            .flat_map(split_batch)
            .collect::<Vec<_>>();
        let (policy, route) = self.events.inspect(|snapshot| {
            let preferences = &snapshot.preferences.values;
            (
                ExternalPolicy::from_preferences(preferences),
                crate::category_dir::category_route(preferences),
            )
        });
        if requests.is_empty() {
            return Ok(json!({ "taskIds": [] }));
        }
        if origin == CaptureOrigin::Direct {
            return self.create_all(requests, None, true, None).await;
        }
        if origin == CaptureOrigin::External && policy.silent {
            let save_dir = policy.remembered_save_dir.as_deref();
            return self
                .create_all(requests, save_dir, policy.skip_selection, route.as_ref())
                .await;
        }
        self.enqueue(requests, route.as_ref()).await
    }

    /// 直接建任务（不经确认）。建成的任务随后以 [`AgentEvent::CaptureTasksStarted`] 通知官方
    /// UI；单任务按两个窗口开关登记冷启动意图，批量请求不自动拉起下载提示窗口。
    async fn create_all(
        &self,
        requests: Vec<DownloadRequest>,
        fallback_save_dir: Option<&str>,
        unattended: bool,
        route: Option<&CategoryRouteDto>,
    ) -> Result<Value, CaptureError> {
        let single_request = requests.len() == 1;
        let mut task_ids = Vec::with_capacity(requests.len());
        let mut started = Vec::with_capacity(requests.len());
        let mut outcome = Ok(());
        for request in requests {
            let plan = CategoryPlan::of(&request, route);
            let mut create = captured_create_request(request);
            match plan {
                Some(plan) => plan.apply(&mut create, fallback_save_dir),
                None => {
                    if let Some(dir) = fallback_save_dir {
                        fill_if_blank(&mut create.request.save_dir, dir.to_owned());
                    }
                }
            }
            let created = match self.create(create, None, unattended).await {
                Ok(created) => created,
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            };
            let task_id = created.get("taskId").cloned().unwrap_or(Value::Null);
            if let Some(id) = task_id.as_str() {
                started.push(id.to_owned());
            }
            task_ids.push(task_id);
        }
        // 批量只成功首项后失败，也不能被 UI 误认为单任务用户意图。
        if !started.is_empty() && (single_request || started.len() > 1) {
            if single_request && let [task_id] = started.as_slice() {
                self.shell.launch_for_task_windows(task_id);
            }
            self.events
                .publish(AgentEvent::CaptureTasksStarted(started));
        }
        outcome.map(|()| json!({ "taskIds": task_ids }))
    }

    /// 插件页面脚本调用开始的任务（`daemon.plugin.pageInvoke` 应答）按单任务静默捕获同一规则
    /// 登记：恰好一个任务开始时才拉起 / 通知官方 UI 弹提示窗口；新建发 `CaptureTasksStarted`，
    /// 恢复已有任务发 `CaptureTasksResumed`。一次开始多个与批量捕获一样不弹。
    pub fn announce_page_started(&self, created: &[String], resumed: &[String]) {
        let (task_id, event) = match (created, resumed) {
            ([task_id], []) => (
                task_id,
                AgentEvent::CaptureTasksStarted(vec![task_id.clone()]),
            ),
            ([], [task_id]) => (
                task_id,
                AgentEvent::CaptureTasksResumed(vec![task_id.clone()]),
            ),
            _ => return,
        };
        self.shell.launch_for_task_windows(task_id);
        self.events.publish(event);
    }

    async fn enqueue(
        &self,
        requests: Vec<DownloadRequest>,
        route: Option<&CategoryRouteDto>,
    ) -> Result<Value, CaptureError> {
        let transaction_ids = {
            let mut pending = self.pending.lock().await;
            if pending.len() + requests.len() > CAPTURE_CAPACITY {
                return Err(CaptureError::Full);
            }
            let mut transaction_ids = Vec::with_capacity(requests.len());
            for request in requests {
                let category = CategoryPlan::of(&request, route);
                let mut public = pending_capture_dto(&request);
                if let Some(plan) = &category {
                    public.save_dir.clone_from(&plan.prefill);
                }
                transaction_ids.push(public.transaction_id.clone());
                pending.push_back(CaptureTransaction {
                    public,
                    request,
                    category,
                    group_creation: None,
                });
            }
            transaction_ids
        };
        self.publish().await;
        Ok(json!({ "transactionIds": transaction_ids }))
    }

    /// 用户选定的本机 `.torrent`（已上传为 daemon blob）直接建任务；`options.unattended`
    /// 为 true 时全选文件不弹选择框，否则由 daemon 发 BT 文件选择请求。保存目录取
    /// `request.save_dir`（空 = daemon 默认），队列与暂停取 `options`。
    pub async fn create_torrent(
        &self,
        request: DownloadRequest,
        torrent_blob_id: String,
        options: TorrentCreateOptions,
    ) -> Result<Value, CaptureError> {
        let mut spec = captured_create_request(request);
        if let Some(queue_id) = options.queue_id {
            spec.request.queue_id = queue_id;
        }
        spec.request.start_paused = options.start_paused;
        self.create(spec, Some(torrent_blob_id), options.unattended)
            .await
    }

    pub async fn list(&self) -> Vec<PendingCaptureDto> {
        self.pending
            .lock()
            .await
            .iter()
            .map(|transaction| transaction.public.clone())
            .collect()
    }

    /// 克隆捕获上下文只读预解析；UI 不会拿到 Cookie、请求头值或请求体。
    /// group 协议无法表达非 GET / body / 音频轨，故这些捕获返回空清单走旧确认路径。
    pub async fn preview(
        &self,
        transaction_id: &str,
        confirmed: CreateTaskRequest,
    ) -> Result<ResolvePreviewResponse, CaptureError> {
        let captured = self
            .pending
            .lock()
            .await
            .iter()
            .find(|transaction| transaction.public.transaction_id == transaction_id)
            .ok_or(CaptureError::NotFound)?
            .request
            .clone();
        let request = merge_confirmed_request(captured, confirmed).request;
        if !supports_manifest(&request) {
            return Ok(ResolvePreviewResponse {
                name: String::new(),
                source_url: request.url,
                error: String::new(),
                plugin_id: String::new(),
                items: Vec::new(),
                archive_metadata: None,
            });
        }
        let source_url = request.url.clone();
        let mut preview: ResolvePreviewResponse = self
            .daemon
            .call(
                fluxdown_protocol::method::DAEMON_GROUP_RESOLVE_PREVIEW,
                Some(into_preview_request(request)),
            )
            .await
            .map_err(CaptureError::Daemon)?;
        preview.source_url = source_url;
        Ok(preview)
    }

    /// 最终选择建组：无效条目不占用事务；daemon 失败可重试，成功后单次消费。
    ///
    /// 创建工作独立存活于调用者：UI 取消等待不能解除尚在运行的 single-flight 锁。
    /// 同一事务创建期间，包括旧 `resolve` 在内的再次确认/拒绝都返回 Conflict。
    pub async fn create_group(
        self: &Arc<Self>,
        transaction_id: &str,
        request: CreateGroupRequest,
        context: CreateTaskRequest,
    ) -> Result<CreateGroupResponse, CaptureError> {
        validate_group_items(&request)?;
        let offered = self.events.inspect(|snapshot| {
            offered_save_dir(
                snapshot,
                &ExternalPolicy::from_preferences(&snapshot.preferences.values),
            )
        });
        let (request, claim) = {
            let mut pending = self.pending.lock().await;
            let transaction = pending
                .iter_mut()
                .find(|transaction| transaction.public.transaction_id == transaction_id)
                .ok_or(CaptureError::NotFound)?;
            let mut request = merge_group_request(transaction.request.clone(), request, &context)?;
            // 任务组落在默认目录：预填的是按捕获名（多为分享页地址）算出的分类目录，不代表组内
            // 文件；用户未改选目录时换回表单默认目录，组内文件也不再逐个按后缀分类。
            request.category_route = None;
            if let Some(plan) = &transaction.category
                && !offered.is_empty()
                && plan.route.is_category_dir(request.save_dir.trim())
            {
                request.save_dir = offered;
                request.category_route = Some(plan.route.clone());
            }
            let claim = transaction
                .group_creation
                .get_or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
                .try_lock_owned()
                .map_err(|_| CaptureError::Busy)?;
            (request, claim)
        };
        let save_auth = (context.save_site_auth && !context.http_user.is_empty()).then(|| {
            fluxdown_protocol::SiteAuthSaveRequest {
                site: request.source_url.clone(),
                user: context.http_user,
                pass: context.http_password,
            }
        });
        let service = Arc::clone(self);
        let transaction_id = transaction_id.to_owned();
        tokio::spawn(async move {
            let _claim = claim;
            let created: CreateGroupResponse = service
                .daemon
                .call(fluxdown_protocol::method::DAEMON_GROUP_CREATE, Some(request))
                .await
                .map_err(CaptureError::Daemon)?;
            {
                let mut pending = service.pending.lock().await;
                let index = pending
                    .iter()
                    .position(|transaction| transaction.public.transaction_id == transaction_id)
                    .ok_or(CaptureError::NotFound)?;
                pending.remove(index).ok_or(CaptureError::NotFound)?;
                service.record_outcome(&transaction_id, CaptureOutcome::Accepted);
            }
            service.publish().await;
            if let Some(save_auth) = save_auth
                && let Err(error) = service
                    .daemon
                    .call::<_, fluxdown_protocol::SiteAuthEntryDto>(
                        fluxdown_protocol::method::DAEMON_SITE_AUTH_SAVE,
                        Some(save_auth),
                    )
                    .await
            {
                // 与 TaskCreate 一样，凭据保存失败不撤销已创建的下载；不记录凭据或 URL。
                tracing::warn!(code = ?error.code, "could not save site authentication after capture group creation");
            }
            Ok(created)
        })
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "capture group creation worker failed");
            CaptureError::Daemon(RpcErrorData::new(ApplicationErrorCode::Internal, false))
        })?
    }

    /// 确认/拒绝均只消费一次；确认时 `confirmed`（官方 UI 表单结果）经
    /// [`merge_confirmed_request`] 合并进捕获原请求，`None` 按原请求建任务。
    pub async fn resolve(
        &self,
        transaction_id: &str,
        accepted: bool,
        disable_category_route: bool,
        confirmed: Option<CreateTaskRequest>,
    ) -> Result<Value, CaptureError> {
        let CaptureTransaction {
            request, category, ..
        } = {
            let mut pending = self.pending.lock().await;
            let index = pending
                .iter()
                .position(|transaction| transaction.public.transaction_id == transaction_id)
                .ok_or(CaptureError::NotFound)?;
            if pending[index]
                .group_creation
                .as_ref()
                .is_some_and(|lock| lock.try_lock().is_err())
            {
                return Err(CaptureError::Busy);
            }
            pending.remove(index).ok_or(CaptureError::NotFound)?
        };
        self.publish().await;
        if !accepted {
            self.record_outcome(transaction_id, CaptureOutcome::Rejected);
            return Ok(json!({ "accepted": false }));
        }
        let mut spec = match confirmed {
            Some(confirmed) => merge_confirmed_request(request, confirmed),
            None => captured_create_request(request),
        };
        if !disable_category_route && let Some(plan) = category {
            let (offered, remembered) = self.events.inspect(|snapshot| {
                let policy = ExternalPolicy::from_preferences(&snapshot.preferences.values);
                (
                    offered_save_dir(snapshot, &policy),
                    policy.remembered_save_dir,
                )
            });
            if plan.follows(&spec.request.save_dir, &offered) {
                plan.apply(&mut spec, remembered.as_deref());
            }
        }
        match self.create(spec, None, false).await {
            Ok(created) => {
                self.record_outcome(transaction_id, CaptureOutcome::Accepted);
                Ok(created)
            }
            Err(error) => {
                self.record_outcome(transaction_id, CaptureOutcome::Failed);
                Err(error)
            }
        }
    }

    async fn create(
        &self,
        spec: CaptureCreate,
        torrent_blob_id: Option<String>,
        unattended: bool,
    ) -> Result<Value, CaptureError> {
        self.daemon
            .call(
                fluxdown_protocol::method::DAEMON_TASK_CREATE,
                Some(DaemonCreateTaskParams {
                    request: spec.request,
                    torrent_blob_id,
                    unattended,
                    hint_file_size: spec.hint_file_size,
                    name_hint: spec.name_hint,
                    category_route: spec.category_route,
                }),
            )
            .await
            .map_err(CaptureError::Daemon)
    }

    async fn publish(&self) {
        self.events
            .publish(AgentEvent::PendingCapturesChanged(self.list().await));
    }
}

/// 捕获请求 → UI 可见摘要：只暴露头名与是否带 Cookie，不含任何值。
fn pending_capture_dto(request: &DownloadRequest) -> PendingCaptureDto {
    let mut header_names = request
        .headers
        .iter()
        .flatten()
        .map(|(name, _)| name.clone())
        .filter(|name| !name.eq_ignore_ascii_case("cookie"))
        .collect::<Vec<_>>();
    header_names.sort_by_key(|name| name.to_ascii_lowercase());
    let cookie_header = request
        .headers
        .iter()
        .flatten()
        .any(|(name, value)| name.eq_ignore_ascii_case("cookie") && !value.trim().is_empty());
    PendingCaptureDto {
        transaction_id: Uuid::new_v4().to_string(),
        url: request.url.clone(),
        file_name: request.filename.clone(),
        created_at_unix_ms: now_unix_ms(),
        file_size: request.file_size.unwrap_or(0).max(0),
        referrer: request.referrer.clone(),
        save_dir: request.save_dir.clone(),
        has_cookies: !request.cookies.trim().is_empty() || cookie_header,
        header_names,
    }
}

/// `/download/batch` 把多个 URL 以换行连接成单个请求（`fluxdown_api::takeover::parse_batch`）；
/// 建任务与确认事务都必须逐 URL 进行，否则 daemon 收到多行 URL，确认表单也会把它们拆成与事务
/// 对不上的普通链接而丢掉 Cookie / Referer / 请求头。单 URL 原样保留；多 URL 只共享
/// Cookie / Referer / 请求头 / 保存目录，文件名 / method / body / 音频轨 / 大小是单请求语义，丢弃。
fn split_batch(request: DownloadRequest) -> Vec<DownloadRequest> {
    let urls = request
        .url
        .lines()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if urls.len() <= 1 {
        return vec![request];
    }
    urls.into_iter()
        .map(|url| DownloadRequest {
            url,
            filename: String::new(),
            save_dir: request.save_dir.clone(),
            referrer: request.referrer.clone(),
            cookies: request.cookies.clone(),
            headers: request.headers.clone(),
            file_size: None,
            mime_type: None,
            method: None,
            body: None,
            audio_url: None,
        })
        .collect()
}

/// 建任务请求 + 不在 `CreateTaskRequest` 内的捕获附加信息（大小提示、参照文件名、分类路由）。
struct CaptureCreate {
    request: CreateTaskRequest,
    /// 仅 >0 才有意义：daemon 据此跳过一次性 URL 的探测请求。
    hint_file_size: Option<i64>,
    /// 表单改了名时，浏览器定下的原始名：用户名缺扩展名时引擎借它的扩展名补全。
    name_hint: Option<String>,
    /// 由 [`CategoryPlan::apply`] 设置；`None` = 保存目录即最终目录。
    category_route: Option<CategoryRouteDto>,
}

/// 按捕获原请求建任务（静默提交 / 未带表单结果的确认）；队列与分段走 daemon 默认。
fn captured_create_request(request: DownloadRequest) -> CaptureCreate {
    let hint_file_size = request.file_size.filter(|size| *size > 0);
    CaptureCreate {
        request: CreateTaskRequest {
            url: request.url,
            file_name: request.filename,
            save_dir: request.save_dir,
            segments: 0,
            cookies: request.cookies,
            referrer: request.referrer,
            proxy_url: String::new(),
            user_agent: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            headers: request.headers,
            torrent_b64: None,
            method: request.method,
            body: request.body,
            audio_url: request.audio_url,
            start_paused: false,
            http_user: String::new(),
            http_password: String::new(),
            save_site_auth: false,
        },
        hint_file_size,
        name_hint: None,
        category_route: None,
    }
}

/// 官方 UI 表单结果合并进捕获原请求（规则见 `CaptureResolveParams::request`）。
///
/// 表单看不到的请求上下文（method / body / 音频轨）与事务身份（url）恒取捕获值；
/// 表单留空的字段回退捕获值；请求头以捕获为底、表单同名覆盖，表单填了 UA 时去掉
/// 捕获的 `User-Agent` 头。表单的 HTTP 认证原样保留：引擎对非空 `httpUser` 注入的
/// `Authorization` 会覆盖捕获头，留空则沿用浏览器头 / 已保存站点凭据。
fn merge_confirmed_request(
    captured: DownloadRequest,
    mut confirmed: CreateTaskRequest,
) -> CaptureCreate {
    let CaptureCreate {
        request: base,
        hint_file_size,
        ..
    } = captured_create_request(captured);
    confirmed.url = base.url;
    confirmed.method = base.method;
    confirmed.body = base.body;
    confirmed.audio_url = base.audio_url;
    confirmed.torrent_b64 = None;
    let name_hint = if confirmed.file_name.trim().is_empty() {
        confirmed.file_name = base.file_name;
        None
    } else {
        Some(base.file_name).filter(|name| !name.trim().is_empty())
    };
    fill_if_blank(&mut confirmed.save_dir, base.save_dir);
    fill_if_blank(&mut confirmed.cookies, base.cookies);
    fill_if_blank(&mut confirmed.referrer, base.referrer);
    let mut headers = base.headers.unwrap_or_default();
    if !confirmed.user_agent.trim().is_empty() {
        headers.retain(|name, _| !name.eq_ignore_ascii_case("user-agent"));
    }
    for (name, value) in confirmed.headers.take().unwrap_or_default() {
        headers.retain(|existing, _| !existing.eq_ignore_ascii_case(&name));
        headers.insert(name, value);
    }
    confirmed.headers = (!headers.is_empty()).then_some(headers);
    CaptureCreate {
        request: confirmed,
        hint_file_size,
        name_hint,
        category_route: None,
    }
}

fn supports_manifest(request: &CreateTaskRequest) -> bool {
    request
        .method
        .as_ref()
        .is_none_or(|method| method.eq_ignore_ascii_case("GET"))
        && request.body.is_none()
        && request.audio_url.is_none()
}

fn into_preview_request(mut request: CreateTaskRequest) -> ResolvePreviewRequest {
    let mut extra_headers = request.headers.take().unwrap_or_default();
    inject_http_auth(&mut extra_headers, &request);
    ResolvePreviewRequest {
        url: request.url,
        cookies: request.cookies,
        referrer: request.referrer,
        user_agent: request.user_agent,
        extra_headers,
    }
}

/// 组的最终选项不由旧 context 覆盖；context 仅提供 group wire 缺少的 Basic 凭据。
fn merge_group_request(
    captured: DownloadRequest,
    mut request: CreateGroupRequest,
    context: &CreateTaskRequest,
) -> Result<CreateGroupRequest, CaptureError> {
    let base = captured_create_request(captured).request;
    if !supports_manifest(&base) {
        return Err(CaptureError::InvalidGroup);
    }
    request.source_url = base.url;
    fill_if_blank(&mut request.cookies, base.cookies);
    fill_if_blank(&mut request.referrer, base.referrer);
    let mut headers = base.headers.unwrap_or_default();
    if !request.user_agent.trim().is_empty() {
        headers.retain(|name, _| !name.eq_ignore_ascii_case("user-agent"));
    }
    for (name, value) in request.extra_headers {
        headers.retain(|existing, _| !existing.eq_ignore_ascii_case(&name));
        headers.insert(name, value);
    }
    inject_http_auth(&mut headers, context);
    request.extra_headers = headers;
    Ok(request)
}

fn inject_http_auth(headers: &mut HashMap<String, String>, context: &CreateTaskRequest) {
    // 与 TaskCreate 的 HTTP Basic 规则一致：只检查 is_empty，不修剪凭据。
    if !context.http_user.is_empty() {
        headers.retain(|name, _| !name.eq_ignore_ascii_case("authorization"));
        headers.insert(
            "Authorization".to_owned(),
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD
                    .encode(format!("{}:{}", context.http_user, context.http_password))
            ),
        );
    }
}

fn validate_group_items(request: &CreateGroupRequest) -> Result<(), CaptureError> {
    if request.items.is_empty()
        || request.items.iter().any(|item| {
            item.resolver_item.trim().is_empty()
                || item.file_name.trim().is_empty()
                || item.file_name.contains(['/', '\\'])
                || !is_safe_relative_path(&item.file_name)
                || (!item.rel_path.is_empty() && !is_safe_relative_path(&item.rel_path))
        })
    {
        return Err(CaptureError::InvalidGroup);
    }
    Ok(())
}

// 与插件 manifest 的相对路径规则一致；agent 不依赖 engine，不能调用其验证器。
fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let bytes = path.as_bytes();
    let is_windows_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if is_windows_drive || path.chars().any(char::is_control) {
        return false;
    }
    path.split(['/', '\\'])
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn fill_if_blank(target: &mut String, fallback: String) {
    if target.trim().is_empty() {
        *target = fallback;
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(i64::MAX as u128) as i64
        })
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("capture confirmation queue is full")]
    Full,
    #[error("capture transaction not found")]
    NotFound,
    #[error("capture transaction is already creating a group")]
    Busy,
    #[error("capture cannot create the selected group")]
    InvalidGroup,
    #[error("daemon capture create failed: {0:?}")]
    Daemon(fluxdown_protocol::RpcErrorData),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Platform(#[from] crate::platform::PlatformError),
}

/// daemon 专用二进制上传端点（`POST /blobs/{torrents|plugins}`）的客户端。
///
/// 鉴权与目标地址都取自已认证 WebSocket 会话（会话级凭据 + 该会话所连 daemon 的 HTTP 基址），
/// 不使用长期 token；daemon 换端口重连后自动跟随。
pub struct DaemonBlobClient {
    session: crate::daemon_client::HttpSession,
    http: reqwest::Client,
}

/// 上传的 blob 类型，对应 daemon 端点路径段。
#[derive(Clone, Copy, Debug)]
pub enum BlobKind {
    Torrent,
    Plugin,
}

impl BlobKind {
    fn path(self) -> &'static str {
        match self {
            Self::Torrent => "/blobs/torrents",
            Self::Plugin => "/blobs/plugins",
        }
    }
}

impl DaemonBlobClient {
    pub fn new(config: &crate::daemon_client::DaemonClientConfig) -> Result<Self, BlobError> {
        // 只连本机回环 daemon：不需要根证书库，跳过系统根证书加载（macOS 钥匙串，每次约 70ms，
        // 位于 Gateway 开始服务前的装配路径上）。
        let http = reqwest::Client::builder()
            .tls_built_in_root_certs(false)
            // 回环 daemon 不能经环境 / 系统代理转发，否则会话凭据与文件字节会离开本机。
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            session: config.http_session(),
            http,
        })
    }

    /// 上传字节并返回 daemon 分配的 `blobId`。
    pub async fn upload(&self, kind: BlobKind, bytes: Vec<u8>) -> Result<String, BlobError> {
        // 没有已认证的 daemon 会话（未连上 / 已断开）时没有可用凭据：按 daemon 不可用处理，
        // 绝不退回长期 token。
        let Some(access) = self.session.access() else {
            return Err(BlobError::Status(503));
        };
        let url = access
            .base_url
            .join(kind.path())
            .map_err(|error| BlobError::Url(error.to_string()))?;
        let response = self
            .http
            .post(url)
            .bearer_auth(access.credential)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(BlobError::Status(status.as_u16()));
        }
        let body = response.json::<Value>().await?;
        body.get("blobId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or(BlobError::Decode)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("daemon URL is invalid: {0}")]
    Url(String),
    #[error("daemon blob upload transport failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("daemon blob upload rejected with HTTP {0}")]
    Status(u16),
    #[error("daemon blob upload response has no blobId")]
    Decode,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use fluxdown_protocol::{
        AgentEvent, AgentSnapshot, ApplicationErrorCode, CreateGroupRequest, CreateTaskRequest,
        RequestBody, RpcErrorData, RpcErrorObject, RpcRequest, RpcResponse, ServiceEvent,
    };
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};

    use super::{
        CaptureError, CaptureOrigin, CaptureService, CategoryPlan, DownloadRequest, ExternalPolicy,
        captured_create_request, into_preview_request, merge_confirmed_request,
        merge_group_request, pending_capture_dto, split_batch,
    };

    fn preferences(
        value: serde_json::Value,
    ) -> std::collections::BTreeMap<String, serde_json::Value> {
        serde_json::from_value(value).expect("preference map")
    }

    /// 断开的 daemon：静默路径的建任务调用必然失败，从而可观察「没有入确认队列」。
    fn service(prefs: serde_json::Value) -> CaptureService {
        let daemon = std::sync::Arc::new(crate::daemon_client::DaemonClient::disconnected());
        let mut snapshot = AgentSnapshot::default();
        snapshot.preferences.values = preferences(prefs);
        let events = crate::event_hub::AgentEventHub::new(snapshot);
        let shell = crate::shell::ShellState::new(
            crate::shell::TrayAvailability::Unavailable(
                fluxdown_protocol::TrayUnavailableReason::NotBuilt,
            ),
            daemon.clone(),
            events.clone(),
        );
        CaptureService::new(daemon, events, shell)
    }

    #[test]
    fn external_policy_follows_silent_preferences_and_remembered_dir() {
        let off = ExternalPolicy::from_preferences(&preferences(json!({})));
        assert!(!off.silent);
        assert!(!off.skip_selection);
        assert_eq!(off.remembered_save_dir, None);

        let on = ExternalPolicy::from_preferences(&preferences(json!({
            "download.silent_download": true,
            "download.silent_skip_selection": true,
            "download.remember_last_save_dir": true,
            "download.last_save_dir": "  /last  ",
        })));
        assert!(on.silent);
        assert!(on.skip_selection);
        assert_eq!(on.remembered_save_dir.as_deref(), Some("/last"));

        // 「跟随上次」关闭时忽略记录；旧的无命名空间子开关键不生效。
        let not_remembered = ExternalPolicy::from_preferences(&preferences(json!({
            "download.silent_download": true,
            "silent_skip_selection": true,
            "download.last_save_dir": "/last",
        })));
        assert!(!not_remembered.skip_selection);
        assert_eq!(not_remembered.remembered_save_dir, None);
    }

    #[tokio::test]
    async fn silent_external_capture_creates_directly_instead_of_prompting() {
        let capture = service(json!({ "download.silent_download": true }));
        let result = capture.submit(captured(), CaptureOrigin::External).await;
        assert!(matches!(result, Err(super::CaptureError::Daemon(_))));
        assert!(capture.list().await.is_empty());
    }

    #[tokio::test]
    async fn oversized_batch_is_rejected_whole_without_partial_enqueue() {
        let capture = service(json!({}));
        let too_many = (0..=super::CAPTURE_CAPACITY).map(|_| captured()).collect();
        let result = capture.submit_many(too_many, CaptureOrigin::External).await;
        assert!(matches!(result, Err(super::CaptureError::Full)));
        assert!(capture.list().await.is_empty());

        let fits = (0..super::CAPTURE_CAPACITY).map(|_| captured()).collect();
        capture
            .submit_many(fits, CaptureOrigin::External)
            .await
            .expect("whole batch fits");
        assert_eq!(capture.list().await.len(), super::CAPTURE_CAPACITY);
    }

    #[test]
    fn category_route_applies_only_when_the_dir_was_not_chosen() {
        let prefs = preferences(json!({
            "custom_categories": [
                { "id": "bin", "name": "bin", "extensions": ["bin"], "position": 1,
                  "saveDir": "/category" },
                { "id": "img", "name": "img", "extensions": ["jpg"], "position": 2,
                  "saveDir": "/pictures" },
            ],
        }));
        let route = crate::category_dir::category_route(&prefs);
        // 捕获方给了目录：不路由。
        assert!(CategoryPlan::of(&captured(), route.as_ref()).is_none());
        let mut unspecified = captured();
        unspecified.save_dir = String::new();
        let plan = CategoryPlan::of(&unspecified, route.as_ref()).expect("routable capture");
        assert_eq!(plan.prefill, "/category");
        // 表单保留预填 / 默认目录 / 留空 = 仍跟随分类；改选别的目录 = 用户选择为准。
        assert!(plan.follows("", "/default"));
        assert!(plan.follows(" /category ", "/default"));
        assert!(plan.follows("/default", "/default"));
        assert!(!plan.follows("/elsewhere", "/default"));
        // 多条捕获共用一张表单时目录框是另一条的预填分类目录：仍各自按分类路由。
        assert!(plan.follows("/pictures", "/default"));
        // 交给 daemon 时保存目录是基准目录（跟随上次 / daemon 默认），分类由引擎定。
        let mut spec = captured_create_request(unspecified);
        plan.clone().apply(&mut spec, Some("/last"));
        assert_eq!(spec.request.save_dir, "/last");
        assert_eq!(spec.category_route, route);
        let mut spec = captured_create_request(captured());
        plan.apply(&mut spec, None);
        assert_eq!(spec.request.save_dir, "");
    }

    #[tokio::test]
    async fn manual_category_directory_disables_capture_route() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (capture, mut calls, _events, stop, server) = daemon_service().await;
            capture.events.publish(AgentEvent::PreferencesChanged(
                fluxdown_protocol::AgentPreferencesDto {
                    revision: 1,
                    values: [(
                        fluxdown_protocol::CUSTOM_CATEGORIES_PREF_KEY.to_owned(),
                        json!([{
                            "id": "bin",
                            "name": "Binary",
                            "extensions": ["bin"],
                            "position": 1,
                            "saveDir": "/category"
                        }]),
                    )]
                    .into(),
                },
            ));
            let mut original = captured();
            original.save_dir.clear();
            let queued = capture
                .submit(original, CaptureOrigin::External)
                .await
                .expect("queue capture");
            let transaction_id = queued["transactionIds"][0]
                .as_str()
                .expect("transaction id")
                .to_owned();
            let confirming = {
                let capture = capture.clone();
                tokio::spawn(async move {
                    capture
                        .resolve(
                            &transaction_id,
                            true,
                            true,
                            Some(form(json!({
                                "url": "https://example.com/a.bin",
                                "saveDir": "/category"
                            }))),
                        )
                        .await
                })
            };
            let (request, reply) = calls.recv().await.expect("task create RPC");
            assert_eq!(
                request.method,
                fluxdown_protocol::method::DAEMON_TASK_CREATE
            );
            let params: fluxdown_protocol::DaemonCreateTaskParams =
                serde_json::from_value(request.params.expect("task create params"))
                    .expect("decode task create params");
            assert_eq!(params.request.save_dir, "/category");
            assert!(params.category_route.is_none());
            reply
                .send(Ok(json!({"taskId": "manual-category"})))
                .expect("daemon reply");
            confirming
                .await
                .expect("confirmation task")
                .expect("confirm capture");
            stop.send(()).expect("stop test daemon");
            drop(capture);
            server.await.expect("daemon server");
        })
        .await
        .expect("manual capture category lifecycle");
    }

    #[test]
    fn batch_request_splits_per_url_sharing_only_request_context() {
        let mut batch = captured();
        batch.url = "https://example.com/a.bin\n\n  https://example.com/b.bin  \n".to_owned();
        let split = split_batch(batch);
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].url, "https://example.com/a.bin");
        assert_eq!(split[1].url, "https://example.com/b.bin");
        for request in &split {
            assert_eq!(request.cookies, "sid=1");
            assert_eq!(request.referrer, "https://example.com/page");
            assert_eq!(request.save_dir, "/captured");
            assert_eq!(request.headers, captured().headers);
            assert!(request.filename.is_empty());
            assert!(request.method.is_none() && request.body.is_none());
            assert!(request.audio_url.is_none() && request.file_size.is_none());
        }

        let single = split_batch(captured());
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].url, "https://example.com/a.bin");
        assert_eq!(single[0].filename, "a.bin");
        assert_eq!(single[0].method.as_deref(), Some("POST"));
        assert!(single[0].audio_url.is_some());
    }

    fn captured() -> DownloadRequest {
        DownloadRequest {
            url: "https://example.com/a.bin".to_owned(),
            filename: "a.bin".to_owned(),
            save_dir: "/captured".to_owned(),
            referrer: "https://example.com/page".to_owned(),
            cookies: "sid=1".to_owned(),
            headers: Some(HashMap::from([
                ("User-Agent".to_owned(), "Browser/1".to_owned()),
                ("Authorization".to_owned(), "Basic YTpi".to_owned()),
                ("Accept".to_owned(), "*/*".to_owned()),
            ])),
            file_size: Some(-1),
            mime_type: None,
            method: Some("POST".to_owned()),
            body: Some(RequestBody::Urlencoded {
                raw: "k=v".to_owned(),
            }),
            audio_url: Some("https://example.com/a.m4a".to_owned()),
        }
    }

    fn form(value: serde_json::Value) -> CreateTaskRequest {
        serde_json::from_value(value).expect("form request")
    }

    fn get_capture() -> DownloadRequest {
        DownloadRequest {
            method: None,
            body: None,
            audio_url: None,
            ..captured()
        }
    }

    fn group() -> CreateGroupRequest {
        serde_json::from_value(json!({
            "sourceUrl": "https://changed.example/other",
            "groupName": "Selected",
            "saveDir": "/selected",
            "queueId": "selected-queue",
            "segments": 3,
            "startPaused": true,
            "items": [{ "resolverItem": "opaque@chosen", "fileName": "chosen.bin" }],
        }))
        .expect("group request")
    }

    // Seed only the transaction under test: do not launch the desktop confirmation UI.
    async fn queued_capture(capture: &CaptureService, request: DownloadRequest) -> String {
        let public = pending_capture_dto(&request);
        let id = public.transaction_id.clone();
        capture
            .pending
            .lock()
            .await
            .push_back(super::CaptureTransaction {
                public,
                request,
                category: None,
                group_creation: None,
            });
        capture.publish().await;
        id
    }

    #[test]
    fn capture_preview_and_group_auth_override_browser_headers_without_changing_source() {
        let context = form(json!({
            "url": "https://changed.example/other",
            "saveDir": "/old-base",
            "queueId": "old-queue",
            "segments": 99,
            "startPaused": false,
            "httpUser": "alice",
            "httpPassword": "secret",
            "userAgent": "Form/2",
            "headers": { "authorization": "Bearer form", "accept": "form/type" },
        }));
        let preview =
            into_preview_request(merge_confirmed_request(get_capture(), context.clone()).request);
        assert_eq!(preview.url, captured().url);
        assert_eq!(preview.cookies, "sid=1");
        assert_eq!(preview.referrer, captured().referrer);
        assert_eq!(preview.user_agent, "Form/2");
        assert_eq!(
            preview.extra_headers["Authorization"],
            "Basic YWxpY2U6c2VjcmV0"
        );
        assert!(!preview.extra_headers.contains_key("authorization"));
        assert_eq!(preview.extra_headers["accept"], "form/type");
        assert!(!preview.extra_headers.contains_key("User-Agent"));

        let mut selected = group();
        selected
            .extra_headers
            .insert("authorization".to_owned(), "Bearer selected".to_owned());
        selected
            .extra_headers
            .insert("accept".to_owned(), "selected/type".to_owned());
        let merged = merge_group_request(get_capture(), selected, &context).expect("merge group");
        assert_eq!(merged.source_url, captured().url);
        assert_eq!(merged.save_dir, "/selected");
        assert_eq!(merged.queue_id, "selected-queue");
        assert_eq!(merged.segments, 3);
        assert!(merged.start_paused);
        assert_eq!(merged.items[0].resolver_item, "opaque@chosen");
        assert_eq!(merged.cookies, "sid=1");
        assert_eq!(merged.referrer, captured().referrer);
        assert_eq!(merged.extra_headers["User-Agent"], "Browser/1");
        assert_eq!(
            merged.extra_headers["Authorization"],
            "Basic YWxpY2U6c2VjcmV0"
        );
        assert!(!merged.extra_headers.contains_key("authorization"));
        assert_eq!(merged.extra_headers["accept"], "selected/type");
        assert!(!merged.extra_headers.contains_key("Accept"));

        let mut selected = group();
        selected.cookies = "chosen=2".to_owned();
        selected.referrer = "https://chosen.example".to_owned();
        selected.user_agent = "Chosen/3".to_owned();
        let merged = merge_group_request(get_capture(), selected, &form(json!({ "url": "" })))
            .expect("merge without explicit credentials");
        assert_eq!(merged.cookies, "chosen=2");
        assert_eq!(merged.referrer, "https://chosen.example");
        assert_eq!(merged.user_agent, "Chosen/3");
        assert!(!merged.extra_headers.contains_key("User-Agent"));
        assert_eq!(merged.extra_headers["Authorization"], "Basic YTpi");
    }

    #[tokio::test]
    async fn unsupported_capture_preview_keeps_original_request_for_legacy_confirmation() {
        for request in [
            captured(),
            DownloadRequest {
                method: Some("POST".to_owned()),
                ..get_capture()
            },
            DownloadRequest {
                body: captured().body,
                ..get_capture()
            },
            DownloadRequest {
                audio_url: captured().audio_url,
                ..get_capture()
            },
        ] {
            let capture = Arc::new(service(json!({})));
            let id = queued_capture(&capture, request.clone()).await;
            let preview = capture
                .preview(
                    &id,
                    form(json!({ "url": "https://changed.example", "method": "GET" })),
                )
                .await
                .expect("skip inexpressible request without contacting disconnected daemon");
            assert!(preview.items.is_empty());
            assert!(preview.error.is_empty());
            assert_eq!(preview.source_url, request.url);
            let pending = capture.pending.lock().await;
            let original = serde_json::to_value(&request).expect("original request");
            assert_eq!(
                serde_json::to_value(&pending[0].request).expect("pending request"),
                original
            );
            drop(pending);
            assert!(matches!(
                capture
                    .create_group(&id, group(), form(json!({ "url": "" })))
                    .await,
                Err(CaptureError::InvalidGroup)
            ));
            assert_eq!(capture.list().await.len(), 1);
            capture
                .resolve(&id, false, false, None)
                .await
                .expect("legacy rejection");
            assert!(matches!(
                capture.resolve(&id, false, false, None).await,
                Err(CaptureError::NotFound)
            ));
        }
    }

    #[tokio::test]
    async fn invalid_group_items_and_failed_creation_do_not_consume_capture() {
        let capture = Arc::new(service(json!({})));
        let id = queued_capture(&capture, get_capture()).await;
        for (name, path, token) in [
            ("", "", "opaque"),
            ("safe.bin", "", ""),
            ("../outside.bin", "", "opaque"),
            ("safe.bin", "/outside", "opaque"),
            ("safe.bin", "..\\outside", "opaque"),
            ("safe.bin", "C:\\outside", "opaque"),
            ("safe.bin", "parent/../outside", "opaque"),
            ("safe.bin", "parent\0outside", "opaque"),
        ] {
            let mut request = group();
            request.items[0].file_name = name.to_owned();
            request.items[0].rel_path = path.to_owned();
            request.items[0].resolver_item = token.to_owned();
            assert!(
                matches!(
                    capture
                        .create_group(&id, request, form(json!({ "url": "" })))
                        .await,
                    Err(CaptureError::InvalidGroup)
                ),
                "{name:?} {path:?} {token:?}"
            );
        }
        let mut empty = group();
        empty.items.clear();
        assert!(matches!(
            capture
                .create_group(&id, empty, form(json!({ "url": "" })))
                .await,
            Err(CaptureError::InvalidGroup)
        ));
        for _ in 0..2 {
            assert!(matches!(
                capture
                    .create_group(&id, group(), form(json!({ "url": "" })))
                    .await,
                Err(CaptureError::Daemon(_))
            ));
            assert_eq!(capture.list().await.len(), 1);
        }
        capture
            .resolve(&id, false, false, None)
            .await
            .expect("transaction remains rejectable");
    }

    type DaemonCall = (
        RpcRequest,
        tokio::sync::oneshot::Sender<Result<Value, RpcErrorData>>,
    );

    type TestSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    async fn socket_request(socket: &mut TestSocket) -> RpcRequest {
        loop {
            let frame = socket
                .next()
                .await
                .expect("daemon client frame")
                .expect("valid frame");
            if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
                return serde_json::from_str(&text).expect("RPC request");
            }
        }
    }

    async fn socket_response(socket: &mut TestSocket, response: RpcResponse) {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&response)
                    .expect("encode response")
                    .into(),
            ))
            .await
            .expect("send response");
    }

    async fn authenticate_test_daemon(socket: &mut TestSocket, token: &str) {
        use fluxdown_protocol::handshake::{
            AuthChallengeParams, AuthProveParams, server_proof, verify_client_proof,
        };
        use fluxdown_protocol::{ServiceHello, ServiceRole, Snapshot, SnapshotBody};

        let challenge = socket_request(socket).await;
        let params: AuthChallengeParams =
            serde_json::from_value(challenge.params.expect("challenge params")).expect("challenge");
        let nonce = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        socket_response(
            socket,
            RpcResponse::success(challenge.id, json!({
                "serverNonce": nonce,
                "serverProof": server_proof(token, &params.client_nonce, &nonce).expect("server proof"),
            })),
        ).await;
        let prove = socket_request(socket).await;
        let proof: AuthProveParams =
            serde_json::from_value(prove.params.expect("prove params")).expect("proof");
        assert!(verify_client_proof(
            token,
            &params.client_nonce,
            &nonce,
            &proof.client_proof
        ));
        socket_response(
            socket,
            RpcResponse::success(prove.id, json!({ "authenticated": true })),
        )
        .await;
        let hello = socket_request(socket).await;
        socket_response(
            socket,
            RpcResponse::success(
                hello.id,
                serde_json::to_value(ServiceHello::new(
                    ServiceRole::Daemon,
                    "daemon",
                    "test",
                    "capture-test",
                    Vec::new(),
                ))
                .expect("hello"),
            ),
        )
        .await;
        let snapshot = socket_request(socket).await;
        socket_response(
            socket,
            RpcResponse::success(
                snapshot.id,
                serde_json::to_value(Snapshot {
                    epoch: "capture-test".to_owned(),
                    sequence: 0,
                    body: SnapshotBody::Daemon(Box::default()),
                })
                .expect("snapshot"),
            ),
        )
        .await;
    }

    /// 真实 agent RPC 传输，测试控制 daemon 完成时间与失败结果来覆盖事务状态转换。
    async fn daemon_service() -> (
        Arc<CaptureService>,
        tokio::sync::mpsc::Receiver<DaemonCall>,
        tokio::sync::mpsc::Receiver<crate::daemon_client::DaemonClientEvent>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        const TOKEN: &str = "capture-behavior-test-token";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind daemon");
        let address = listener.local_addr().expect("daemon address");
        let (calls, requests) = tokio::sync::mpsc::channel(8);
        let (stop, mut stopping) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept agent");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("WebSocket");
            authenticate_test_daemon(&mut socket, TOKEN).await;
            loop {
                let frame = tokio::select! {
                    biased;
                    _ = &mut stopping => break,
                    frame = socket.next() => frame,
                };
                let Some(frame) = frame else {
                    break;
                };
                let frame = frame.expect("valid client frame");
                if frame.is_close() {
                    break;
                }
                let tokio_tungstenite::tungstenite::Message::Text(text) = frame else {
                    continue;
                };
                let request: RpcRequest = serde_json::from_str(&text).expect("daemon request");
                let id = request.id.clone();
                let (reply, response) = tokio::sync::oneshot::channel();
                calls
                    .send((request, reply))
                    .await
                    .expect("deliver daemon call");
                let response = match response.await.expect("test daemon outcome") {
                    Ok(value) => RpcResponse::success(id, value),
                    Err(error) => {
                        RpcResponse::failure(id, RpcErrorObject::application("rejected", error))
                    }
                };
                socket_response(&mut socket, response).await;
            }
        });
        let (daemon, events) = crate::daemon_client::DaemonClient::start(
            crate::daemon_client::DaemonClientConfig::new(format!("ws://{address}/rpc"), TOKEN),
            Arc::new(crate::supervisor::DaemonSupervisor::new(Some(address))),
            tokio_util::sync::CancellationToken::new(),
        )
        .expect("start daemon client");
        let daemon = Arc::new(daemon);
        daemon
            .wait_ready(Duration::from_secs(2))
            .await
            .expect("daemon ready");
        let hub = crate::event_hub::AgentEventHub::new(AgentSnapshot::default());
        let shell = crate::shell::ShellState::new(
            crate::shell::TrayAvailability::Unavailable(
                fluxdown_protocol::TrayUnavailableReason::NotBuilt,
            ),
            daemon.clone(),
            hub.clone(),
        );
        (
            Arc::new(CaptureService::new(daemon, hub, shell)),
            requests,
            events,
            stop,
            server,
        )
    }

    #[tokio::test]
    async fn capture_notifications_preserve_single_intents_and_batch_boundaries() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (capture, mut calls, daemon_events, stop, server) = daemon_service().await;
            let (mut events, _) = capture.events.subscribe_and_snapshot();
            let single = {
                let capture = Arc::clone(&capture);
                tokio::spawn(
                    async move { capture.submit(get_capture(), CaptureOrigin::Direct).await },
                )
            };
            let (request, reply) = calls.recv().await.expect("single create");
            assert_eq!(
                request.method,
                fluxdown_protocol::method::DAEMON_TASK_CREATE
            );
            reply
                .send(Ok(json!({ "taskId": "single" })))
                .expect("created single");
            single
                .await
                .expect("single caller")
                .expect("single capture");
            assert!(matches!(
                events.try_recv().expect("single notification").event,
                ServiceEvent::Agent(AgentEvent::CaptureTasksStarted(ids)) if ids == ["single"]
            ));

            for fail_second in [false, true] {
                let batch = {
                    let capture = Arc::clone(&capture);
                    tokio::spawn(async move {
                        capture
                            .submit_many(vec![get_capture(), get_capture()], CaptureOrigin::Direct)
                            .await
                    })
                };
                let (request, reply) = calls.recv().await.expect("batch first create");
                assert_eq!(
                    request.method,
                    fluxdown_protocol::method::DAEMON_TASK_CREATE
                );
                reply
                    .send(Ok(json!({ "taskId": "first" })))
                    .expect("first created");
                let (request, reply) = calls.recv().await.expect("batch second create");
                assert_eq!(
                    request.method,
                    fluxdown_protocol::method::DAEMON_TASK_CREATE
                );
                if fail_second {
                    reply
                        .send(Err(RpcErrorData::new(
                            ApplicationErrorCode::InvalidArgument,
                            false,
                        )))
                        .expect("second rejected");
                    assert!(matches!(
                        batch.await.expect("batch caller"),
                        Err(CaptureError::Daemon(_))
                    ));
                    assert!(
                        matches!(
                            events.try_recv(),
                            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                        ),
                        "partial batch must not become a single-task window intent"
                    );
                } else {
                    reply
                        .send(Ok(json!({ "taskId": "second" })))
                        .expect("second created");
                    batch.await.expect("batch caller").expect("batch capture");
                    assert!(matches!(
                        events.try_recv().expect("batch notification").event,
                        ServiceEvent::Agent(AgentEvent::CaptureTasksStarted(ids))
                            if ids == ["first", "second"]
                    ));
                }
            }
            stop.send(()).expect("stop test daemon");
            drop(capture);
            drop(daemon_events);
            server.await.expect("daemon server");
        })
        .await
        .expect("capture notification lifecycle");
    }

    /// 页面脚本调用开始的任务：恰好一个才通知，新建与恢复各用各的事件；多个（含一建一恢复）不通知。
    #[tokio::test]
    async fn page_started_tasks_notify_only_a_single_start() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (capture, _calls, daemon_events, stop, server) = daemon_service().await;
            let (mut events, _) = capture.events.subscribe_and_snapshot();
            let ids = |ids: &[&str]| ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();

            capture.announce_page_started(&ids(&["new"]), &[]);
            assert!(matches!(
                events.try_recv().expect("created notification").event,
                ServiceEvent::Agent(AgentEvent::CaptureTasksStarted(ids)) if ids == ["new"]
            ));
            capture.announce_page_started(&[], &ids(&["old"]));
            assert!(matches!(
                events.try_recv().expect("resumed notification").event,
                ServiceEvent::Agent(AgentEvent::CaptureTasksResumed(ids)) if ids == ["old"]
            ));
            capture.announce_page_started(&ids(&["a"]), &ids(&["b"]));
            capture.announce_page_started(&ids(&["a", "b"]), &[]);
            capture.announce_page_started(&[], &[]);
            assert!(events.try_recv().is_err(), "multi-start must stay silent");

            stop.send(()).expect("stop test daemon");
            drop(capture);
            drop(daemon_events);
            server.await.expect("daemon server");
        })
        .await
        .expect("page start notification lifecycle");
    }

    #[tokio::test]
    async fn capture_preview_is_read_only_and_returns_metadata_without_request_secrets() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (capture, mut calls, daemon_events, stop, server) = daemon_service().await;
            let id = queued_capture(&capture, get_capture()).await;
            let previewing = {
                let capture = capture.clone();
                let id = id.clone();
                tokio::spawn(async move {
                    capture.preview(&id, form(json!({
                        "url": "https://changed.example/other",
                        "httpUser": "alice", "httpPassword": "secret",
                        "saveSiteAuth": true,
                    }))).await
                })
            };
            let (request, reply) = calls.recv().await.expect("preview request");
            assert_eq!(request.method, fluxdown_protocol::method::DAEMON_GROUP_RESOLVE_PREVIEW);
            let params: fluxdown_protocol::ResolvePreviewRequest =
                serde_json::from_value(request.params.expect("preview params")).expect("params");
            assert_eq!(params.url, captured().url);
            assert_eq!(params.cookies, "sid=1");
            assert_eq!(params.extra_headers["User-Agent"], "Browser/1");
            assert_eq!(params.extra_headers["Authorization"], "Basic YWxpY2U6c2VjcmV0");
            reply.send(Ok(json!({
                "name": "Manifest", "sourceUrl": "https://changed.example",
                "items": [{ "id": "opaque", "name": "safe.bin", "path": "", "size": 16, "variants": [] }],
                "cookies": "sid=1", "headers": params.extra_headers, "body": "k=v",
            }))).expect("respond preview");
            let preview = previewing.await.expect("preview caller").expect("preview");
            assert_eq!(preview.source_url, captured().url);
            assert_eq!(preview.items[0].id, "opaque");
            let wire = serde_json::to_string(&preview).expect("preview metadata");
            for secret in ["sid=1", "Browser/1", "YWxpY2U6c2VjcmV0", "k=v", "cookies", "headers", "body"] {
                assert!(!wire.contains(secret), "{secret}");
            }
            assert_eq!(capture.list().await[0].transaction_id, id);
            capture.resolve(&id, false, false, None).await.expect("still rejectable");
            assert!(calls.try_recv().is_err(), "preview did not create any task");
            stop.send(()).expect("stop test daemon");
            drop(capture);
            drop(daemon_events);
            server.await.expect("daemon server");
        }).await.expect("preview lifecycle");
    }

    #[tokio::test]
    async fn capture_group_retry_single_flight_and_ui_cancellation_preserve_single_consumption() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (capture, mut calls, daemon_events, stop, server) = daemon_service().await;
            let id = queued_capture(&capture, get_capture()).await;
            let create = |capture: Arc<CaptureService>, id: String| tokio::spawn(async move {
                capture.create_group(&id, group(), form(json!({ "url": "" }))).await
            });
            let failing = create(capture.clone(), id.clone());
            let (request, reply) = calls.recv().await.expect("first group request");
            assert_eq!(request.method, fluxdown_protocol::method::DAEMON_GROUP_CREATE);
            reply.send(Err(RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)))
                .expect("reject before creation");
            assert!(matches!(failing.await.expect("first caller"), Err(CaptureError::Daemon(_))));
            assert_eq!(capture.list().await[0].transaction_id, id);

            let creating = create(capture.clone(), id.clone());
            let (request, reply) = calls.recv().await.expect("retry group request");
            assert_eq!(request.method, fluxdown_protocol::method::DAEMON_GROUP_CREATE);
            for accepted in [false, true] {
                assert!(matches!(
                    capture.resolve(&id, accepted, false, Some(form(json!({ "url": "" })))).await,
                    Err(CaptureError::Busy)
                ));
            }
            assert!(matches!(
                capture.create_group(&id, group(), form(json!({ "url": "" }))).await,
                Err(CaptureError::Busy)
            ));
            creating.abort();
            assert!(creating.await.expect_err("UI caller cancelled").is_cancelled());
            assert!(matches!(
                capture.create_group(&id, group(), form(json!({ "url": "" }))).await,
                Err(CaptureError::Busy)
            ), "dropping the UI future must not release the running creation");
            let (mut events, _) = capture.events.subscribe_and_snapshot();
            reply.send(Ok(json!({ "groupId": "created-group" }))).expect("create group");
            let event = events.recv().await.expect("consumption published");
            assert!(matches!(
                event.event,
                ServiceEvent::Agent(AgentEvent::PendingCapturesChanged(pending)) if pending.is_empty()
            ));
            assert!(capture.list().await.is_empty());
            assert!(matches!(
                capture.create_group(&id, group(), form(json!({ "url": "" }))).await,
                Err(CaptureError::NotFound)
            ));
            assert!(matches!(
                capture.resolve(&id, true, false, None).await,
                Err(CaptureError::NotFound)
            ));
            assert!(calls.try_recv().is_err(), "no duplicate group or ordinary task");
            stop.send(()).expect("stop test daemon");
            drop(capture);
            drop(daemon_events);
            server.await.expect("daemon server");
        }).await.expect("group lifecycle");
    }

    #[tokio::test]
    async fn capture_group_saves_credentials_for_original_site_without_revoking_success() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (capture, mut calls, daemon_events, stop, server) = daemon_service().await;
            let id = queued_capture(&capture, get_capture()).await;
            let creating =
                {
                    let capture = capture.clone();
                    tokio::spawn(async move {
                        capture.create_group(&id, group(), form(json!({
                        "url": "https://changed.example/other",
                        "httpUser": "alice", "httpPassword": "secret", "saveSiteAuth": true,
                    }))).await
                    })
                };
            let (request, reply) = calls.recv().await.expect("group create");
            assert_eq!(
                request.method,
                fluxdown_protocol::method::DAEMON_GROUP_CREATE
            );
            reply
                .send(Ok(json!({ "groupId": "saved-group" })))
                .expect("create success");
            let (request, reply) = calls.recv().await.expect("site authentication save");
            assert_eq!(
                request.method,
                fluxdown_protocol::method::DAEMON_SITE_AUTH_SAVE
            );
            let saved: fluxdown_protocol::SiteAuthSaveRequest =
                serde_json::from_value(request.params.expect("save params")).expect("credentials");
            assert_eq!(saved.site, captured().url);
            assert_eq!(saved.user, "alice");
            assert_eq!(saved.pass, "secret");
            assert!(
                capture.list().await.is_empty(),
                "group already consumed the transaction"
            );
            reply
                .send(Err(RpcErrorData::new(
                    ApplicationErrorCode::Unavailable,
                    true,
                )))
                .expect("save failure");
            let created = creating
                .await
                .expect("caller")
                .expect("group remains successful");
            assert_eq!(created.group_id, "saved-group");
            assert!(
                calls.try_recv().is_err(),
                "save failure does not retry group creation"
            );
            stop.send(()).expect("stop test daemon");
            drop(capture);
            drop(daemon_events);
            server.await.expect("daemon server");
        })
        .await
        .expect("credential save lifecycle");
    }

    #[test]
    fn pending_summary_exposes_header_names_but_no_secret_values() {
        let dto = pending_capture_dto(&captured());
        assert_eq!(dto.header_names, ["Accept", "Authorization", "User-Agent"]);
        assert!(dto.has_cookies);
        assert!(dto.has_authorization());
        assert_eq!(dto.file_size, 0);
        let wire = serde_json::to_string(&dto).expect("serialize summary");
        assert!(!wire.contains("sid=1"));
        assert!(!wire.contains("YTpi"));
    }

    #[test]
    fn declared_file_size_becomes_a_create_hint_only_when_positive() {
        let with_size = |size: Option<i64>| DownloadRequest {
            file_size: size,
            ..captured()
        };
        assert_eq!(
            captured_create_request(with_size(Some(4096))).hint_file_size,
            Some(4096)
        );
        // 未知（-1 / 0 / 缺省）不提示，daemon 照常探测。
        for size in [Some(-1), Some(0), None] {
            assert_eq!(
                captured_create_request(with_size(size)).hint_file_size,
                None
            );
        }
        // 表单确认沿用捕获方声明的大小。
        let merged = merge_confirmed_request(with_size(Some(4096)), form(json!({ "url": "" })));
        assert_eq!(merged.hint_file_size, Some(4096));
        let merged = merge_confirmed_request(with_size(None), form(json!({ "url": "" })));
        assert_eq!(merged.hint_file_size, None);
    }

    #[test]
    fn renamed_form_carries_captured_name_as_extension_hint() {
        let renamed =
            merge_confirmed_request(captured(), form(json!({ "url": "", "fileName": "123" })));
        assert_eq!(renamed.request.file_name, "123");
        assert_eq!(renamed.name_hint.as_deref(), Some("a.bin"));
        // 表单未改名：沿用捕获名本身，无需参照。
        let kept = merge_confirmed_request(captured(), form(json!({ "url": "" })));
        assert_eq!(kept.request.file_name, "a.bin");
        assert_eq!(kept.name_hint, None);
    }

    #[test]
    fn blank_form_keeps_captured_context_and_form_choices() {
        let merged = merge_confirmed_request(
            captured(),
            form(json!({
                "url": "https://evil.example/other",
                "queueId": "later",
                "segments": 8,
                "startPaused": true,
            })),
        )
        .request;
        assert_eq!(merged.url, "https://example.com/a.bin");
        assert_eq!(merged.file_name, "a.bin");
        assert_eq!(merged.save_dir, "/captured");
        assert_eq!(merged.cookies, "sid=1");
        assert_eq!(merged.referrer, "https://example.com/page");
        assert_eq!(merged.method.as_deref(), Some("POST"));
        assert!(matches!(merged.body, Some(RequestBody::Urlencoded { .. })));
        assert_eq!(
            merged.audio_url.as_deref(),
            Some("https://example.com/a.m4a")
        );
        assert_eq!(merged.queue_id, "later");
        assert_eq!(merged.segments, 8);
        assert!(merged.start_paused);
        let headers = merged.headers.expect("captured headers kept");
        assert_eq!(
            headers.get("User-Agent").map(String::as_str),
            Some("Browser/1")
        );
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Basic YTpi")
        );
    }

    #[test]
    fn form_values_override_captured_context_case_insensitively() {
        let merged = merge_confirmed_request(
            captured(),
            form(json!({
                "url": "https://example.com/a.bin",
                "fileName": "renamed.bin",
                "saveDir": "/chosen",
                "cookies": "sid=2",
                "userAgent": "Custom/2",
                "headers": { "accept": "application/octet-stream", "X-Extra": "1" },
                "httpUser": "alice",
                "httpPassword": "secret",
                "saveSiteAuth": true,
            })),
        )
        .request;
        assert_eq!(merged.file_name, "renamed.bin");
        assert_eq!(merged.save_dir, "/chosen");
        assert_eq!(merged.cookies, "sid=2");
        assert_eq!(merged.user_agent, "Custom/2");
        assert_eq!(merged.http_user, "alice");
        assert!(merged.save_site_auth);
        let headers = merged.headers.expect("merged headers");
        assert!(
            !headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("user-agent")),
            "form UA replaces captured User-Agent header"
        );
        assert!(!headers.contains_key("Accept"));
        assert_eq!(
            headers.get("accept").map(String::as_str),
            Some("application/octet-stream")
        );
        assert_eq!(headers.get("X-Extra").map(String::as_str), Some("1"));
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Basic YTpi")
        );
    }

    /// 读到请求头结束与整个请求体为止，返回头部文本（小写化便于断言）。
    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut received = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).await.expect("read request");
            assert!(read > 0, "client closed before finishing the request");
            received.extend_from_slice(&chunk[..read]);
            let Some(head_end) = received.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let head = String::from_utf8_lossy(&received[..head_end]).to_ascii_lowercase();
            let body_len = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if received.len() >= head_end + 4 + body_len {
                return head;
            }
        }
    }

    #[tokio::test]
    async fn blob_upload_needs_a_daemon_session_and_authenticates_with_its_credential() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake daemon");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let head = read_http_request(&mut stream).await;
            let body = br#"{"blobId":"blob-1"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("respond");
            stream.write_all(body).await.expect("respond body");
            head
        });
        let rpc_url = format!("ws://{address}/rpc");
        let config = crate::daemon_client::DaemonClientConfig::new(
            rpc_url.clone(),
            "long-lived-daemon-token",
        );
        let client = super::DaemonBlobClient::new(&config).expect("blob client");

        // 还没有已认证会话：不发任何请求（服务端尚未被连接），更不会退回长期 token。
        let error = client
            .upload(super::BlobKind::Torrent, vec![1, 2, 3])
            .await
            .expect_err("no session yet");
        assert!(matches!(error, super::BlobError::Status(503)), "{error}");
        assert!(!server.is_finished());

        config.http_session().install(
            crate::daemon_client::DaemonHttpAccess::from_rpc_url(
                &rpc_url,
                "session-credential".to_owned(),
            )
            .expect("session access"),
        );
        let blob_id = client
            .upload(super::BlobKind::Torrent, vec![1, 2, 3])
            .await
            .expect("upload with session credential");
        assert_eq!(blob_id, "blob-1");
        let head = server.await.expect("server");
        assert!(
            head.contains("authorization: bearer session-credential"),
            "{head}"
        );
        assert!(!head.contains("long-lived-daemon-token"), "{head}");
    }
}
