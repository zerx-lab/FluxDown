//! 配置同步：目录 ownership、pull-before-push、dirty/tombstone、self-echo 与「本机专属」状态机。
//!
//! 约束（契约 §3）：
//! - 只有同步目录（`fluxdown_protocol::SYNC_SETTING_SPECS`）内的键才会上云 / 被应用，目录外键
//!   是设备本地偏好；
//! - `local_only_keys` 设备本地持久化：本机专属键不推送、也不应用云端值；
//! - 登录后默认开启（除非用户曾显式关闭）；关闭在 SSE 存活期间立即生效；
//! - `sync_device_untrusted` → `halted`，停止自动重试；`online_device_limit`（在线名额已满）不 halt：
//!   记录原因、每 30 秒重试，本机任务通道连上时立即恢复；
//! - 冲突按编辑时间裁决：推送带 `baseVersion` + `editedAt`，云端判负的条目随 `conflicts` 回来，
//!   按云端拉取值应用；
//! - 水位与脏键按账号隔离（见 `AgentState::bind_account`）；
//! - 拉取到的单条毒值只跳过并记录，不阻塞水位前进；
//! - 首次 / resync 把目录里云端没有的本机值播种上云。

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{
    AgentEvent, CUSTOM_THEMES_KEY, CloudConnectionState, DaemonConfigPatch, ErrorReason,
    RpcErrorData, ServiceEvent, SettingSpec, SyncStatusDto, custom_theme_id, daemon_config_default,
    daemon_config_to_value, normalize_daemon_config_value, sync_scope_key,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Mutex, Notify, broadcast};
use tokio_util::sync::CancellationToken;

use crate::cloud::{CloudApi, CloudError};
use crate::daemon_client::DaemonClient;
use crate::event_hub::AgentEventHub;
use crate::state::{AgentState, PersistedSyncEntry, StateStore};
use fluxdown_protocol::{setting_spec, validate_value, value_to_daemon_config};

pub use fluxdown_protocol::SettingOwner as SyncOwner;

/// 自定义分类的同步键；线上值剥离各设备不同的 `saveDir`。
const CUSTOM_CATEGORIES_KEY: &str = "custom_categories";

/// 键的同步归属；目录外的键一律 `Excluded`（设备本地，不上云、不应用）。
#[must_use]
pub fn owner_for_key(key: &str) -> SyncOwner {
    setting_spec(key).map_or(SyncOwner::Excluded, |spec| spec.owner)
}

/// 键（或其所属集合，如自定义主题键之于 [`CUSTOM_THEMES_KEY`]）已设为本机专属。
fn is_local_only(local_only_keys: &[String], key: &str) -> bool {
    let scope = sync_scope_key(key);
    local_only_keys.iter().any(|local| local == scope)
}

const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(75);
const LOCAL_DEBOUNCE: Duration = Duration::from_millis(600);
const RESYNC_PAUSE: Duration = Duration::from_secs(1);
const SSE_STABLE: Duration = Duration::from_secs(60);
const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(60),
    Duration::from_secs(300),
];
/// SSE 单行上限；超过说明流已损坏。
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
/// 单次推送的条目上限，镜像 FluxCloud `sync::MAX_ITEMS_PER_PUSH`：自定义主题逐个成键，
/// 首次播种时脏键数可能超出，必须分批。
const MAX_ITEMS_PER_PUSH: usize = 128;
/// 在线设备名额已满后的固定重试间隔（不走指数退避）。
const ONLINE_LIMIT_RETRY: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullResult {
    revision: u64,
    #[serde(default)]
    resync: bool,
    #[serde(default)]
    items: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncItem {
    key: String,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    deleted: bool,
    version: u64,
    #[serde(default)]
    device_id: String,
    /// 云端记录该值的编辑时刻（unix 毫秒）；旧云端不带。
    #[serde(default)]
    edited_at: Option<i64>,
}

/// 一次 SSE 会话的结束原因。
#[derive(Debug, Eq, PartialEq)]
enum SseEnd {
    Cancelled,
    /// 同步被关闭（或会话结束）：回到外层循环重新评估。
    Stopped,
    /// 云端广播滞后：需要全量重拉。
    Resync,
}

pub struct SyncService {
    cloud: CloudApi,
    daemon: Arc<DaemonClient>,
    events: AgentEventHub,
    state: Arc<Mutex<AgentState>>,
    store: Arc<StateStore>,
    gate: Mutex<()>,
    /// 本地变更 / 开关变化：唤醒等待中的循环。
    wake: Notify,
    /// 用户处理完 `halted` 原因后（重新启用 / 立即同步 / 重新登录）恢复自动同步。
    resume: Notify,
}

impl SyncService {
    #[must_use]
    pub fn new(
        cloud: CloudApi,
        daemon: Arc<DaemonClient>,
        events: AgentEventHub,
        state: Arc<Mutex<AgentState>>,
        store: Arc<StateStore>,
    ) -> Self {
        Self {
            cloud,
            daemon,
            events,
            state,
            store,
            gate: Mutex::new(()),
            wake: Notify::new(),
            resume: Notify::new(),
        }
    }

    #[must_use]
    pub async fn status(&self) -> SyncStatusDto {
        self.state.lock().await.sync.clone()
    }

    async fn persist(&self) -> Result<(), SyncError> {
        self.store.persist(&self.state).await?;
        Ok(())
    }

    /// 开 / 关同步；用户显式关闭会被记住（登录后不再自动开启）。
    pub async fn set_enabled(&self, enabled: bool) -> Result<(), SyncError> {
        let epoch = self.cloud.request_epoch();
        {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            state.sync.enabled = enabled;
            state.sync_user_disabled = !enabled;
            if enabled {
                state.sync.halted = false;
                state.sync.last_error = None;
                state.sync.last_error_reason = None;
            } else {
                state.sync.connected = false;
            }
            self.events
                .publish(AgentEvent::SyncChanged(state.sync.clone()));
        }
        self.persist().await?;
        if enabled {
            self.resume.notify_one();
        }
        self.wake.notify_one();
        Ok(())
    }

    /// 设置 / 取消「本机专属」：专属键不推送、不应用云端值。只接受同步目录内的精确键。
    pub async fn set_local_only(
        &self,
        keys: &[String],
        local_only: bool,
    ) -> Result<SyncStatusDto, SyncError> {
        let epoch = self.cloud.request_epoch();
        for key in keys {
            if owner_for_key(key) == SyncOwner::Excluded {
                return Err(SyncError::UnknownKey(key.clone()));
            }
        }
        let status = {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            let mut set = state
                .sync
                .local_only_keys
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            let mut rejoined = false;
            for key in keys {
                if local_only {
                    set.insert(key.clone());
                    // 专属键（含集合下的全部成员键）不再推送本机的待上传编辑。
                    for (entry_key, entry) in &mut state.sync_entries {
                        if sync_scope_key(entry_key) == key {
                            entry.dirty = false;
                        }
                    }
                } else if set.remove(key) {
                    rejoined = true;
                }
            }
            state.sync.local_only_keys = set.into_iter().collect();
            if rejoined {
                // 重新加入同步：下一轮全量拉取，让云端值落到这些键上，并播种云端缺失的键。
                state.sync.revision = 0;
                state.sync_pulled = false;
            }
            state.refresh_sync_projection();
            self.events
                .publish(AgentEvent::SyncChanged(state.sync.clone()));
            state.sync.clone()
        };
        self.persist().await?;
        self.wake.notify_one();
        Ok(status)
    }

    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        let mut retry_attempt = 0_usize;
        let (mut session_events, _) = self.events.subscribe_and_snapshot();
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let epoch = self.cloud.request_epoch();
            if !self.cloud.is_authenticated().await {
                self.set_connected_epoch(false, epoch).await;
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = self.wake.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {},
                    _ = wait_for_session(&mut session_events) => {},
                }
                continue;
            }
            match self.enable_by_default(epoch).await {
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(error = %error, "enabling config sync after login failed")
                }
            }
            let (enabled, halted) = {
                let state = self.state.lock().await;
                (state.sync.enabled, state.sync.halted)
            };
            if !enabled {
                self.set_connected_epoch(false, epoch).await;
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = self.wake.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {},
                    _ = wait_for_session(&mut session_events) => {},
                }
                continue;
            }
            if halted {
                // 不可自动恢复：只在用户处理（重新启用 / 立即同步）或重新登录后继续。
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = self.resume.notified() => {},
                    _ = wait_for_session(&mut session_events) => {
                        self.clear_halt(epoch).await;
                    },
                }
                continue;
            }
            if let Err(error) = self.sync_once_epoch(epoch).await {
                self.record_error(&error, epoch).await;
                if error.halts() {
                    continue;
                }
                let limited = error.is_online_limit();
                let delay = if limited {
                    ONLINE_LIMIT_RETRY
                } else {
                    let delay = RETRY_DELAYS[retry_attempt.min(RETRY_DELAYS.len() - 1)];
                    retry_attempt = retry_attempt.saturating_add(1);
                    delay
                };
                if self
                    .wait_retry(&cancel, &mut session_events, delay, limited)
                    .await
                {
                    return;
                }
                continue;
            }
            let sync_started = std::time::Instant::now();
            let device_id = self.state.lock().await.device_id.clone();
            let limited = match self.cloud.at_epoch(epoch).sync_events(&device_id).await {
                Ok(response) => {
                    self.set_connected_epoch(true, epoch).await;
                    let outcome = self
                        .consume_events(response, &cancel, &mut session_events, epoch)
                        .await;
                    self.set_connected_epoch(false, epoch).await;
                    match outcome {
                        Ok(SseEnd::Cancelled) => return,
                        Ok(SseEnd::Stopped) => continue,
                        Ok(SseEnd::Resync) => {
                            tracing::info!("sync SSE asked for resync; reloading everything");
                            self.force_full_pull(epoch).await;
                            tokio::select! {
                                _ = cancel.cancelled() => return,
                                _ = tokio::time::sleep(RESYNC_PAUSE) => {},
                            }
                            continue;
                        }
                        Err(error) => {
                            self.record_error(&error, epoch).await;
                            if error.halts() {
                                continue;
                            }
                            error.is_online_limit()
                        }
                    }
                }
                Err(error) => {
                    let error = SyncError::Cloud(error);
                    self.record_error(&error, epoch).await;
                    if error.halts() {
                        continue;
                    }
                    error.is_online_limit()
                }
            };
            if sync_started.elapsed() >= SSE_STABLE {
                retry_attempt = 0;
            }
            let delay = if limited {
                ONLINE_LIMIT_RETRY
            } else {
                let delay = RETRY_DELAYS[retry_attempt.min(RETRY_DELAYS.len() - 1)];
                retry_attempt = retry_attempt.saturating_add(1);
                delay
            };
            if self
                .wait_retry(&cancel, &mut session_events, delay, limited)
                .await
            {
                return;
            }
        }
    }

    /// 立即结束当前重试等待并重新同步；在线名额被释放（手动让其它设备下线）后调用。
    pub fn request_reconnect(&self) {
        self.wake.notify_one();
    }

    /// 失败后的重试等待；返回 `true` 表示已取消。到点、本地变更唤醒即返回；在线名额受限时
    /// 本机任务通道连上（名额已让出）的瞬间也返回，不必等满 30 秒。
    async fn wait_retry(
        &self,
        cancel: &CancellationToken,
        session_events: &mut broadcast::Receiver<Arc<fluxdown_protocol::EventFrame>>,
        delay: Duration,
        limited: bool,
    ) -> bool {
        tokio::select! {
            _ = cancel.cancelled() => true,
            _ = self.wake.notified() => false,
            _ = tokio::time::sleep(delay) => false,
            _ = self.wait_remote_connected(session_events), if limited => false,
        }
    }

    /// 等到本机任务通道变为 `connected`；接收端 lag 时以当前快照为准。
    async fn wait_remote_connected(
        &self,
        events: &mut broadcast::Receiver<Arc<fluxdown_protocol::EventFrame>>,
    ) {
        loop {
            match events.recv().await {
                Ok(frame) => {
                    if matches!(
                        &frame.event,
                        ServiceEvent::Agent(AgentEvent::CloudConnectionChanged(connection))
                            if connection.state == CloudConnectionState::Connected
                    ) {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let connected = self.events.inspect(|snapshot| {
                        snapshot.cloud_connection.state == CloudConnectionState::Connected
                    });
                    if connected {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
            }
        }
    }

    /// 登录后默认开启同步；用户曾显式关闭则保持关闭。
    async fn enable_by_default(&self, epoch: crate::cloud::RequestEpoch) -> Result<(), SyncError> {
        {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            if state.sync.enabled || state.sync_user_disabled {
                return Ok(());
            }
            state.sync.enabled = true;
            self.events
                .publish(AgentEvent::SyncChanged(state.sync.clone()));
        }
        self.persist().await?;
        Ok(())
    }

    async fn clear_halt(&self, epoch: crate::cloud::RequestEpoch) {
        {
            let mut state = match self.cloud.lock_epoch(epoch).await {
                Ok(state) => state,
                Err(error) => {
                    tracing::debug!(%error, "discarding stale sync resume");
                    return;
                }
            };
            if !state.sync.halted {
                return;
            }
            state.sync.halted = false;
            self.events
                .publish(AgentEvent::SyncChanged(state.sync.clone()));
        }
    }

    async fn set_connected_epoch(&self, connected: bool, epoch: crate::cloud::RequestEpoch) {
        {
            let mut state = match self.cloud.lock_epoch(epoch).await {
                Ok(state) => state,
                Err(error) => {
                    tracing::debug!(%error, "discarding stale sync connection state");
                    return;
                }
            };
            if state.sync.connected == connected {
                return;
            }
            state.sync.connected = connected;
            self.events
                .publish(AgentEvent::SyncChanged(state.sync.clone()));
        }
    }

    /// 让下一轮同步 `since=0` 并重新播种（resync 事件）。
    async fn force_full_pull(&self, epoch: crate::cloud::RequestEpoch) {
        let mut state = match self.cloud.lock_epoch(epoch).await {
            Ok(state) => state,
            Err(error) => {
                tracing::debug!(%error, "discarding stale sync resync request");
                return;
            }
        };
        state.sync.revision = 0;
        state.sync_pulled = false;
    }

    async fn consume_events(
        &self,
        response: reqwest::Response,
        cancel: &CancellationToken,
        session_events: &mut broadcast::Receiver<Arc<fluxdown_protocol::EventFrame>>,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<SseEnd, SyncError> {
        let stream_uid = self.cloud.current_user_id().await;
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        // 空闲看门狗：截止时间在循环外，只有收到流数据才顺延。
        let idle = tokio::time::sleep(SSE_IDLE_TIMEOUT);
        tokio::pin!(idle);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(SseEnd::Cancelled),
                () = &mut idle => {
                    return Err(SyncError::Protocol("sync SSE idle timeout".to_owned()));
                }
                frame = session_events.recv() => match frame {
                    Ok(frame) => {
                        if matches!(
                            &frame.event,
                            ServiceEvent::Agent(AgentEvent::SessionChanged(session))
                                if match &**session {
                                    None => true,
                                    Some(session) => stream_uid
                                        .as_deref()
                                        .is_some_and(|uid| uid != session.user.id),
                                }
                        ) {
                            return Ok(SseEnd::Stopped);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if !self.cloud.is_authenticated().await {
                            return Ok(SseEnd::Stopped);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(SyncError::Protocol("agent event hub closed".to_owned()));
                    }
                },
                _ = self.wake.notified() => {
                    // 关闭 / 会话结束在 SSE 存活期间立即生效。
                    if !self.sync_allowed().await {
                        return Ok(SseEnd::Stopped);
                    }
                    tokio::time::sleep(LOCAL_DEBOUNCE).await;
                    if !self.sync_allowed().await {
                        return Ok(SseEnd::Stopped);
                    }
                    self.sync_once_epoch(epoch).await?;
                }
                chunk = stream.next() => {
                    let chunk = chunk
                        .ok_or_else(|| SyncError::Protocol("sync SSE disconnected".to_owned()))?
                        .map_err(|error| SyncError::Protocol(format!("sync SSE read failed: {error:#}")))?;
                    idle.as_mut().reset(tokio::time::Instant::now() + SSE_IDLE_TIMEOUT);
                    buffer.extend_from_slice(&chunk);
                    if buffer.len() > MAX_SSE_LINE_BYTES && !buffer.contains(&b'\n') {
                        return Err(SyncError::Protocol("sync SSE line too long".to_owned()));
                    }
                    while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                        let line = buffer.drain(..=newline).collect::<Vec<_>>();
                        if let Some(end) = self.handle_sse_line(&line, epoch).await? {
                            return Ok(end);
                        }
                    }
                }
            }
        }
    }

    /// 已启用且已登录且未 halted。
    async fn sync_allowed(&self) -> bool {
        let (enabled, halted) = {
            let state = self.state.lock().await;
            (state.sync.enabled, state.sync.halted)
        };
        enabled && !halted && self.cloud.is_authenticated().await
    }

    /// 单行 SSE：坏行只记录；`resync` 结束流；有新 revision 则同步。
    async fn handle_sse_line(
        &self,
        line: &[u8],
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<Option<SseEnd>, SyncError> {
        drop(self.cloud.lock_epoch(epoch).await?);
        let Ok(line) = std::str::from_utf8(line) else {
            tracing::warn!("sync SSE line is not UTF-8; skipped");
            return Ok(None);
        };
        let Some(payload) = line.trim().strip_prefix("data:") else {
            return Ok(None);
        };
        let event = match serde_json::from_str::<Value>(payload.trim()) {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(error = %error, "sync SSE event is not JSON; skipped");
                return Ok(None);
            }
        };
        if event.get("kind").and_then(Value::as_str) == Some("cdn_config") {
            return Ok(None);
        }
        if event.get("type").and_then(Value::as_str) == Some("resync") {
            return Ok(Some(SseEnd::Resync));
        }
        if let Some(revision) = event.get("revision").and_then(Value::as_u64) {
            let current = self.state.lock().await.sync.revision;
            if revision != current {
                if !self.sync_allowed().await {
                    return Ok(Some(SseEnd::Stopped));
                }
                self.sync_once_epoch(epoch).await?;
            }
        }
        Ok(None)
    }

    /// 只在内存里记录错误（不落盘：每次重试都写盘没有意义）；不可自动恢复的错误置 `halted`。
    async fn record_error(&self, error: &SyncError, epoch: crate::cloud::RequestEpoch) {
        tracing::warn!(error = %error, reason = ?error.reason(), "config sync failed");
        {
            let mut state = match self.cloud.lock_epoch(epoch).await {
                Ok(state) => state,
                Err(error) => {
                    tracing::debug!(%error, "discarding stale sync error");
                    return;
                }
            };
            state.sync.last_error = Some(error.to_string());
            state.sync.last_error_reason = error.reason();
            state.sync.connected = false;
            if error.halts() {
                state.sync.halted = true;
            }
            self.events
                .publish(AgentEvent::SyncChanged(state.sync.clone()));
        }
    }

    /// 用户显式「立即同步」：清除 `halted` 后执行一次同步；仍失败则重新 halted 并返回错误。
    pub async fn sync_now(&self) -> Result<(), SyncError> {
        let epoch = self.cloud.request_epoch();
        if !self.cloud.lock_epoch(epoch).await?.sync.enabled {
            return Err(SyncError::Disabled);
        }
        self.clear_halt(epoch).await;
        match self.sync_once_epoch(epoch).await {
            Ok(()) => {
                self.resume.notify_one();
                Ok(())
            }
            Err(error) => {
                self.record_error(&error, epoch).await;
                Err(error)
            }
        }
    }

    /// 执行一次严格 pull-before-push 同步。
    #[cfg(test)]
    async fn sync_once(&self) -> Result<(), SyncError> {
        let epoch = self.cloud.request_epoch();
        self.sync_once_epoch(epoch).await
    }

    async fn sync_once_epoch(&self, epoch: crate::cloud::RequestEpoch) -> Result<(), SyncError> {
        let _gate = self.gate.lock().await;
        drop(self.cloud.lock_epoch(epoch).await?);
        let uid = self
            .cloud
            .current_user_id()
            .await
            .ok_or_else(|| SyncError::Cloud(CloudError::unauthorized()))?;
        let (since, device_id, first_sync) = {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            state.bind_account(Some(&uid));
            (
                state.sync.revision,
                state.device_id.clone(),
                state.sync.revision == 0 || !state.sync_pulled,
            )
        };
        let cloud = self.cloud.at_epoch(epoch);
        let pull_value = cloud.sync_pull(since, &device_id).await?;
        let pull = serde_json::from_value::<PullResult>(pull_value)
            .map_err(|error| SyncError::Protocol(format!("sync pull response: {error}")))?;
        let no_pulled_items = pull.items.is_empty();
        let items = parse_items(pull.items);

        let current_daemon = daemon_config_values(&self.events);
        let (daemon_changes, mut prefs_changed, sent_entries) = {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            if state.account_uid.as_deref() != Some(uid.as_str()) {
                return Err(SyncError::AccountChanged);
            }
            let remote_keys = items
                .iter()
                .map(|item| item.key.clone())
                .collect::<HashSet<_>>();
            let report = apply_pull(&mut state, &device_id, items, &current_daemon);
            if first_sync || pull.resync {
                seed_local_values(&mut state, &remote_keys, &current_daemon, pull.resync);
            }
            let sent = dirty_entries(&state);
            (report.daemon_changes, report.prefs_changed, sent)
        };

        if !daemon_changes.is_empty() {
            self.patch_daemon(daemon_changes, epoch).await?;
        }
        let mut idle = false;
        if !sent_entries.is_empty() {
            let sent_entries = sent_entries.into_iter().collect::<Vec<_>>();
            // 每批回包恰好是上一水位的下一个 revision（本批无实变时等于上一水位）才说明期间无他人
            // 写入；否则保持 pull 水位，让后续 SSE 触发的 pull 补回并发写入。
            let mut watermark = Some(pull.revision);
            for batch in sent_entries.chunks(MAX_ITEMS_PER_PUSH) {
                let payload = batch
                    .iter()
                    .map(|(key, entry)| push_item(key, entry))
                    .collect::<Vec<_>>();
                let response = self
                    .cloud
                    .at_epoch(epoch)
                    .sync_push(&serde_json::json!({
                        "deviceId": device_id,
                        "items": payload,
                    }))
                    .await?;
                let revision = response
                    .get("revision")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        SyncError::Protocol("sync push returned no revision".to_owned())
                    })?;
                let conflicts = parse_items(
                    response
                        .get("conflicts")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                );
                let conflict_daemon_changes = {
                    let mut state = self.cloud.lock_epoch(epoch).await?;
                    if state.account_uid.as_deref() != Some(uid.as_str()) {
                        return Err(SyncError::AccountChanged);
                    }
                    // 这一批已被云端处理：被采纳的与被判负的都不再脏（判负的下面按云端值覆盖）。
                    for (key, sent) in batch {
                        if let Some(entry) = state.sync_entries.get_mut(key)
                            && entry.value == sent.value
                            && entry.deleted == sent.deleted
                            && entry.version == sent.version
                        {
                            entry.dirty = false;
                        }
                    }
                    if conflicts.is_empty() {
                        BTreeMap::new()
                    } else {
                        // 云端判负的条目：返回的是云端胜出的现值，按拉取值应用（非脏）。
                        let report = apply_pull(
                            &mut state,
                            &device_id,
                            conflicts,
                            &daemon_config_values(&self.events),
                        );
                        prefs_changed |= report.prefs_changed;
                        report.daemon_changes
                    }
                };
                if !conflict_daemon_changes.is_empty() {
                    self.patch_daemon(conflict_daemon_changes, epoch).await?;
                }
                watermark = watermark
                    .filter(|current| revision == *current || revision == current.saturating_add(1))
                    .map(|_| revision);
            }
            let mut state = self.cloud.lock_epoch(epoch).await?;
            if state.account_uid.as_deref() != Some(uid.as_str()) {
                return Err(SyncError::AccountChanged);
            }
            state.sync.revision = watermark.unwrap_or(pull.revision);
        } else {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            if state.account_uid.as_deref() != Some(uid.as_str()) {
                return Err(SyncError::AccountChanged);
            }
            state.sync.revision = pull.revision;
            idle = no_pulled_items && !first_sync && pull.revision == since;
        }

        {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            idle = idle && state.sync_pulled && state.sync.last_error.is_none();
            state.sync_pulled = true;
            state.refresh_sync_projection();
            state.sync.last_error = None;
            state.sync.last_error_reason = None;
            state.sync.halted = false;
            state.sync.last_synced_at_unix_ms = Some(now_unix_ms());
            if prefs_changed {
                self.events
                    .publish(AgentEvent::PreferencesChanged(state.preferences.clone()));
            }
            self.events
                .publish(AgentEvent::SyncChanged(state.sync.clone()));
        }
        if !idle {
            self.persist().await?;
        }
        Ok(())
    }

    /// 把云端值写进 daemon。`normalize` 已在 [`apply_pull`] 里剔除毒值；revision 冲突时按
    /// 错误里给出的最新 revision 重试一次。
    async fn patch_daemon(
        &self,
        values: BTreeMap<String, String>,
        epoch: crate::cloud::RequestEpoch,
    ) -> Result<(), SyncError> {
        let mut expected_revision = daemon_revision(&self.events);
        for attempt in 0..2 {
            drop(self.cloud.lock_epoch(epoch).await?);
            let result = self
                .daemon
                .call::<DaemonConfigPatch, Value>(
                    fluxdown_protocol::method::DAEMON_CONFIG_PATCH,
                    Some(DaemonConfigPatch {
                        expected_revision,
                        values: values.clone(),
                    }),
                )
                .await;
            match result {
                Ok(_) => return Ok(()),
                Err(error)
                    if attempt == 0
                        && error.code == fluxdown_protocol::ApplicationErrorCode::Conflict =>
                {
                    expected_revision = error
                        .revision
                        .unwrap_or_else(|| daemon_revision(&self.events));
                }
                Err(error) => return Err(SyncError::Daemon(error)),
            }
        }
        Err(SyncError::Protocol(
            "daemon config revision kept changing during sync".to_owned(),
        ))
    }

    /// 本地变更立即置 dirty；真正 push 等下一次 pull 成功后执行。
    ///
    /// 目录外键（设备本地偏好）与本机专属键：只在本机生效（写偏好 / 应用到 daemon），不产生同步条目。
    /// `deleted = true` 是墓碑（恢复默认）：本机移除该偏好 / daemon 键回到默认值，并把删除同步给云端。
    ///
    /// 返回写入落定后的偏好 revision（只落 daemon 的键不改变 revision，返回当前值）。
    pub async fn mark_local(
        &self,
        key: String,
        value: Value,
        deleted: bool,
    ) -> Result<u64, SyncError> {
        let epoch = self.cloud.request_epoch();
        if key == CUSTOM_THEMES_KEY {
            // 集合范围键只承载分组与本机专属开关；主题逐个写 `appearance.custom_themes.<id>`。
            return Err(SyncError::InvalidValue(format!(
                "{key} is a collection; write per-theme keys"
            )));
        }
        let Some(spec) = setting_spec(&key).filter(|spec| spec.owner != SyncOwner::Excluded) else {
            return self.set_local_preference(key, value, deleted).await;
        };
        if !deleted {
            validate_value(spec.key, &value).map_err(SyncError::InvalidValue)?;
        }
        let wire = if deleted {
            Value::Null
        } else if key == CUSTOM_CATEGORIES_KEY {
            categories_to_wire(&value).map_err(SyncError::InvalidValue)?
        } else {
            value.clone()
        };
        if spec.owner == SyncOwner::Daemon {
            let daemon_value = if deleted {
                daemon_config_default(spec.storage_key).to_owned()
            } else {
                value_to_daemon_config(spec, &value).map_err(SyncError::InvalidValue)?
            };
            self.patch_daemon(
                BTreeMap::from([(spec.storage_key.to_owned(), daemon_value)]),
                epoch,
            )
            .await?;
        }
        let edited_at = self.cloud.server_now_ms();
        let revision = {
            let mut state = self.cloud.lock_epoch(epoch).await?;
            if matches!(spec.owner, SyncOwner::Agent | SyncOwner::Preferences) {
                if deleted {
                    state.preferences.values.remove(&key);
                } else {
                    state.preferences.values.insert(key.clone(), value);
                }
                state.preferences.revision = state.preferences.revision.saturating_add(1);
            }
            if !is_local_only(&state.sync.local_only_keys, &key) {
                let entry = state.sync_entries.entry(key).or_default();
                entry.value = wire;
                entry.deleted = deleted;
                entry.dirty = true;
                entry.edited_at_ms = Some(edited_at);
            }
            state.refresh_sync_projection();
            self.events
                .publish(AgentEvent::PreferencesChanged(state.preferences.clone()));
            self.events
                .publish(AgentEvent::SyncChanged(state.sync.clone()));
            state.preferences.revision
        };
        self.persist().await?;
        self.wake.notify_one();
        Ok(revision)
    }

    /// 只写本机偏好，不进入同步（设备本地偏好、目录外键）；返回写入后的偏好 revision。
    pub async fn set_local_preference(
        &self,
        key: String,
        value: Value,
        deleted: bool,
    ) -> Result<u64, SyncError> {
        let preferences = {
            let mut state = self.state.lock().await;
            if deleted {
                state.preferences.values.remove(&key);
            } else {
                state.preferences.values.insert(key, value);
            }
            state.preferences.revision = state.preferences.revision.saturating_add(1);
            state.preferences.clone()
        };
        let revision = preferences.revision;
        self.persist().await?;
        self.events
            .publish(AgentEvent::PreferencesChanged(preferences));
        Ok(revision)
    }
}

/// 解析云端条目（拉取项 / 推送冲突项）：坏条目只记录并跳过。
fn parse_items(values: Vec<Value>) -> Vec<SyncItem> {
    values
        .into_iter()
        .filter_map(
            |value| match serde_json::from_value::<SyncItem>(value.clone()) {
                Ok(item) => Some(item),
                Err(error) => {
                    tracing::warn!(error = %error, item = %value, "skipping malformed sync item");
                    None
                }
            },
        )
        .collect()
}

/// 推送线上形态：`baseVersion` 是本机编辑所基于的云端版本（脏键拉取期间不上浮），
/// `editedAt` 是本机编辑时刻（毫秒）；没有真实编辑时刻（播种）时省略，由云端取当前时间。
fn push_item(key: &str, entry: &PersistedSyncEntry) -> Value {
    let mut item = serde_json::Map::new();
    item.insert("key".to_owned(), Value::from(key));
    item.insert("value".to_owned(), entry.value.clone());
    item.insert("deleted".to_owned(), Value::from(entry.deleted));
    item.insert("baseVersion".to_owned(), Value::from(entry.version));
    if let Some(edited_at) = entry.edited_at_ms {
        item.insert("editedAt".to_owned(), Value::from(edited_at));
    }
    Value::Object(item)
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// 拉取结果应用报告。
#[derive(Default)]
struct PullReport {
    /// daemon 存储键 → 需要写入 daemon 的 wire 值。
    daemon_changes: BTreeMap<String, String>,
    prefs_changed: bool,
    /// 被跳过的（键，原因），仅用于日志与测试。
    skipped: Vec<(String, &'static str)>,
}

/// 把一批云端条目应用到本机状态（纯函数，不做 I/O）。
///
/// - 目录外 / `Excluded` / 本机专属键：忽略；
/// - 本机回声：只确认版本与脏标记，不重复 patch daemon；
/// - 本机有未推送编辑（dirty）：本机值胜出；
/// - 毒值（校验 / 转换失败）：跳过并记录，水位照常前进；
/// - 墓碑：偏好移除，daemon 键回到其默认值。
fn apply_pull(
    state: &mut AgentState,
    local_device: &str,
    items: Vec<SyncItem>,
    current_daemon: &BTreeMap<String, String>,
) -> PullReport {
    let mut report = PullReport::default();
    let local_only = state
        .sync
        .local_only_keys
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    for item in items {
        let Some(spec) = setting_spec(&item.key).filter(|spec| spec.owner != SyncOwner::Excluded)
        else {
            report.skipped.push((item.key, "outside the sync catalog"));
            continue;
        };
        if item.key == CUSTOM_THEMES_KEY {
            report.skipped.push((item.key, "collection scope key"));
            continue;
        }
        if local_only.contains(sync_scope_key(&item.key)) {
            report.skipped.push((item.key, "local-only key"));
            continue;
        }
        if item.device_id == local_device && state.sync_entries.contains_key(&item.key) {
            let entry = state.sync_entries.entry(item.key).or_default();
            entry.version = entry.version.max(item.version);
            if entry.value == item.value && entry.deleted == item.deleted {
                entry.dirty = false;
            }
            continue;
        }
        if state
            .sync_entries
            .get(&item.key)
            .is_some_and(|entry| entry.dirty)
        {
            // 本机有未推送编辑：保持其 `version`（= 编辑所基于的云端版本），推送时云端按编辑时间裁决；
            // 不能在这里把版本抬到云端最新值，否则冲突永远被当成「已看到」而盲写覆盖。
            continue;
        }
        let applied = match prepare_remote_value(spec, &item, state) {
            Ok(applied) => applied,
            Err(reason) => {
                tracing::warn!(key = %item.key, reason = %reason, "skipping unusable synced value");
                state
                    .sync_entries
                    .entry(item.key.clone())
                    .or_default()
                    .version = item.version;
                report.skipped.push((item.key, "invalid value"));
                continue;
            }
        };
        match applied {
            Applied::Daemon(wire) => {
                if current_daemon.get(spec.storage_key) != Some(&wire) {
                    report
                        .daemon_changes
                        .insert(spec.storage_key.to_owned(), wire);
                }
            }
            Applied::Preference(local_value) => {
                match local_value {
                    Some(local_value) => {
                        state
                            .preferences
                            .values
                            .insert(item.key.clone(), local_value);
                    }
                    None => {
                        state.preferences.values.remove(&item.key);
                    }
                }
                state.preferences.revision = state.preferences.revision.saturating_add(1);
                report.prefs_changed = true;
            }
        }
        let entry = state.sync_entries.entry(item.key).or_default();
        entry.value = item.value;
        entry.deleted = item.deleted;
        entry.version = item.version;
        entry.edited_at_ms = item.edited_at;
        entry.dirty = false;
    }
    report
}

enum Applied {
    /// 需要写入 daemon 的 wire 值。
    Daemon(String),
    /// 需要写入偏好的本机值；`None` = 移除（墓碑）。
    Preference(Option<Value>),
}

fn prepare_remote_value(
    spec: &SettingSpec,
    item: &SyncItem,
    state: &AgentState,
) -> Result<Applied, String> {
    match spec.owner {
        SyncOwner::Daemon => {
            let wire = if item.deleted {
                daemon_config_default(spec.storage_key).to_owned()
            } else {
                validate_value(spec.key, &item.value)?;
                let wire = value_to_daemon_config(spec, &item.value)?;
                // daemon 用同一个函数校验补丁；提前剔除，避免一个毒值让整批补丁被拒。
                normalize_daemon_config_value(spec.storage_key, &wire)
                    .map_err(|error| format!("{error}"))?
            };
            Ok(Applied::Daemon(wire))
        }
        SyncOwner::Agent | SyncOwner::Preferences => {
            if item.deleted {
                return Ok(Applied::Preference(None));
            }
            validate_value(spec.key, &item.value)?;
            if spec.key == CUSTOM_CATEGORIES_KEY {
                Ok(Applied::Preference(Some(merge_categories_from_wire(
                    &item.value,
                    state.preferences.values.get(CUSTOM_CATEGORIES_KEY),
                )?)))
            } else {
                Ok(Applied::Preference(Some(item.value.clone())))
            }
        }
        SyncOwner::Excluded => Err("excluded key".to_owned()),
    }
}

/// 首次 / resync：目录里云端没有的键，用本机当前值播种（标脏重传）。
/// resync 时已有条目也重新标脏（云端可能已丢失它们）。本机专属键不播种。
/// 自定义主题按成员键逐个播种（集合范围键本身不承载值）。
fn seed_local_values(
    state: &mut AgentState,
    remote_keys: &HashSet<String>,
    current_daemon: &BTreeMap<String, String>,
    resync: bool,
) {
    let theme_keys = state
        .preferences
        .values
        .keys()
        .chain(state.sync_entries.keys())
        .filter(|key| custom_theme_id(key).is_some())
        .cloned()
        .collect::<BTreeSet<_>>();
    let candidates = fluxdown_protocol::SYNC_SETTING_SPECS
        .iter()
        .filter(|spec| spec.key != CUSTOM_THEMES_KEY)
        .map(|spec| spec.key.to_owned())
        .chain(theme_keys);
    for key in candidates {
        let Some(spec) = setting_spec(&key).filter(|spec| spec.owner != SyncOwner::Excluded) else {
            continue;
        };
        if remote_keys.contains(&key) || is_local_only(&state.sync.local_only_keys, &key) {
            continue;
        }
        if let Some(entry) = state.sync_entries.get_mut(&key) {
            if resync && !entry.deleted {
                entry.dirty = true;
            }
            continue;
        }
        let local_value = match spec.owner {
            SyncOwner::Daemon => current_daemon.get(spec.storage_key).and_then(|wire| {
                match daemon_config_to_value(spec, wire) {
                    Ok(value) => Some(value),
                    Err(reason) => {
                        tracing::warn!(key = %spec.key, reason = %reason, "not seeding unusable daemon config value");
                        None
                    }
                }
            }),
            SyncOwner::Agent | SyncOwner::Preferences => state.preferences.values.get(&key).cloned(),
            SyncOwner::Excluded => None,
        };
        let Some(local_value) = local_value else {
            continue;
        };
        let wire = if key == CUSTOM_CATEGORIES_KEY {
            match categories_to_wire(&local_value) {
                Ok(wire) => wire,
                Err(reason) => {
                    tracing::warn!(reason = %reason, "not seeding unusable custom categories");
                    continue;
                }
            }
        } else {
            if let Err(reason) = validate_value(spec.key, &local_value) {
                tracing::warn!(key = %key, reason = %reason, "not seeding invalid local setting");
                continue;
            }
            local_value
        };
        state.sync_entries.insert(
            key,
            PersistedSyncEntry {
                value: wire,
                version: 0,
                dirty: true,
                deleted: false,
                edited_at_ms: None,
            },
        );
    }
}

/// 待推送条目：仅目录内、非本机专属的脏键。
fn dirty_entries(state: &AgentState) -> BTreeMap<String, PersistedSyncEntry> {
    state
        .sync_entries
        .iter()
        .filter(|(key, entry)| {
            entry.dirty
                && owner_for_key(key) != SyncOwner::Excluded
                && !is_local_only(&state.sync.local_only_keys, key)
        })
        .map(|(key, entry)| (key.clone(), entry.clone()))
        .collect()
}

/// 自定义分类线上形态：分类对象数组，剥离各设备目录不同的 `saveDir`。
fn categories_to_wire(value: &Value) -> Result<Value, String> {
    let mut list = parse_category_list(value)?;
    for category in &mut list {
        if let Some(object) = category.as_object_mut() {
            object.remove("saveDir");
        }
    }
    Ok(Value::Array(list))
}

/// 云端分类合并进本机：保留本机同 id 分类的 `saveDir`；本机偏好的存储形态（JSON 字符串 / 数组）不变。
fn merge_categories_from_wire(remote: &Value, local: Option<&Value>) -> Result<Value, String> {
    let mut list = parse_category_list(remote)?;
    let local_categories = match local.map(parse_category_list).transpose() {
        Ok(categories) => categories.unwrap_or_default(),
        Err(reason) => {
            // 旧的本机值不可解析时仍可恢复云端分类；无法保留的本机目录必须可诊断。
            tracing::warn!(reason = %reason, "cannot preserve local category directories while merging cloud categories");
            Vec::new()
        }
    };
    let local_dirs = local_categories
        .into_iter()
        .filter_map(|category| {
            let id = category.get("id")?.as_str()?.to_owned();
            let dir = category.get("saveDir")?.as_str()?.to_owned();
            (!dir.is_empty()).then_some((id, dir))
        })
        .collect::<BTreeMap<_, _>>();
    for category in &mut list {
        let Some(object) = category.as_object_mut() else {
            continue;
        };
        object.remove("saveDir");
        if let Some(dir) = object
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| local_dirs.get(id))
        {
            object.insert("saveDir".to_owned(), Value::String(dir.clone()));
        }
    }
    let merged = Value::Array(list);
    if matches!(local, Some(Value::String(_))) {
        serde_json::to_string(&merged)
            .map(Value::String)
            .map_err(|error| error.to_string())
    } else {
        Ok(merged)
    }
}

fn parse_category_list(value: &Value) -> Result<Vec<Value>, String> {
    match value {
        Value::Array(list) => Ok(list.clone()),
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(list)) => Ok(list),
            Ok(_) => Err("custom_categories must be a JSON array".to_owned()),
            Err(error) => Err(format!("custom_categories is not valid JSON: {error}")),
        },
        _ => Err("custom_categories must be an array".to_owned()),
    }
}

fn daemon_config_values(events: &AgentEventHub) -> BTreeMap<String, String> {
    events.inspect(|snapshot| snapshot.daemon.config.values.clone())
}

fn daemon_revision(events: &AgentEventHub) -> u64 {
    events.inspect(|snapshot| snapshot.daemon.config.revision)
}

/// 等到下一条 `SessionChanged(Some)`；接收端 lag 时也返回，由调用方重新检查登录态。
async fn wait_for_session(events: &mut broadcast::Receiver<Arc<fluxdown_protocol::EventFrame>>) {
    loop {
        match events.recv().await {
            Ok(frame) => {
                if let ServiceEvent::Agent(AgentEvent::SessionChanged(session)) = &frame.event
                    && session.is_some()
                {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => return,
            Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
        }
    }
}

fn daemon_summary(error: &RpcErrorData) -> String {
    format!("{:?}, retryable={}", error.code, error.retryable)
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Cloud(#[from] CloudError),
    #[error("daemon rejected the synced settings ({})", daemon_summary(.0))]
    Daemon(RpcErrorData),
    #[error("sync protocol error: {0}")]
    Protocol(String),
    #[error(transparent)]
    State(#[from] crate::state::StateError),
    /// 同步已被关闭。
    #[error("config sync is disabled")]
    Disabled,
    /// 同步期间登录账号发生变化，本轮结果作废。
    #[error("the signed-in account changed during sync")]
    AccountChanged,
    /// 调用方给出的偏好值不合法。
    #[error("invalid synced value: {0}")]
    InvalidValue(String),
    /// 不在同步目录内的键。
    #[error("`{0}` is not a synced setting")]
    UnknownKey(String),
}

impl SyncError {
    /// 细分失败原因（云端错误码 / 网络）；供 UI 本地化。
    #[must_use]
    pub fn reason(&self) -> Option<ErrorReason> {
        match self {
            Self::Cloud(error) => error.reason(),
            Self::Daemon(error) => error.reason,
            _ => None,
        }
    }

    /// 不可自动恢复：本设备不受信任，需要用户重新登录。在线名额已满不属于此类（见 [`Self::is_online_limit`]）。
    #[must_use]
    pub fn halts(&self) -> bool {
        self.reason() == Some(ErrorReason::DeviceUntrusted)
    }

    /// 在线设备名额已满：本设备保持登录，固定间隔重试，不 halt。
    #[must_use]
    pub fn is_online_limit(&self) -> bool {
        self.reason() == Some(ErrorReason::OnlineDeviceLimit)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use fluxdown_protocol::{AgentEvent, CloudConnectionDto, CloudConnectionState, ErrorReason};
    use futures_util::StreamExt;
    use serde_json::{Value, json};
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use super::{
        SyncItem, SyncOwner, SyncService, apply_pull, categories_to_wire, dirty_entries,
        merge_categories_from_wire, owner_for_key, seed_local_values,
    };
    use crate::state::{AgentState, CloudCredentials, PersistedSyncEntry, StateStore};

    fn item(key: &str, value: Value, version: u64, device: &str) -> SyncItem {
        SyncItem {
            key: key.to_owned(),
            value,
            deleted: false,
            version,
            device_id: device.to_owned(),
            edited_at: None,
        }
    }

    fn dirty(value: Value) -> PersistedSyncEntry {
        PersistedSyncEntry {
            value,
            version: 1,
            dirty: true,
            deleted: false,
            edited_at_ms: None,
        }
    }

    #[test]
    fn ownership_table_routes_daemon_agent_and_preferences_and_excludes_everything_else() {
        assert_eq!(
            owner_for_key("download.max_concurrent_tasks"),
            SyncOwner::Daemon
        );
        assert_eq!(owner_for_key("download.keep_awake"), SyncOwner::Agent);
        assert_eq!(
            owner_for_key("appearance.theme_mode"),
            SyncOwner::Preferences
        );
        assert_eq!(owner_for_key("custom_categories"), SyncOwner::Preferences);
        // 目录外键（含引擎学习键与设备本地偏好）永不同步。
        assert_eq!(owner_for_key("cdn_node_health"), SyncOwner::Excluded);
        assert_eq!(
            owner_for_key("ui.show_sidebar_devices"),
            SyncOwner::Excluded
        );
        assert_eq!(owner_for_key("totally.unknown"), SyncOwner::Excluded);
    }

    #[test]
    fn dirty_local_value_wins_and_self_echo_only_confirms_equal_value() {
        let mut state = AgentState::default();
        state
            .sync_entries
            .insert("appearance.theme_mode".to_owned(), dirty(json!("dark")));
        apply_pull(
            &mut state,
            "device-1",
            vec![item("appearance.theme_mode", json!("light"), 2, "device-2")],
            &BTreeMap::new(),
        );
        assert_eq!(
            state.sync_entries["appearance.theme_mode"].value,
            json!("dark")
        );
        assert!(state.sync_entries["appearance.theme_mode"].dirty);
        assert!(
            !state
                .preferences
                .values
                .contains_key("appearance.theme_mode")
        );

        let report = apply_pull(
            &mut state,
            "device-1",
            vec![item("appearance.theme_mode", json!("dark"), 3, "device-1")],
            &BTreeMap::new(),
        );
        assert!(!state.sync_entries["appearance.theme_mode"].dirty);
        assert!(!report.prefs_changed);
    }

    #[test]
    fn keys_outside_the_catalog_and_local_only_keys_are_never_applied() {
        let mut state = AgentState::default();
        state.sync.local_only_keys = vec!["appearance.theme_mode".to_owned()];
        let report = apply_pull(
            &mut state,
            "device-1",
            vec![
                item("ui.show_sidebar_devices", json!(false), 1, "device-2"),
                item("appearance.theme_mode", json!("dark"), 2, "device-2"),
                item("general.locale", json!("zh"), 3, "device-2"),
            ],
            &BTreeMap::new(),
        );
        assert!(
            !state
                .preferences
                .values
                .contains_key("ui.show_sidebar_devices")
        );
        assert!(
            !state
                .preferences
                .values
                .contains_key("appearance.theme_mode")
        );
        assert_eq!(state.preferences.values["general.locale"], json!("zh"));
        assert_eq!(report.skipped.len(), 2);
        assert!(!state.sync_entries.contains_key("ui.show_sidebar_devices"));
    }

    #[test]
    fn poison_pill_is_skipped_without_blocking_the_other_items() {
        let mut state = AgentState::default();
        let report = apply_pull(
            &mut state,
            "device-1",
            vec![
                // 校验失败（范围外 / 类型错）的条目被跳过……
                item(
                    "download.max_concurrent_tasks",
                    json!(999_999),
                    1,
                    "device-2",
                ),
                item("appearance.theme_mode", json!(42), 2, "device-2"),
                // ……后面的合法条目照常应用，daemon 键进入待写集合。
                item("general.locale", json!("en"), 3, "device-2"),
                item("download.default_segments", json!(8), 4, "device-2"),
            ],
            &BTreeMap::new(),
        );
        assert_eq!(state.preferences.values["general.locale"], json!("en"));
        assert_eq!(
            report.daemon_changes.get("default_segments"),
            Some(&"8".to_owned())
        );
        assert!(!report.daemon_changes.contains_key("max_concurrent_tasks"));
        assert!(
            !state
                .preferences
                .values
                .contains_key("appearance.theme_mode")
        );
        assert_eq!(report.skipped.len(), 2);
    }

    #[test]
    fn echo_and_equal_values_do_not_patch_the_daemon_again() {
        let mut state = AgentState::default();
        let daemon = BTreeMap::from([("default_segments".to_owned(), "8".to_owned())]);
        // 本机回声。
        let echo = apply_pull(
            &mut state,
            "device-1",
            vec![item("download.default_segments", json!(8), 4, "device-1")],
            &daemon,
        );
        assert!(echo.daemon_changes.is_empty());
        // 别的设备写入的值与 daemon 当前值相同。
        let same = apply_pull(
            &mut state,
            "device-1",
            vec![item("download.default_segments", json!(8), 5, "device-2")],
            &daemon,
        );
        assert!(same.daemon_changes.is_empty());
        // 不同则需要写入。
        let different = apply_pull(
            &mut state,
            "device-1",
            vec![item("download.default_segments", json!(16), 6, "device-2")],
            &daemon,
        );
        assert_eq!(different.daemon_changes.len(), 1);
    }

    #[test]
    fn tombstones_restore_defaults_for_preferences_and_daemon_keys() {
        let mut state = AgentState::default();
        state
            .preferences
            .values
            .insert("general.locale".to_owned(), json!("zh"));
        let mut locale = item("general.locale", Value::Null, 5, "device-2");
        locale.deleted = true;
        let mut segments = item("download.default_segments", Value::Null, 6, "device-2");
        segments.deleted = true;
        let report = apply_pull(
            &mut state,
            "device-1",
            vec![locale, segments],
            &BTreeMap::from([("default_segments".to_owned(), "8".to_owned())]),
        );
        assert!(!state.preferences.values.contains_key("general.locale"));
        assert_eq!(
            report
                .daemon_changes
                .get("default_segments")
                .map(String::as_str),
            Some(fluxdown_protocol::daemon_config_default("default_segments"))
        );
        assert!(state.sync_entries["general.locale"].deleted);
    }

    #[test]
    fn first_sync_seeds_local_values_missing_from_the_cloud_but_never_local_only_keys() {
        let mut state = AgentState::default();
        state
            .preferences
            .values
            .insert("appearance.theme_mode".to_owned(), json!("dark"));
        state
            .preferences
            .values
            .insert("general.locale".to_owned(), json!("zh"));
        state
            .preferences
            .values
            .insert("ui.show_sidebar_devices".to_owned(), json!(false));
        state.sync.local_only_keys = vec!["general.locale".to_owned()];
        let daemon = BTreeMap::from([("default_segments".to_owned(), "4".to_owned())]);
        // 云端已经有 theme_mode：不播种覆盖。
        let remote_keys = ["appearance.theme_mode".to_owned()].into_iter().collect();
        seed_local_values(&mut state, &remote_keys, &daemon, false);
        let seeded = dirty_entries(&state);
        assert!(!seeded.contains_key("appearance.theme_mode"));
        assert!(
            !seeded.contains_key("general.locale"),
            "local-only keys are not seeded"
        );
        assert!(
            !seeded.contains_key("ui.show_sidebar_devices"),
            "device-local keys never sync"
        );
        assert_eq!(seeded["download.default_segments"].value, json!(4));
    }

    #[test]
    fn resync_re_dirties_existing_entries_the_cloud_lost() {
        let mut state = AgentState::default();
        state.sync_entries.insert(
            "general.locale".to_owned(),
            PersistedSyncEntry {
                value: json!("zh"),
                version: 3,
                dirty: false,
                deleted: false,
                edited_at_ms: None,
            },
        );
        seed_local_values(&mut state, &Default::default(), &BTreeMap::new(), true);
        assert!(state.sync_entries["general.locale"].dirty);
    }

    #[test]
    fn dirty_entries_exclude_local_only_and_out_of_catalog_keys() {
        let mut state = AgentState::default();
        state.sync.local_only_keys = vec!["general.locale".to_owned()];
        for key in [
            "general.locale",
            "ui.show_sidebar_devices",
            "appearance.theme_mode",
        ] {
            state.sync_entries.insert(key.to_owned(), dirty(json!("x")));
        }
        let keys = dirty_entries(&state).into_keys().collect::<Vec<_>>();
        assert_eq!(keys, ["appearance.theme_mode"]);
    }

    fn theme_text(name: &str) -> Value {
        json!(format!(r#"{{"meta":{{"name":"{name}"}}}}"#))
    }

    /// 主题逐个成键：首次播种逐个上云，云端增删按键应用；整组本机专属时成员键既不推送也不应用，
    /// 集合范围键本身即便出现在云端也不落成偏好。
    #[test]
    fn custom_themes_sync_per_theme_and_follow_the_collection_local_only_switch() {
        let ocean = fluxdown_protocol::custom_theme_key("ocean").expect("key");
        let nord = fluxdown_protocol::custom_theme_key("nord-square").expect("key");
        let rose = fluxdown_protocol::custom_theme_key("rose").expect("key");
        let mut state = AgentState::default();
        state
            .preferences
            .values
            .insert(ocean.clone(), theme_text("Ocean"));
        state
            .preferences
            .values
            .insert(nord.clone(), theme_text("Nord"));
        let remote_keys = [nord.clone()].into_iter().collect();
        seed_local_values(&mut state, &remote_keys, &BTreeMap::new(), false);
        let seeded = dirty_entries(&state);
        assert_eq!(seeded[&ocean].value, theme_text("Ocean"));
        assert!(
            !seeded.contains_key(&nord),
            "the cloud copy is not overwritten"
        );
        assert!(!seeded.contains_key(fluxdown_protocol::CUSTOM_THEMES_KEY));

        let mut gone = item(&nord, Value::Null, 5, "device-2");
        gone.deleted = true;
        let report = apply_pull(
            &mut state,
            "device-1",
            vec![
                gone,
                item(&rose, theme_text("Rose"), 6, "device-2"),
                item(
                    fluxdown_protocol::CUSTOM_THEMES_KEY,
                    theme_text("Root"),
                    7,
                    "device-2",
                ),
            ],
            &BTreeMap::new(),
        );
        assert!(report.prefs_changed);
        assert!(!state.preferences.values.contains_key(&nord));
        assert_eq!(state.preferences.values[&rose], theme_text("Rose"));
        assert!(
            !state
                .preferences
                .values
                .contains_key(fluxdown_protocol::CUSTOM_THEMES_KEY)
        );

        state.sync.local_only_keys = vec![fluxdown_protocol::CUSTOM_THEMES_KEY.to_owned()];
        assert!(
            dirty_entries(&state).is_empty(),
            "local-only members are not pushed"
        );
        apply_pull(
            &mut state,
            "device-1",
            vec![item(&rose, theme_text("Rose 2"), 8, "device-2")],
            &BTreeMap::new(),
        );
        assert_eq!(state.preferences.values[&rose], theme_text("Rose"));
    }

    #[test]
    fn categories_are_pushed_without_save_dir_and_pulled_keeping_the_local_one() {
        let local = json!([
            {"id": "video", "name": "Video", "saveDir": "/mnt/video"},
            {"id": "docs", "name": "Docs", "saveDir": ""}
        ]);
        let wire = categories_to_wire(&local).expect("wire");
        assert!(
            wire.as_array()
                .expect("array")
                .iter()
                .all(|category| category.get("saveDir").is_none())
        );

        let remote = json!([
            {"id": "video", "name": "Movies"},
            {"id": "new", "name": "Fresh", "saveDir": "/other/machine"}
        ]);
        let merged = merge_categories_from_wire(&remote, Some(&local)).expect("merged");
        let merged = merged.as_array().expect("array");
        assert_eq!(merged[0]["name"], "Movies");
        assert_eq!(merged[0]["saveDir"], "/mnt/video", "local saveDir survives");
        assert!(
            merged[1].get("saveDir").is_none(),
            "another device's directory is dropped"
        );

        // 本机偏好是 JSON 字符串时保持字符串形态。
        let local_string = Value::String(local.to_string());
        let merged =
            merge_categories_from_wire(&remote, Some(&local_string)).expect("merged string");
        assert!(merged.is_string());
        assert!(merge_categories_from_wire(&json!("nope"), None).is_err());
    }

    // ───────────────────────── 需要 FluxCloud mock 的流程 ─────────────────────────

    #[derive(Default)]
    struct SyncMockState {
        pulls: AtomicUsize,
        events: AtomicUsize,
        push_revision: AtomicUsize,
        pushes: Mutex<Vec<Value>>,
        /// 推送响应里的 `conflicts`（云端判负、返回胜出现值的条目）。
        push_conflicts: Mutex<Vec<Value>>,
        pull_status: Mutex<Option<(StatusCode, Value)>>,
        pull_items: Mutex<Vec<Value>>,
        pull_barrier: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
        /// `true`：SSE 发出首个事件后保持连接不再有数据（长连接）；否则发完即断（触发重连）。
        hold_open: std::sync::atomic::AtomicBool,
    }

    async fn mock_pull(State(state): State<Arc<SyncMockState>>) -> axum::response::Response {
        state.pulls.fetch_add(1, Ordering::SeqCst);
        let barrier = state.pull_barrier.lock().await.clone();
        if let Some((started, release)) = barrier {
            started.notify_one();
            release.notified().await;
        }
        if let Some((status, body)) = state.pull_status.lock().await.clone() {
            return (status, axum::Json(body)).into_response();
        }
        let items = state.pull_items.lock().await.clone();
        axum::Json(json!({"revision": 7, "resync": false, "items": items})).into_response()
    }

    async fn mock_push(
        State(state): State<Arc<SyncMockState>>,
        axum::Json(body): axum::Json<Value>,
    ) -> impl IntoResponse {
        state.pushes.lock().await.push(body);
        let revision = match state.push_revision.load(Ordering::SeqCst) {
            0 => 9,
            value => value,
        };
        let conflicts = state.push_conflicts.lock().await.clone();
        axum::Json(json!({"revision": revision, "conflicts": conflicts}))
    }

    async fn mock_events(
        State(state): State<Arc<SyncMockState>>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        assert_eq!(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer access")
        );
        state.events.fetch_add(1, Ordering::SeqCst);
        let first = axum::body::Bytes::from_static(b"data: {\"revision\":7}\n\n");
        let body = if state.hold_open.load(Ordering::SeqCst) {
            axum::body::Body::from_stream(
                futures_util::stream::once(std::future::ready(Ok::<_, std::convert::Infallible>(
                    first,
                )))
                .chain(futures_util::stream::pending()),
            )
        } else {
            axum::body::Body::from(first)
        };
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            body,
        )
    }

    #[tokio::test]
    async fn old_sync_event_cannot_resample_reopened_same_uid() {
        let harness = Harness::new("sync_event_epoch", |state| state.sync.enabled = true).await;
        let epoch = harness.service.cloud.request_epoch();
        let credentials = harness.state.lock().await.credentials.clone();
        harness.service.cloud.clear_session().await.expect("logout");
        {
            let mut state = harness.state.lock().await;
            state.credentials = credentials;
            state.bind_account(Some("u1"));
            state.sync.enabled = true;
            state.sync.revision = 99;
        }
        assert!(
            harness
                .service
                .handle_sse_line(b"data: {\"revision\":7}\n", epoch)
                .await
                .is_err()
        );
        assert_eq!(harness.mock.pulls.load(Ordering::SeqCst), 0);
        assert_eq!(harness.state.lock().await.sync.revision, 99);
        harness.finish().await;
    }

    #[tokio::test]
    async fn old_sync_response_and_error_cannot_modify_reopened_account() {
        let harness = Harness::new("sync_epoch", |state| state.sync.enabled = true).await;
        let epoch = harness.service.cloud.request_epoch();
        let credentials = harness.state.lock().await.credentials.clone();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *harness.mock.pull_barrier.lock().await = Some((started.clone(), release.clone()));
        let request = harness.service.sync_once();
        let replace = async {
            started.notified().await;
            harness.service.cloud.clear_session().await.expect("logout");
            let mut state = harness.state.lock().await;
            // Reopening the same UID is a different epoch too (UID-only guards
            // cannot distinguish this logout/login ABA).
            state.credentials = credentials;
            state.bind_account(Some("u1"));
            state.sync.revision = 99;
            state.sync.last_error = None;
            drop(state);
            release.notify_one();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(request, replace)
        })
        .await
        .expect("barrier completes");
        let error = result.expect_err("old pull rejected");
        harness.service.record_error(&error, epoch).await;
        let state = harness.state.lock().await;
        assert_eq!(state.sync.revision, 99);
        assert!(state.sync.last_error.is_none());
        assert!(!state.sync_pulled);
        drop(state);
        harness.finish().await;
    }

    struct Harness {
        service: Arc<SyncService>,
        state: Arc<Mutex<AgentState>>,
        store: Arc<StateStore>,
        mock: Arc<SyncMockState>,
        dir: std::path::PathBuf,
    }

    impl Harness {
        async fn new(label: &str, configure: impl FnOnce(&mut AgentState)) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind sync mock");
            let address = listener.local_addr().expect("sync mock address");
            let mock = Arc::new(SyncMockState::default());
            let app = Router::new()
                .route("/api/v1/sync/items", get(mock_pull).put(mock_push))
                .route("/api/v1/sync/events", get(mock_events))
                .with_state(mock.clone());
            tokio::spawn(async move {
                axum::serve(listener, app).await.expect("serve sync mock");
            });
            let dir = std::env::temp_dir().join(format!(
                "fluxdown_sync_{label}_{}_{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let store = Arc::new(
                StateStore::open(dir.clone())
                    .await
                    .expect("sync state store"),
            );
            let session = serde_json::from_value(json!({
                "user": { "id": "u1", "email": "user@example.com" },
                "device": { "id": "row1", "deviceId": "device-1" }
            }))
            .expect("session dto");
            let mut initial = AgentState {
                device_id: "device-1".to_owned(),
                account_uid: Some("u1".to_owned()),
                credentials: Some(CloudCredentials {
                    access_token: "access".to_owned(),
                    refresh_token: "refresh".to_owned(),
                    expires_at_unix: i64::MAX,
                    session: Some(session),
                }),
                ..Default::default()
            };
            configure(&mut initial);
            store.save(&initial).await.expect("save sync state");
            let state = Arc::new(Mutex::new(initial));
            let cloud_client = crate::cloud::CloudClient::new(
                format!("http://{address}"),
                state.clone(),
                store.clone(),
            )
            .expect("sync cloud client");
            let events =
                crate::event_hub::AgentEventHub::new(fluxdown_protocol::AgentSnapshot::default());
            let service = Arc::new(SyncService::new(
                crate::cloud::CloudApi::new(cloud_client),
                Arc::new(crate::daemon_client::DaemonClient::disconnected()),
                events,
                state.clone(),
                store.clone(),
            ));
            Self {
                service,
                state,
                store,
                mock,
                dir,
            }
        }

        async fn wait_until(
            &self,
            what: &str,
            condition: impl Fn(&AgentState, &SyncMockState) -> bool,
        ) {
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                loop {
                    {
                        let state = self.state.lock().await;
                        if condition(&state, &self.mock) {
                            return;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for: {what}"));
        }

        async fn finish(self) {
            let Self {
                service,
                state,
                store,
                dir,
                ..
            } = self;
            drop(service);
            drop(state);
            drop(store);
            if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!(path = %dir.display(), error = %error, "could not remove sync test directory");
            }
        }
    }

    #[tokio::test]
    async fn enabled_worker_reconnects_authenticated_sse_after_disconnect() {
        let harness = Harness::new("reconnect", |state| state.sync.enabled = true).await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        let mock = harness.mock.clone();
        // 断开后按真实的 `RETRY_DELAYS[0]`（5s）退避重连；超时须留足余量，避免慢 CI 偶发超时。
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while mock.pulls.load(Ordering::SeqCst) < 2 || mock.events.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("sync worker reconnected SSE");
        cancel.cancel();
        worker.await.expect("join sync worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn login_enables_sync_by_default_unless_the_user_turned_it_off() {
        let harness = Harness::new("auto_enable", |_| {}).await;
        harness.mock.hold_open.store(true, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        harness
            .wait_until("sync enabled and connected after login", |state, _| {
                state.sync.enabled && state.sync.connected
            })
            .await;
        assert!(
            harness
                .state
                .lock()
                .await
                .sync
                .last_synced_at_unix_ms
                .is_some()
        );
        cancel.cancel();
        worker.await.expect("join worker");
        harness.finish().await;

        let disabled = Harness::new("stays_off", |state| state.sync_user_disabled = true).await;
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(disabled.service.clone().run(cancel.clone()));
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(!disabled.state.lock().await.sync.enabled);
        assert_eq!(disabled.mock.pulls.load(Ordering::SeqCst), 0);
        cancel.cancel();
        worker.await.expect("join worker");
        disabled.finish().await;
    }

    #[tokio::test]
    async fn disabling_takes_effect_immediately_while_the_sse_stream_is_alive() {
        let harness = Harness::new("disable_live", |state| state.sync.enabled = true).await;
        // 长连接：SSE 之后一直沉默。
        harness.mock.hold_open.store(true, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        harness
            .wait_until("sync connected", |state, _| state.sync.connected)
            .await;
        harness
            .service
            .set_enabled(false)
            .await
            .expect("disable sync");
        harness
            .wait_until("sync stopped", |state, _| !state.sync.connected)
            .await;
        let pulls = harness.mock.pulls.load(Ordering::SeqCst);
        // 关闭之后本地编辑不再触发任何云端往返。
        harness
            .service
            .mark_local("appearance.theme_mode".to_owned(), json!("dark"), false)
            .await
            .expect("local edit");
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        assert_eq!(harness.mock.pulls.load(Ordering::SeqCst), pulls);
        assert!(harness.state.lock().await.sync_user_disabled);
        cancel.cancel();
        worker.await.expect("join worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn untrusted_device_halts_automatic_retries_and_reports_the_reason() {
        let harness = Harness::new("halt", |state| state.sync.enabled = true).await;
        *harness.mock.pull_status.lock().await = Some((
            StatusCode::FORBIDDEN,
            json!({"code": "sync_device_untrusted", "message": "device is not trusted"}),
        ));
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        harness
            .wait_until("halted", |state, _| state.sync.halted)
            .await;
        {
            let state = harness.state.lock().await;
            assert_eq!(
                state.sync.last_error_reason,
                Some(ErrorReason::DeviceUntrusted)
            );
            assert!(!state.sync.connected);
        }
        let pulls = harness.mock.pulls.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        assert_eq!(
            harness.mock.pulls.load(Ordering::SeqCst),
            pulls,
            "no automatic retry while halted"
        );

        // 用户重新启用后恢复。SSE 保持长连接：否则 `connected` 只在发完即断的一瞬为真，
        // 10ms 轮询可能错过，下一次重连又落在退避之后而超时。
        harness.mock.hold_open.store(true, Ordering::SeqCst);
        *harness.mock.pull_status.lock().await = None;
        harness.service.set_enabled(true).await.expect("re-enable");
        harness
            .wait_until("recovered", |state, _| {
                !state.sync.halted && state.sync.connected
            })
            .await;
        cancel.cancel();
        worker.await.expect("join worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn online_device_limit_never_halts_and_resumes_when_the_remote_channel_connects() {
        let harness = Harness::new("online_limit", |state| state.sync.enabled = true).await;
        *harness.mock.pull_status.lock().await = Some((
            StatusCode::FORBIDDEN,
            json!({"code": "online_device_limit", "message": "too many online devices"}),
        ));
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        harness
            .wait_until("limit recorded", |state, _| {
                state.sync.last_error_reason == Some(ErrorReason::OnlineDeviceLimit)
            })
            .await;
        {
            let state = harness.state.lock().await;
            assert!(!state.sync.halted, "the online limit must not halt sync");
            assert!(!state.sync.connected);
        }
        let pulls = harness.mock.pulls.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert_eq!(
            harness.mock.pulls.load(Ordering::SeqCst),
            pulls,
            "retries wait a fixed 30s instead of escalating or spinning"
        );

        // 名额让出：本机任务通道连上的瞬间立即恢复，不必等满 30 秒。
        harness.mock.hold_open.store(true, Ordering::SeqCst);
        *harness.mock.pull_status.lock().await = None;
        harness
            .service
            .events
            .publish(AgentEvent::CloudConnectionChanged(CloudConnectionDto {
                state: CloudConnectionState::Connected,
                last_error: None,
                last_error_reason: None,
            }));
        harness
            .wait_until("recovered", |state, _| {
                state.sync.connected && state.sync.last_error_reason.is_none()
            })
            .await;
        cancel.cancel();
        worker.await.expect("join worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn manual_reconnect_request_ends_the_online_limit_wait() {
        let harness = Harness::new("online_limit_kick", |state| state.sync.enabled = true).await;
        *harness.mock.pull_status.lock().await = Some((
            StatusCode::FORBIDDEN,
            json!({"code": "online_device_limit", "message": "too many online devices"}),
        ));
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(harness.service.clone().run(cancel.clone()));
        harness
            .wait_until("limit recorded", |state, _| {
                state.sync.last_error_reason == Some(ErrorReason::OnlineDeviceLimit)
            })
            .await;
        harness.mock.hold_open.store(true, Ordering::SeqCst);
        *harness.mock.pull_status.lock().await = None;
        harness.service.request_reconnect();
        harness
            .wait_until("recovered", |state, _| state.sync.connected)
            .await;
        cancel.cancel();
        worker.await.expect("join worker");
        harness.finish().await;
    }

    #[tokio::test]
    async fn push_carries_base_version_and_edit_time_and_conflicts_are_applied_clean() {
        let harness = Harness::new("push_conflicts", |state| {
            state.sync.enabled = true;
            state.sync.revision = 5;
            state.sync_pulled = true;
            state
                .preferences
                .values
                .insert("appearance.theme_mode".to_owned(), json!("dark"));
            state.sync_entries.insert(
                "appearance.theme_mode".to_owned(),
                PersistedSyncEntry {
                    value: json!("dark"),
                    version: 3,
                    dirty: true,
                    deleted: false,
                    edited_at_ms: Some(1_000),
                },
            );
            state.sync_entries.insert(
                "general.locale".to_owned(),
                PersistedSyncEntry {
                    value: json!("zh"),
                    version: 0,
                    dirty: true,
                    deleted: false,
                    edited_at_ms: None,
                },
            );
        })
        .await;
        *harness.mock.push_conflicts.lock().await = vec![json!({
            "key": "appearance.theme_mode",
            "value": "light",
            "deleted": false,
            "version": 5,
            "deviceId": "device-2",
            "editedAt": 2_000
        })];
        harness.service.sync_now().await.expect("sync now");

        let pushes = harness.mock.pushes.lock().await.clone();
        assert_eq!(pushes.len(), 1);
        let items = pushes[0]["items"].as_array().expect("items");
        let find = |key: &str| {
            items
                .iter()
                .find(|item| item["key"] == key)
                .unwrap_or_else(|| panic!("{key} pushed"))
        };
        let theme = find("appearance.theme_mode");
        assert_eq!(theme["baseVersion"], 3);
        assert_eq!(theme["editedAt"], 1_000);
        assert_eq!(theme["deleted"], false);
        assert!(
            theme.get("version").is_none(),
            "legacy `version` is not sent"
        );
        let locale = find("general.locale");
        assert_eq!(locale["baseVersion"], 0);
        assert!(
            locale.get("editedAt").is_none(),
            "seeded entries have no real edit time"
        );

        let state = harness.state.lock().await;
        let theme = &state.sync_entries["appearance.theme_mode"];
        assert_eq!(theme.value, json!("light"));
        assert!(!theme.dirty);
        assert_eq!(theme.version, 5);
        assert_eq!(theme.edited_at_ms, Some(2_000));
        assert_eq!(
            state.preferences.values["appearance.theme_mode"],
            json!("light"),
            "the winning cloud value replaces the rejected local edit"
        );
        assert!(!state.sync_entries["general.locale"].dirty);
        assert!(state.sync.dirty_keys.is_empty());
        drop(state);
        harness.finish().await;
    }

    #[tokio::test]
    async fn local_edits_record_an_edit_time_and_keep_the_base_version() {
        let harness = Harness::new("edit_time", |state| {
            state.sync.enabled = true;
            state.sync_entries.insert(
                "appearance.theme_mode".to_owned(),
                PersistedSyncEntry {
                    value: json!("light"),
                    version: 4,
                    dirty: false,
                    deleted: false,
                    edited_at_ms: Some(10),
                },
            );
        })
        .await;
        let before = super::now_unix_ms();
        harness
            .service
            .mark_local("appearance.theme_mode".to_owned(), json!("dark"), false)
            .await
            .expect("mark local");
        let after = super::now_unix_ms();
        let state = harness.state.lock().await;
        let entry = &state.sync_entries["appearance.theme_mode"];
        assert!(entry.dirty);
        assert_eq!(
            entry.version, 4,
            "the base version is the last cloud version seen"
        );
        let edited_at = entry.edited_at_ms.expect("edit time recorded");
        assert!((before..=after).contains(&edited_at));
        drop(state);
        harness.finish().await;
    }

    #[test]
    fn pull_keeps_the_base_version_of_dirty_entries_and_records_remote_edit_time() {
        let mut state = AgentState::default();
        state
            .sync_entries
            .insert("appearance.theme_mode".to_owned(), dirty(json!("dark")));
        apply_pull(
            &mut state,
            "device-1",
            vec![item("appearance.theme_mode", json!("light"), 7, "device-2")],
            &BTreeMap::new(),
        );
        let theme = &state.sync_entries["appearance.theme_mode"];
        assert!(theme.dirty);
        assert_eq!(
            theme.version, 1,
            "base version must not jump past unseen cloud edits"
        );

        let mut locale = item("general.locale", json!("zh"), 2, "device-2");
        locale.edited_at = Some(42);
        apply_pull(&mut state, "device-1", vec![locale], &BTreeMap::new());
        assert_eq!(state.sync_entries["general.locale"].edited_at_ms, Some(42));
    }

    #[test]
    fn legacy_persisted_entries_without_an_edit_time_still_load() {
        let entry = serde_json::from_value::<PersistedSyncEntry>(
            json!({"value": "x", "version": 2, "dirty": true, "deleted": false}),
        )
        .expect("legacy entry");
        assert_eq!(entry.edited_at_ms, None);
        assert_eq!(entry.version, 2);
    }

    #[tokio::test]
    async fn network_failures_report_cloud_unreachable() {
        // 指向一个已关闭端口。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind port");
        let address = listener.local_addr().expect("address");
        drop(listener);
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_sync_unreachable_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("store"));
        let session = serde_json::from_value(json!({
            "user": { "id": "u1", "email": "user@example.com" },
            "device": { "id": "row1", "deviceId": "device-1" }
        }))
        .expect("session dto");
        let state = Arc::new(Mutex::new(AgentState {
            device_id: "device-1".to_owned(),
            account_uid: Some("u1".to_owned()),
            sync: fluxdown_protocol::SyncStatusDto {
                enabled: true,
                ..Default::default()
            },
            credentials: Some(CloudCredentials {
                access_token: "access".to_owned(),
                refresh_token: "refresh".to_owned(),
                expires_at_unix: i64::MAX,
                session: Some(session),
            }),
            ..Default::default()
        }));
        let service = SyncService::new(
            crate::cloud::CloudApi::new(
                crate::cloud::CloudClient::new(
                    format!("http://{address}"),
                    state.clone(),
                    store.clone(),
                )
                .expect("client"),
            ),
            Arc::new(crate::daemon_client::DaemonClient::disconnected()),
            crate::event_hub::AgentEventHub::new(fluxdown_protocol::AgentSnapshot::default()),
            state,
            store.clone(),
        );
        let error = service.sync_once().await.expect_err("nothing is listening");
        assert_eq!(error.reason(), Some(ErrorReason::CloudUnreachable));
        assert!(!error.halts(), "network failures retry automatically");
        drop(service);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "could not remove unreachable sync test directory");
        }
    }

    #[tokio::test]
    async fn a_pull_seeds_and_pushes_local_values_and_skips_cloud_items_outside_the_catalog() {
        let harness = Harness::new("seed_push", |state| {
            state.sync.enabled = true;
            state
                .preferences
                .values
                .insert("appearance.theme_mode".to_owned(), json!("dark"));
        })
        .await;
        *harness.mock.pull_items.lock().await = vec![
            json!({"key": "ui.show_sidebar_devices", "value": false, "version": 1, "deviceId": "device-2"}),
            json!({"key": "general.locale", "value": "zh", "version": 2, "deviceId": "device-2"}),
        ];
        harness.service.sync_now().await.expect("sync now");
        let state = harness.state.lock().await;
        assert!(
            !state
                .preferences
                .values
                .contains_key("ui.show_sidebar_devices")
        );
        assert_eq!(state.preferences.values["general.locale"], json!("zh"));
        assert_eq!(
            state.sync.revision, 7,
            "a push revision more than one past the pull keeps the pull watermark"
        );
        assert!(state.sync.dirty_keys.is_empty());
        drop(state);
        let pushes = harness.mock.pushes.lock().await.clone();
        assert_eq!(pushes.len(), 1);
        let pushed_keys = pushes[0]["items"]
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|item| item["key"].as_str())
            .collect::<Vec<_>>();
        assert!(
            pushed_keys.contains(&"appearance.theme_mode"),
            "unsynced local value is seeded"
        );
        assert!(!pushed_keys.contains(&"ui.show_sidebar_devices"));
        assert!(
            !pushed_keys.contains(&"general.locale"),
            "echo of a fresh cloud item is not re-pushed"
        );
        harness.finish().await;
    }

    /// 主题逐个成键后首次播种可能超过云端单批 128 条：分批推送且全部确认。
    #[tokio::test]
    async fn many_custom_themes_are_pushed_in_batches_within_the_cloud_item_limit() {
        let harness = Harness::new("theme_batches", |state| {
            state.sync.enabled = true;
            for index in 0..140 {
                let key =
                    fluxdown_protocol::custom_theme_key(&format!("theme-{index}")).expect("key");
                state
                    .preferences
                    .values
                    .insert(key, theme_text(&index.to_string()));
            }
        })
        .await;
        harness.mock.push_revision.store(8, Ordering::SeqCst);
        harness.service.sync_now().await.expect("sync now");
        let pushes = harness.mock.pushes.lock().await.clone();
        let sizes = pushes
            .iter()
            .map(|body| body["items"].as_array().map_or(0, Vec::len))
            .collect::<Vec<_>>();
        assert_eq!(sizes, [128, 12]);
        let state = harness.state.lock().await;
        assert!(state.sync.dirty_keys.is_empty());
        assert_eq!(
            state.sync.revision, 8,
            "both batches confirm the same contiguous revision"
        );
        drop(state);
        let error = harness
            .service
            .mark_local(
                fluxdown_protocol::CUSTOM_THEMES_KEY.to_owned(),
                theme_text("Root"),
                false,
            )
            .await
            .expect_err("the collection key carries no value");
        assert!(matches!(error, super::SyncError::InvalidValue(_)));
        harness.finish().await;
    }

    #[tokio::test]
    async fn a_push_revision_right_after_the_pull_fast_forwards_the_watermark() {
        let harness = Harness::new("push_fast_forward", |state| {
            state.sync.enabled = true;
            state
                .preferences
                .values
                .insert("appearance.theme_mode".to_owned(), json!("dark"));
        })
        .await;
        harness.mock.push_revision.store(8, Ordering::SeqCst);
        harness.service.sync_now().await.expect("sync now");
        assert_eq!(harness.state.lock().await.sync.revision, 8);
        harness.finish().await;
    }

    #[tokio::test]
    async fn a_previous_accounts_dirty_keys_are_stashed_and_never_pushed_to_the_new_account() {
        let harness = Harness::new("account_switch", |state| {
            state.sync.enabled = true;
            state.sync.revision = 20;
            state.sync_pulled = true;
            state
                .sync_entries
                .insert("appearance.theme_mode".to_owned(), dirty(json!("dark")));
            // 同步状态属于 u1，而当前登录会话已经是 u2。
            if let Some(session) = state
                .credentials
                .as_mut()
                .and_then(|credentials| credentials.session.as_mut())
            {
                session.user.id = "u2".to_owned();
            }
        })
        .await;
        harness.service.sync_now().await.expect("sync as u2");
        assert!(
            harness.mock.pushes.lock().await.is_empty(),
            "u1's dirty key must not be pushed to u2"
        );
        let state = harness.state.lock().await;
        assert_eq!(state.account_uid.as_deref(), Some("u2"));
        assert_eq!(state.sync.revision, 7, "u2 starts from its own watermark");
        assert!(state.sync_stash["u1"].entries["appearance.theme_mode"].dirty);
        assert_eq!(state.sync_stash["u1"].revision, 20);
        drop(state);
        harness.finish().await;
    }

    #[tokio::test]
    async fn set_local_only_validates_keys_and_stops_pushing_them() {
        let harness = Harness::new("local_only", |state| state.sync.enabled = true).await;
        let unknown = harness
            .service
            .set_local_only(&["ui.show_sidebar_devices".to_owned()], true)
            .await;
        assert!(matches!(unknown, Err(super::SyncError::UnknownKey(_))));

        harness
            .service
            .mark_local("general.locale".to_owned(), json!("zh"), false)
            .await
            .expect("local edit");
        assert_eq!(
            harness.state.lock().await.sync.dirty_keys,
            ["general.locale"]
        );
        let status = harness
            .service
            .set_local_only(&["general.locale".to_owned()], true)
            .await
            .expect("mark local only");
        assert_eq!(status.local_only_keys, ["general.locale"]);
        assert!(
            status.dirty_keys.is_empty(),
            "pending edit of a local-only key is dropped"
        );

        // 之后的编辑只在本机生效。
        harness
            .service
            .mark_local("general.locale".to_owned(), json!("en"), false)
            .await
            .expect("edit after local-only");
        {
            let state = harness.state.lock().await;
            assert_eq!(state.preferences.values["general.locale"], json!("en"));
            assert!(state.sync.dirty_keys.is_empty());
        }
        harness.service.sync_now().await.expect("sync");
        let pushed = harness.mock.pushes.lock().await.clone();
        assert!(pushed.iter().all(|body| {
            body["items"]
                .as_array()
                .is_some_and(|items| items.iter().all(|item| item["key"] != "general.locale"))
        }));

        // 取消本机专属：下一轮全量重拉。
        harness.state.lock().await.sync.revision = 42;
        let status = harness
            .service
            .set_local_only(&["general.locale".to_owned()], false)
            .await
            .expect("rejoin sync");
        assert!(status.local_only_keys.is_empty());
        assert_eq!(status.revision, 0);
        harness.finish().await;
    }

    #[tokio::test]
    async fn off_catalog_keys_are_stored_locally_but_never_become_sync_entries() {
        let harness = Harness::new("off_catalog", |_| {}).await;
        harness
            .service
            .mark_local("ui.show_sidebar_devices".to_owned(), json!(false), false)
            .await
            .expect("device-local preference");
        let state = harness.state.lock().await;
        assert_eq!(
            state.preferences.values["ui.show_sidebar_devices"],
            json!(false)
        );
        assert!(state.sync_entries.is_empty());
        assert!(state.sync.dirty_keys.is_empty());
        drop(state);
        harness.finish().await;
    }

    #[tokio::test]
    async fn restoring_a_default_creates_a_tombstone_that_is_pushed() {
        let harness = Harness::new("tombstone", |state| state.sync.enabled = true).await;
        harness
            .service
            .mark_local("general.locale".to_owned(), json!("zh"), false)
            .await
            .expect("set");
        harness
            .service
            .mark_local("general.locale".to_owned(), Value::Null, true)
            .await
            .expect("restore default");
        assert!(
            !harness
                .state
                .lock()
                .await
                .preferences
                .values
                .contains_key("general.locale")
        );
        harness.service.sync_now().await.expect("sync");
        let pushes = harness.mock.pushes.lock().await.clone();
        let item = pushes[0]["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|item| item["key"] == "general.locale")
            .cloned()
            .expect("tombstone pushed");
        assert_eq!(item["deleted"], true);
        harness.finish().await;
    }

    /// 客户端靠写入返回的 revision 判断在途写入何时被事件确认：返回值必须恰好是携带
    /// 本次写入的 `PreferencesChanged` 的 revision，且严格单调。
    #[tokio::test]
    async fn preference_writes_return_the_revision_of_the_event_that_carries_them() {
        let harness = Harness::new("write_revision", |_| {}).await;
        let (mut events, _) = harness.service.events.subscribe_and_snapshot();
        let mut next_preferences = async || loop {
            let frame = events.recv().await.expect("event");
            if let fluxdown_protocol::ServiceEvent::Agent(
                fluxdown_protocol::AgentEvent::PreferencesChanged(prefs),
            ) = frame.event.clone()
            {
                return prefs;
            }
        };

        let synced = harness
            .service
            .mark_local("general.locale".to_owned(), json!("zh"), false)
            .await
            .expect("synced write");
        let event = next_preferences().await;
        assert_eq!(event.revision, synced);
        assert_eq!(event.values["general.locale"], json!("zh"));

        let local = harness
            .service
            .set_local_preference("desktop.window.main".to_owned(), json!({ "w": 1 }), false)
            .await
            .expect("local write");
        assert!(local > synced);
        let event = next_preferences().await;
        assert_eq!(event.revision, local);
        assert_eq!(event.values["desktop.window.main"], json!({ "w": 1 }));
        harness.finish().await;
    }
}
