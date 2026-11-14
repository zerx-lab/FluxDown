//! 应用内更新：检查 → 后台下载并校验 → 一键安装并重启。
//!
//! `UpdateService` 独占 `UpdateStatusDto`，经宿主发布（相同状态去重，下载进度节流）：
//! 桌面 / headless agent 发 `AgentEvent::UpdateChanged` 并在本进程替换程序文件；
//! 原生 Android（[`UpdateService::delegated`]）把状态与安装请求交给 [`UpdateDelegate`]，
//! 由 App 经系统安装器完成安装。状态迁移：
//!
//! ```text
//! Idle/UpToDate/Available/Failed --check--> Checking --> UpToDate | Available | Failed(Network)
//! Ready --check(更新的版本)--> Available（旧包作废，下次下载时清理）
//! Available/Failed --download--> Downloading --> Ready | Failed(Network/Verify/Storage)
//! Downloading --cancel--> Available
//! Available/Failed --install--> Downloading(installPending) --> Installing
//! Ready --install--> Installing --> (重启) | Failed(Install/..)
//! ```
//!
//! 下载 / 安装中不再重新检查；就绪时照常检查，但只有查到比就绪包更新的版本才取代它，
//! 失败或同版本保留就绪状态——常驻不重启的机器也能跟上后续发布。

mod download;
pub(crate) mod install;
mod release;
pub mod restart;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fluxdown_protocol::{
    APP_VERSION, AgentEvent, ReleaseNoteDto, UpdateFailure, UpdateManualReason, UpdatePhase,
    UpdateStatusDto,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use self::download::{DownloadError, DownloadSpec, discard_partial, download_verified};
pub use self::install::InstallTarget;
use self::release::{
    ChangelogResponse, ComponentRelease, Endpoint, RELEASE_PAGE_URL, RawRelease,
    checksum_file_names, find_checksum, normalize_channel,
};
pub use self::release::{UpdateError, is_newer};
use crate::egress::{Egress, EgressClient};
use crate::event_hub::AgentEventHub;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// 周期检查按墙钟判定到期：单调时钟是否计入系统睡眠因平台而异（std 不保证），
/// 只靠一次 6h `sleep` 时睡眠 / 休眠会把检查无限推后。轮询只看时间，不碰网络与磁盘。
const CHECK_TICK: Duration = Duration::from_secs(10 * 60);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
const PENDING_FILE: &str = "pending.json";
const PREF_AUTO_CHECK: &str = "general.auto_check_update";
const PREF_CHANNEL: &str = "general.update_channel";

/// 构造桌面 / headless `UpdateService` 所需的依赖。
pub struct UpdateParts {
    pub events: AgentEventHub,
    /// agent 数据目录；更新包落在其 `updates/` 下。
    pub data_dir: PathBuf,
    pub(crate) target: InstallTarget,
    /// 宿主自身下载的统一出口（daemon 的全局代理与插件改写）。
    pub egress: Egress,
    /// 安装成功后触发完全退出（带重启语义）。
    pub request_restart: Box<dyn Fn() + Send + Sync>,
}

/// 由宿主语言安装更新的宿主（原生 Android）。
pub trait UpdateDelegate: Send + Sync + 'static {
    /// 状态变化（已去重、下载进度已节流）。在服务内部锁内调用：实现必须立即返回，
    /// 不得阻塞或回调 `UpdateService`。
    fn status_changed(&self, status: &UpdateStatusDto);
    /// 已校验的安装包就绪且用户已确认安装：宿主发起系统安装。失败 / 取消经
    /// [`UpdateService::install_failed`] 回报；成功时系统替换应用并结束进程。
    fn install_package(&self, package: &Path, version: &str);
}

/// 构造 [`UpdateService::delegated`] 所需的依赖。
pub struct DelegatedParts {
    /// 应用私有目录；更新包落在其 `updates/` 下。
    pub data_dir: PathBuf,
    /// 已安装的应用版本（Android `versionName`）。
    pub current_version: String,
    pub target: InstallTarget,
    pub delegate: Arc<dyn UpdateDelegate>,
    /// 请求的 User-Agent 产品名（如 `fluxdown-android`）。
    pub user_agent_product: &'static str,
    /// 宿主自身下载的统一出口；本机主机未运行时退回系统默认网络。
    pub egress: Egress,
}

/// 状态发布与安装执行的宿主差异。
enum Host {
    /// 桌面 / headless agent：状态进事件总线，安装在本进程替换程序文件后重启。
    Agent {
        events: AgentEventHub,
        request_restart: Box<dyn Fn() + Send + Sync>,
    },
    /// 宿主语言执行安装（Android：交给安装程序）。
    Delegated(Arc<dyn UpdateDelegate>),
}

/// 可安装的更新候选。
#[derive(Clone, Debug)]
struct Candidate {
    version: String,
    tag: String,
    asset_name: String,
    size: u64,
    sha256: String,
}

struct Job {
    id: u64,
    cancel: CancellationToken,
}

struct Core {
    status: UpdateStatusDto,
    published: UpdateStatusDto,
    candidate: Option<Candidate>,
    job: Option<Job>,
    next_job: u64,
    last_progress: Option<Instant>,
    /// 用户取消过下载的版本：周期任务不再为它自动下载。
    declined_auto: Option<String>,
}

/// 上次安装尝试的落盘记录（跨重启）。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingRecord {
    target_version: String,
    from_version: String,
    asset_name: String,
    created_at_ms: u64,
    /// 发起安装时的渠道：未完成时据此重新检查（旧记录缺省为空 → 回退偏好 / 稳定版）。
    #[serde(default)]
    channel: String,
}

/// 一次检查的结果。
struct CheckOutcome {
    release: Option<ComponentRelease>,
    has_update: bool,
    notes: Vec<ReleaseNoteDto>,
    manual_reason: Option<UpdateManualReason>,
    manual_url: String,
    candidate: Option<Candidate>,
}

pub struct UpdateService {
    host: Host,
    current_version: String,
    updates_dir: PathBuf,
    target: InstallTarget,
    endpoint: Endpoint,
    egress: Egress,
    api_http: EgressClient,
    download_http: EgressClient,
    core: Mutex<Core>,
    /// 检查单飞。
    check_gate: AsyncMutex<()>,
    /// 同一时刻只有一个下载任务写 `.part`（取消后的旧任务先退出再轮到新任务）。
    download_gate: AsyncMutex<()>,
}

/// 放进初始 `AgentSnapshot.update` 的状态。
#[must_use]
pub(crate) fn initial_status(target: &InstallTarget) -> UpdateStatusDto {
    initial_status_for(APP_VERSION, target)
}

fn initial_status_for(current_version: &str, target: &InstallTarget) -> UpdateStatusDto {
    UpdateStatusDto {
        current_version: current_version.to_owned(),
        install_kind: target.kind,
        manual_reason: target.manual_reason,
        release_page_url: RELEASE_PAGE_URL.to_owned(),
        ..UpdateStatusDto::default()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// 不支持应用内安装的宿主（移动端内嵌 agent、测试）使用的安装目标。
#[must_use]
pub(crate) fn unsupported_target() -> InstallTarget {
    InstallTarget {
        kind: fluxdown_protocol::UpdateInstallKind::Unknown,
        component: install::ReleaseComponent::Desktop,
        asset_keys: Vec::new(),
        manual_reason: Some(UpdateManualReason::Unsupported),
    }
}

impl UpdateService {
    #[must_use]
    pub fn new(parts: UpdateParts) -> Self {
        Self::with_version(parts, APP_VERSION, Endpoint::resolve())
    }

    /// 不支持应用内安装的宿主（移动端内嵌 agent、测试）：只检查版本，永不下载安装。
    #[must_use]
    pub fn unsupported(events: AgentEventHub, data_dir: PathBuf, egress: Egress) -> Self {
        Self::new(UpdateParts {
            events,
            data_dir,
            target: unsupported_target(),
            egress,
            request_restart: Box::new(|| {}),
        })
    }

    /// 宿主语言安装的应用（原生 Android）：检查 / 下载 / 校验与桌面同一状态机，
    /// 安装交给 [`UpdateDelegate::install_package`]。不可解析的版本（开发构建）只能手动升级。
    #[must_use]
    pub fn delegated(parts: DelegatedParts) -> Self {
        Self::delegated_with(parts, Endpoint::resolve())
    }

    fn delegated_with(parts: DelegatedParts, endpoint: Endpoint) -> Self {
        let mut target = parts.target;
        if target.manual_reason.is_none()
            && is_newer(&parts.current_version, &parts.current_version).is_err()
        {
            target.manual_reason = Some(UpdateManualReason::UnofficialBuild);
        }
        Self::build(
            Host::Delegated(parts.delegate),
            &parts.data_dir,
            target,
            &parts.current_version,
            endpoint,
            parts.user_agent_product,
            parts.egress,
        )
    }

    fn with_version(parts: UpdateParts, current_version: &str, endpoint: Endpoint) -> Self {
        Self::build(
            Host::Agent {
                events: parts.events,
                request_restart: parts.request_restart,
            },
            &parts.data_dir,
            parts.target,
            current_version,
            endpoint,
            "fluxdown-agent",
            parts.egress,
        )
    }

    fn build(
        host: Host,
        data_dir: &Path,
        target: InstallTarget,
        current_version: &str,
        endpoint: Endpoint,
        product: &str,
        egress: Egress,
    ) -> Self {
        let user_agent = format!("{product}/{current_version}");
        let api_agent = user_agent.clone();
        let api_http = EgressClient::new(move || {
            crate::tls::client_builder(crate::tls::HttpVersion::Any)
                .connect_timeout(Duration::from_secs(10))
                .timeout(REQUEST_TIMEOUT)
                .user_agent(api_agent.clone())
        });
        // 更新包体积大：不设整体超时，只限制连接与读停顿。
        let download_http = EgressClient::new(move || {
            crate::tls::client_builder(crate::tls::HttpVersion::Any)
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(30))
                .user_agent(user_agent.clone())
        });
        let status = initial_status_for(current_version, &target);
        Self {
            host,
            current_version: current_version.to_owned(),
            updates_dir: data_dir.join("updates"),
            target,
            endpoint,
            egress,
            api_http,
            download_http,
            core: Mutex::new(Core {
                published: status.clone(),
                status,
                candidate: None,
                job: None,
                next_job: 0,
                last_progress: None,
                declined_auto: None,
            }),
            check_gate: AsyncMutex::new(()),
            download_gate: AsyncMutex::new(()),
        }
    }

    /// 启动后台任务：清理上次替换残留、修复 macOS 已安装 bundle 的隔离标记、对账 `pending.json`，
    /// `auto_check` 时进入周期检查。只用于 agent 宿主；委托宿主由 App 自行调度并调用 [`Self::reconcile`]。
    pub fn start(self: &Arc<Self>, cancel: CancellationToken, auto_check: bool) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = tokio::task::spawn_blocking(install::cleanup_leftovers).await {
                tracing::debug!(error = %error, "update leftover cleanup task failed");
            }
            #[cfg(target_os = "macos")]
            if service.target.kind == fluxdown_protocol::UpdateInstallKind::MacosApp {
                install::heal_installed_quarantine().await;
            }
            service.reconcile().await;
            if auto_check {
                service.periodic(cancel).await;
            }
        });
    }

    #[must_use]
    pub fn status(&self) -> UpdateStatusDto {
        self.lock().status.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 发布与上次已发布不同的状态。
    fn commit(&self, core: &mut Core) {
        if core.status != core.published {
            core.published = core.status.clone();
            match &self.host {
                Host::Agent { events, .. } => {
                    events.publish(AgentEvent::UpdateChanged(core.status.clone()));
                }
                Host::Delegated(delegate) => delegate.status_changed(&core.status),
            }
        }
    }

    fn mutate<R>(&self, change: impl FnOnce(&mut Core) -> R) -> R {
        let mut core = self.lock();
        let result = change(&mut core);
        self.commit(&mut core);
        result
    }

    fn progress(&self, job_id: u64, bytes: u64) {
        let mut core = self.lock();
        if core.job.as_ref().map(|job| job.id) != Some(job_id) {
            return;
        }
        core.status.downloaded_bytes = bytes;
        let due = core
            .last_progress
            .is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL);
        let finished = core.status.asset_size > 0 && bytes >= core.status.asset_size;
        if due || finished {
            core.last_progress = Some(Instant::now());
            self.commit(&mut core);
        }
    }

    // ------------------------------------------------------------------ 检查

    /// 查询渠道最新版本；返回检查后的状态。
    pub async fn check(self: &Arc<Self>, channel: &str) -> Result<UpdateStatusDto, UpdateError> {
        self.check_inner(channel, false).await
    }

    /// `silent`：周期检查失败时静默恢复检查前的状态，不把界面翻成 Failed。
    async fn check_inner(
        self: &Arc<Self>,
        channel: &str,
        silent: bool,
    ) -> Result<UpdateStatusDto, UpdateError> {
        let channel = normalize_channel(channel)?;
        let _gate = match self.check_gate.try_lock() {
            Ok(gate) => gate,
            Err(_) => {
                // 单飞：等在途检查结束，直接用它的结果。
                drop(self.check_gate.lock().await);
                return Ok(self.status());
            }
        };
        // `Some(版本)`：检查前已有就绪包；查询期间不翻 Checking，保留可安装状态。
        let (previous, ready) = {
            let mut core = self.lock();
            match core.status.phase {
                UpdatePhase::Downloading | UpdatePhase::Installing => {
                    return Ok(core.status.clone());
                }
                UpdatePhase::Ready => (
                    core.status.clone(),
                    Some(core.status.latest_version.clone()),
                ),
                _ => {
                    let previous = core.status.clone();
                    core.status.phase = UpdatePhase::Checking;
                    core.status.channel = channel.to_owned();
                    core.status.failure = None;
                    core.status.error_detail.clear();
                    self.commit(&mut core);
                    (previous, None)
                }
            }
        };
        match self.fetch_outcome(channel).await {
            Ok(outcome) => Ok(self.mutate(|core| {
                if let Some(ready_version) = &ready {
                    // 检查期间用户可能已点安装：只在仍就绪且查到更新的版本时取代旧包。
                    let superseded = core.status.phase == UpdatePhase::Ready
                        && outcome.release.as_ref().is_some_and(|release| {
                            is_newer(&release.version, ready_version).unwrap_or(false)
                        });
                    if !superseded {
                        return core.status.clone();
                    }
                    core.status.channel = channel.to_owned();
                }
                apply_outcome(
                    core,
                    outcome,
                    &self.target,
                    self.endpoint.release_page_url(),
                );
                core.status.clone()
            })),
            Err(error) if ready.is_some() => {
                tracing::debug!(error = %error, "update check while ready failed; keeping ready package");
                Ok(self.status())
            }
            Err(error) => {
                self.mutate(|core| {
                    if silent {
                        core.status = previous;
                    } else {
                        core.status.phase = UpdatePhase::Failed;
                        core.status.failure = Some(match error {
                            UpdateError::Http(_)
                            | UpdateError::Status(_)
                            | UpdateError::Client(_) => UpdateFailure::Network,
                            _ => UpdateFailure::Unknown,
                        });
                        core.status.error_detail = format!("{error:#}");
                    }
                });
                Err(error)
            }
        }
    }

    async fn fetch_outcome(&self, channel: &str) -> Result<CheckOutcome, UpdateError> {
        // 版本清单与校验和是完整性锚：只走统一出口的代理，不交给插件改写。
        let plan = self
            .egress
            .plan(&self.endpoint.release_page_url(), false)
            .await;
        let http = self.api_http.get(&plan.network).await?;
        let http = &http;
        let raw = self.fetch_release(http, channel).await?;
        let Some(release) = raw.component(&self.target, &self.endpoint) else {
            return Ok(CheckOutcome {
                release: None,
                has_update: false,
                notes: Vec::new(),
                manual_reason: self.target.manual_reason,
                manual_url: String::new(),
                candidate: None,
            });
        };
        let has_update = is_newer(&release.version, &self.current_version).unwrap_or(false);
        if !has_update {
            return Ok(CheckOutcome {
                release: Some(release),
                has_update,
                notes: Vec::new(),
                manual_reason: self.target.manual_reason,
                manual_url: String::new(),
                candidate: None,
            });
        }
        let notes = self.fetch_notes(http, channel).await;
        let manual_url = release
            .asset
            .as_ref()
            .map(|asset| asset.download_url.clone())
            .unwrap_or_default();
        let mut manual_reason = self.target.manual_reason;
        let mut candidate = None;
        if manual_reason.is_none() {
            match &release.asset {
                None => manual_reason = Some(UpdateManualReason::NoAsset),
                Some(asset) => match self.fetch_checksum(http, &release, &asset.name).await? {
                    None => manual_reason = Some(UpdateManualReason::NoAsset),
                    Some(sha256) => {
                        manual_reason = install::preflight(&self.target).await;
                        if manual_reason.is_none() {
                            candidate = Some(Candidate {
                                version: release.version.clone(),
                                tag: release.tag.clone(),
                                asset_name: asset.name.clone(),
                                size: asset.size,
                                sha256,
                            });
                        }
                    }
                },
            }
        }
        Ok(CheckOutcome {
            release: Some(release),
            has_update,
            notes,
            manual_reason,
            manual_url,
            candidate,
        })
    }

    /// 错峰竞速请求各官方站点的 `/api/release`：上次成功的站点先发，[`Endpoint::stagger`]
    /// 内无结果（或已失败）才发下一个；首个成功者胜出并成为后续请求的站点。全部失败返回
    /// 最先完成的错误。
    async fn fetch_release(
        &self,
        http: &reqwest::Client,
        channel: &str,
    ) -> Result<RawRelease, UpdateError> {
        let mut queue = self.endpoint.attempt_order().into_iter();
        let mut attempts = JoinSet::new();
        let mut first_error = None;
        let spawn = |attempts: &mut JoinSet<_>, index: usize| {
            let Some(url) = self.endpoint.release_url_at(index, channel) else {
                return;
            };
            let http = http.clone();
            attempts.spawn(async move { (index, get_json::<RawRelease>(&http, &url).await) });
        };
        if let Some(index) = queue.next() {
            spawn(&mut attempts, index);
        }
        loop {
            let more = queue.len() > 0;
            tokio::select! {
                joined = attempts.join_next() => {
                    let result = match joined {
                        Some(Ok((index, result))) => result.map(|raw| (index, raw)),
                        Some(Err(error)) => Err(UpdateError::Decode(format!("release request task failed: {error}"))),
                        None => Err(UpdateError::Decode("no update site configured".to_owned())),
                    };
                    match result {
                        Ok((index, raw)) => {
                            self.endpoint.set_active(index);
                            return Ok(raw);
                        }
                        Err(error) => {
                            tracing::debug!(error = %error, "update site request failed");
                            let error = first_error.take().unwrap_or(error);
                            if let Some(index) = queue.next() {
                                spawn(&mut attempts, index);
                            } else if attempts.is_empty() {
                                return Err(error);
                            }
                            first_error = Some(error);
                        }
                    }
                }
                () = tokio::time::sleep(self.endpoint.stagger()), if more => {
                    if let Some(index) = queue.next() {
                        spawn(&mut attempts, index);
                    }
                }
            }
        }
    }

    /// 组件哨兵优先，回退合并清单；都没有该资产的条目 → `None`。
    async fn fetch_checksum(
        &self,
        http: &reqwest::Client,
        release: &ComponentRelease,
        asset_name: &str,
    ) -> Result<Option<String>, UpdateError> {
        for file in checksum_file_names(self.target.component) {
            let url = self.endpoint.package_url(file, &release.tag)?;
            let Some(text) = get_text_optional(http, &url).await? else {
                continue;
            };
            if let Some(sha256) = find_checksum(&text, asset_name) {
                return Ok(Some(sha256));
            }
        }
        Ok(None)
    }

    /// 更新说明是附属信息：拉取失败只降级为空列表，不影响版本判定。
    async fn fetch_notes(&self, http: &reqwest::Client, channel: &str) -> Vec<ReleaseNoteDto> {
        let url = self.endpoint.changelog_url(channel, &self.current_version);
        match get_json::<ChangelogResponse>(http, &url).await {
            Ok(changelog) => changelog.into_notes(&self.current_version),
            Err(error) => {
                tracing::debug!(error = %error, "changelog fetch failed");
                Vec::new()
            }
        }
    }

    // ------------------------------------------------------------------ 下载 / 安装

    /// 后台下载更新包；幂等。
    pub async fn download(self: &Arc<Self>) -> Result<UpdateStatusDto, UpdateError> {
        self.refresh_candidate().await?;
        self.begin(false)
    }

    /// 一键更新：包未就绪时先下载（`installPending`），就绪后安装并重启。
    pub async fn install(self: &Arc<Self>) -> Result<UpdateStatusDto, UpdateError> {
        self.refresh_candidate().await?;
        self.begin(true)
    }

    /// 失败态可能没有候选包（上次安装未完成的对账结果只知道目标版本）：先重新检查拿到
    /// 资产与校验和，再进入下载 / 安装。
    async fn refresh_candidate(self: &Arc<Self>) -> Result<(), UpdateError> {
        let (stale, channel) = {
            let core = self.lock();
            (
                core.candidate.is_none()
                    && core.status.has_update
                    && core.status.phase == UpdatePhase::Failed,
                core.status.channel.clone(),
            )
        };
        if !stale {
            return Ok(());
        }
        let channel = if channel.is_empty() {
            self.pref(PREF_CHANNEL)
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "stable".to_owned())
        } else {
            channel
        };
        self.check_inner(&channel, false).await.map(drop)
    }

    fn begin(self: &Arc<Self>, install_now: bool) -> Result<UpdateStatusDto, UpdateError> {
        let mut core = self.lock();
        if !core.status.has_update {
            return Err(UpdateError::NoUpdate);
        }
        if let Some(reason) = core.status.manual_reason {
            return Err(UpdateError::Manual(reason));
        }
        match core.status.phase {
            UpdatePhase::Installing => return Ok(core.status.clone()),
            UpdatePhase::Downloading => {
                if install_now {
                    core.status.install_pending = true;
                    self.commit(&mut core);
                }
                return Ok(core.status.clone());
            }
            UpdatePhase::Ready => {
                if install_now && let Some(candidate) = core.candidate.clone() {
                    core.status.phase = UpdatePhase::Installing;
                    self.commit(&mut core);
                    self.spawn_install(candidate);
                }
                return Ok(core.status.clone());
            }
            UpdatePhase::Available | UpdatePhase::Failed => {}
            UpdatePhase::Idle | UpdatePhase::Checking | UpdatePhase::UpToDate => {
                return Err(UpdateError::NoUpdate);
            }
        }
        let Some(candidate) = core.candidate.clone() else {
            return Err(UpdateError::NoUpdate);
        };
        core.next_job += 1;
        let id = core.next_job;
        let cancel = CancellationToken::new();
        core.job = Some(Job {
            id,
            cancel: cancel.clone(),
        });
        core.last_progress = None;
        core.status.phase = UpdatePhase::Downloading;
        core.status.downloaded_bytes = 0;
        core.status.install_pending = install_now;
        core.status.failure = None;
        core.status.error_detail.clear();
        self.commit(&mut core);
        let status = core.status.clone();
        drop(core);
        tokio::spawn(Arc::clone(self).download_task(id, candidate, cancel));
        Ok(status)
    }

    /// 取消进行中的下载与待安装请求。
    pub fn cancel(&self) -> UpdateStatusDto {
        self.mutate(|core| {
            if core.status.phase == UpdatePhase::Downloading
                && let Some(job) = core.job.take()
            {
                job.cancel.cancel();
                core.declined_auto = core.candidate.as_ref().map(|c| c.version.clone());
                core.status.phase = UpdatePhase::Available;
                core.status.downloaded_bytes = 0;
                core.status.install_pending = false;
            }
            core.status.clone()
        })
    }

    async fn download_task(
        self: Arc<Self>,
        id: u64,
        candidate: Candidate,
        cancel: CancellationToken,
    ) {
        let _gate = self.download_gate.lock().await;
        if cancel.is_cancelled() {
            return;
        }
        self.prune_other_versions(&candidate.version).await;
        let result = self.run_download(id, &candidate, &cancel).await;
        let mut core = self.lock();
        if core.job.as_ref().map(|job| job.id) != Some(id) {
            return;
        }
        core.job = None;
        match result {
            Ok(_) => {
                core.status.downloaded_bytes = candidate.size.max(core.status.downloaded_bytes);
                if core.status.install_pending {
                    core.status.phase = UpdatePhase::Installing;
                    core.status.install_pending = false;
                    self.commit(&mut core);
                    self.spawn_install(candidate);
                } else {
                    core.status.phase = UpdatePhase::Ready;
                    self.commit(&mut core);
                }
            }
            Err(DownloadError::Cancelled) => {
                core.status.phase = UpdatePhase::Available;
                core.status.install_pending = false;
                self.commit(&mut core);
            }
            Err(error) => {
                tracing::warn!(error = %error, "update download failed");
                core.status.phase = UpdatePhase::Failed;
                core.status.install_pending = false;
                core.status.failure = Some(error.failure());
                core.status.error_detail = format!("{error:#}");
                self.commit(&mut core);
            }
        }
    }

    async fn run_download(
        &self,
        id: u64,
        candidate: &Candidate,
        cancel: &CancellationToken,
    ) -> Result<PathBuf, DownloadError> {
        let url = self
            .endpoint
            .package_url(&candidate.asset_name, &candidate.tag)
            .map_err(|error| DownloadError::InvalidUrl(format!("{error:#}")))?;
        // 安装包有独立来源的 SHA-256：可按下载任务同一规则经插件改写来源，失败回退原地址。
        let plan = self.egress.plan(&url, true).await;
        let http = self.download_http.get(&plan.network).await?;
        let dir = self.version_dir(&candidate.version);
        let mut last_error = None;
        for attempt in plan.attempts() {
            if last_error.is_some() {
                // 换来源：不把上一来源的残片接到这一来源上。
                discard_partial(&dir, &candidate.asset_name).await?;
            }
            let spec = DownloadSpec {
                url: &attempt.url,
                headers: &attempt.headers,
                dir: &dir,
                name: &candidate.asset_name,
                size: candidate.size,
                sha256: &candidate.sha256,
            };
            match download_verified(&http, &spec, cancel, |bytes| self.progress(id, bytes)).await {
                Ok(path) => return Ok(path),
                Err(error @ (DownloadError::Cancelled | DownloadError::Io { .. })) => {
                    return Err(error);
                }
                Err(error) => {
                    if let Some(plugin) = &attempt.plugin {
                        tracing::warn!(plugin, error = %error, "plugin-routed update download failed; falling back to the original address");
                    }
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or(DownloadError::InvalidUrl(url)))
    }

    fn version_dir(&self, version: &str) -> PathBuf {
        self.updates_dir.join(version)
    }

    /// 只保留目标版本的目录与 `pending.json`。
    async fn prune_other_versions(&self, keep: &str) {
        let mut entries = match tokio::fs::read_dir(&self.updates_dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::debug!(error = %error, "cannot list updates dir");
                return;
            }
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => return,
                Err(error) => {
                    tracing::debug!(error = %error, "cannot read updates dir entry");
                    return;
                }
            };
            let is_dir = entry.file_type().await.is_ok_and(|kind| kind.is_dir());
            if is_dir
                && entry.file_name().to_string_lossy() != keep
                && let Err(error) = tokio::fs::remove_dir_all(entry.path()).await
            {
                tracing::debug!(error = %error, path = %entry.path().display(), "cannot remove stale update dir");
            }
        }
    }

    fn spawn_install(self: &Arc<Self>, candidate: Candidate) {
        tokio::spawn(Arc::clone(self).install_task(candidate));
    }

    async fn install_task(self: Arc<Self>, candidate: Candidate) {
        let dir = self.version_dir(&candidate.version);
        let package = dir.join(&candidate.asset_name);
        match &self.host {
            Host::Agent {
                request_restart, ..
            } => {
                let work_dir = dir.join("staging");
                if let Err(error) = self.prepare_install(&candidate, Some(&work_dir)).await {
                    self.fail(UpdateFailure::Storage, format!("{error:#}"));
                    return;
                }
                match install::apply(&self.target, &package, &work_dir).await {
                    Ok(plan) => {
                        restart::schedule(plan);
                        request_restart();
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "update install failed");
                        self.remove_pending().await;
                        self.fail(error.failure(), format!("{error:#}"));
                    }
                }
            }
            Host::Delegated(delegate) => {
                if let Err(error) = self.prepare_install(&candidate, None).await {
                    self.fail(UpdateFailure::Storage, format!("{error:#}"));
                    return;
                }
                delegate.install_package(&package, &candidate.version);
            }
        }
    }

    /// 委托宿主回报安装失败或用户取消；`Installing` 之外的回报忽略。校验类失败（签名 /
    /// 包名 / 版本不符）同时丢弃该版本的安装包，重试时重新下载而不是复用同一个坏包。
    pub async fn install_failed(&self, failure: UpdateFailure, detail: String) -> UpdateStatusDto {
        let version = {
            let core = self.lock();
            if core.status.phase != UpdatePhase::Installing {
                return core.status.clone();
            }
            core.candidate
                .as_ref()
                .map(|candidate| candidate.version.clone())
        };
        tracing::warn!(?failure, detail = %detail, "update install failed");
        self.remove_pending().await;
        if failure == UpdateFailure::Verify
            && let Some(version) = version
        {
            let dir = self.version_dir(&version);
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::debug!(error = %error, path = %dir.display(), "cannot remove rejected update package");
                }
            }
        }
        self.fail(failure, detail);
        self.status()
    }

    /// 写 `pending.json`；`work_dir` 给出时重建空的暂存目录。
    async fn prepare_install(
        &self,
        candidate: &Candidate,
        work_dir: Option<&Path>,
    ) -> std::io::Result<()> {
        let record = PendingRecord {
            target_version: candidate.version.clone(),
            from_version: self.current_version.clone(),
            asset_name: candidate.asset_name.clone(),
            created_at_ms: now_ms(),
            channel: self.lock().status.channel.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?;
        let pending = self.updates_dir.join(PENDING_FILE);
        let temp = self.updates_dir.join(format!("{PENDING_FILE}.tmp"));
        tokio::fs::write(&temp, bytes).await?;
        tokio::fs::rename(&temp, &pending).await?;
        let Some(work_dir) = work_dir else {
            return Ok(());
        };
        match tokio::fs::remove_dir_all(work_dir).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        tokio::fs::create_dir_all(work_dir).await
    }

    async fn remove_pending(&self) {
        let pending = self.updates_dir.join(PENDING_FILE);
        match tokio::fs::remove_file(&pending).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::debug!(error = %error, "cannot remove pending update record"),
        }
    }

    fn fail(&self, failure: UpdateFailure, detail: String) {
        self.mutate(|core| {
            core.job = None;
            core.status.phase = UpdatePhase::Failed;
            core.status.install_pending = false;
            core.status.failure = Some(failure);
            core.status.error_detail = detail;
        });
    }

    // ------------------------------------------------------------------ 启动对账 / 周期

    /// 上次安装留下 `pending.json`：版本已是目标 → 成功、清理并返回该版本；否则报告未完成。
    pub async fn reconcile(&self) -> Option<String> {
        let pending = self.updates_dir.join(PENDING_FILE);
        let bytes = match tokio::fs::read(&pending).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::debug!(error = %error, "cannot read pending update record");
                return None;
            }
        };
        let record = match serde_json::from_slice::<PendingRecord>(&bytes) {
            Ok(record) => record,
            Err(error) => {
                tracing::debug!(error = %error, "invalid pending update record dropped");
                self.remove_pending().await;
                return None;
            }
        };
        let installed = record.target_version.trim_start_matches('v')
            == self.current_version.trim_start_matches('v');
        if installed {
            tracing::info!(version = %record.target_version, "update installed; cleaning update files");
            if let Err(error) = tokio::fs::remove_dir_all(&self.updates_dir).await {
                tracing::debug!(error = %error, "cannot clean updates dir");
            }
            return Some(record.target_version);
        }
        tracing::warn!(
            target = %record.target_version,
            running = %self.current_version,
            "previous update did not complete"
        );
        self.remove_pending().await;
        self.mutate(|core| {
            if core.status.phase != UpdatePhase::Idle {
                return;
            }
            core.status.phase = UpdatePhase::Failed;
            core.status.has_update =
                is_newer(&record.target_version, &self.current_version).unwrap_or(false);
            core.status.latest_version = record.target_version.clone();
            core.status.channel = record.channel.clone();
            core.status.failure = Some(UpdateFailure::InstallIncomplete);
            core.status.error_detail = format!(
                "running version {} after installing {}",
                self.current_version, record.target_version
            );
        });
        None
    }

    fn pref(&self, key: &str) -> Option<serde_json::Value> {
        match &self.host {
            Host::Agent { events, .. } => {
                events.inspect(|snapshot| snapshot.preferences.values.get(key).cloned())
            }
            Host::Delegated(_) => None,
        }
    }

    async fn periodic(self: &Arc<Self>, cancel: CancellationToken) {
        let mut due = SystemTime::now() + FIRST_CHECK_DELAY;
        loop {
            let wait = due
                .duration_since(SystemTime::now())
                .map_or(Duration::ZERO, |left| left.min(CHECK_TICK));
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
            let now = SystemTime::now();
            if let Ok(left) = due.duration_since(now)
                && !left.is_zero()
            {
                // 墙钟被往回拨：到期点不得比一个周期更远。
                if left > CHECK_INTERVAL {
                    due = now + CHECK_INTERVAL;
                }
                continue;
            }
            due = now + CHECK_INTERVAL;
            if self.pref(PREF_AUTO_CHECK).and_then(|value| value.as_bool()) == Some(false) {
                continue;
            }
            let channel = self
                .pref(PREF_CHANNEL)
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "stable".to_owned());
            let status = match self.check_inner(&channel, true).await {
                Ok(status) => status,
                Err(error) => {
                    tracing::debug!(error = %error, "periodic update check failed");
                    continue;
                }
            };
            // 下载中取消过、或在提醒窗口「跳过此版本」的版本不再自动下载。
            let cancelled =
                self.lock().declined_auto.as_deref() == Some(status.latest_version.as_str());
            let skipped = self
                .pref(fluxdown_protocol::update_prompt::SKIPPED_VERSION_KEY)
                .is_some_and(|value| value.as_str() == Some(status.latest_version.as_str()));
            let declined = cancelled || skipped;
            if status.phase == UpdatePhase::Available
                && status.has_update
                && status.manual_reason.is_none()
                && !declined
                && let Err(error) = self.download().await
            {
                tracing::debug!(error = %error, "automatic update download not started");
            }
        }
    }
}

fn apply_outcome(
    core: &mut Core,
    outcome: CheckOutcome,
    target: &InstallTarget,
    release_page_url: String,
) {
    let status = &mut core.status;
    status.phase = if outcome.has_update {
        UpdatePhase::Available
    } else {
        UpdatePhase::UpToDate
    };
    status.install_kind = target.kind;
    status.has_update = outcome.has_update;
    status.latest_version = outcome
        .release
        .as_ref()
        .map(|release| release.version.clone())
        .unwrap_or_default();
    status.manual_reason = outcome.manual_reason;
    let asset = outcome
        .release
        .as_ref()
        .and_then(|release| release.asset.as_ref())
        .filter(|_| outcome.has_update);
    status.asset_name = asset.map(|asset| asset.name.clone()).unwrap_or_default();
    status.asset_size = asset.map_or(0, |asset| asset.size);
    status.downloaded_bytes = 0;
    status.install_pending = false;
    status.download_url = outcome.manual_url;
    status.release_page_url = release_page_url;
    status.notes = outcome.notes;
    status.failure = None;
    status.error_detail.clear();
    status.checked_at_ms = now_ms();
    core.candidate = outcome.candidate;
}

async fn get_json<T: DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
) -> Result<T, UpdateError> {
    let response = http.get(url).send().await?;
    if !response.status().is_success() {
        return Err(UpdateError::Status(response.status().as_u16()));
    }
    response
        .json::<T>()
        .await
        .map_err(|error| UpdateError::Decode(error.to_string()))
}

/// 404 → `None`（清单文件不存在）；其他非成功状态按错误处理。
async fn get_text_optional(
    http: &reqwest::Client,
    url: &str,
) -> Result<Option<String>, UpdateError> {
    let response = http.get(url).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(UpdateError::Status(response.status().as_u16()));
    }
    Ok(Some(response.text().await?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use axum::Router;
    use axum::routing::get;
    use fluxdown_protocol::{AgentSnapshot, UpdateInstallKind};

    use super::install::ReleaseComponent;
    use super::release::FALLBACK_STAGGER;
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fluxdown-upd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn service(
        dir: &Path,
        current: &str,
        base: &str,
        manual_reason: Option<UpdateManualReason>,
    ) -> Arc<UpdateService> {
        let parts = UpdateParts {
            events: AgentEventHub::new(AgentSnapshot::default()),
            data_dir: dir.to_path_buf(),
            target: InstallTarget {
                kind: UpdateInstallKind::Docker,
                component: ReleaseComponent::Desktop,
                asset_keys: vec!["setup"],
                manual_reason,
            },
            egress: Egress::system_default(),
            request_restart: Box::new(|| {}),
        };
        Arc::new(UpdateService::with_version(
            parts,
            current,
            Endpoint::with_bases(&[base], true, FALLBACK_STAGGER),
        ))
    }

    fn events(service: &UpdateService) -> &AgentEventHub {
        match &service.host {
            Host::Agent { events, .. } => events,
            Host::Delegated(_) => unreachable!("agent host expected"),
        }
    }

    #[derive(Default)]
    struct RecordingDelegate {
        statuses: Mutex<Vec<UpdateStatusDto>>,
        installs: Mutex<Vec<(PathBuf, String)>>,
    }

    impl UpdateDelegate for RecordingDelegate {
        fn status_changed(&self, status: &UpdateStatusDto) {
            self.statuses.lock().unwrap().push(status.clone());
        }

        fn install_package(&self, package: &Path, version: &str) {
            self.installs
                .lock()
                .unwrap()
                .push((package.to_path_buf(), version.to_owned()));
        }
    }

    const APK: &[u8] = b"fake apk bytes";

    /// 官网形状的移动端发布：`mobile.assets` 只有 arm64 分包，`SHA256SUMS-mobile.txt` 带其校验和。
    async fn mobile_site(version: &'static str) -> String {
        mobile_site_live(Arc::new(Mutex::new(version))).await
    }

    fn mobile_apk(version: &str) -> String {
        format!("FluxDown-{version}-android-arm64-v8a.apk")
    }

    /// 同上，发布版本可在测试中途更换（模拟常驻期间出了新版）。
    async fn mobile_site_live(version: Arc<Mutex<&'static str>>) -> String {
        use sha2::Digest;
        let digest = hex::encode(sha2::Sha256::digest(APK));
        let release_version = version.clone();
        let app = Router::new()
            .route(
                "/api/release",
                get(move || {
                    let version = *release_version.lock().unwrap();
                    let apk = mobile_apk(version);
                    async move {
                        axum::Json(serde_json::json!({
                            "version": "9.9.9",
                            "assets": {},
                            "mobile": {
                                "version": version,
                                "tag": format!("v{version}"),
                                "assets": {
                                    "android_arm64": { "name": apk, "size": APK.len(), "download_url": format!("/api/download/{apk}") },
                                    "android_universal": null
                                }
                            }
                        }))
                    }
                }),
            )
            .route(
                "/api/changelog",
                get(|| async { axum::Json(serde_json::json!({ "releases": [] })) }),
            )
            .route(
                "/api/download/{name}",
                get(
                    move |axum::extract::Path(name): axum::extract::Path<String>| {
                        let apk = mobile_apk(*version.lock().unwrap());
                        let sums = format!("{digest}  {apk}\n");
                        async move {
                            if name == "SHA256SUMS-mobile.txt" {
                                Ok(sums.into_bytes())
                            } else if name == apk {
                                Ok(APK.to_vec())
                            } else {
                                Err(axum::http::StatusCode::NOT_FOUND)
                            }
                        }
                    },
                ),
            );
        serve(app).await
    }

    /// 常驻不重启：已就绪的包不能挡住后续发布；同版本 / 检查失败保留就绪包。
    #[tokio::test]
    async fn ready_package_is_superseded_only_by_newer_release() {
        let dir = temp_dir();
        let version = Arc::new(Mutex::new("2.0.0"));
        let base = mobile_site_live(version.clone()).await;
        let (service, _delegate) = delegated(
            &dir,
            "1.0.0",
            Endpoint::with_bases(&[&base], true, FALLBACK_STAGGER),
            &["arm64-v8a"],
        );
        service.check("stable").await.unwrap();
        service.download().await.unwrap();
        wait_phase(&service, UpdatePhase::Ready).await;

        let status = service.check("stable").await.unwrap();
        assert_eq!(status.phase, UpdatePhase::Ready, "same version keeps ready");
        assert_eq!(status.latest_version, "2.0.0");

        *version.lock().unwrap() = "2.1.0";
        let status = service.check("stable").await.unwrap();
        assert_eq!(status.phase, UpdatePhase::Available);
        assert_eq!(status.latest_version, "2.1.0");
        assert_eq!(status.asset_name, mobile_apk("2.1.0"));
        service.download().await.unwrap();
        wait_phase(&service, UpdatePhase::Ready).await;
        let updates = dir.join("updates");
        assert!(updates.join("2.1.0").join(mobile_apk("2.1.0")).exists());
        assert!(!updates.join("2.0.0").exists(), "superseded package pruned");

        // 已就绪时站点不可达：保留就绪包，不翻 Failed。
        let (offline, _delegate) = delegated(
            &dir,
            "1.0.0",
            Endpoint::with_bases(&[&dead_base().await], true, FALLBACK_STAGGER),
            &["arm64-v8a"],
        );
        offline.lock().status = service.status();
        let status = offline.check("stable").await.unwrap();
        assert_eq!(status.phase, UpdatePhase::Ready);
        assert_eq!(status.latest_version, "2.1.0");
        std::fs::remove_dir_all(dir).unwrap();
    }

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    /// 绑定后立即释放的端口：连接被拒绝。
    async fn dead_base() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    }

    fn delegated(
        dir: &Path,
        current: &str,
        endpoint: Endpoint,
        abis: &[&str],
    ) -> (Arc<UpdateService>, Arc<RecordingDelegate>) {
        let delegate = Arc::new(RecordingDelegate::default());
        let abis: Vec<String> = abis.iter().map(|abi| (*abi).to_owned()).collect();
        let service = UpdateService::delegated_with(
            DelegatedParts {
                data_dir: dir.to_path_buf(),
                current_version: current.to_owned(),
                target: InstallTarget::android(&abis, None),
                delegate: delegate.clone(),
                user_agent_product: "fluxdown-test",
                egress: Egress::system_default(),
            },
            endpoint,
        );
        (Arc::new(service), delegate)
    }

    async fn wait_phase(service: &UpdateService, phase: UpdatePhase) -> UpdateStatusDto {
        for _ in 0..200 {
            let status = service.status();
            if status.phase == phase {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("phase {phase:?} not reached: {:?}", service.status());
    }

    /// `Installing` 在校验与交接宿主之前就已提交：交接是异步的，需等到宿主真正收到安装请求。
    async fn wait_installs(delegate: &RecordingDelegate, count: usize) {
        for _ in 0..200 {
            if delegate.installs.lock().unwrap().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("host did not receive {count} install request(s)");
    }

    #[test]
    fn android_target_prefers_first_published_abi_then_universal() {
        let abis = |list: &[&str]| list.iter().map(|abi| (*abi).to_owned()).collect::<Vec<_>>();
        let arm = InstallTarget::android(&abis(&["arm64-v8a", "armeabi-v7a", "armeabi"]), None);
        assert_eq!(arm.asset_keys, ["android_arm64", "android_universal"]);
        assert_eq!(arm.component, ReleaseComponent::Mobile);
        assert_eq!(arm.kind, UpdateInstallKind::AndroidApk);
        // 32 位 x86 没有分包：跳到下一个可运行的 ABI。
        let x86 = InstallTarget::android(&abis(&["x86", "armeabi-v7a"]), None);
        assert_eq!(x86.asset_keys, ["android_armv7", "android_universal"]);
        let unknown = InstallTarget::android(&abis(&["mips"]), None);
        assert_eq!(unknown.asset_keys, ["android_universal"]);
    }

    #[tokio::test]
    async fn delegated_flow_downloads_verifies_and_hands_package_to_host() {
        let dir = temp_dir();
        let base = mobile_site("2.0.0").await;
        let (service, delegate) = delegated(
            &dir,
            "1.0.0",
            Endpoint::with_bases(&[&base], true, FALLBACK_STAGGER),
            &["arm64-v8a"],
        );
        let status = service.check("stable").await.unwrap();
        assert_eq!(status.phase, UpdatePhase::Available);
        assert_eq!(status.install_kind, UpdateInstallKind::AndroidApk);
        assert_eq!(status.manual_reason, None);
        assert_eq!(status.asset_name, "FluxDown-2.0.0-android-arm64-v8a.apk");
        assert_eq!(status.release_page_url, format!("{base}/changelog"));

        service.install().await.unwrap();
        wait_phase(&service, UpdatePhase::Installing).await;
        wait_installs(&delegate, 1).await;
        let installs = delegate.installs.lock().unwrap().clone();
        assert_eq!(installs.len(), 1);
        let (package, version) = &installs[0];
        assert_eq!(version, "2.0.0");
        assert_eq!(std::fs::read(package).unwrap(), APK);
        assert!(dir.join("updates").join(PENDING_FILE).exists());
        assert!(
            delegate
                .statuses
                .lock()
                .unwrap()
                .iter()
                .any(|status| status.phase == UpdatePhase::Downloading)
        );

        // 用户在系统确认框取消：回到失败态，包保留，重试直接复用（不重新下载）。
        let status = service
            .install_failed(UpdateFailure::ElevationCancelled, "aborted".to_owned())
            .await;
        assert_eq!(status.phase, UpdatePhase::Failed);
        assert_eq!(status.failure, Some(UpdateFailure::ElevationCancelled));
        assert!(!dir.join("updates").join(PENDING_FILE).exists());
        assert!(package.exists());
        service.install().await.unwrap();
        wait_phase(&service, UpdatePhase::Installing).await;
        wait_installs(&delegate, 2).await;
        assert_eq!(delegate.installs.lock().unwrap().len(), 2);

        // 签名 / 包名不符：丢弃该版本的包。
        service
            .install_failed(UpdateFailure::Verify, "signature mismatch".to_owned())
            .await;
        assert!(!package.exists());
        // 非安装中的回报被忽略。
        let status = service
            .install_failed(UpdateFailure::Install, "late".to_owned())
            .await;
        assert_eq!(status.failure, Some(UpdateFailure::Verify));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 插件给出的改写来源（如 GitHub 加速）内容不符：回退原地址，安装的仍是官方校验和对应的包；
    /// 版本清单与校验和（非 `verifiedPayload`）不交给插件。
    #[tokio::test]
    async fn tampered_plugin_route_falls_back_to_the_official_package() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct RewriteTo(String);

        #[async_trait::async_trait]
        impl crate::egress::EgressSource for RewriteTo {
            async fn plan(
                &self,
                params: fluxdown_protocol::EgressPlanParams,
            ) -> Result<fluxdown_protocol::EgressPlanDto, String> {
                Ok(if params.verified_payload {
                    fluxdown_protocol::EgressPlanDto {
                        url: self.0.clone(),
                        plugin: Some("test@accel".to_owned()),
                        ..Default::default()
                    }
                } else {
                    fluxdown_protocol::EgressPlanDto {
                        url: params.url,
                        ..Default::default()
                    }
                })
            }
        }

        let dir = temp_dir();
        let base = mobile_site("2.0.0").await;
        let mirror_hits = Arc::new(AtomicUsize::new(0));
        let hits = mirror_hits.clone();
        let mirror = serve(Router::new().route(
            "/pkg",
            get(move || {
                hits.fetch_add(1, Ordering::SeqCst);
                async { b"tampered bytes".to_vec() }
            }),
        ))
        .await;
        let source: Arc<dyn crate::egress::EgressSource> =
            Arc::new(RewriteTo(format!("{mirror}/pkg")));
        let delegate = Arc::new(RecordingDelegate::default());
        let service = Arc::new(UpdateService::delegated_with(
            DelegatedParts {
                data_dir: dir.clone(),
                current_version: "1.0.0".to_owned(),
                target: InstallTarget::android(&["arm64-v8a".to_owned()], None),
                delegate: delegate.clone(),
                user_agent_product: "fluxdown-test",
                egress: Egress::dynamic(move || Some(source.clone())),
            },
            Endpoint::with_bases(&[&base], true, FALLBACK_STAGGER),
        ));

        assert_eq!(
            service.check("stable").await.unwrap().phase,
            UpdatePhase::Available
        );
        service.install().await.unwrap();
        wait_installs(&delegate, 1).await;
        let (package, _) = delegate.installs.lock().unwrap()[0].clone();
        assert_eq!(std::fs::read(package).unwrap(), APK);
        assert_eq!(mirror_hits.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn delegated_dev_version_is_manual_only() {
        let dir = temp_dir();
        let base = mobile_site("2.0.0").await;
        let (service, _delegate) = delegated(
            &dir,
            "dev",
            Endpoint::with_bases(&[&base], true, FALLBACK_STAGGER),
            &["arm64-v8a"],
        );
        assert_eq!(
            service.status().manual_reason,
            Some(UpdateManualReason::UnofficialBuild)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn unreachable_primary_site_falls_back_and_sticks() {
        let dir = temp_dir();
        let dead = dead_base().await;
        let base = mobile_site("2.0.0").await;
        let endpoint = Endpoint::with_bases(&[&dead, &base], true, Duration::from_secs(30));
        let (service, _delegate) = delegated(&dir, "1.0.0", endpoint.clone(), &["arm64-v8a"]);
        let started = Instant::now();
        let status = service.check("stable").await.unwrap();
        // 连接被拒绝立即换站，不必等错峰间隔。
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(status.phase, UpdatePhase::Available);
        assert_eq!(status.release_page_url, format!("{base}/changelog"));
        assert_eq!(endpoint.attempt_order(), [1, 0]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn stalled_primary_site_loses_the_staggered_race() {
        let dir = temp_dir();
        let stalled = Router::new().route(
            "/api/release",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                axum::Json(serde_json::json!({}))
            }),
        );
        let stalled = serve(stalled).await;
        let base = mobile_site("2.0.0").await;
        let endpoint = Endpoint::with_bases(&[&stalled, &base], true, Duration::from_millis(50));
        let (service, _delegate) = delegated(&dir, "1.0.0", endpoint, &["arm64-v8a"]);
        let started = Instant::now();
        let status = service.check("stable").await.unwrap();
        assert!(started.elapsed() < REQUEST_TIMEOUT);
        assert_eq!(status.latest_version, "2.0.0");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn all_sites_failing_reports_network_failure() {
        let dir = temp_dir();
        let (first, second) = (dead_base().await, dead_base().await);
        let endpoint = Endpoint::with_bases(&[&first, &second], true, Duration::from_millis(50));
        let (service, _delegate) = delegated(&dir, "1.0.0", endpoint, &["arm64-v8a"]);
        assert!(matches!(
            service.check("stable").await,
            Err(UpdateError::Http(_))
        ));
        assert_eq!(service.status().failure, Some(UpdateFailure::Network));
        std::fs::remove_dir_all(dir).unwrap();
    }

    async fn site(version: &'static str) -> String {
        let app = Router::new()
            .route(
                "/api/release",
                get(move || async move {
                    axum::Json(serde_json::json!({
                        "version": version,
                        "tag": format!("v{version}"),
                        "assets": {
                            "setup": { "name": "FluxDown-setup.exe", "size": 5, "download_url": "/api/download/FluxDown-setup.exe" }
                        }
                    }))
                }),
            )
            .route(
                "/api/changelog",
                get(|| async {
                    axum::Json(serde_json::json!({ "releases": [
                        { "version": "2.0.0", "published_at": "", "body": "notes" },
                        { "version": "1.0.0", "published_at": "", "body": "old" }
                    ]}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    fn write_pending(dir: &Path, target: &str) {
        let updates = dir.join("updates");
        std::fs::create_dir_all(updates.join(target)).unwrap();
        let record = PendingRecord {
            target_version: target.to_owned(),
            from_version: "1.0.0".to_owned(),
            asset_name: "a".to_owned(),
            created_at_ms: 1,
            channel: "frontier".to_owned(),
        };
        std::fs::write(
            updates.join(PENDING_FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn pending_with_matching_version_is_success_and_cleans_updates() {
        let dir = temp_dir();
        write_pending(&dir, "2.0.0");
        let service = service(&dir, "2.0.0", "http://127.0.0.1:9", None);
        assert_eq!(service.reconcile().await.as_deref(), Some("2.0.0"));
        assert!(!dir.join("updates").exists());
        assert_eq!(service.status().phase, UpdatePhase::Idle);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn pending_with_old_version_reports_install_incomplete() {
        let dir = temp_dir();
        write_pending(&dir, "2.0.0");
        let service = service(&dir, "1.0.0", "http://127.0.0.1:9", None);
        assert_eq!(service.reconcile().await, None);
        let status = service.status();
        assert_eq!(status.phase, UpdatePhase::Failed);
        assert_eq!(status.failure, Some(UpdateFailure::InstallIncomplete));
        assert_eq!(status.latest_version, "2.0.0");
        // 重试按发起安装时的渠道重新检查。
        assert_eq!(status.channel, "frontier");
        assert!(status.has_update);
        assert!(!dir.join("updates").join(PENDING_FILE).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn check_reports_up_to_date_and_publishes_once() {
        let dir = temp_dir();
        let base = site("1.0.0").await;
        let service = service(&dir, "1.0.0", &base, None);
        let status = service.check("stable").await.unwrap();
        assert_eq!(status.phase, UpdatePhase::UpToDate);
        assert_eq!(status.latest_version, "1.0.0");
        assert!(!status.has_update);
        assert!(matches!(
            service.download().await,
            Err(UpdateError::NoUpdate)
        ));
        assert!(matches!(
            service.install().await,
            Err(UpdateError::NoUpdate)
        ));
        let sequence = events(&service).snapshot().sequence;
        service.check("stable").await.unwrap();
        // Checking → UpToDate 两次迁移都有变化；状态相同的重复提交不会额外发布。
        assert_eq!(events(&service).snapshot().sequence, sequence + 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn managed_installs_get_manual_reason_and_reject_download() {
        let dir = temp_dir();
        let base = site("2.0.0").await;
        let service = service(
            &dir,
            "1.0.0",
            &base,
            Some(UpdateManualReason::ManagedPackage),
        );
        let status = service.check("frontier").await.unwrap();
        assert_eq!(status.phase, UpdatePhase::Available);
        assert_eq!(status.channel, "frontier");
        assert!(status.has_update);
        assert_eq!(
            status.manual_reason,
            Some(UpdateManualReason::ManagedPackage)
        );
        assert_eq!(status.asset_name, "FluxDown-setup.exe");
        assert_eq!(
            status.download_url,
            format!("{base}/api/download/FluxDown-setup.exe")
        );
        assert_eq!(status.notes.len(), 1);
        assert!(matches!(
            service.download().await,
            Err(UpdateError::Manual(UpdateManualReason::ManagedPackage))
        ));
        assert!(matches!(
            service.install().await,
            Err(UpdateError::Manual(_))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn missing_checksum_makes_update_manual_only() {
        let dir = temp_dir();
        let base = site("2.0.0").await;
        // 站点没有 SHA256SUMS 文件（404）→ NoAsset。
        let service = service(&dir, "1.0.0", &base, None);
        let status = service.check("stable").await.unwrap();
        assert_eq!(status.phase, UpdatePhase::Available);
        assert_eq!(status.manual_reason, Some(UpdateManualReason::NoAsset));
        assert!(matches!(
            service.download().await,
            Err(UpdateError::Manual(UpdateManualReason::NoAsset))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn invalid_channel_fails_before_any_state_change() {
        let dir = temp_dir();
        let service = service(&dir, "1.0.0", "http://127.0.0.1:9", None);
        assert!(matches!(
            service.check("nightly").await,
            Err(UpdateError::InvalidChannel(_))
        ));
        assert_eq!(service.status().phase, UpdatePhase::Idle);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
