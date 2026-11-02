use std::sync::Arc;

use base64::Engine as _;
use fluxdown_protocol::method;
use fluxdown_ui_downloads::{DownloadsCommand, DownloadsPort, DownloadsResult, PortFuture};
use serde_json::{Value, json};

use crate::agent_client::AgentClient;

pub struct AgentDownloadsPort {
    client: Arc<AgentClient>,
}

impl AgentDownloadsPort {
    #[must_use]
    pub fn new(client: Arc<AgentClient>) -> Self {
        Self { client }
    }
}

/// Shared by every window through AgentClient. Waiting submissions retain their
/// unsubmitted commands in run_submission, including after their window closes.
pub(crate) struct SubmissionGate {
    state: tokio::sync::watch::Sender<SubmissionState>,
    boundary: Arc<tokio::sync::Mutex<()>>,
    controls: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    pub(crate) interruption: tokio::sync::watch::Sender<Option<String>>,
    /// 系统打开 / 拖入的链接或种子文件被 agent 拒绝（分享链接解析失败、不支持等）：
    /// 界面据此提示用户，而不是静默什么都不发生。每次发送都会唤醒订阅者。
    pub(crate) capture_failure: tokio::sync::watch::Sender<Option<fluxdown_protocol::RpcErrorData>>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct SubmissionState {
    ready: bool,
    paused: bool,
    control_pending: bool,
    revision: u64,
    pause_generation: u64,
}

impl SubmissionState {
    /// 导入被「全部暂停」或结果不确定的提交挂起，直到「全部开始」成功才解除。
    pub(crate) fn suspended(self) -> bool {
        self.paused
    }
}

#[derive(Clone, Copy, Default)]
struct ControlRevision {
    sequence: u64,
    pause_generation: u64,
}

impl Default for SubmissionGate {
    fn default() -> Self {
        Self {
            state: tokio::sync::watch::channel(SubmissionState::default()).0,
            boundary: Arc::new(tokio::sync::Mutex::new(())),
            controls: std::sync::Mutex::new(None),
            interruption: tokio::sync::watch::channel(None).0,
            capture_failure: tokio::sync::watch::channel(None).0,
        }
    }
}

impl SubmissionGate {
    fn control_turn(
        &self,
    ) -> (
        Option<tokio::sync::oneshot::Receiver<()>>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (done, next) = tokio::sync::oneshot::channel();
        let mut controls = match self.controls.lock() {
            Ok(controls) => controls,
            Err(error) => {
                log::error!("recovering poisoned submission control queue");
                error.into_inner()
            }
        };
        let previous = controls.replace(next);
        (previous, done)
    }

    pub(crate) fn set_ready(&self, ready: bool) {
        self.state.send_modify(|state| state.ready = ready);
    }

    /// 订阅闸门状态（主窗口据此让「全部开始」在导入挂起时保持可点）。
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<SubmissionState> {
        self.state.subscribe()
    }

    fn begin_control(&self, pause: bool) -> ControlRevision {
        let mut revision = ControlRevision::default();
        self.state.send_modify(|state| {
            // Resume only blocks admission temporarily; its failure must not
            // create a new persistent pause (including overlapping resumes).
            state.paused |= pause;
            if pause {
                state.pause_generation = state.pause_generation.wrapping_add(1);
            }
            state.control_pending = true;
            state.revision = state.revision.wrapping_add(1);
            revision = ControlRevision {
                sequence: state.revision,
                pause_generation: state.pause_generation,
            };
        });
        revision
    }

    fn pause(&self) -> ControlRevision {
        let revision = self.begin_control(true);
        self.complete_control(revision, false, false);
        revision
    }

    #[cfg(test)]
    fn resume(&self, revision: ControlRevision) {
        self.complete_control(revision, true, true);
    }

    fn complete_control(&self, revision: ControlRevision, resume: bool, succeeded: bool) {
        self.state.send_modify(|state| {
            // A later Resume does not revoke an earlier successful Resume.
            // Only a newer Pause or uncertain submission creates a new intent.
            if resume && succeeded && state.pause_generation == revision.pause_generation {
                state.paused = false;
            }
            if state.revision == revision.sequence {
                state.control_pending = false;
            }
        });
    }

    pub(crate) async fn admit(
        &self,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, fluxdown_protocol::RpcErrorData> {
        let mut state = self.state.subscribe();
        loop {
            let current = *state.borrow_and_update();
            if current.ready && !current.paused && !current.control_pending {
                let guard = self.boundary.clone().lock_owned().await;
                let current = *state.borrow();
                if current.ready && !current.paused && !current.control_pending {
                    return Ok(guard);
                }
            }
            if state.changed().await.is_err() {
                log::error!("submission admission channel closed");
                return Err(fluxdown_protocol::RpcErrorData::new(
                    fluxdown_protocol::ApplicationErrorCode::Unavailable,
                    true,
                ));
            }
        }
    }
}

/// 失败后结果不确定的提交：服务端可能已落库，重发会造成重复，所以结果不确定时暂停整条导入。
/// `CreateMany` 按 `batchId` 幂等，调用方可原样重发，不属于此类。
fn is_non_idempotent_submission(command: &DownloadsCommand) -> bool {
    matches!(
        command,
        DownloadsCommand::Create(_)
            | DownloadsCommand::CreateGroup { .. }
            | DownloadsCommand::SubmitTorrentFile { .. }
            | DownloadsCommand::SeedLocal { .. }
            | DownloadsCommand::Redownload(..)
    ) || matches!(command, DownloadsCommand::CaptureResolve(params) if params.accepted)
}

fn queue_fields(fields: &fluxdown_ui_downloads::QueueFields) -> Value {
    json!({
        "name": fields.name,
        "speedLimitKbps": fields.speed_limit_kbps,
        "uploadLimitKbps": fields.upload_limit_kbps,
        "maxConcurrent": fields.max_concurrent,
        "defaultSaveDir": fields.default_save_dir,
        "defaultSegments": fields.default_segments,
        "defaultUserAgent": fields.default_user_agent,
    })
}

impl DownloadsPort for AgentDownloadsPort {
    fn execute(&self, command: DownloadsCommand) -> PortFuture<DownloadsResult> {
        let submission = is_non_idempotent_submission(&command);
        let source = match &command {
            DownloadsCommand::Create(params) => Some(params.request.url.clone()),
            DownloadsCommand::CreateGroup { context, .. } => Some(context.url.clone()),
            DownloadsCommand::CaptureResolve(params) => {
                params.request.as_ref().map(|request| request.url.clone())
            }
            DownloadsCommand::SubmitTorrentFile { path, .. }
            | DownloadsCommand::SeedLocal { path, .. } => Some(path.clone()),
            DownloadsCommand::Redownload(request, _) => Some(request.url.clone()),
            _ => None,
        };
        let pause = matches!(&command, DownloadsCommand::PauseAll);
        let resume = matches!(&command, DownloadsCommand::ResumeAll);
        // Close admission synchronously, before this RPC future can be scheduled.
        let revision = (pause || resume).then(|| self.client.submissions.begin_control(pause));
        let turn = revision.map(|_| self.client.submissions.control_turn());
        let client = self.client.clone();
        let operation = self.execute_rpc(command);
        Box::pin(async move {
            let done = if let Some((previous, done)) = turn {
                if let Some(previous) = previous
                    && previous.await.is_err()
                {
                    log::debug!("previous global download command was cancelled");
                }
                Some(done)
            } else {
                None
            };
            let _boundary = if pause || resume {
                Some(client.submissions.boundary.lock().await)
            } else {
                None
            };
            let result = operation.await;
            if let Some(revision) = revision {
                client
                    .submissions
                    .complete_control(revision, resume, result.is_ok());
            }
            if submission
                && result.as_ref().is_err_and(|error| {
                    matches!(
                        error.code,
                        fluxdown_protocol::ApplicationErrorCode::Unavailable
                            | fluxdown_protocol::ApplicationErrorCode::Timeout
                    )
                })
            {
                // The server may have committed this command. Never retry it;
                // retain the remaining list until explicit ResumeAll after reconnect.
                client.submissions.pause();
                client.submissions.interruption.send_replace(source);
                log::warn!(
                    "download import suspended after an unavailable response; the uncertain command will not be retried"
                );
            }
            if let Some(done) = done
                && done.send(()).is_err()
            {
                log::debug!("global download command successor released");
            }
            result
        })
    }
}

impl AgentDownloadsPort {
    fn execute_rpc(&self, command: DownloadsCommand) -> PortFuture<DownloadsResult> {
        let client = self.client.clone();
        Box::pin(async move {
            let (method, params) = match command {
                DownloadsCommand::TaskActivity(query) => {
                    let page = client
                        .call(method::DAEMON_TASK_ACTIVITY, Some(query))
                        .await?;
                    return Ok(DownloadsResult::TaskActivity(page));
                }
                DownloadsCommand::TaskSpeedHistory(query) => {
                    let history = client
                        .call(method::DAEMON_TASK_SPEED_HISTORY, Some(query))
                        .await?;
                    return Ok(DownloadsResult::TaskSpeedHistory(history));
                }
                DownloadsCommand::BtStatus(params) => {
                    let status = client
                        .call(method::DAEMON_TASK_BT_STATUS, Some(params))
                        .await?;
                    return Ok(DownloadsResult::BtStatus(status));
                }
                DownloadsCommand::BtGuardAddEntry(params) => {
                    (method::DAEMON_BT_GUARD_ENTRIES_ADD, serialize(params)?)
                }
                DownloadsCommand::BtGuardEvidence { ip_group } => {
                    let (entries_params, events_params) = evidence_params(&ip_group);
                    let entries: fluxdown_protocol::BtGuardEntriesListDto = client
                        .call(method::DAEMON_BT_GUARD_ENTRIES_LIST, Some(entries_params))
                        .await?;
                    let events: fluxdown_protocol::BtGuardEventsListDto = client
                        .call(method::DAEMON_BT_GUARD_EVENTS_LIST, Some(events_params))
                        .await?;
                    return Ok(DownloadsResult::BtGuardEvidence(
                        fluxdown_ui_downloads::BtGuardEvidence {
                            entries: entries.items,
                            events: events.items,
                        },
                    ));
                }
                DownloadsCommand::FileIcon(params) => {
                    let icon: fluxdown_protocol::PlatformFileIconDto = client
                        .call(method::AGENT_PLATFORM_FILE_ICON, Some(params))
                        .await?;
                    let png = base64::engine::general_purpose::STANDARD
                        .decode(icon.png)
                        .map_err(|_| internal_error())?;
                    return Ok(DownloadsResult::FileIcon(png));
                }
                DownloadsCommand::CreateMany(params) => {
                    let result = client
                        .call(method::DAEMON_TASK_CREATE_MANY, Some(params))
                        .await?;
                    return Ok(DownloadsResult::CreateMany(result));
                }
                DownloadsCommand::Create(params) => {
                    (method::DAEMON_TASK_CREATE, serialize(params)?)
                }
                DownloadsCommand::Pause { task_id } => {
                    (method::DAEMON_TASK_PAUSE, json!({ "taskId": task_id }))
                }
                DownloadsCommand::Resume { task_id } => {
                    (method::DAEMON_TASK_RESUME, json!({ "taskId": task_id }))
                }
                DownloadsCommand::Rename { task_id, file_name } => (
                    method::DAEMON_TASK_RENAME,
                    json!({ "taskId": task_id, "fileName": file_name }),
                ),
                DownloadsCommand::ChangeUrl { task_id, url } => (
                    method::DAEMON_TASK_CHANGE_URL,
                    serialize(fluxdown_protocol::ChangeTaskUrlParams { task_id, url })?,
                ),
                DownloadsCommand::SetSegments { task_id, segments } => (
                    method::DAEMON_TASK_SET_SEGMENTS,
                    serialize(fluxdown_protocol::SetTaskSegmentsParams { task_id, segments })?,
                ),
                DownloadsCommand::VerifyTask { task_id } => {
                    let result = client
                        .call(
                            method::DAEMON_TASK_VERIFY,
                            Some(json!({ "taskId": task_id })),
                        )
                        .await?;
                    return Ok(DownloadsResult::TaskVerify(result));
                }
                DownloadsCommand::TorrentShare {
                    task_id,
                    include_torrent,
                } => {
                    let share: fluxdown_protocol::TorrentShareDto = client
                        .call(
                            method::DAEMON_TASK_TORRENT_SHARE,
                            Some(serialize(fluxdown_protocol::DaemonTorrentShareParams {
                                task_id,
                                include_torrent,
                            })?),
                        )
                        .await?;
                    let torrent = share
                        .torrent_b64
                        .map(|encoded| base64::engine::general_purpose::STANDARD.decode(encoded))
                        .transpose()
                        .map_err(|_| internal_error())?;
                    return Ok(DownloadsResult::TorrentShare(
                        fluxdown_ui_downloads::TorrentShare {
                            name: share.name,
                            magnet: share.magnet,
                            torrent,
                        },
                    ));
                }
                DownloadsCommand::SeedLocal {
                    path,
                    torrent_path,
                    trackers,
                    unlimited,
                } => (
                    method::DAEMON_TASK_SEED_LOCAL,
                    serialize(fluxdown_protocol::DaemonSeedLocalParams {
                        path,
                        torrent_path,
                        trackers,
                        unlimited,
                    })?,
                ),
                DownloadsCommand::Delete {
                    task_id,
                    delete_files,
                } => (
                    method::DAEMON_TASK_DELETE,
                    json!({ "taskId": task_id, "deleteFiles": delete_files }),
                ),
                DownloadsCommand::PauseMany { task_ids } => (
                    method::DAEMON_TASK_PAUSE_MANY,
                    json!({ "taskIds": task_ids }),
                ),
                DownloadsCommand::ResumeMany { task_ids } => (
                    method::DAEMON_TASK_RESUME_MANY,
                    json!({ "taskIds": task_ids }),
                ),
                DownloadsCommand::DeleteMany {
                    task_ids,
                    delete_files,
                } => (
                    method::DAEMON_TASK_DELETE_MANY,
                    json!({ "taskIds": task_ids, "deleteFiles": delete_files }),
                ),
                DownloadsCommand::PauseAll => (method::DAEMON_TASK_PAUSE_ALL, json!({})),
                DownloadsCommand::ResumeAll => (method::DAEMON_TASK_RESUME_ALL, json!({})),
                DownloadsCommand::IgnorePluginRetry { task_id } => (
                    method::DAEMON_PLUGIN_IGNORE_RETRY,
                    json!({ "taskId": task_id }),
                ),
                DownloadsCommand::ToggleBoost { task_id } => {
                    (method::DAEMON_QUEUE_BOOST, json!({ "id": task_id }))
                }
                DownloadsCommand::Redownload(request, task_id) => {
                    client
                        .call::<Value, Value>(
                            method::DAEMON_TASK_DELETE,
                            Some(json!({ "taskId": task_id, "deleteFiles": true })),
                        )
                        .await?;
                    (
                        method::DAEMON_TASK_CREATE,
                        serialize(fluxdown_protocol::DaemonCreateTaskParams {
                            request: *request,
                            torrent_blob_id: None,
                            unattended: false,
                            hint_file_size: None,
                            name_hint: None,
                            category_route: None,
                        })?,
                    )
                }
                DownloadsCommand::MoveToQueue { task_id, queue_id } => (
                    method::DAEMON_QUEUE_MOVE_TASK,
                    json!({ "taskId": task_id, "queueId": queue_id }),
                ),
                DownloadsCommand::SetSeedLimits { task_id, limits } => (
                    method::DAEMON_TASK_SET_SEED_LIMITS,
                    json!({
                        "taskId": task_id,
                        "ratioLimitMilli": limits.ratio_limit_milli,
                        "postRatioLimitMilli": limits.post_ratio_limit_milli,
                        "seedTimeLimitMinutes": limits.seed_time_limit_minutes,
                        "inactiveTimeLimitMinutes": limits.inactive_time_limit_minutes,
                        "uploadLimitBps": limits.upload_limit_bps,
                    }),
                ),
                DownloadsCommand::PatchConfig {
                    values,
                    expected_revision,
                } => (
                    method::DAEMON_CONFIG_PATCH,
                    json!({ "expectedRevision": expected_revision, "values": values }),
                ),
                DownloadsCommand::QueueCreate(fields) => {
                    (method::DAEMON_QUEUE_CREATE, queue_fields(&fields))
                }
                DownloadsCommand::QueueUpdate { queue_id, fields } => {
                    let mut params = queue_fields(&fields);
                    if let Value::Object(map) = &mut params {
                        map.insert("queueId".to_owned(), Value::String(queue_id));
                    }
                    (method::DAEMON_QUEUE_UPDATE, params)
                }
                DownloadsCommand::QueueSchedule {
                    queue_id,
                    enabled,
                    start_time,
                    stop_time,
                    days,
                } => (
                    method::DAEMON_QUEUE_SCHEDULE,
                    json!({
                        "queueId": queue_id,
                        "enabled": enabled,
                        "startTime": start_time,
                        "stopTime": stop_time,
                        "days": days,
                    }),
                ),
                DownloadsCommand::QueueStart { queue_id } => {
                    (method::DAEMON_QUEUE_START, json!({ "queueId": queue_id }))
                }
                DownloadsCommand::QueueStop { queue_id } => {
                    (method::DAEMON_QUEUE_STOP, json!({ "queueId": queue_id }))
                }
                DownloadsCommand::QueueDelete { queue_id } => {
                    (method::DAEMON_QUEUE_DELETE, json!({ "queueId": queue_id }))
                }
                DownloadsCommand::GroupPause { group_id } => {
                    (method::DAEMON_GROUP_PAUSE, json!({ "groupId": group_id }))
                }
                DownloadsCommand::GroupResume { group_id } => {
                    (method::DAEMON_GROUP_RESUME, json!({ "groupId": group_id }))
                }
                DownloadsCommand::GroupDelete {
                    group_id,
                    delete_files,
                } => (
                    method::DAEMON_GROUP_DELETE,
                    json!({ "groupId": group_id, "deleteFiles": delete_files }),
                ),
                DownloadsCommand::ResolveSelection(params) => {
                    (method::DAEMON_SELECTION_RESOLVE, serialize(params)?)
                }
                DownloadsCommand::CaptureResolve(params) => {
                    (method::AGENT_CAPTURE_RESOLVE, serialize(params)?)
                }
                DownloadsCommand::ResolvePreview {
                    request,
                    transaction_id,
                } => {
                    let preview = match transaction_id {
                        Some(transaction_id) => {
                            client
                                .call(
                                    method::AGENT_CAPTURE_PREVIEW,
                                    Some(fluxdown_protocol::CapturePreviewParams {
                                        transaction_id,
                                        request: *request,
                                    }),
                                )
                                .await?
                        }
                        None => {
                            client
                                .call(
                                    method::DAEMON_GROUP_RESOLVE_PREVIEW,
                                    Some(preview_request(*request)),
                                )
                                .await?
                        }
                    };
                    return Ok(DownloadsResult::Preview(preview));
                }
                DownloadsCommand::CreateGroup {
                    mut request,
                    context,
                    transaction_id,
                } => match transaction_id {
                    Some(transaction_id) => (
                        method::AGENT_CAPTURE_CREATE_GROUP,
                        serialize(fluxdown_protocol::CaptureCreateGroupParams {
                            transaction_id,
                            request: *request,
                            context: *context,
                        })?,
                    ),
                    None => {
                        apply_basic_auth(&mut request.extra_headers, &context);
                        let value: Value = client
                            .call(method::DAEMON_GROUP_CREATE, Some(request))
                            .await?;
                        if context.save_site_auth && !context.http_user.is_empty() {
                            let credentials = fluxdown_protocol::SiteAuthSaveRequest {
                                site: context.url,
                                user: context.http_user,
                                pass: context.http_password,
                            };
                            if let Err(error) = client
                                .call::<_, Value>(method::DAEMON_SITE_AUTH_SAVE, Some(credentials))
                                .await
                            {
                                log::warn!(
                                    "group created but HTTP credentials could not be saved: {:?}",
                                    error.code
                                );
                            }
                        }
                        return Ok(DownloadsResult::Value(value));
                    }
                },
                DownloadsCommand::SiteAuthMatch { url } => (
                    method::DAEMON_SITE_AUTH_MATCH,
                    serialize(fluxdown_protocol::SiteAuthMatchParams { url })?,
                ),
                DownloadsCommand::ProbeLink { url } => (
                    method::DAEMON_LINK_PROBE,
                    serialize(fluxdown_protocol::LinkProbeParams { url })?,
                ),
                DownloadsCommand::RemoteDispatch(params) => {
                    (method::AGENT_REMOTE_DISPATCH, serialize(params)?)
                }
                DownloadsCommand::LinkDispatch(params) => {
                    (method::AGENT_LINK_DISPATCH, serialize(params)?)
                }
                DownloadsCommand::RemoteCommand(params) => {
                    (method::AGENT_REMOTE_COMMAND, serialize(params)?)
                }
                // Windows 由前台界面进程拉起一次性 agent 打开，系统才会把窗口置前（见 shell_open）。
                #[cfg(windows)]
                DownloadsCommand::OpenTask { task_id } => {
                    crate::shell_open::task(&client, task_id, false).await?;
                    return Ok(DownloadsResult::Unit);
                }
                #[cfg(windows)]
                DownloadsCommand::RevealTask { task_id } => {
                    crate::shell_open::task(&client, task_id, true).await?;
                    return Ok(DownloadsResult::Unit);
                }
                #[cfg(not(windows))]
                DownloadsCommand::OpenTask { task_id } => (
                    method::AGENT_PLATFORM_OPEN_TASK,
                    json!({ "taskId": task_id }),
                ),
                #[cfg(not(windows))]
                DownloadsCommand::RevealTask { task_id } => (
                    method::AGENT_PLATFORM_REVEAL_TASK,
                    json!({ "taskId": task_id }),
                ),
                // 剪贴板不涉及前台授权，Windows 同样由常驻 agent 写入（进程常驻，X11 剪贴板内容不丢）。
                DownloadsCommand::CopyTaskFiles { task_ids } => (
                    method::AGENT_PLATFORM_COPY_TASK_FILES,
                    serialize(fluxdown_protocol::PlatformCopyTaskFilesParams { task_ids })?,
                ),
                DownloadsCommand::RescanFiles => (method::DAEMON_TASK_RESCAN, json!({})),
                DownloadsCommand::SubmitTorrentFile {
                    path,
                    silent,
                    save_dir,
                    queue_id,
                    start_paused,
                } => (
                    method::AGENT_CAPTURE_SUBMIT_TORRENT_FILE,
                    torrent_file_params(path, silent, save_dir, queue_id, start_paused),
                ),
                DownloadsCommand::SetLocalPreference { key, value } => (
                    method::AGENT_PREFERENCES_PATCH,
                    json!({ "values": { key: value }, "sync": false }),
                ),
                DownloadsCommand::SetSyncedPreference { key, value } => (
                    method::AGENT_PREFERENCES_PATCH,
                    json!({ "values": { key: value } }),
                ),
            };
            let value: Value = client.call(method, Some(params)).await?;
            Ok(if value == json!({ "ok": true }) {
                DownloadsResult::Unit
            } else {
                DownloadsResult::Value(value)
            })
        })
    }
}

fn serialize<T: serde::Serialize>(value: T) -> Result<Value, fluxdown_protocol::RpcErrorData> {
    serde_json::to_value(value).map_err(|_| internal_error())
}

/// 「查看证据」的两次查询参数：条目取覆盖该 IP 的全部名单条目，事件按 IP 组精确过滤，均由 daemon 筛选。
fn evidence_params(
    ip_group: &str,
) -> (
    fluxdown_protocol::BtGuardEntriesListParams,
    fluxdown_protocol::BtGuardEventsListParams,
) {
    (
        fluxdown_protocol::BtGuardEntriesListParams {
            contains_ip: Some(ip_group.to_owned()),
            limit: fluxdown_protocol::BT_GUARD_LIST_MAX_LIMIT,
            ..Default::default()
        },
        fluxdown_protocol::BtGuardEventsListParams {
            ip_group: Some(ip_group.to_owned()),
            limit: fluxdown_protocol::BT_GUARD_LIST_MAX_LIMIT,
            ..Default::default()
        },
    )
}

fn internal_error() -> fluxdown_protocol::RpcErrorData {
    fluxdown_protocol::RpcErrorData::new(fluxdown_protocol::ApplicationErrorCode::Internal, false)
}

fn preview_request(
    mut request: fluxdown_protocol::CreateTaskRequest,
) -> fluxdown_protocol::ResolvePreviewRequest {
    let mut extra_headers = request.headers.take().unwrap_or_default();
    apply_basic_auth(&mut extra_headers, &request);
    fluxdown_protocol::ResolvePreviewRequest {
        url: request.url,
        cookies: request.cookies,
        referrer: request.referrer,
        user_agent: request.user_agent,
        extra_headers,
    }
}

fn apply_basic_auth(
    headers: &mut std::collections::HashMap<String, String>,
    request: &fluxdown_protocol::CreateTaskRequest,
) {
    if !request.http_user.is_empty() {
        headers.retain(|name, _| !name.eq_ignore_ascii_case("authorization"));
        let credentials = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", request.http_user, request.http_password));
        headers.insert("Authorization".to_owned(), format!("Basic {credentials}"));
    }
}

/// `agent.capture.submitTorrentFile` 参数：`saveDir` / `queueId` / `startPaused` 仅在表单入口
/// 携带，缺省时省略，agent 维持原有默认行为。
fn torrent_file_params(
    path: String,
    silent: bool,
    save_dir: Option<String>,
    queue_id: Option<String>,
    start_paused: Option<bool>,
) -> Value {
    let mut params = json!({ "path": path, "silent": silent });
    if let Value::Object(map) = &mut params {
        if let Some(save_dir) = save_dir {
            map.insert("saveDir".to_owned(), Value::String(save_dir));
        }
        if let Some(queue_id) = queue_id {
            map.insert("queueId".to_owned(), Value::String(queue_id));
        }
        if let Some(start_paused) = start_paused {
            map.insert("startPaused".to_owned(), Value::Bool(start_paused));
        }
    }
    params
}

#[cfg(test)]
mod evidence_params_tests {
    use super::evidence_params;

    #[test]
    fn evidence_queries_are_filtered_by_the_daemon() {
        let (entries, events) = evidence_params("2001:db8::/64");
        assert_eq!(entries.contains_ip.as_deref(), Some("2001:db8::/64"));
        assert!(entries.query.is_none());
        assert_eq!(events.ip_group.as_deref(), Some("2001:db8::/64"));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::torrent_file_params;

    #[tokio::test]
    async fn new_import_waits_while_paused_and_stale_resume_cannot_reopen_gate() {
        let gate = super::SubmissionGate::default();
        gate.set_ready(true);
        let first = gate.pause();
        let admission = gate.admit();
        tokio::pin!(admission);
        assert!(futures_util::poll!(&mut admission).is_pending());
        let latest = gate.pause();
        gate.resume(first);
        assert!(futures_util::poll!(&mut admission).is_pending());
        gate.resume(latest);
        let guard = admission.await.expect("resumed admission");
        drop(guard);
        gate.set_ready(false);
        let admission = gate.admit();
        tokio::pin!(admission);
        assert!(futures_util::poll!(&mut admission).is_pending());
        gate.set_ready(true);
        drop(admission.await.expect("reconnected admission"));
    }

    #[tokio::test]
    async fn global_controls_keep_call_order_even_when_polled_in_reverse() {
        let gate = super::SubmissionGate::default();
        let (first, first_done) = gate.control_turn();
        let (second, second_done) = gate.control_turn();
        assert!(first.is_none());
        let second = second.expect("prior control");
        tokio::pin!(second);
        assert!(futures_util::poll!(&mut second).is_pending());
        first_done.send(()).expect("second control remains queued");
        second.await.expect("first control completed");
        second_done.send(()).expect("gate retains control tail");
    }

    #[test]
    fn user_initiated_torrent_carries_form_options() {
        let params = torrent_file_params(
            "/tmp/a.torrent".to_owned(),
            false,
            Some("/data".to_owned()),
            Some("q1".to_owned()),
            Some(true),
        );
        assert_eq!(
            params,
            json!({
                "path": "/tmp/a.torrent",
                "silent": false,
                "saveDir": "/data",
                "queueId": "q1",
                "startPaused": true,
            })
        );
    }

    #[test]
    fn torrent_without_form_options_omits_them() {
        let params = torrent_file_params("/tmp/a.torrent".to_owned(), true, None, None, None);
        assert_eq!(params, json!({ "path": "/tmp/a.torrent", "silent": true }));
    }
}
