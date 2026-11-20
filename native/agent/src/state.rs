//! agent 私有状态的独占锁、权限与原子持久化。

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fluxdown_protocol::local_service::{GATEWAY_ENDPOINT_FILE, LocalEndpoint};
use fluxdown_protocol::{
    AgentPreferencesDto, AgentSessionDto, GatewayStatusDto, RemoteTaskDto, SyncStatusDto,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use uuid::Uuid;

/// FluxCloud 令牌只存在 agent 私有状态与云传输层。
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_unix: i64,
    pub session: Option<AgentSessionDto>,
    /// 签发这组凭据的 FluxCloud 地址（规范化、无尾斜杠）；旧状态文件缺省为空 = 未知。
    /// 仅调试构建据此判断「当前地址的拒绝是否代表会话失效」（见 `CloudClient::reject_refresh_token`）。
    #[serde(default)]
    pub issuer: String,
}

/// 单个配置同步键的私有持久化状态。
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedSyncEntry {
    pub value: serde_json::Value,
    pub version: u64,
    pub dirty: bool,
    pub deleted: bool,
    /// 该键当前值的编辑时刻（unix 毫秒）：本机编辑取按 FluxCloud 时钟校正的本机时间，云端回写取云端
    /// `editedAt`；推送时作为冲突裁决依据。播种不是真实编辑，为 `None`。旧状态文件缺省为 `None`。
    #[serde(default)]
    pub edited_at_ms: Option<i64>,
}

/// 单个账号的同步水位、条目与远程任务接单绑定；账号切换 / 登出时暂存，重新登录同一账号时恢复。
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct AccountSyncData {
    pub revision: u64,
    pub entries: BTreeMap<String, PersistedSyncEntry>,
    pub pulled: bool,
    /// 远程任务 → 本机 daemon 任务。设备身份跨登出不变、FluxCloud 登出也不终止任务，所以重新登录同一账号后
    /// 本机仍是这些任务的执行端：恢复绑定才能接着上报进度、执行对端的暂停 / 取消，而不是凭 URL 猜。
    pub remote_bindings: BTreeMap<String, String>,
}

/// 暂存账号数上限；超出时丢弃最先的账号，避免状态文件无限增长。
const SYNC_STASH_LIMIT: usize = 8;

/// 设备身份旁路文件名（与 `agent-state.json` 同目录、同权限）。
const DEVICE_ID_FILE: &str = "device-id";

/// 与 FluxCloud 设备身份契约一致：非空、≤128 字符、不含空白与控制字符。
fn valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= 128
        && !id.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// 云端推送的设备本地偏好；默认全关（上报开启必须经 UI 显式同意）。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct CloudNotifyPrefs {
    /// 本设备是否向 FluxCloud 上报任务事件。
    pub reporting: bool,
    /// 上报时附带下载地址。
    pub include_url: bool,
    /// 上报时附带保存目录。
    pub include_save_dir: bool,
}

/// agent 可恢复状态；不包含 daemon 下载快照或捕获 header/cookie。
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct AgentState {
    pub device_id: String,
    pub device_name: String,
    pub platform: String,
    pub credentials: Option<CloudCredentials>,
    pub sync: SyncStatusDto,
    pub preferences: AgentPreferencesDto,
    /// 当前账号的同步条目（水位在 `sync.revision`）；账号切换由 [`AgentState::bind_account`] 隔离。
    pub sync_entries: BTreeMap<String, PersistedSyncEntry>,
    pub sync_pulled: bool,
    /// `sync_entries` / `sync.revision` / `remote_*` 所属账号；`None` = 未登录。
    pub account_uid: Option<String>,
    /// 其他账号暂存的同步数据（按账号隔离水位与脏键）。
    pub sync_stash: BTreeMap<String, AccountSyncData>,
    /// 用户曾显式关闭同步：登录后不再自动开启。
    pub sync_user_disabled: bool,
    pub gateway: GatewayStatusDto,
    pub gateway_user_token: String,
    pub link_identity: Option<serde_json::Value>,
    pub linked_devices: Vec<serde_json::Value>,
    pub remote_tasks: Vec<RemoteTaskDto>,
    /// 已接单的远程任务 → 本机 daemon 任务；持久化以防重启后重复建任务，并恢复进度绑定。
    pub remote_bindings: BTreeMap<String, String>,
    pub link_migration_revision: Option<u64>,
    pub gateway_migration_revision: Option<u64>,
    pub analytics_install_reported: bool,
    pub analytics_last_active_day: u64,
    /// 匿名统计专用随机 ID；刻意与 FluxCloud `device_id` 分离，统计无法关联到账号 / 设备。
    pub analytics_id: String,
    /// 调试构建下用户覆盖的 FluxCloud 地址；正式构建启动时忽略（锁定固定地址）。
    pub cloud_base_url_override: Option<String>,
    /// 云端推送的设备本地偏好（上报开关 / 隐私字段）；不进配置同步，换号不重置。
    pub cloud_notify: CloudNotifyPrefs,
}

impl AgentState {
    /// 把账号维度的状态（同步水位 / 脏键 / 远程任务与绑定）切换到 `uid`；返回是否发生了切换。
    ///
    /// 旧账号的同步数据与接单绑定暂存，重新登录同一账号时恢复；未登录期间产生的本地编辑（无所属账号）
    /// 被首个登录账号接管。远程任务投影属于账号，切换即清空，登录后由云端快照重建。
    pub fn bind_account(&mut self, uid: Option<&str>) -> bool {
        if self.account_uid.as_deref() == uid {
            return false;
        }
        let current = AccountSyncData {
            revision: self.sync.revision,
            entries: std::mem::take(&mut self.sync_entries),
            pulled: self.sync_pulled,
            remote_bindings: std::mem::take(&mut self.remote_bindings),
        };
        let next = match (self.account_uid.take(), uid) {
            (Some(previous), _) => {
                self.sync_stash.insert(previous, current);
                while self.sync_stash.len() > SYNC_STASH_LIMIT {
                    let Some(oldest) = self.sync_stash.keys().next().cloned() else {
                        break;
                    };
                    self.sync_stash.remove(&oldest);
                }
                uid.and_then(|uid| self.sync_stash.remove(uid))
                    .unwrap_or_default()
            }
            // 未登录期间的本地编辑：新账号有暂存则恢复暂存，否则接管这些编辑（水位从 0 开始）。
            // 未登录时没有绑定可接管。
            (None, Some(uid)) => self.sync_stash.remove(uid).unwrap_or(AccountSyncData {
                revision: 0,
                entries: current.entries,
                pulled: false,
                remote_bindings: BTreeMap::new(),
            }),
            (None, None) => AccountSyncData::default(),
        };
        self.sync.revision = next.revision;
        self.sync_entries = next.entries;
        self.sync_pulled = next.pulled;
        self.remote_bindings = next.remote_bindings;
        self.account_uid = uid.map(str::to_owned);
        self.remote_tasks.clear();
        self.refresh_sync_projection();
        self.reset_sync_runtime();
        true
    }

    /// 升级前的状态没有 `account_uid`：已登录时把现有同步数据归属当前会话账号。
    pub fn adopt_session_account(&mut self) {
        if self.account_uid.is_none()
            && let Some(uid) = self
                .credentials
                .as_ref()
                .and_then(|credentials| credentials.session.as_ref())
                .map(|session| session.user.id.clone())
        {
            self.account_uid = Some(uid);
        }
    }

    /// 由 `sync_entries` 重新推导投影的脏键列表。
    pub fn refresh_sync_projection(&mut self) {
        self.sync.dirty_keys = self
            .sync_entries
            .iter()
            .filter(|(_, entry)| entry.dirty)
            .map(|(key, _)| key.clone())
            .collect();
    }

    /// 同步的运行期字段不随进程 / 账号存活。
    pub fn reset_sync_runtime(&mut self) {
        self.sync.connected = false;
        self.sync.halted = false;
        self.sync.last_error = None;
        self.sync.last_error_reason = None;
        self.sync.last_synced_at_unix_ms = None;
    }
}

/// 私有状态存储错误。
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("agent state is already locked")]
    Locked,
    #[error("agent state I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("agent state JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("agent state ACL setup failed: {0}")]
    Acl(String),
}

/// 损坏状态文件的隔离备份保留个数。
const CORRUPT_BACKUP_LIMIT: usize = 3;

/// 持有 agent 独占锁的状态存储。
///
/// 写盘按「序列化时取得的代际」排序：并发调用方各自在持有状态锁时序列化，写盘阶段只需
/// 串行化文件操作，较旧的快照永远不会覆盖较新的快照，慢盘 / ACL 也不会拖住状态锁。
pub struct StateStore {
    data_dir: PathBuf,
    state_path: PathBuf,
    _lock: File,
    /// 下一个快照代际。
    generation: AtomicU64,
    /// 已落盘的最新代际；同时充当写盘互斥锁。
    written: Mutex<u64>,
    /// 数据目录已一次性设置为仅当前用户可访问（Windows ACL 可继承），保存时不再逐文件 spawn 子进程。
    acl_dir_ready: bool,
}

/// Windows 上锁争用是 `ERROR_LOCK_VIOLATION`，std 不把它映射为 `WouldBlock`。
fn is_lock_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error
            .raw_os_error()
            .is_some_and(|code| fs2::lock_contended_error().raw_os_error() == Some(code))
}

impl StateStore {
    /// 打开状态目录并获取 `<data-dir>/agent.lock`。
    pub async fn open(data_dir: PathBuf) -> Result<Self, StateError> {
        tokio::fs::create_dir_all(&data_dir).await?;
        set_private_dir_permissions(&data_dir).await?;
        let lock_path = data_dir.join("agent.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.try_lock_exclusive().map_err(|error| {
            if is_lock_contended(&error) {
                StateError::Locked
            } else {
                StateError::Io(error)
            }
        })?;
        let acl_dir_ready = match apply_windows_dir_acl(&data_dir).await {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(error = %error, "agent data directory ACL setup failed; falling back to per-file ACL");
                false
            }
        };
        remove_stale_temp_files(&data_dir).await;
        Ok(Self {
            state_path: data_dir.join("agent-state.json"),
            data_dir,
            _lock: lock,
            generation: AtomicU64::new(1),
            written: Mutex::new(0),
            acl_dir_ready,
        })
    }

    /// 读取状态；文件不存在时返回默认状态。
    ///
    /// 文件损坏（JSON 无法解析）时不让 agent 启动失败：把坏文件隔离为
    /// `agent-state.corrupt-<毫秒时间戳>.json` 备份，并以默认状态启动。
    pub async fn load(&self) -> Result<AgentState, StateError> {
        let bytes = match tokio::fs::read(&self.state_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AgentState::default());
            }
            Err(error) => return Err(StateError::Io(error)),
        };
        match serde_json::from_slice::<AgentState>(&bytes) {
            Ok(mut state) => {
                state.reset_sync_runtime();
                Ok(state)
            }
            Err(error) => {
                let backup = self.quarantine_corrupt_state().await?;
                tracing::error!(
                    error = %error,
                    backup = %backup.display(),
                    "agent state file is corrupt; started with default state (backup kept)"
                );
                Ok(AgentState::default())
            }
        }
    }

    async fn quarantine_corrupt_state(&self) -> Result<PathBuf, StateError> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let backup = self
            .data_dir
            .join(format!("agent-state.corrupt-{stamp:020}.json"));
        tokio::fs::rename(&self.state_path, &backup).await?;
        match tokio::fs::read_dir(&self.data_dir).await {
            Ok(mut entries) => {
                let mut backups = Vec::new();
                loop {
                    let entry = match entries.next_entry().await {
                        Ok(Some(entry)) => entry,
                        Ok(None) => break,
                        Err(error) => {
                            tracing::warn!(path = %self.data_dir.display(), error = %error, "could not enumerate corrupt agent state backups");
                            break;
                        }
                    };
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with("agent-state.corrupt-") && name.ends_with(".json") {
                        backups.push(entry.path());
                    }
                }
                backups.sort();
                let surplus = backups.len().saturating_sub(CORRUPT_BACKUP_LIMIT);
                for stale in backups.into_iter().take(surplus) {
                    // 已消失的旧备份无需再清理，其他失败不能掩盖隔离结果。
                    if let Err(error) = tokio::fs::remove_file(&stale).await
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        tracing::warn!(path = %stale.display(), error = %error, "could not prune old corrupt agent state backup");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(path = %self.data_dir.display(), error = %error, "could not list corrupt agent state backups");
            }
        }
        Ok(backup)
    }

    /// temp-write + fsync + atomic rename 持久化完整状态。
    ///
    /// 调用方可以持有状态锁调用（序列化在第一个 await 之前完成）；更推荐用 [`StateStore::persist`]
    /// 在写盘期间释放状态锁。
    pub async fn save(&self, state: &AgentState) -> Result<(), StateError> {
        let bytes = serde_json::to_vec(state)?;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst);
        self.write_snapshot(generation, &bytes).await
    }

    /// 持有状态锁只做序列化，写盘期间不占用状态锁。
    pub async fn persist(&self, state: &Mutex<AgentState>) -> Result<(), StateError> {
        let (bytes, generation) = {
            let state = state.lock().await;
            let bytes = serde_json::to_vec(&*state)?;
            (bytes, self.generation.fetch_add(1, Ordering::SeqCst))
        };
        self.write_snapshot(generation, &bytes).await
    }

    /// 读取旁路保存的设备身份（`<data-dir>/device-id`）；不存在、读失败或内容不合法时返回 `None`。
    ///
    /// 设备身份是 FluxCloud 识别「同一台设备」的唯一依据（任务只对发起端 / 执行端可见）。它另存一份，
    /// `agent-state.json` 损坏被隔离、以默认状态启动时仍能找回原身份，重新登录后认回自己的任务。
    pub(crate) async fn load_device_id(&self) -> Option<String> {
        let path = self.data_dir.join(DEVICE_ID_FILE);
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let id = String::from_utf8(bytes).ok()?;
                let id = id.trim();
                valid_device_id(id).then(|| id.to_owned())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                tracing::warn!(path = %path.display(), error = %error, "could not read the saved device identity");
                None
            }
        }
    }

    /// 让旁路设备身份文件与 `device_id` 一致；内容相同不写盘（空闲静默：只在首次 / 身份变化时写一次）。
    pub(crate) async fn save_device_id(&self, device_id: &str) -> Result<(), StateError> {
        if !valid_device_id(device_id) {
            return Ok(());
        }
        let path = self.data_dir.join(DEVICE_ID_FILE);
        match tokio::fs::read(&path).await {
            Ok(current) if current == device_id.as_bytes() => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StateError::Io(error)),
        }
        self.write_atomic(&path, ".device-id", device_id.as_bytes())
            .await
    }

    /// 发布桌面 Gateway 的当前端点；与 bearer 同目录，不携带任何凭据。
    /// 在已验证的运行期切换与启动时调用；相同内容不重复写盘。
    pub(crate) async fn save_gateway_endpoint(
        &self,
        directory: &Path,
        bound: std::net::SocketAddr,
    ) -> Result<(), StateError> {
        let path = directory.join(GATEWAY_ENDPOINT_FILE);
        let rpc_address = gateway_client_address(bound);
        let bytes = serde_json::to_vec(&LocalEndpoint {
            rpc_url: format!("ws://{rpc_address}/rpc"),
        })?;
        match tokio::fs::read(&path).await {
            Ok(current) if current == bytes => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StateError::Io(error)),
        }
        tokio::fs::create_dir_all(directory).await?;
        self.write_atomic(&path, ".gateway-endpoint", &bytes).await
    }

    /// 状态锁由调用方持有。两个私有文件都落定前，不提交 listener 或发布 GatewayChanged。
    /// 保留原始文件内容，也覆盖 rename 成功但父目录 fsync 失败的回滚路径。
    pub(crate) async fn save_gateway_change(
        &self,
        state: &AgentState,
        directory: &Path,
        bound: std::net::SocketAddr,
    ) -> Result<(), StateError> {
        let endpoint_path = directory.join(GATEWAY_ENDPOINT_FILE);
        let old_state = read_optional_file(&self.state_path).await?;
        let old_endpoint = read_optional_file(&endpoint_path).await?;
        let result = async {
            self.save_gateway_endpoint(directory, bound).await?;
            self.save(state).await
        }
        .await;
        if let Err(error) = result {
            let endpoint_rollback = self
                .restore_gateway_file(&endpoint_path, ".gateway-endpoint", old_endpoint.as_deref())
                .await;
            let state_rollback = self
                .restore_gateway_file(&self.state_path, ".agent-state", old_state.as_deref())
                .await;
            let mut first_rollback_error = None;
            for rollback in [endpoint_rollback, state_rollback] {
                if let Err(rollback) = rollback {
                    tracing::error!(error = %error, rollback = %rollback, "gateway persistence rollback failed");
                    if first_rollback_error.is_none() {
                        first_rollback_error = Some(rollback);
                    }
                }
            }
            if let Some(rollback) = first_rollback_error {
                return Err(StateError::Io(std::io::Error::other(format!(
                    "{error}; gateway rollback failed: {rollback}"
                ))));
            }
            return Err(error);
        }
        Ok(())
    }

    async fn restore_gateway_file(
        &self,
        path: &Path,
        prefix: &str,
        previous: Option<&[u8]>,
    ) -> Result<(), StateError> {
        if read_optional_file(path).await?.as_deref() == previous {
            return Ok(());
        }
        if path == self.state_path {
            // A newer rollback generation also fences already-serialized stale snapshots.
            let generation = self.generation.fetch_add(1, Ordering::SeqCst);
            let mut written = self.written.lock().await;
            match previous {
                Some(bytes) => self.write_atomic(path, prefix, bytes).await?,
                None => remove_optional_file(path).await?,
            }
            *written = generation;
        } else {
            match previous {
                Some(bytes) => self.write_atomic(path, prefix, bytes).await?,
                None => remove_optional_file(path).await?,
            }
        }
        Ok(())
    }

    async fn write_snapshot(&self, generation: u64, bytes: &[u8]) -> Result<(), StateError> {
        let mut written = self.written.lock().await;
        if generation < *written {
            // 更新的快照已经落盘。
            return Ok(());
        }
        let result = self
            .write_atomic(&self.state_path, ".agent-state", bytes)
            .await;
        if result.is_ok() {
            *written = generation;
        }
        result
    }

    async fn write_atomic(
        &self,
        destination: &Path,
        prefix: &str,
        bytes: &[u8],
    ) -> Result<(), StateError> {
        let directory = destination.parent().unwrap_or(&self.data_dir);
        let temp = directory.join(format!("{prefix}.{}.tmp", Uuid::new_v4()));
        let result = self.write_temp_then_rename(&temp, destination, bytes).await;
        if result.is_err() {
            // 创建临时文件前失败或 rename 后 fsync 失败时，临时路径可能本就不存在。
            if let Err(error) = tokio::fs::remove_file(&temp).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %temp.display(), error = %error, "could not remove failed agent atomic file");
            }
        }
        result
    }

    async fn write_temp_then_rename(
        &self,
        temp: &Path,
        destination: &Path,
        bytes: &[u8],
    ) -> Result<(), StateError> {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temp)
            .await?;
        set_private_file_permissions(temp).await?;
        if !self.acl_dir_ready || temp.parent() != Some(self.data_dir.as_path()) {
            apply_windows_acl(temp).await?;
        }
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(temp, destination).await?;
        sync_parent(destination.parent().unwrap_or(&self.data_dir)).await?;
        Ok(())
    }

    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

/// Only wildcard binds need a loopback substitution; specific LAN and IPv6 addresses stay intact.
pub(crate) fn gateway_client_address(bound: std::net::SocketAddr) -> std::net::SocketAddr {
    let ip = match bound.ip() {
        std::net::IpAddr::V4(ip) if ip.is_unspecified() => std::net::Ipv4Addr::LOCALHOST.into(),
        std::net::IpAddr::V6(ip) if ip.is_unspecified() => std::net::Ipv6Addr::LOCALHOST.into(),
        ip => ip,
    };
    std::net::SocketAddr::new(ip, bound.port())
}

async fn read_optional_file(path: &Path) -> Result<Option<Vec<u8>>, StateError> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StateError::Io(error)),
    }
}

async fn remove_optional_file(path: &Path) -> Result<(), StateError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {
            sync_parent(path.parent().unwrap_or_else(|| Path::new("."))).await?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StateError::Io(error)),
    }
}

/// 清理上次崩溃遗留的临时状态文件（调用方已持有独占锁，不会误删并发写入）。
async fn remove_stale_temp_files(dir: &Path) {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(path = %dir.display(), error = %error, "could not list stale agent state temp files");
            return;
        }
    };
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(path = %dir.display(), error = %error, "could not enumerate stale agent state temp files");
                break;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        // 清理时已消失的文件已达成目的；保留其他失败的诊断。
        if (name.starts_with(".agent-state.")
            || name.starts_with(".gateway-endpoint.")
            || name.starts_with(".device-id.")
            || name.starts_with(crate::startup_failure::TEMP_PREFIX))
            && name.ends_with(".tmp")
            && let Err(error) = tokio::fs::remove_file(entry.path()).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(file = %name, error = %error, "could not remove stale agent state temp file");
        }
    }
}

#[cfg(unix)]
pub(crate) async fn set_private_dir_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await
}

#[cfg(not(unix))]
pub(crate) async fn set_private_dir_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
pub(crate) async fn set_private_file_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await
}

#[cfg(not(unix))]
pub(crate) async fn set_private_file_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
async fn sync_parent(path: &Path) -> Result<(), std::io::Error> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
async fn sync_parent(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

/// 当前用户 SID；进程内只查询一次（`whoami` 子进程开销大）。
#[cfg(windows)]
async fn current_user_sid() -> Result<String, StateError> {
    use std::os::windows::process::CommandExt;
    static SID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if let Some(sid) = SID.get() {
        return Ok(sid.clone());
    }
    let output = tokio::task::spawn_blocking(|| {
        std::process::Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    })
    .await
    .map_err(|error| StateError::Acl(error.to_string()))??;
    if !output.status.success() {
        return Err(StateError::Acl("whoami /user failed".to_owned()));
    }
    let sid = parse_whoami_sid(&output.stdout)
        .ok_or_else(|| StateError::Acl("could not parse current SID".to_owned()))?;
    Ok(SID.get_or_init(|| sid).clone())
}

/// 从 `whoami /user /fo csv /nh` 的原始输出取 SID。
///
/// 输出按控制台 OEM 代码页编码（简体中文系统为 GBK），计算机名 / 用户名含非 ASCII 字符时不是
/// 合法 UTF-8。SID 是末尾字段且纯 ASCII，`,` 也不会出现在任何多字节编码的尾字节里，所以按字节
/// 切出末字段再解码，与代码页无关。
#[cfg(any(windows, test))]
fn parse_whoami_sid(output: &[u8]) -> Option<String> {
    let field = output.rsplit(|&byte| byte == b',').next()?;
    let sid = std::str::from_utf8(field).ok()?.trim().trim_matches('"');
    (sid.starts_with("S-1-")
        && sid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
    .then(|| sid.to_owned())
}

#[cfg(windows)]
async fn icacls_grant_full(path: &Path, inheritable: bool) -> Result<(), StateError> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let sid = current_user_sid().await?;
    let grant = if inheritable {
        format!("*{sid}:(OI)(CI)(F)")
    } else {
        format!("*{sid}:(F)")
    };
    let path = path.to_owned();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new("icacls")
            .arg(path)
            .args(["/inheritance:r", "/grant:r", &grant])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
    })
    .await
    .map_err(|error| StateError::Acl(error.to_string()))??;
    if status.success() {
        Ok(())
    } else {
        Err(StateError::Acl("icacls failed".to_owned()))
    }
}

/// 单个文件仅当前用户可访问。
#[cfg(windows)]
pub(crate) async fn apply_windows_acl(path: &Path) -> Result<(), StateError> {
    icacls_grant_full(path, false).await
}

/// 数据目录仅当前用户可访问，且 ACL 可继承：其后在目录内新建的文件（状态临时文件）
/// 自动继承，无需每次保存都 spawn `whoami` / `icacls`。
#[cfg(windows)]
pub(crate) async fn apply_windows_dir_acl(path: &Path) -> Result<(), StateError> {
    icacls_grant_full(path, true).await
}

#[cfg(not(windows))]
pub(crate) async fn apply_windows_acl(_path: &Path) -> Result<(), StateError> {
    Ok(())
}

#[cfg(not(windows))]
pub(crate) async fn apply_windows_dir_acl(_path: &Path) -> Result<(), StateError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::{AgentState, DEVICE_ID_FILE, PersistedSyncEntry, StateError, StateStore};

    #[test]
    fn platform_lock_contention_error_is_recognized() {
        assert!(super::is_lock_contended(&fs2::lock_contended_error()));
        assert!(super::is_lock_contended(&std::io::Error::from(
            std::io::ErrorKind::WouldBlock
        )));
        assert!(!super::is_lock_contended(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn whoami_sid_is_parsed_from_non_utf8_oem_output() {
        // 简体中文系统的 OEM 代码页（GBK）输出，计算机名与用户名为「张三」：整行不是合法 UTF-8。
        let gbk = b"\"\xd5\xc5\xc8\xfd-PC\\\xd5\xc5\xc8\xfd\",\"S-1-5-21-1-2-3-1001\"\r\n";
        assert_eq!(
            super::parse_whoami_sid(gbk).as_deref(),
            Some("S-1-5-21-1-2-3-1001")
        );
        assert_eq!(
            super::parse_whoami_sid(b"\"pc\\user\",\"S-1-5-21-1-2-3-1001\"\r\n").as_deref(),
            Some("S-1-5-21-1-2-3-1001")
        );
        assert_eq!(super::parse_whoami_sid(b"garbage"), None);
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "fluxdown_agent_{label}_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    fn dirty_entry(value: &str) -> PersistedSyncEntry {
        PersistedSyncEntry {
            value: serde_json::json!(value),
            version: 1,
            dirty: true,
            deleted: false,
            edited_at_ms: None,
        }
    }

    #[test]
    fn account_switch_isolates_sync_watermark_dirty_keys_and_remote_state() {
        let mut state = AgentState::default();
        // 未登录期间的本地编辑被首个登录账号接管，水位从 0 开始。
        state
            .sync_entries
            .insert("appearance.theme_mode".to_owned(), dirty_entry("dark"));
        state.sync.revision = 9;
        assert!(state.bind_account(Some("user-a")));
        assert_eq!(state.sync.revision, 0);
        assert!(state.sync_entries["appearance.theme_mode"].dirty);
        assert_eq!(state.sync.dirty_keys, ["appearance.theme_mode"]);

        state.sync.revision = 40;
        state
            .remote_tasks
            .push(serde_json::from_value(serde_json::json!({"id": "r1"})).expect("remote task"));
        state
            .remote_bindings
            .insert("r1".to_owned(), "t1".to_owned());

        // 切到账号 B：A 的脏键与水位不外泄，远程任务 / 绑定清空。
        assert!(state.bind_account(Some("user-b")));
        assert_eq!(state.sync.revision, 0);
        assert!(state.sync_entries.is_empty());
        assert!(state.sync.dirty_keys.is_empty());
        assert!(state.remote_tasks.is_empty());
        assert!(state.remote_bindings.is_empty());
        state
            .remote_bindings
            .insert("rb".to_owned(), "tb".to_owned());

        // 登出后再登录 A：恢复 A 的水位、脏键与接单绑定（本机仍是这些任务的执行端），B 的绑定不串号。
        state.sync.revision = 5;
        assert!(state.bind_account(None));
        assert_eq!(state.account_uid, None);
        assert!(state.remote_bindings.is_empty());
        assert!(state.bind_account(Some("user-a")));
        assert_eq!(state.sync.revision, 40);
        assert!(state.sync_entries["appearance.theme_mode"].dirty);
        assert_eq!(
            state.remote_bindings,
            BTreeMap::from([("r1".to_owned(), "t1".to_owned())])
        );
        assert!(state.remote_tasks.is_empty(), "投影由云端快照重建");
        assert!(state.bind_account(Some("user-b")));
        assert_eq!(
            state.remote_bindings,
            BTreeMap::from([("rb".to_owned(), "tb".to_owned())])
        );
        assert!(state.bind_account(Some("user-a")));

        // 相同账号再次绑定是空操作。
        assert!(!state.bind_account(Some("user-a")));
    }

    #[test]
    fn stash_is_bounded() {
        let mut state = AgentState::default();
        for index in 0..20 {
            state.bind_account(Some(&format!("user-{index:02}")));
        }
        assert!(state.sync_stash.len() <= super::SYNC_STASH_LIMIT);
    }

    #[tokio::test]
    async fn gateway_endpoint_is_private_credential_free_and_only_rewritten_on_change() {
        let dir = temp_dir("gateway_endpoint");
        let store = StateStore::open(dir.clone()).await.expect("open store");
        // 显式 bearer 覆盖目录与 agent 状态目录可以不同。
        let endpoint_dir = dir.join("bearer-location");
        tokio::fs::create_dir(&endpoint_dir)
            .await
            .expect("create external bearer directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&endpoint_dir, std::fs::Permissions::from_mode(0o755))
                .await
                .expect("set external directory permissions");
        }
        let bound = "0.0.0.0:17999".parse().expect("LAN bind");
        store
            .save_gateway_endpoint(&endpoint_dir, bound)
            .await
            .expect("publish endpoint");
        let path = endpoint_dir.join("gateway-endpoint.json");
        let first = tokio::fs::read(&path).await.expect("read endpoint");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&first).expect("endpoint JSON"),
            serde_json::json!({ "rpcUrl": "ws://127.0.0.1:17999/rpc" })
        );
        let metadata = tokio::fs::metadata(&path).await.expect("endpoint metadata");
        store
            .save_gateway_endpoint(&endpoint_dir, bound)
            .await
            .expect("publish identical endpoint");
        let unchanged = tokio::fs::metadata(&path)
            .await
            .expect("unchanged metadata");
        assert_eq!(
            metadata.modified().expect("first modification time"),
            unchanged.modified().expect("unchanged modification time")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!(
                metadata.ino(),
                unchanged.ino(),
                "identical endpoint must not be replaced"
            );
            assert_eq!(
                tokio::fs::metadata(&endpoint_dir)
                    .await
                    .expect("external directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o755,
                "endpoint publication must not chmod an external bearer parent"
            );
        }
        store
            .save_gateway_endpoint(&endpoint_dir, "[::1]:18001".parse().expect("IPv6 loopback"))
            .await
            .expect("publish changed endpoint");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &tokio::fs::read(&path).await.expect("read changed endpoint")
            )
            .expect("changed endpoint JSON"),
            serde_json::json!({ "rpcUrl": "ws://[::1]:18001/rpc" })
        );
        let temps: Vec<_> = std::fs::read_dir(&endpoint_dir)
            .expect("list endpoint directory")
            .map(|entry| entry.expect("read endpoint directory entry").file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            temps.is_empty(),
            "endpoint atomic writes must not leak temporary files"
        );
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove endpoint test directory");
        }
    }

    #[tokio::test]
    async fn gateway_endpoint_io_failure_is_reported_without_replacing_the_destination() {
        let dir = temp_dir("gateway_endpoint_failure");
        let store = StateStore::open(dir.clone()).await.expect("open store");
        let path = dir.join("gateway-endpoint.json");
        tokio::fs::create_dir(&path)
            .await
            .expect("block endpoint destination");
        let error = store
            .save_gateway_endpoint(&dir, "127.0.0.1:18001".parse().expect("loopback bind"))
            .await
            .expect_err("endpoint failure must propagate");
        assert!(matches!(error, StateError::Io(_)));
        assert!(
            path.is_dir(),
            "failed endpoint write must preserve the destination"
        );
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove failed endpoint directory");
        }
    }

    #[tokio::test]
    async fn failed_atomic_rename_removes_temp_and_does_not_report_a_saved_snapshot() {
        let dir = temp_dir("state_failed_rename");
        let store = StateStore::open(dir.clone()).await.expect("open store");
        // 用目录阻止状态文件原子替换：temp 已创建且写入，rename 必须失败。
        let state_path = dir.join("agent-state.json");
        tokio::fs::create_dir(&state_path)
            .await
            .expect("block state rename");
        let state = AgentState {
            device_id: "must-not-report-saved".to_owned(),
            ..AgentState::default()
        };
        let error = store
            .save(&state)
            .await
            .expect_err("state rename must fail");
        assert!(matches!(error, StateError::Io(_)));
        assert!(
            state_path.is_dir(),
            "failed save must preserve the destination"
        );
        let temps: Vec<_> = std::fs::read_dir(&dir)
            .expect("list state directory")
            .map(|entry| entry.expect("read state directory entry").file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            temps.is_empty(),
            "failed save must remove its temporary snapshot"
        );
        tokio::fs::remove_dir(&state_path)
            .await
            .expect("unblock state rename");
        store.save(&state).await.expect("save after rename failure");
        assert_eq!(
            store.load().await.expect("load saved state").device_id,
            state.device_id
        );
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove failed rename directory");
        }
    }

    #[tokio::test]
    async fn corrupt_state_is_quarantined_and_agent_starts_from_default() {
        let dir = temp_dir("state_corrupt");
        let store = StateStore::open(dir.clone()).await.expect("open store");
        tokio::fs::write(dir.join("agent-state.json"), b"{ not json")
            .await
            .expect("write corrupt state");
        let loaded = store
            .load()
            .await
            .expect("corrupt state must not fail load");
        assert!(loaded.device_id.is_empty());
        assert!(!dir.join("agent-state.json").exists());
        let backups = std::fs::read_dir(&dir)
            .expect("list dir")
            .map(|entry| entry.expect("read backup directory entry"))
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("agent-state.corrupt-")
            })
            .count();
        assert_eq!(backups, 1);
        // 之后可以正常保存并读回。
        let state = AgentState {
            device_id: "fresh".to_owned(),
            ..AgentState::default()
        };
        store.save(&state).await.expect("save after recovery");
        assert_eq!(store.load().await.expect("reload").device_id, "fresh");
        drop(store);
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), error = %error, "remove recovered state directory");
        }
    }

    /// 设备身份旁路文件：状态文件损坏被隔离后仍能读回原身份；不合法内容不被采用；内容相同不重写。
    #[tokio::test]
    async fn device_identity_backup_survives_a_corrupt_state_file() {
        let dir = temp_dir("state_device_id");
        let store = StateStore::open(dir.clone()).await.expect("open store");
        assert_eq!(store.load_device_id().await, None);
        store
            .save_device_id("device-1")
            .await
            .expect("save device id");
        let path = dir.join(DEVICE_ID_FILE);
        let written = std::fs::metadata(&path)
            .expect("device id file")
            .modified()
            .expect("mtime");
        store
            .save_device_id("device-1")
            .await
            .expect("save unchanged device id");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("device id file")
                .modified()
                .expect("mtime"),
            written,
            "内容相同不重写"
        );

        tokio::fs::write(dir.join("agent-state.json"), b"{ not json")
            .await
            .expect("write corrupt state");
        assert!(store.load().await.expect("load").device_id.is_empty());
        assert_eq!(store.load_device_id().await.as_deref(), Some("device-1"));

        for invalid in ["", "has space", "a\u{0}b"] {
            tokio::fs::write(&path, invalid)
                .await
                .expect("write invalid id");
            assert_eq!(store.load_device_id().await, None, "{invalid:?}");
        }
        store
            .save_device_id("bad id")
            .await
            .expect("invalid id is ignored");
        drop(store);
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), error = %error, "remove device id directory");
        }
    }

    #[tokio::test]
    async fn concurrent_persists_never_let_an_older_snapshot_win() {
        let dir = temp_dir("state_persist_order");
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("open store"));
        let state = Arc::new(Mutex::new(AgentState::default()));
        let mut tasks = Vec::new();
        for index in 0..16 {
            let store = store.clone();
            let state = state.clone();
            tasks.push(tokio::spawn(async move {
                state.lock().await.device_name = format!("name-{index}");
                store.persist(&state).await.expect("persist");
            }));
        }
        for task in tasks {
            task.await.expect("join persist task");
        }
        let in_memory = state.lock().await.device_name.clone();
        assert_eq!(store.load().await.expect("load").device_name, in_memory);
        let temps = std::fs::read_dir(&dir)
            .expect("list dir")
            .map(|entry| entry.expect("read state directory entry"))
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(temps, 0, "no temp files may leak");
        drop(store);
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), error = %error, "remove concurrent persist directory");
        }
    }

    #[tokio::test]
    async fn state_is_atomic_private_and_exclusively_locked() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_agent_state_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = StateStore::open(dir.clone()).await.expect("open store");
        assert!(matches!(
            StateStore::open(dir.clone()).await,
            Err(StateError::Locked)
        ));
        let state = AgentState {
            device_id: "device-1".to_owned(),
            device_name: "Desktop".to_owned(),
            platform: "linux".to_owned(),
            ..AgentState::default()
        };
        store.save(&state).await.expect("save");
        let loaded = store.load().await.expect("load");
        assert_eq!(loaded.device_id, "device-1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("agent-state.json"))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        drop(store);
        let reopened = StateStore::open(dir.clone()).await.expect("reopen");
        drop(reopened);
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), error = %error, "remove state directory");
        }
    }
}
