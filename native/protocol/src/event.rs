//! 可重放事件帧与全量快照契约。

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::agent::{
    AgentPreferencesDto, AgentSessionDto, CloudDevice, GatewayStatusDto, PendingCaptureDto,
    PowerStatusDto, RemoteTaskDto, ShellStatusDto, SyncStatusDto,
};
use crate::daemon::{
    ComponentStatusDto, DaemonConfigSnapshot, DaemonRuntimeStatsDto, GroupDto, LinkDeviceInfo,
    PluginDto, QueueDto, QueueOrderEntryDto, QueuePositionDto, RssSourceDto, SelectionRequestDto,
    TaskDto, WebhookDeliveryDto, WsServerMsg,
};

/// daemon 的完整物化投影。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DaemonSnapshot {
    #[serde(default)]
    pub task_runtime: BTreeMap<String, crate::TaskRuntimeDto>,
    /// 任务全表。线上快照（`system.snapshot` / 替换事件）不内嵌：`task_total` 为 `Some` 时
    /// 本字段为空，须经 `system.taskPage` 分页载入（见 [`crate::table_sync`]）。
    #[serde(default)]
    pub tasks: Vec<TaskDto>,
    /// 任务表尚未载入完整：`Some(n)` = 服务端当前共 `n` 个任务待分页拉取；`None` = `tasks`
    /// 已是完整表（内联小表，或分页载入完毕）。客户端在它为 `Some` 期间只能显示加载态，
    /// 不得把 `tasks` 当成完整列表（计数、删除判定都会错）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_total: Option<u64>,
    /// 队列位置表尚未载入完整：语义同 `task_total`，对应 `queue_positions`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_total: Option<u64>,
    pub queues: Vec<QueueDto>,
    /// 队列位置全表（按 `order` 升序，名次 = 下标 + 1）；`queue_total` 为 `Some` 时为空，
    /// 须经 `system.taskPage` 的 `queue` 分区分页载入。
    #[serde(default)]
    pub queue_positions: Vec<QueuePositionDto>,
    pub groups: Vec<GroupDto>,
    pub config: DaemonConfigSnapshot,
    pub rss_sources: Vec<RssSourceDto>,
    pub rss_item_revisions: BTreeMap<String, u64>,
    pub plugins: Vec<PluginDto>,
    pub components: Vec<ComponentStatusDto>,
    pub webhook_deliveries: Vec<WebhookDeliveryDto>,
    pub priority: Vec<String>,
    pub runtime_stats: DaemonRuntimeStatsDto,
    pub pending_selections: Vec<SelectionRequestDto>,
    /// `tasks` 的 task_id → 下标缓存，让逐帧进度 / 分段事件的任务定位保持 O(1)。
    /// 不参与序列化；克隆后为空，首次命中后惰性重建。构造快照时用 `..Default::default()` 填充。
    #[serde(skip)]
    #[cfg_attr(feature = "openapi", schema(ignore))]
    pub task_index: TaskIndex,
}

/// [`DaemonSnapshot::tasks`] 的下标缓存（内容不对外开放）。
///
/// 记录建索引时 `tasks` 的缓冲区地址与长度：二者都没变时索引完整，未命中即「不存在」
/// （O(1)，新任务批量到达不再逐条全表扫描）；调用方直接替换 / 增删 `tasks` 会改变地址或
/// 长度，下次可变访问时整表重建一次。命中仍回读校验 id。
#[derive(Debug, Default)]
pub struct TaskIndex {
    ids: HashMap<String, usize>,
    tasks_ptr: usize,
    tasks_len: usize,
}

/// 克隆得到的是新缓冲区，索引必然要重建；不复制整张表。
impl Clone for TaskIndex {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl DaemonSnapshot {
    fn index_current(&self) -> bool {
        self.task_index.tasks_len == self.tasks.len()
            && self.task_index.tasks_ptr == self.tasks.as_ptr() as usize
    }

    fn record_index_shape(&mut self) {
        self.task_index.tasks_len = self.tasks.len();
        self.task_index.tasks_ptr = self.tasks.as_ptr() as usize;
    }

    fn indexed_hit(&self, task_id: &str) -> Option<usize> {
        self.task_index.ids.get(task_id).copied().filter(|&index| {
            self.tasks
                .get(index)
                .is_some_and(|task| task.task_id == task_id)
        })
    }

    /// 任务在 `tasks` 中的下标。索引完整时 O(1)（未命中即不存在）；否则线性扫描。
    fn task_position(&self, task_id: &str) -> Option<usize> {
        if self.index_current() {
            if let Some(index) = self.indexed_hit(task_id) {
                return Some(index);
            }
            if !self.task_index.ids.contains_key(task_id) {
                return None;
            }
        }
        self.tasks.iter().position(|task| task.task_id == task_id)
    }

    /// 索引不完整时整表重建一次。
    fn ensure_index(&mut self) {
        if !self.index_current() {
            self.reindex_tasks();
        }
    }

    /// [`Self::task_position`] 的可变版本：先保证索引完整；命中回读不一致（调用方原地改过
    /// `task_id`）时重建后再查。
    fn locate_task(&mut self, task_id: &str) -> Option<usize> {
        self.ensure_index();
        if let Some(index) = self.indexed_hit(task_id) {
            return Some(index);
        }
        if !self.task_index.ids.contains_key(task_id) {
            return None;
        }
        self.reindex_tasks();
        self.indexed_hit(task_id)
    }

    fn task_mut(&mut self, task_id: &str) -> Option<&mut TaskDto> {
        let index = self.locate_task(task_id)?;
        self.tasks.get_mut(index)
    }

    /// 按 id 取任务。经 reducer 应用过事件的快照索引完整，O(1)；刚克隆 / 外部改过
    /// `tasks` 且尚未被 reducer 访问时退回线性扫描。
    #[must_use]
    pub fn task(&self, task_id: &str) -> Option<&TaskDto> {
        self.task_position(task_id)
            .and_then(|index| self.tasks.get(index))
    }

    fn reindex_tasks(&mut self) {
        self.task_index.ids.clear();
        self.task_index.ids.reserve(self.tasks.len());
        for (index, task) in self.tasks.iter().enumerate() {
            self.task_index.ids.insert(task.task_id.clone(), index);
        }
        self.record_index_shape();
    }

    fn push_task(&mut self, task: TaskDto) {
        self.ensure_index();
        self.task_index
            .ids
            .insert(task.task_id.clone(), self.tasks.len());
        self.tasks.push(task);
        self.record_index_shape();
    }

    fn remove_task(&mut self, task_id: &str) {
        let before = self.tasks.len();
        self.tasks.retain(|task| task.task_id != task_id);
        if self.tasks.len() != before {
            self.reindex_tasks();
        }
    }

    /// 一次移除一批任务：整表只扫描、重建下标各一次（逐条移除是 O(N·B)）。
    fn remove_tasks(&mut self, task_ids: &[String]) {
        let removed: std::collections::HashSet<&str> =
            task_ids.iter().map(String::as_str).collect();
        let before = self.tasks.len();
        self.tasks
            .retain(|task| !removed.contains(task.task_id.as_str()));
        if self.tasks.len() != before {
            self.reindex_tasks();
        }
        self.task_runtime
            .retain(|task_id, _| !removed.contains(task_id.as_str()));
    }

    /// 任务表换成 `tasks` 后的收尾：重建下标，丢弃已不存在或已完成任务的运行时，并让非活跃
    /// 任务的运行时读数归零（与 `TasksSnapshot` 整表替换同一规则）。
    fn settle_task_table(&mut self) {
        self.reindex_tasks();
        let tasks = &self.tasks;
        let ids = &self.task_index.ids;
        self.task_runtime.retain(|task_id, runtime| {
            let Some(task) = ids
                .get(task_id.as_str())
                .and_then(|&index| tasks.get(index))
            else {
                return false;
            };
            if !runtime_retained(task.status) {
                return false;
            }
            if !matches!(task.status, 1 | 5) {
                zero_runtime_activity(runtime);
            }
            true
        });
    }

    /// 线上形态：去掉任务表与队列位置表，只留 `task_total` / `queue_total`（空表就是完整表，
    /// 不标记）。`system.snapshot` 与替换事件一律发它，表经 `system.taskPage` 分页拉取。
    #[must_use]
    pub fn paged_header(&self) -> Self {
        Self {
            task_runtime: self.task_runtime.clone(),
            tasks: Vec::new(),
            task_total: table_total(self.tasks.len()).or(self.task_total),
            queue_total: table_total(self.queue_positions.len()).or(self.queue_total),
            queues: self.queues.clone(),
            queue_positions: Vec::new(),
            groups: self.groups.clone(),
            config: self.config.clone(),
            rss_sources: self.rss_sources.clone(),
            rss_item_revisions: self.rss_item_revisions.clone(),
            plugins: self.plugins.clone(),
            components: self.components.clone(),
            webhook_deliveries: self.webhook_deliveries.clone(),
            priority: self.priority.clone(),
            runtime_stats: self.runtime_stats.clone(),
            pending_selections: self.pending_selections.clone(),
            task_index: TaskIndex::default(),
        }
    }

    /// 任务表或队列位置表仍待分页载入：此时 `tasks` / `queue_positions` 不是完整表。
    #[must_use]
    pub fn tables_loading(&self) -> bool {
        self.task_total.is_some() || self.queue_total.is_some()
    }

    /// 任务数：载入期间取服务端报告的总数，载入完毕取实际行数。
    #[must_use]
    pub fn task_count(&self) -> usize {
        self.task_total.map_or(self.tasks.len(), |total| {
            usize::try_from(total).unwrap_or(usize::MAX)
        })
    }

    /// 按任务 id 升序取 `cursor` 之后的至多 `limit` 个任务，并返回下一页游标（`None` = 已到
    /// 末页）。游标是上一页最后一个任务 id：任务在翻页期间被删除 / 新增都不会让已翻过的
    /// 位置错位（下标游标会）；新增的任务经事件补齐。
    #[must_use]
    pub fn task_page(&self, cursor: Option<&str>, limit: usize) -> (Vec<TaskDto>, Option<String>) {
        let limit = limit.max(1);
        let mut rest: Vec<&TaskDto> = self
            .tasks
            .iter()
            .filter(|task| cursor.is_none_or(|cursor| task.task_id.as_str() > cursor))
            .collect();
        let more = rest.len() > limit;
        if more {
            rest.select_nth_unstable_by(limit, |a, b| a.task_id.cmp(&b.task_id));
            rest.truncate(limit);
        }
        rest.sort_unstable_by(|a, b| a.task_id.cmp(&b.task_id));
        let next = more
            .then(|| rest.last().map(|task| task.task_id.clone()))
            .flatten();
        (rest.into_iter().cloned().collect(), next)
    }

    /// 按 `order` 升序取 `cursor`（上一页最后一个 `order`）之后的至多 `limit` 条队列位置；
    /// 与最后一条同 `order` 的条目一并带上，保证相同键不会被页边界拆开。
    #[must_use]
    pub fn queue_page(
        &self,
        cursor: Option<i64>,
        limit: usize,
    ) -> (Vec<QueueOrderEntryDto>, Option<String>) {
        let start = cursor.map_or(0, |cursor| {
            self.queue_positions
                .partition_point(|entry| entry.order <= cursor)
        });
        let rest = self.queue_positions.get(start..).unwrap_or_default();
        let mut end = limit.max(1).min(rest.len());
        if let Some(last) = end.checked_sub(1).and_then(|index| rest.get(index)) {
            end += rest
                .get(end..)
                .unwrap_or_default()
                .iter()
                .take_while(|entry| entry.order == last.order)
                .count();
        }
        let page = rest.get(..end).unwrap_or_default();
        let next = (end < rest.len())
            .then(|| page.last().map(|entry| entry.order.to_string()))
            .flatten();
        let entries = page
            .iter()
            .map(|entry| QueueOrderEntryDto {
                task_id: entry.task_id.clone(),
                order: entry.order,
            })
            .collect();
        (entries, next)
    }

    /// 以分页载入的完整任务表替换 `tasks`，并清除 `task_total`（表已完整）。
    pub fn install_tasks(&mut self, tasks: Vec<TaskDto>) {
        self.tasks = tasks;
        self.task_total = None;
        self.settle_task_table();
    }

    /// 以分页载入的队列位置替换 `queue_positions`（按 `order` 升序、名次 = 下标 + 1），并清除
    /// `queue_total`。
    pub fn install_queue(&mut self, mut entries: Vec<QueueOrderEntryDto>) {
        entries.sort_by(|a, b| {
            a.order
                .cmp(&b.order)
                .then_with(|| a.task_id.cmp(&b.task_id))
        });
        self.queue_positions = entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| QueuePositionDto {
                task_id: entry.task_id,
                position: i32::try_from(index + 1).unwrap_or(i32::MAX),
                order: entry.order,
            })
            .collect();
        self.queue_total = None;
    }

    /// 服务端宣告表需重新载入（[`DaemonEvent::TablesReloading`]）：清空对应表并标记待载入。
    fn begin_reload(&mut self, tasks: Option<u64>, queue: Option<u64>) {
        if let Some(total) = tasks {
            self.tasks = Vec::new();
            self.reindex_tasks();
            self.task_total = Some(total);
        }
        if let Some(total) = queue {
            self.queue_positions = Vec::new();
            self.queue_total = Some(total);
        }
    }
}

/// agent 的完整物化投影。下载事实只来自嵌套 daemon 快照。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentSnapshot {
    pub daemon: DaemonSnapshot,
    #[serde(default)]
    pub daemon_connected: bool,
    pub session: Option<AgentSessionDto>,
    pub sync: SyncStatusDto,
    #[serde(default)]
    pub cloud_connection: crate::CloudConnectionDto,
    pub preferences: AgentPreferencesDto,
    pub gateway: GatewayStatusDto,
    pub cloud_devices: Vec<CloudDevice>,
    pub linked_devices: Vec<LinkDeviceInfo>,
    pub remote_tasks: Vec<RemoteTaskDto>,
    pub pending_captures: Vec<PendingCaptureDto>,
    /// 等待本机确认的入站局域网配对请求。
    #[serde(default)]
    pub link_pairing_requests: Vec<crate::LinkPairingRequestDto>,
    /// 局域网发现开启期间看到的未配对 / 已配对 FluxDown 设备。
    #[serde(default)]
    pub link_discovered: Vec<crate::LinkDiscoveredPeer>,
    #[serde(default)]
    pub shell: ShellStatusDto,
    #[serde(default)]
    pub power: PowerStatusDto,
    #[serde(default)]
    pub update: crate::UpdateStatusDto,
    /// 云端推送状态（未登录时只保留本机上报开关与隐私字段，`overview` 为空）。
    #[serde(default)]
    pub cloud_notify: crate::CloudNotifyStateDto,
}

impl AgentSnapshot {
    /// 线上形态：嵌套 daemon 快照去掉任务表与队列位置表（[`DaemonSnapshot::paged_header`]），
    /// 其余字段原样。不先克隆整份再丢弃，避免 O(任务数) 的无谓拷贝。
    #[must_use]
    pub fn paged_header(&self) -> Self {
        Self {
            daemon: self.daemon.paged_header(),
            daemon_connected: self.daemon_connected,
            session: self.session.clone(),
            sync: self.sync.clone(),
            cloud_connection: self.cloud_connection.clone(),
            preferences: self.preferences.clone(),
            gateway: self.gateway.clone(),
            cloud_devices: self.cloud_devices.clone(),
            linked_devices: self.linked_devices.clone(),
            remote_tasks: self.remote_tasks.clone(),
            pending_captures: self.pending_captures.clone(),
            link_pairing_requests: self.link_pairing_requests.clone(),
            link_discovered: self.link_discovered.clone(),
            shell: self.shell.clone(),
            power: self.power,
            update: self.update.clone(),
            cloud_notify: self.cloud_notify.clone(),
        }
    }
}

/// `system.snapshot` 的服务角色对应主体。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "role", content = "snapshot", rename_all = "camelCase")]
pub enum SnapshotBody {
    Daemon(Box<DaemonSnapshot>),
    Agent(Box<AgentSnapshot>),
}

/// 带原子事件位置的全量快照。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub epoch: String,
    pub sequence: u64,
    pub body: SnapshotBody,
}

/// daemon 状态变化事件。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DaemonEvent {
    TaskRuntimeChanged(crate::TaskRuntimeDto),
    TaskActivityAdded(crate::TaskActivityDto),
    SnapshotReplaced(DaemonSnapshot),
    /// 任务表 / 队列位置表整表替换且行数超过 [`TABLE_INLINE_LIMIT`]：线上不内嵌整表，只宣告
    /// 对应表（`Some(总行数)`）已失效，客户端据此进入加载态，经 `system.taskPage` 重新分页
    /// 载入（拉取期间到达的帧缓冲后回放）。服务端内部仍以 `WsServerMsg::TasksSnapshot` /
    /// `QueuePositionsChanged` 整表事件维护投影，只在广播边界转成本事件（见
    /// [`DaemonEvent::into_wire`]）。
    TablesReloading {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tasks: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue: Option<u64>,
    },
    Engine(WsServerMsg),
    TaskChanged(TaskDto),
    /// 一批任务事实（批量建任务）：语义等同按序逐条应用 [`DaemonEvent::TaskChanged`]。
    TasksChanged(Vec<TaskDto>),
    TaskDeleted {
        task_id: String,
    },
    /// 一批任务已删除（`daemon.task.deleteMany` 一批一条）：语义等同按序逐条
    /// [`DaemonEvent::TaskDeleted`]。
    TasksDeleted {
        task_ids: Vec<String>,
    },
    QueuesChanged(Vec<QueueDto>),
    GroupsChanged(Vec<GroupDto>),
    ConfigChanged(DaemonConfigSnapshot),
    RssChanged {
        source_id: String,
        item_revision: u64,
    },
    PluginsChanged(Vec<PluginDto>),
    ComponentsChanged(Vec<ComponentStatusDto>),
    /// 投递日志增量：按 `deliveryId` 合并进 `webhookDeliveries`（上限
    /// [`WEBHOOK_DELIVERY_LIMIT`]，按 `timestampMs` 降序）。空增量不改变列表；
    /// 清空走 [`DaemonEvent::WebhooksCleared`]。
    WebhooksChanged(Vec<WebhookDeliveryDto>),
    /// 投递日志被显式清空。
    WebhooksCleared,
    RuntimeStatsChanged(DaemonRuntimeStatsDto),
    SelectionPending(SelectionRequestDto),
    SelectionResolved {
        request_id: String,
    },
    /// 任务生命周期语义事件（与 webhook 同源）。只供 agent 消费云端推送，agent 不转发给 UI；
    /// 不改变 daemon 快照。
    TaskNotice(crate::TaskNoticeDto),
    /// 常驻插件宿主的可移植视图增量帧。不改变 daemon 快照；agent 原样转发给全部 UI
    /// 连接，客户端按 `session` 过滤、忽略未知会话。
    PluginViewFrame(crate::PluginViewFrameDto),
    /// 可移植视图会话已结束（`reason`：`closed` / `expired` / `pluginUnloaded` /
    /// `requested`——插件经 `ctx.close()` 请求关闭承载它的容器，见
    /// [`crate::VIEW_CLOSE_REASON_REQUESTED`]）。不改变 daemon 快照；agent 原样转发。
    PluginViewClosed {
        session: String,
        reason: String,
    },
    /// 插件经 `flux.ui.toast` 请求的提示（`level`：`info` / `success` / `warning` /
    /// `error`）。不改变 daemon 快照；agent 原样转发。
    PluginToast {
        identity: String,
        level: String,
        text: String,
    },
    /// BT 防吸血的条目 / 状态发生变化（UI 据此重新拉取 `daemon.bt.guard.*`）。不改变 daemon 快照。
    BtGuardChanged,
    /// daemon 请求 agent 刷新云名单（BT 会话启动时 + 会话存活期间每 6 小时）。`have_sequence` 为
    /// daemon 当前持有的文档序号（0 = 无）。只供 agent 消费，不转发给 UI；不改变 daemon 快照。
    BtGuardCloudRefreshWanted {
        have_sequence: u64,
    },
    /// 待上报证据已累积（仅 `bt_guard_report_enabled` 时产生）。`count` 为队列当前条数。
    /// 只供 agent 消费；不改变 daemon 快照。
    BtGuardEvidenceReady {
        count: u32,
    },
}

/// agent 自有或转发的状态变化事件。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AgentEvent {
    Daemon(DaemonEvent),
    DaemonSnapshotReplaced(DaemonSnapshot),
    DaemonConnectionChanged(bool),
    SessionChanged(Box<Option<AgentSessionDto>>),
    SyncChanged(SyncStatusDto),
    CloudConnectionChanged(crate::CloudConnectionDto),
    PreferencesChanged(AgentPreferencesDto),
    GatewayChanged(GatewayStatusDto),
    CloudDevicesChanged(Vec<CloudDevice>),
    LinkedDevicesChanged(Vec<LinkDeviceInfo>),
    LinkPairingRequestsChanged(Vec<crate::LinkPairingRequestDto>),
    LinkDiscoveredChanged(Vec<crate::LinkDiscoveredPeer>),
    RemoteTasksChanged(Vec<RemoteTaskDto>),
    PendingCapturesChanged(Vec<PendingCaptureDto>),
    ShellChanged(ShellStatusDto),
    PowerChanged(PowerStatusDto),
    UpdateChanged(crate::UpdateStatusDto),
    /// 云端推送状态整体替换（`AgentSnapshot.cloud_notify`）。
    CloudNotifyChanged(crate::CloudNotifyStateDto),
    /// 外部来源未经确认直接建成的任务（免打扰下载 / 系统打开链接 / 拖入 / 插件页面脚本经
    /// `flux.tasks.create` 新建）。一次性通知，不进快照；官方 UI 据此为单任务弹出进度窗口。
    /// 失败条目不在列表中。
    CaptureTasksStarted(Vec<String>),
    /// 外部来源恢复的已有任务（插件页面脚本经 `flux.tasks.resume`）。一次性通知，不进快照；
    /// 官方 UI 对单任务按「重新开始」规则（须见到命令后的新状态）弹出进度窗口。
    CaptureTasksResumed(Vec<String>),
    /// 会话被非用户主动地结束（设备被移除 / 被替换 → `deviceUntrusted`；令牌失效或被管理员撤销 →
    /// `sessionExpired`；账号停用 → `accountDisabled`）。一次性通知，不进快照；紧随其后会有
    /// `SessionChanged(None)`。用户主动登出 / 删除本设备不发送。
    SessionRevoked(crate::ErrorReason),
}

/// `service.event` notification 的事件主体。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "service", content = "event", rename_all = "camelCase")]
pub enum ServiceEvent {
    Daemon(DaemonEvent),
    Agent(AgentEvent),
}

/// 整表替换事件在线上内联的行数上限：不超过它仍按旧形态内联整表（一帧 < 2 MiB，小表不
/// 必进加载态）；超过则改发 [`DaemonEvent::TablesReloading`]，客户端分页重载。
pub const TABLE_INLINE_LIMIT: usize = 2_000;

impl DaemonEvent {
    /// 广播 / 线上形态。服务端投影内部保留整表事件（`SnapshotReplaced(全量)`、
    /// `Engine(TasksSnapshot)`、`Engine(QueuePositionsChanged)`），广播前经本方法转换，
    /// 任何一帧都不随任务数线性增长：
    ///
    /// - `SnapshotReplaced` 一律去掉任务表与队列位置表（[`DaemonSnapshot::paged_header`]）；
    /// - 行数超过 [`TABLE_INLINE_LIMIT`] 的 `TasksSnapshot` / `QueuePositionsChanged` 改为
    ///   [`DaemonEvent::TablesReloading`]；
    /// - 其余事件原样返回。
    #[must_use]
    pub fn into_wire(self) -> Self {
        match self {
            Self::SnapshotReplaced(snapshot) => Self::SnapshotReplaced(snapshot.paged_header()),
            Self::Engine(WsServerMsg::TasksSnapshot { tasks })
                if tasks.len() > TABLE_INLINE_LIMIT =>
            {
                Self::TablesReloading {
                    tasks: Some(u64::try_from(tasks.len()).unwrap_or(u64::MAX)),
                    queue: None,
                }
            }
            Self::Engine(WsServerMsg::QueuePositionsChanged { positions })
                if positions.len() > TABLE_INLINE_LIMIT =>
            {
                Self::TablesReloading {
                    tasks: None,
                    queue: Some(u64::try_from(positions.len()).unwrap_or(u64::MAX)),
                }
            }
            other => other,
        }
    }

    /// 本事件是否要求接收方分页重载表（`SnapshotReplaced` 头里带了待载入标记，或
    /// [`DaemonEvent::TablesReloading`]）。
    #[must_use]
    pub fn reloads_tables(&self) -> bool {
        match self {
            Self::SnapshotReplaced(snapshot) => snapshot.tables_loading(),
            Self::TablesReloading { tasks, queue } => tasks.is_some() || queue.is_some(),
            _ => false,
        }
    }
}

impl AgentEvent {
    /// 广播 / 线上形态：见 [`DaemonEvent::into_wire`]；`DaemonSnapshotReplaced` 同样去掉表。
    #[must_use]
    pub fn into_wire(self) -> Self {
        match self {
            Self::Daemon(event) => Self::Daemon(event.into_wire()),
            Self::DaemonSnapshotReplaced(snapshot) => {
                Self::DaemonSnapshotReplaced(snapshot.paged_header())
            }
            other => other,
        }
    }

    /// 见 [`DaemonEvent::reloads_tables`]。
    #[must_use]
    pub fn reloads_tables(&self) -> bool {
        match self {
            Self::Daemon(event) => event.reloads_tables(),
            Self::DaemonSnapshotReplaced(snapshot) => snapshot.tables_loading(),
            _ => false,
        }
    }
}

impl ServiceEvent {
    /// 本事件是否要求接收方分页重载表。
    #[must_use]
    pub fn reloads_tables(&self) -> bool {
        match self {
            Self::Daemon(event) => event.reloads_tables(),
            Self::Agent(event) => event.reloads_tables(),
        }
    }
}

/// 单个严格递增的服务事件帧。
///
/// 帧覆盖序号区间 `(previous, sequence]`：转发者为落后的订阅者合并过可取代的帧时，用
/// `previous` 声明被合并掉的序号（见 [`crate::coalesce::FrameQueue`]）；缺省即
/// `sequence - 1`（逐帧连续）。消费方统一用 [`judge_frame`] 判定。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct EventFrame {
    pub epoch: String,
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<u64>,
    pub event: ServiceEvent,
}

impl EventFrame {
    /// 本帧覆盖区间的左开端点：`previous`，缺省 `sequence - 1`。
    #[must_use]
    pub fn covers_from(&self) -> u64 {
        self.previous
            .unwrap_or_else(|| self.sequence.saturating_sub(1))
    }
}

/// 消费方对一帧的判定（[`judge_frame`]）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameVerdict {
    /// 应用该帧并把游标推进到 `frame.sequence`。
    Accept,
    /// 快照或已应用的帧已经包含它：丢弃。
    Skip,
    /// 游标与帧之间有缺口（或 epoch 已换）：必须重新拉快照。
    Resync,
}

/// 按游标 `(epoch, sequence)` 判定一帧：同 epoch、`sequence` 超过游标，且覆盖区间
/// `(covers_from, sequence]` 与游标相接（`covers_from <= cursor`）时接受。
///
/// 覆盖区间可以越过游标（合并帧里早于快照的部分已在快照中）：合并只发生在「后帧完整取代
/// 前帧」的事件之间，重放一遍结果不变。
#[must_use]
pub fn judge_frame(epoch: &str, cursor: u64, frame: &EventFrame) -> FrameVerdict {
    if frame.epoch != epoch {
        return FrameVerdict::Resync;
    }
    if frame.sequence <= cursor {
        return FrameVerdict::Skip;
    }
    if frame.covers_from() <= cursor {
        FrameVerdict::Accept
    } else {
        FrameVerdict::Resync
    }
}

/// 把队列顺序增量并入按 `order` 升序的全量位置表：先移除 `removed`，再按键插入 / 更新
/// `added`，最后按名次重写 `position`（1 起）。`positions` 入参须已按 `order` 升序
/// （快照与全量消息都满足）；同一任务重复出现时以最后一项为准。
pub fn apply_queue_order(
    positions: &mut Vec<QueuePositionDto>,
    removed: &[String],
    added: &[QueueOrderEntryDto],
) {
    if !removed.is_empty() || !added.is_empty() {
        let mut dropped: std::collections::HashSet<&str> =
            removed.iter().map(String::as_str).collect();
        dropped.extend(added.iter().map(|entry| entry.task_id.as_str()));
        positions.retain(|entry| !dropped.contains(entry.task_id.as_str()));
        let mut incoming: Vec<&QueueOrderEntryDto> = Vec::with_capacity(added.len());
        let mut seen = std::collections::HashSet::with_capacity(added.len());
        for entry in added.iter().rev() {
            if seen.insert(entry.task_id.as_str()) {
                incoming.push(entry);
            }
        }
        incoming.sort_by_key(|entry| entry.order);
        if !incoming.is_empty() {
            let existing = std::mem::take(positions);
            positions.reserve(existing.len() + incoming.len());
            let mut incoming = incoming.into_iter().peekable();
            for entry in existing {
                while let Some(next) = incoming.next_if(|next| next.order < entry.order) {
                    positions.push(QueuePositionDto {
                        task_id: next.task_id.clone(),
                        position: 0,
                        order: next.order,
                    });
                }
                positions.push(entry);
            }
            positions.extend(incoming.map(|next| QueuePositionDto {
                task_id: next.task_id.clone(),
                position: 0,
                order: next.order,
            }));
        }
    }
    for (index, entry) in positions.iter_mut().enumerate() {
        entry.position = i32::try_from(index + 1).unwrap_or(i32::MAX);
    }
}

/// 将同源 agent 增量应用于完整 DTO 投影；daemon 与桌面会话共享此规则。
pub fn apply_agent_event(snapshot: &mut AgentSnapshot, event: &AgentEvent) {
    match event {
        AgentEvent::Daemon(event) => apply_daemon_event(&mut snapshot.daemon, event),
        AgentEvent::DaemonSnapshotReplaced(daemon) => snapshot.daemon.clone_from(daemon),
        AgentEvent::DaemonConnectionChanged(connected) => {
            snapshot.daemon_connected = *connected;
            if !*connected {
                snapshot.daemon.task_runtime.clear();
            }
        }
        AgentEvent::SessionChanged(session) => {
            snapshot.session.clone_from(session.as_ref());
            // 会话结束（登出 / 被撤销）后账号维度的投影随之失效，不留旧账号的设备与任务。
            if snapshot.session.is_none() {
                snapshot.cloud_devices.clear();
                snapshot.remote_tasks.clear();
                snapshot.cloud_connection = crate::CloudConnectionDto::default();
                snapshot.cloud_notify.overview = None;
                snapshot.cloud_notify.loading = false;
                snapshot.cloud_notify.last_error_reason = None;
                snapshot.cloud_notify.updated_at_unix_ms = None;
                snapshot.cloud_notify.recent_deliveries.clear();
                snapshot.cloud_notify.recent_next_cursor = None;
            }
        }
        AgentEvent::SyncChanged(sync) => snapshot.sync.clone_from(sync),
        AgentEvent::CloudConnectionChanged(connection) => {
            snapshot.cloud_connection.clone_from(connection);
            if connection.state != crate::CloudConnectionState::Connected {
                for device in &mut snapshot.cloud_devices {
                    if device.is_current {
                        device.is_online = false;
                    }
                }
            }
        }
        AgentEvent::PreferencesChanged(preferences) => snapshot.preferences.clone_from(preferences),
        AgentEvent::GatewayChanged(gateway) => snapshot.gateway.clone_from(gateway),
        AgentEvent::CloudDevicesChanged(devices) => snapshot.cloud_devices.clone_from(devices),
        AgentEvent::LinkedDevicesChanged(devices) => snapshot.linked_devices.clone_from(devices),
        AgentEvent::LinkPairingRequestsChanged(requests) => {
            snapshot.link_pairing_requests.clone_from(requests);
        }
        AgentEvent::LinkDiscoveredChanged(peers) => snapshot.link_discovered.clone_from(peers),
        AgentEvent::RemoteTasksChanged(tasks) => snapshot.remote_tasks.clone_from(tasks),
        AgentEvent::PendingCapturesChanged(captures) => {
            snapshot.pending_captures.clone_from(captures)
        }
        AgentEvent::ShellChanged(shell) => snapshot.shell.clone_from(shell),
        AgentEvent::PowerChanged(power) => snapshot.power = *power,
        AgentEvent::UpdateChanged(update) => snapshot.update.clone_from(update),
        AgentEvent::CloudNotifyChanged(state) => snapshot.cloud_notify.clone_from(state),
        AgentEvent::CaptureTasksStarted(_)
        | AgentEvent::CaptureTasksResumed(_)
        | AgentEvent::SessionRevoked(_) => {}
    }
}

/// 任务运行时（分段图、传输读数）是否仍有可见用途。已完成任务（3）的进度条退化为满条、
/// 来源构成读持久化的 `TaskDto.source_bytes`，其运行时不再保留：快照头不能随历史跑过的
/// 任务数无界增长。暂停 / 失败任务保留（分段图仍按真实字节区间绘制）。
#[must_use]
pub const fn runtime_retained(status: i32) -> bool {
    status != 3
}

/// 若任务存在、未完成且采样没有落后于已投影的源端序号，返回任务状态。
#[must_use]
pub fn accepted_runtime_status(
    snapshot: &DaemonSnapshot,
    runtime: &crate::TaskRuntimeDto,
) -> Option<i32> {
    if snapshot
        .task_runtime
        .get(&runtime.task_id)
        .is_some_and(|previous| {
            previous.sample_sequence != 0 && runtime.sample_sequence <= previous.sample_sequence
        })
    {
        return None;
    }
    snapshot
        .task_position(&runtime.task_id)
        .and_then(|index| snapshot.tasks.get(index))
        .map(|task| task.status)
        .filter(|status| runtime_retained(*status))
}

pub fn apply_daemon_event(snapshot: &mut DaemonSnapshot, event: &DaemonEvent) {
    snapshot.ensure_index();
    match event {
        DaemonEvent::TaskRuntimeChanged(runtime) => {
            let Some(status) = accepted_runtime_status(snapshot, runtime) else {
                return;
            };
            let inactive = !matches!(status, 1 | 5);
            let mut runtime = runtime.clone();
            if runtime.segments.is_empty()
                && let Some(previous) = snapshot.task_runtime.get(&runtime.task_id)
            {
                runtime.segments.clone_from(&previous.segments);
            }
            if inactive {
                runtime.active_transfers = Some(0);
                for segment in &mut runtime.segments {
                    segment.active = Some(false);
                }
            }
            snapshot
                .task_runtime
                .insert(runtime.task_id.clone(), runtime);
        }
        DaemonEvent::TaskActivityAdded(_)
        | DaemonEvent::TaskNotice(_)
        | DaemonEvent::PluginViewFrame(_)
        | DaemonEvent::PluginViewClosed { .. }
        | DaemonEvent::PluginToast { .. }
        | DaemonEvent::BtGuardChanged
        | DaemonEvent::BtGuardCloudRefreshWanted { .. }
        | DaemonEvent::BtGuardEvidenceReady { .. } => {}
        DaemonEvent::SnapshotReplaced(replacement) => snapshot.clone_from(replacement),
        DaemonEvent::TablesReloading { tasks, queue } => snapshot.begin_reload(*tasks, *queue),
        DaemonEvent::Engine(message) => apply_engine_message(snapshot, message),
        DaemonEvent::TaskChanged(task) => apply_task_changed(snapshot, task),
        DaemonEvent::TasksChanged(tasks) => {
            for task in tasks {
                apply_task_changed(snapshot, task);
            }
        }
        DaemonEvent::TaskDeleted { task_id } => {
            snapshot.remove_task(task_id);
            snapshot.task_runtime.remove(task_id);
        }
        DaemonEvent::TasksDeleted { task_ids } => snapshot.remove_tasks(task_ids),
        DaemonEvent::QueuesChanged(queues) => snapshot.queues.clone_from(queues),
        DaemonEvent::GroupsChanged(groups) => snapshot.groups.clone_from(groups),
        DaemonEvent::ConfigChanged(config) => snapshot.config.clone_from(config),
        DaemonEvent::RssChanged {
            source_id,
            item_revision,
        } => {
            snapshot
                .rss_item_revisions
                .insert(source_id.clone(), *item_revision);
        }
        DaemonEvent::PluginsChanged(plugins) => snapshot.plugins.clone_from(plugins),
        DaemonEvent::ComponentsChanged(components) => snapshot.components.clone_from(components),
        DaemonEvent::WebhooksChanged(deliveries) => {
            merge_webhook_deliveries(&mut snapshot.webhook_deliveries, deliveries);
        }
        DaemonEvent::WebhooksCleared => snapshot.webhook_deliveries.clear(),
        DaemonEvent::RuntimeStatsChanged(stats) => snapshot.runtime_stats.clone_from(stats),
        DaemonEvent::SelectionPending(request) => {
            snapshot
                .pending_selections
                .retain(|item| item.request_id != request.request_id);
            snapshot.pending_selections.push(request.clone());
        }
        DaemonEvent::SelectionResolved { request_id } => {
            snapshot
                .pending_selections
                .retain(|item| item.request_id != *request_id);
        }
    }
}

fn apply_task_changed(snapshot: &mut DaemonSnapshot, task: &TaskDto) {
    if let Some(existing) = snapshot.task_mut(&task.task_id) {
        existing.clone_from(task);
    } else {
        snapshot.push_task(task.clone());
    }
    settle_runtime(snapshot, &task.task_id, task.status);
}

fn apply_engine_message(snapshot: &mut DaemonSnapshot, message: &WsServerMsg) {
    match message {
        WsServerMsg::TasksSnapshot { tasks } => {
            snapshot.tasks.clone_from(tasks);
            snapshot.task_total = None;
            snapshot.settle_task_table();
        }
        WsServerMsg::TaskProgress {
            task_id,
            status,
            downloaded_bytes,
            total_bytes,
            file_name,
            save_dir,
            url,
            error_message,
            uploaded_bytes,
            seeding_status,
            seeding_message,
            seeding_time_secs,
            ..
        } => {
            if *status == 4 && error_message == "deleted" {
                snapshot.task_runtime.remove(task_id);
                snapshot.remove_task(task_id);
                return;
            }
            settle_runtime(snapshot, task_id, *status);
            if let Some(task) = snapshot.task_mut(task_id) {
                task.status = *status;
                task.downloaded_bytes = *downloaded_bytes;
                task.total_bytes = *total_bytes;
                if !file_name.is_empty() {
                    task.file_name.clone_from(file_name);
                }
                if !save_dir.is_empty() {
                    task.save_dir.clone_from(save_dir);
                }
                if !url.is_empty() {
                    task.url.clone_from(url);
                }
                task.error_message.clone_from(error_message);
                task.uploaded_bytes = *uploaded_bytes;
                task.seeding_status = *seeding_status;
                task.seeding_message.clone_from(seeding_message);
                task.seeding_time_secs = *seeding_time_secs;
            }
        }
        WsServerMsg::SegmentProgress {
            task_id,
            total_bytes,
            segments,
            ..
        } => {
            let Some(status) = snapshot.task_mut(task_id).map(|task| task.status) else {
                return;
            };
            if !runtime_retained(status) {
                return;
            }
            let inactive = !matches!(status, 1 | 5);
            let runtime = snapshot
                .task_runtime
                .entry(task_id.clone())
                .or_insert_with(|| crate::TaskRuntimeDto {
                    task_id: task_id.clone(),
                    active_transfers: inactive.then_some(0),
                    ..Default::default()
                });
            // 持久几何无传输采样；真实采样存在时不得覆盖它的分段/活跃读数。
            if runtime.sample_sequence == 0 {
                runtime.total_bytes = *total_bytes;
                runtime.segments = segments
                    .iter()
                    .map(|segment| crate::TaskSegmentDto {
                        index: segment.index,
                        start_byte: segment.start_byte,
                        end_byte: segment.end_byte,
                        downloaded_bytes: segment.downloaded_bytes,
                        active: inactive.then_some(false),
                    })
                    .collect();
            }
        }
        WsServerMsg::TaskMetaProbed {
            task_id,
            file_name,
            total_bytes,
        } => {
            if let Some(task) = snapshot.task_mut(task_id) {
                if !file_name.is_empty() {
                    task.file_name.clone_from(file_name);
                }
                task.total_bytes = *total_bytes;
            }
        }
        WsServerMsg::TaskQueueChanged { task_id, queue_id } => {
            if let Some(task) = snapshot.task_mut(task_id) {
                task.queue_id.clone_from(queue_id);
            }
        }
        WsServerMsg::TaskRouteChanged { task_id, route } => {
            if let Some(task) = snapshot.task_mut(task_id) {
                task.auto_route.clone_from(route);
            }
        }
        WsServerMsg::QueuesChanged { queues } => snapshot.queues.clone_from(queues),
        WsServerMsg::QueuePositionsChanged { positions } => {
            snapshot.queue_positions.clone_from(positions);
            snapshot.queue_total = None;
        }
        WsServerMsg::QueueOrderChanged { removed, added } => {
            apply_queue_order(&mut snapshot.queue_positions, removed, added);
        }
        WsServerMsg::GroupsChanged { groups } => snapshot.groups.clone_from(groups),
        WsServerMsg::RssSourcesChanged { sources } => snapshot.rss_sources.clone_from(sources),
        WsServerMsg::RssItemsChanged { source_id, .. } => {
            let revision = snapshot
                .rss_item_revisions
                .entry(source_id.clone())
                .or_default();
            *revision = revision.saturating_add(1);
        }
        WsServerMsg::WebhookDeliveriesChanged { deliveries } => {
            merge_webhook_deliveries(&mut snapshot.webhook_deliveries, deliveries);
        }
        WsServerMsg::FileMissingChanged { updates } => {
            for update in updates {
                if let Some(task) = snapshot.task_mut(&update.task_id) {
                    task.file_missing = update.missing;
                }
            }
        }
        WsServerMsg::PriorityTaskChanged {
            priority_task_id, ..
        } => {
            snapshot.priority.clear();
            if !priority_task_id.is_empty() {
                snapshot.priority.push(priority_task_id.clone());
            }
        }
        WsServerMsg::PluginAutoDisabled { identity, reason } => {
            if let Some(plugin) = snapshot
                .plugins
                .iter_mut()
                .find(|plugin| plugin.identity == *identity)
            {
                plugin.enabled = false;
                plugin.disabled_reason.clone_from(reason);
            }
        }
        _ => {}
    }
}

/// 任务状态变化后的运行时收尾：完成即删除条目，其余非活跃状态只清零传输读数。
fn settle_runtime(snapshot: &mut DaemonSnapshot, task_id: &str, status: i32) {
    if !runtime_retained(status) {
        snapshot.task_runtime.remove(task_id);
    } else if !matches!(status, 1 | 5)
        && let Some(runtime) = snapshot.task_runtime.get_mut(task_id)
    {
        zero_runtime_activity(runtime);
    }
}

fn zero_runtime_activity(runtime: &mut crate::TaskRuntimeDto) {
    runtime.active_transfers = Some(0);
    runtime.connected_peers = Some(0);
    for segment in &mut runtime.segments {
        segment.active = Some(false);
    }
}

/// 空表就是完整表，不需要分页载入；非空表返回行数。
fn table_total(len: usize) -> Option<u64> {
    (len > 0).then(|| u64::try_from(len).unwrap_or(u64::MAX))
}

/// 投递日志保留上限（与引擎内存环一致）。
pub const WEBHOOK_DELIVERY_LIMIT: usize = 1000;

/// 把一批投递记录按 `deliveryId` 合并进列表：同 id 以增量为准，结果按 `timestampMs`
/// 降序并截到 [`WEBHOOK_DELIVERY_LIMIT`]。增量为空时列表不变——清空由
/// [`DaemonEvent::WebhooksCleared`] 显式表达。
pub fn merge_webhook_deliveries(
    current: &mut Vec<WebhookDeliveryDto>,
    delta: &[WebhookDeliveryDto],
) {
    if delta.is_empty() {
        return;
    }
    let incoming: std::collections::HashSet<&str> = delta
        .iter()
        .map(|delivery| delivery.delivery_id.as_str())
        .collect();
    current.retain(|delivery| !incoming.contains(delivery.delivery_id.as_str()));
    current.extend(delta.iter().cloned());
    current.sort_by_key(|delivery| std::cmp::Reverse(delivery.timestamp_ms));
    current.truncate(WEBHOOK_DELIVERY_LIMIT);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DaemonEvent, EventFrame, ServiceEvent, Snapshot, SnapshotBody};

    fn positions(entries: &[(&str, i64)]) -> Vec<crate::QueuePositionDto> {
        entries
            .iter()
            .enumerate()
            .map(|(index, (id, order))| crate::QueuePositionDto {
                task_id: (*id).to_owned(),
                position: i32::try_from(index + 1).expect("position"),
                order: *order,
            })
            .collect()
    }

    #[test]
    fn queue_order_delta_removes_then_upserts_by_key_and_renumbers() {
        let mut list = positions(&[("a", 10), ("b", 20), ("c", 30)]);
        let entry = |id: &str, order| crate::QueueOrderEntryDto {
            task_id: id.to_owned(),
            order,
        };
        // 队首出队 + 尾部追加：其余任务的键不变，名次整体前移。
        super::apply_queue_order(&mut list, &["a".to_owned()], &[entry("d", 40)]);
        assert_eq!(list, positions(&[("b", 20), ("c", 30), ("d", 40)]));
        // 同一批里先删后加 = 重新入队；插到队首与中间按键定位。
        super::apply_queue_order(
            &mut list,
            &["c".to_owned()],
            &[entry("c", 5), entry("e", 25), entry("e", 35)],
        );
        assert_eq!(
            list,
            positions(&[("c", 5), ("b", 20), ("e", 35), ("d", 40)])
        );
    }

    #[test]
    fn task_index_finds_new_and_cloned_tasks_without_stale_hits() {
        let task = |id: &str| -> crate::TaskDto {
            serde_json::from_value(json!({
                "taskId": id, "url": "u", "fileName": "", "saveDir": "/tmp", "status": 0,
                "downloadedBytes": 0, "totalBytes": 0, "errorMessage": "", "createdAt": "1",
                "proxyUrl": "", "queueId": "main", "checksum": ""
            }))
            .expect("task")
        };
        let mut snapshot = super::DaemonSnapshot {
            tasks: vec![task("a")],
            ..Default::default()
        };
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::TasksChanged(vec![task("b"), task("a"), task("c")]),
        );
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .map(|t| t.task_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        // 克隆后索引重建；调用方整体替换 `tasks` 后也不会命中旧下标。
        let mut cloned = snapshot.clone();
        cloned.tasks = vec![task("c"), task("x")];
        super::apply_daemon_event(&mut cloned, &DaemonEvent::TaskChanged(task("x")));
        super::apply_daemon_event(&mut cloned, &DaemonEvent::TaskChanged(task("a")));
        assert_eq!(
            cloned
                .tasks
                .iter()
                .map(|t| t.task_id.as_str())
                .collect::<Vec<_>>(),
            ["c", "x", "a"]
        );
        super::apply_daemon_event(
            &mut cloned,
            &DaemonEvent::TaskDeleted {
                task_id: "x".to_owned(),
            },
        );
        super::apply_daemon_event(&mut cloned, &DaemonEvent::TaskChanged(task("c")));
        assert_eq!(
            cloned
                .tasks
                .iter()
                .map(|t| t.task_id.as_str())
                .collect::<Vec<_>>(),
            ["c", "a"]
        );
    }

    #[test]
    fn cloud_connection_defaults_for_old_snapshot_and_resets_on_logout() {
        let mut wire = serde_json::to_value(super::AgentSnapshot::default()).expect("snapshot");
        wire.as_object_mut()
            .expect("object")
            .remove("cloudConnection");
        let mut snapshot: super::AgentSnapshot =
            serde_json::from_value(wire).expect("old snapshot");
        assert_eq!(
            snapshot.cloud_connection,
            crate::CloudConnectionDto::default()
        );
        let connection = crate::CloudConnectionDto {
            state: crate::CloudConnectionState::Connected,
            ..Default::default()
        };
        let event = super::AgentEvent::CloudConnectionChanged(connection.clone());
        assert_eq!(
            serde_json::to_value(&event).expect("event")["type"],
            "cloudConnectionChanged"
        );
        super::apply_agent_event(&mut snapshot, &event);
        assert_eq!(snapshot.cloud_connection, connection);
        super::apply_agent_event(
            &mut snapshot,
            &super::AgentEvent::SessionChanged(Box::new(None)),
        );
        assert_eq!(
            snapshot.cloud_connection,
            crate::CloudConnectionDto::default()
        );
    }

    #[test]
    fn snapshot_carries_atomic_epoch_and_sequence() -> Result<(), serde_json::Error> {
        let snapshot = Snapshot {
            epoch: "epoch-a".to_owned(),
            sequence: 41,
            body: SnapshotBody::Daemon(Box::default()),
        };
        let wire = serde_json::to_value(&snapshot)?;
        assert_eq!(wire["epoch"], "epoch-a");
        assert_eq!(wire["sequence"], 41);
        assert_eq!(wire["body"]["role"], "daemon");
        assert!(wire["body"]["snapshot"]["tasks"].is_array());
        Ok(())
    }

    #[test]
    fn event_frame_uses_strict_monotonic_cursor_shape() -> Result<(), serde_json::Error> {
        let frame = EventFrame {
            epoch: "epoch-a".to_owned(),
            sequence: 42,
            previous: None,
            event: ServiceEvent::Daemon(DaemonEvent::TaskDeleted {
                task_id: "task-1".to_owned(),
            }),
        };
        let wire = serde_json::to_value(frame)?;
        assert_eq!(
            wire,
            json!({
                "epoch": "epoch-a",
                "sequence": 42,
                "event": {
                    "service": "daemon",
                    "event": {
                        "type": "taskDeleted",
                        "data": { "taskId": "task-1" }
                    }
                }
            })
        );
        Ok(())
    }

    #[test]
    fn restart_geometry_never_invents_active_reads_or_overrides_live_sample()
    -> Result<(), serde_json::Error> {
        let task: super::TaskDto = serde_json::from_value(json!({
            "taskId":"t", "url":"https://example.com/t", "fileName":"t", "saveDir":"/tmp",
            "status":2, "downloadedBytes":25, "totalBytes":100, "errorMessage":"",
            "createdAt":"1", "proxyUrl":"", "queueId":"main", "checksum":""
        }))?;
        let mut snapshot = super::DaemonSnapshot {
            tasks: vec![task],
            ..Default::default()
        };
        let geometry = DaemonEvent::Engine(super::WsServerMsg::SegmentProgress {
            task_id: "t".into(),
            total_bytes: 100,
            segment_count: 1,
            segments: vec![crate::daemon::SegmentDetailDto {
                index: 0,
                start_byte: 0,
                end_byte: 99,
                downloaded_bytes: 25,
            }],
        });
        super::apply_daemon_event(&mut snapshot, &geometry);
        assert_eq!(snapshot.task_runtime["t"].segments[0].downloaded_bytes, 25);
        assert_eq!(snapshot.task_runtime["t"].active_transfers, Some(0));
        assert_eq!(snapshot.task_runtime["t"].segments[0].active, Some(false));
        let mut active = snapshot.tasks[0].clone();
        active.status = 1;
        super::apply_daemon_event(&mut snapshot, &DaemonEvent::TaskChanged(active.clone()));
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::TaskRuntimeChanged(crate::TaskRuntimeDto {
                task_id: "t".into(),
                sampled_at_ms: 1234,
                sample_sequence: 10,
                active_transfers: Some(2),
                total_bytes: 100,
                segments: vec![crate::TaskSegmentDto {
                    index: 0,
                    start_byte: 0,
                    end_byte: 99,
                    downloaded_bytes: 40,
                    active: Some(true),
                }],
                ..Default::default()
            }),
        );
        super::apply_daemon_event(&mut snapshot, &geometry);
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::TaskRuntimeChanged(crate::TaskRuntimeDto {
                task_id: "t".into(),
                sampled_at_ms: 5000,
                sample_sequence: 9,
                active_transfers: Some(1),
                segments: vec![crate::TaskSegmentDto {
                    downloaded_bytes: 20,
                    ..Default::default()
                }],
                ..Default::default()
            }),
        );
        assert_eq!(snapshot.task_runtime["t"].segments[0].downloaded_bytes, 40);
        assert_eq!(snapshot.task_runtime["t"].active_transfers, Some(2));
        active.status = 4;
        super::apply_daemon_event(&mut snapshot, &DaemonEvent::TaskChanged(active));
        assert_eq!(snapshot.task_runtime["t"].active_transfers, Some(0));
        assert_eq!(snapshot.task_runtime["t"].segments[0].active, Some(false));
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::TaskRuntimeChanged(crate::TaskRuntimeDto {
                task_id: "t".into(),
                sampled_at_ms: 1234,
                sample_sequence: 10,
                active_transfers: Some(1),
                segments: vec![crate::TaskSegmentDto {
                    downloaded_bytes: 20,
                    ..Default::default()
                }],
                ..Default::default()
            }),
        );
        assert_eq!(snapshot.task_runtime["t"].segments[0].downloaded_bytes, 40);
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::TaskRuntimeChanged(crate::TaskRuntimeDto {
                task_id: "t".into(),
                sampled_at_ms: 100,
                sample_sequence: 11,
                active_transfers: Some(0),
                segments: vec![crate::TaskSegmentDto {
                    downloaded_bytes: 100,
                    ..Default::default()
                }],
                ..Default::default()
            }),
        );
        assert_eq!(snapshot.task_runtime["t"].segments[0].downloaded_bytes, 100);
        assert_eq!(snapshot.task_runtime["t"].active_transfers, Some(0));
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::TaskRuntimeChanged(crate::TaskRuntimeDto {
                task_id: "t".into(),
                sampled_at_ms: 9000,
                sample_sequence: 0,
                active_transfers: Some(1),
                ..Default::default()
            }),
        );
        assert_eq!(snapshot.task_runtime["t"].segments[0].downloaded_bytes, 100);
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::TaskDeleted {
                task_id: "t".into(),
            },
        );
        assert!(snapshot.task_runtime.is_empty());
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::TaskRuntimeChanged(crate::TaskRuntimeDto {
                task_id: "t".into(),
                active_transfers: Some(3),
                ..Default::default()
            }),
        );
        assert!(
            snapshot.task_runtime.is_empty(),
            "late sample must not resurrect deleted task"
        );
        Ok(())
    }

    fn runtime_sample(id: &str, sequence: u64) -> crate::TaskRuntimeDto {
        crate::TaskRuntimeDto {
            task_id: id.into(),
            sample_sequence: sequence,
            active_transfers: Some(2),
            total_bytes: 100,
            segments: vec![crate::TaskSegmentDto {
                index: 0,
                start_byte: 0,
                end_byte: 99,
                downloaded_bytes: 10,
                active: Some(true),
            }],
            ..Default::default()
        }
    }

    fn progress_frame(id: &str, status: i32) -> DaemonEvent {
        DaemonEvent::Engine(super::WsServerMsg::TaskProgress {
            task_id: id.into(),
            status,
            downloaded_bytes: 100,
            total_bytes: 100,
            speed: 0,
            upload_speed: 0,
            file_name: String::new(),
            save_dir: String::new(),
            url: String::new(),
            error_message: String::new(),
            uploaded_bytes: 0,
            seeding_status: 0,
            seeding_message: String::new(),
            seeding_time_secs: 0,
        })
    }

    fn geometry_frame(id: &str) -> DaemonEvent {
        DaemonEvent::Engine(super::WsServerMsg::SegmentProgress {
            task_id: id.into(),
            total_bytes: 100,
            segment_count: 1,
            segments: vec![crate::daemon::SegmentDetailDto {
                index: 0,
                start_byte: 0,
                end_byte: 99,
                downloaded_bytes: 100,
            }],
        })
    }

    #[test]
    fn completed_tasks_leave_no_runtime_and_late_frames_cannot_readd_it()
    -> Result<(), serde_json::Error> {
        let mut snapshot = super::DaemonSnapshot::default();
        for index in 0..600_u64 {
            let id = format!("t{index}");
            super::apply_daemon_event(
                &mut snapshot,
                &DaemonEvent::TaskChanged(sample_task(&id, 1)?),
            );
            super::apply_daemon_event(
                &mut snapshot,
                &DaemonEvent::TaskRuntimeChanged(runtime_sample(&id, index + 1)),
            );
            assert_eq!(snapshot.task_runtime.len(), 1, "{id}");
            // 三条完成路径：TaskChanged / TasksChanged / 引擎 TaskProgress。
            let done = match index % 3 {
                0 => DaemonEvent::TaskChanged(sample_task(&id, 3)?),
                1 => DaemonEvent::TasksChanged(vec![sample_task(&id, 3)?]),
                _ => progress_frame(&id, 3),
            };
            super::apply_daemon_event(&mut snapshot, &done);
            assert!(snapshot.task_runtime.is_empty(), "{id}");
            // 迟到的采样（序号更高也一样）与持久几何都不得把条目加回来。
            let late = DaemonEvent::TaskRuntimeChanged(runtime_sample(&id, index + 10_000));
            super::apply_daemon_event(&mut snapshot, &late);
            super::apply_daemon_event(&mut snapshot, &geometry_frame(&id));
            assert!(snapshot.task_runtime.is_empty(), "{id}");
            assert!(
                super::accepted_runtime_status(&snapshot, &runtime_sample(&id, 99_999)).is_none()
            );
        }
        assert_eq!(snapshot.tasks.len(), 600);
        // 头快照里不再有任何 per-task 运行时：体积不随跑过的任务数增长。
        assert!(snapshot.paged_header().task_runtime.is_empty());
        Ok(())
    }

    #[test]
    fn task_table_install_prunes_completed_runtime_but_keeps_paused_and_failed()
    -> Result<(), serde_json::Error> {
        let mut snapshot = super::DaemonSnapshot::default();
        for id in ["run", "paused", "failed", "done", "gone"] {
            snapshot
                .task_runtime
                .insert(id.into(), runtime_sample(id, 1));
        }
        snapshot.install_tasks(vec![
            sample_task("run", 1)?,
            sample_task("paused", 2)?,
            sample_task("failed", 4)?,
            sample_task("done", 3)?,
        ]);
        let mut kept: Vec<&str> = snapshot.task_runtime.keys().map(String::as_str).collect();
        kept.sort_unstable();
        assert_eq!(kept, ["failed", "paused", "run"]);
        assert_eq!(snapshot.task_runtime["run"].active_transfers, Some(2));
        assert_eq!(snapshot.task_runtime["paused"].active_transfers, Some(0));
        assert_eq!(
            snapshot.task_runtime["failed"].segments[0].active,
            Some(false)
        );

        // 引擎整表替换走同一规则。
        snapshot
            .task_runtime
            .insert("paused".into(), runtime_sample("paused", 2));
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::Engine(super::WsServerMsg::TasksSnapshot {
                tasks: vec![sample_task("paused", 3)?, sample_task("run", 1)?],
            }),
        );
        let mut kept: Vec<&str> = snapshot.task_runtime.keys().map(String::as_str).collect();
        kept.sort_unstable();
        assert_eq!(kept, ["run"]);
        Ok(())
    }

    fn sample_task(id: &str, status: i32) -> Result<super::TaskDto, serde_json::Error> {
        serde_json::from_value(json!({
            "taskId": id, "url": "https://example.com/t", "fileName": id, "saveDir": "/tmp",
            "status": status, "downloadedBytes": 0, "totalBytes": 100, "errorMessage": "",
            "createdAt": "1", "proxyUrl": "", "queueId": "main", "checksum": ""
        }))
    }

    fn sample_delivery(
        id: &str,
        timestamp_ms: i64,
    ) -> Result<super::WebhookDeliveryDto, serde_json::Error> {
        serde_json::from_value(json!({
            "deliveryId": id, "timestampMs": timestamp_ms, "event": "task.completed",
            "endpointId": "e", "endpointName": "e", "url": "https://example.com/hook",
            "requestHeaders": "", "requestBody": "", "statusCode": 200, "responseBody": "",
            "latencyMs": 1, "attempts": 1, "success": true, "error": ""
        }))
    }

    #[test]
    fn webhook_delta_merges_by_delivery_id_and_keeps_newest_first() -> Result<(), serde_json::Error>
    {
        let mut snapshot = super::DaemonSnapshot::default();
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::WebhooksChanged(vec![
                sample_delivery("b", 20)?,
                sample_delivery("a", 10)?,
            ]),
        );
        let mut retried = sample_delivery("a", 30)?;
        retried.success = false;
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::WebhooksChanged(vec![retried, sample_delivery("c", 15)?]),
        );
        let ids: Vec<&str> = snapshot
            .webhook_deliveries
            .iter()
            .map(|delivery| delivery.delivery_id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert!(
            !snapshot.webhook_deliveries[0].success,
            "same id takes the delta version"
        );
        Ok(())
    }

    #[test]
    fn empty_webhook_delta_keeps_history_and_only_cleared_event_empties_it()
    -> Result<(), serde_json::Error> {
        let mut snapshot = super::DaemonSnapshot::default();
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::WebhooksChanged(vec![sample_delivery("a", 1)?]),
        );
        super::apply_daemon_event(&mut snapshot, &DaemonEvent::WebhooksChanged(Vec::new()));
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::Engine(super::WsServerMsg::WebhookDeliveriesChanged {
                deliveries: Vec::new(),
            }),
        );
        assert_eq!(snapshot.webhook_deliveries.len(), 1);
        super::apply_daemon_event(&mut snapshot, &DaemonEvent::WebhooksCleared);
        assert!(snapshot.webhook_deliveries.is_empty());
        Ok(())
    }

    #[test]
    fn webhook_history_is_capped_at_the_limit_dropping_oldest() -> Result<(), serde_json::Error> {
        let mut snapshot = super::DaemonSnapshot::default();
        let total = i64::try_from(super::WEBHOOK_DELIVERY_LIMIT).unwrap_or(1000) + 5;
        for chunk_start in (0..total).step_by(100) {
            let delta = (chunk_start..(chunk_start + 100).min(total))
                .rev()
                .map(|n| sample_delivery(&format!("d{n}"), n))
                .collect::<Result<Vec<_>, _>>()?;
            super::apply_daemon_event(&mut snapshot, &DaemonEvent::WebhooksChanged(delta));
        }
        assert_eq!(
            snapshot.webhook_deliveries.len(),
            super::WEBHOOK_DELIVERY_LIMIT
        );
        assert_eq!(snapshot.webhook_deliveries[0].timestamp_ms, total - 1);
        assert_eq!(
            snapshot.webhook_deliveries.last().map(|d| d.timestamp_ms),
            Some(5)
        );
        Ok(())
    }

    #[test]
    fn task_lookup_survives_direct_task_list_edits() -> Result<(), serde_json::Error> {
        let mut snapshot = super::DaemonSnapshot::default();
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::Engine(super::WsServerMsg::TasksSnapshot {
                tasks: vec![
                    sample_task("a", 2)?,
                    sample_task("b", 2)?,
                    sample_task("c", 2)?,
                ],
            }),
        );
        // 调用方绕过事件直接改了列表顺序：缓存必须自愈而不是指错任务。
        snapshot.tasks.remove(0);
        snapshot.tasks.push(sample_task("d", 1)?);
        for id in ["b", "c", "d"] {
            super::apply_daemon_event(
                &mut snapshot,
                &DaemonEvent::Engine(super::WsServerMsg::TaskMetaProbed {
                    task_id: id.into(),
                    file_name: format!("{id}.bin"),
                    total_bytes: 7,
                }),
            );
        }
        let names: Vec<&str> = snapshot
            .tasks
            .iter()
            .map(|t| t.file_name.as_str())
            .collect();
        assert_eq!(names, ["b.bin", "c.bin", "d.bin"]);
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::TaskDeleted {
                task_id: "b".into(),
            },
        );
        super::apply_daemon_event(
            &mut snapshot,
            &DaemonEvent::Engine(super::WsServerMsg::TaskMetaProbed {
                task_id: "d".into(),
                file_name: "d2.bin".into(),
                total_bytes: 9,
            }),
        );
        assert_eq!(snapshot.tasks.len(), 2);
        assert_eq!(snapshot.tasks[1].file_name, "d2.bin");
        assert_eq!(snapshot.tasks[1].total_bytes, 9);
        Ok(())
    }

    /// 批量删除 = 按序逐条删除：剩余任务保序、下标仍可用，运行时随任务清除。
    #[test]
    fn tasks_deleted_matches_sequential_task_deleted() -> Result<(), serde_json::Error> {
        let mut batched = super::DaemonSnapshot::default();
        super::apply_daemon_event(
            &mut batched,
            &DaemonEvent::Engine(super::WsServerMsg::TasksSnapshot {
                tasks: vec![
                    sample_task("a", 1)?,
                    sample_task("b", 2)?,
                    sample_task("c", 2)?,
                    sample_task("d", 1)?,
                ],
            }),
        );
        for id in ["a", "d"] {
            super::apply_daemon_event(
                &mut batched,
                &DaemonEvent::TaskRuntimeChanged(crate::TaskRuntimeDto {
                    task_id: id.into(),
                    ..crate::TaskRuntimeDto::default()
                }),
            );
        }
        assert_eq!(batched.task_runtime.len(), 2);
        let mut sequential = batched.clone();
        super::apply_daemon_event(
            &mut batched,
            &DaemonEvent::TasksDeleted {
                task_ids: vec!["d".into(), "missing".into(), "a".into()],
            },
        );
        for id in ["d", "missing", "a"] {
            super::apply_daemon_event(
                &mut sequential,
                &DaemonEvent::TaskDeleted { task_id: id.into() },
            );
        }
        let ids = |snapshot: &super::DaemonSnapshot| -> Vec<String> {
            snapshot.tasks.iter().map(|t| t.task_id.clone()).collect()
        };
        assert_eq!(ids(&batched), ["b", "c"]);
        assert_eq!(ids(&batched), ids(&sequential));
        assert!(batched.task_runtime.is_empty());
        super::apply_daemon_event(
            &mut batched,
            &DaemonEvent::Engine(super::WsServerMsg::TaskMetaProbed {
                task_id: "c".into(),
                file_name: "c2.bin".into(),
                total_bytes: 3,
            }),
        );
        assert_eq!(batched.tasks[1].file_name, "c2.bin");
        Ok(())
    }
}
