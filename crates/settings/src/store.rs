//! 设置视图模型：agent 快照投影、乐观本地覆盖、防抖合并写回与动作调用。
//!
//! 页面闭包只经 `Entity<SettingsStore>` 读写；视图 observe 本实体重绘。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{
    AgentEvent, AgentPreferencesDto, AgentSnapshot, ApplicationErrorCode,
    BtGuardFilterSourcesRefreshDto, BtGuardStateDto, ComponentStatusDto, ConnPolicySummaryDto,
    DaemonConfigPatch, DaemonConfigSnapshot, DaemonEvent, DiagnosticsReportDto, ErrorReason,
    GatewayPatchParams, GatewayStatusDto, PlatformIntegrationDto, PluginDto, QueueDto,
    RpcErrorData, ServiceEvent, SettingOwner, ShellStatusDto, SiteAuthEntryDto, SyncStatusDto,
    SystemProxyDto, UpdateStatusDto, WebhookDeliveryDto, method, setting_spec, setting_value_kind,
    value_to_daemon_config,
};
use fluxdown_ui_components::BusyHold;
use gpui::{Context, SharedString};
use serde_json::{Value, json};

use crate::port::{PortFuture, SettingsPort};

mod mutation;

mod cloud_notify;

pub(crate) use cloud_notify::CloudDeliveryLog;

/// 本地编辑到写回 RPC 的合并窗口。
const FLUSH_DEBOUNCE: Duration = Duration::from_millis(250);
/// `load_attempts` 里防吸血总览的加载闸门（渲染期按需加载，`BtGuardChanged` 后重新放行）。
pub(crate) const BT_GUARD_STATE_LOAD: &str = "btGuardState";
/// daemon 修订冲突时的自动重试上限。
const MAX_CONFLICT_RETRIES: u8 = 3;
/// Doctor 上一次修复结果（`{key, ok}`，`key` 为 i18n 键）在 `transient` 里的键。
pub(crate) const DOCTOR_REPAIR_OUTCOME: &str = "doctor_repair_outcome";

/// 设置写回失败的 UI 可展示分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsErrorKind {
    Disconnected,
    Conflict,
    InvalidArgument,
    GatewayPortInUse,
    GatewayRestartFailed,
    Failed,
}

impl SettingsErrorKind {
    #[must_use]
    pub fn from_rpc(error: &RpcErrorData) -> Self {
        match error.reason {
            Some(ErrorReason::GatewayPortInUse) => Self::GatewayPortInUse,
            Some(ErrorReason::GatewayRestartFailed) => Self::GatewayRestartFailed,
            _ => match error.code {
                ApplicationErrorCode::Unavailable => Self::Disconnected,
                ApplicationErrorCode::Conflict => Self::Conflict,
                ApplicationErrorCode::InvalidArgument => Self::InvalidArgument,
                _ => Self::Failed,
            },
        }
    }

    /// 对应 `assets/i18n` 的错误说明键。
    #[must_use]
    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::Disconnected => "localServiceDisconnected",
            Self::Conflict => "localServiceConflict",
            Self::InvalidArgument => "localServiceInvalidArgument",
            Self::GatewayPortInUse => "apiServicePortInUse",
            Self::GatewayRestartFailed => "apiServiceRestartFailed",
            Self::Failed => "localServiceActionFailed",
        }
    }
}

/// RPC 错误的本地化说明（不透传服务端 message，优先按原因、再按错误码归类）。
#[must_use]
pub(crate) fn rpc_error_text(
    translator: &fluxdown_ui_i18n::Translator,
    error: &RpcErrorData,
) -> String {
    translator
        .text(SettingsErrorKind::from_rpc(error).i18n_key())
        .to_owned()
}

/// 一次设置写回的可展示错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsError {
    pub kind: SettingsErrorKind,
    /// 校验失败时的具体说明（引擎/协议给出的英文原文）；无则为空。
    pub detail: SharedString,
}

pub struct SettingsStore {
    port: Arc<dyn SettingsPort>,
    daemon: DaemonConfigSnapshot,
    gateway: GatewayStatusDto,
    shell: ShellStatusDto,
    preferences: AgentPreferencesDto,
    sync: SyncStatusDto,
    queues: Vec<QueueDto>,
    plugins: Vec<PluginDto>,
    components: Vec<ComponentStatusDto>,
    webhook_deliveries: Vec<WebhookDeliveryDto>,
    session: Option<fluxdown_protocol::AgentSessionDto>,
    /// 云账号下的全部设备（含本机）：侧栏设备区「未设置时自动显示」的依据。
    cloud_devices: Vec<fluxdown_protocol::CloudDevice>,
    /// 云端推送的本机状态投影（`AgentSnapshot.cloud_notify`）。
    cloud_notify: fluxdown_protocol::CloudNotifyStateDto,
    /// 云端投递记录的「加载更多」状态（第一页来自 `cloud_notify.recent_deliveries`）。
    cloud_log: CloudDeliveryLog,
    /// 最近一次云端推送上报开关 / 隐私设置失败的原因。
    cloud_action_error: Option<RpcErrorData>,
    /// 已配对局域网设备数：侧栏设备区「未设置时自动显示」的依据。
    linked_devices: usize,
    daemon_connected: bool,
    stale: bool,

    /// 已编辑、尚未发送的 daemon 原始键（不经云同步目录）。
    pending_daemon: BTreeMap<String, String>,
    /// 已发送、等待回执的 daemon 原始键。
    inflight_daemon: BTreeMap<String, String>,
    /// 已编辑、尚未发送的偏好；`bool` = 是否进入云同步。
    pending_prefs: BTreeMap<String, (Value, bool)>,
    inflight_prefs: BTreeMap<String, (Value, bool)>,
    /// 编辑前最后一次服务端确认值，写回失败时用于回滚（`None` = 当时不存在）。
    baseline_daemon: BTreeMap<String, Option<String>>,
    baseline_prefs: BTreeMap<String, Option<Value>>,
    flush_scheduled: bool,
    flush_inflight: bool,
    conflict_retries: u8,
    /// 整值键的读-改-写链（见 [`mutation`]）。
    mutations: mutation::DaemonMutations,

    // ── 按需加载的动作结果 ──
    integration: Option<PlatformIntegrationDto>,
    diagnostics: Option<DiagnosticsReportDto>,
    site_auth: Vec<SiteAuthEntryDto>,
    conn_policy: Option<ConnPolicySummaryDto>,
    system_proxy: Option<SystemProxyDto>,
    update: UpdateStatusDto,
    /// BT 防吸血总览（`daemon.bt.guard.state`）；按需加载，`BtGuardChanged` 后重新放行加载。
    bt_guard: Option<BtGuardStateDto>,
    /// 每收到一次 `BtGuardChanged` +1：条目 / 事件面板据此重新拉取。
    bt_guard_revision: u64,
    /// 真正在途的动作；内部去重 / 自动加载闸门只看它（[`Self::in_flight`]）。
    busy: BTreeSet<&'static str>,
    /// 每个动作的最短可见「进行中」：本机 RPC 常在一帧内返回，没有它按钮转圈根本画不出来。
    busy_holds: BTreeMap<&'static str, BusyHold>,
    /// 同一 `action` 下具体是哪个按钮发起的（如某条修复 / 某个站点）；下次同 `action` 发起时清除。
    busy_tags: BTreeMap<&'static str, SharedString>,
    last_error: Option<SettingsError>,
    /// 最近一次动作的成功提示（i18n 键）。
    last_notice: Option<&'static str>,
    /// 页面级临时值（测试结果等），不持久化、不发送。
    transient: BTreeMap<&'static str, Value>,
    /// 渲染期按需加载的「本轮已尝试」标记，见 [`Self::begin_load`]。
    load_attempts: BTreeSet<&'static str>,
    /// 网关状态变化 / 快照重建后置位：`transient("gateway_user_token")` 可能已过期，
    /// 下次展示时重新读取；发起读取时清除。
    gateway_token_stale: bool,
}

impl SettingsStore {
    #[must_use]
    pub fn new(port: Arc<dyn SettingsPort>) -> Self {
        Self {
            port,
            daemon: DaemonConfigSnapshot::default(),
            gateway: GatewayStatusDto::default(),
            shell: ShellStatusDto::default(),
            preferences: AgentPreferencesDto::default(),
            sync: SyncStatusDto::default(),
            queues: Vec::new(),
            plugins: Vec::new(),
            components: Vec::new(),
            webhook_deliveries: Vec::new(),
            session: None,
            cloud_devices: Vec::new(),
            cloud_notify: fluxdown_protocol::CloudNotifyStateDto::default(),
            cloud_log: CloudDeliveryLog::default(),
            cloud_action_error: None,
            linked_devices: 0,
            daemon_connected: false,
            stale: true,
            pending_daemon: BTreeMap::new(),
            inflight_daemon: BTreeMap::new(),
            pending_prefs: BTreeMap::new(),
            inflight_prefs: BTreeMap::new(),
            baseline_daemon: BTreeMap::new(),
            baseline_prefs: BTreeMap::new(),
            flush_scheduled: false,
            flush_inflight: false,
            conflict_retries: 0,
            mutations: mutation::DaemonMutations::default(),
            integration: None,
            diagnostics: None,
            site_auth: Vec::new(),
            conn_policy: None,
            system_proxy: None,
            update: UpdateStatusDto::default(),
            bt_guard: None,
            bt_guard_revision: 0,
            busy: BTreeSet::new(),
            busy_holds: BTreeMap::new(),
            busy_tags: BTreeMap::new(),
            last_error: None,
            last_notice: None,
            transient: BTreeMap::new(),
            load_attempts: BTreeSet::new(),
            gateway_token_stale: true,
        }
    }

    // ───────────────────────── 快照与事件 ─────────────────────────

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) {
        self.daemon.clone_from(&snapshot.daemon.config);
        self.gateway.clone_from(&snapshot.gateway);
        self.reopen_load_gates();
        self.shell.clone_from(&snapshot.shell);
        self.preferences.clone_from(&snapshot.preferences);
        self.sync.clone_from(&snapshot.sync);
        self.update.clone_from(&snapshot.update);
        self.queues.clone_from(&snapshot.daemon.queues);
        self.plugins.clone_from(&snapshot.daemon.plugins);
        self.components.clone_from(&snapshot.daemon.components);
        self.webhook_deliveries
            .clone_from(&snapshot.daemon.webhook_deliveries);
        self.session.clone_from(&snapshot.session);
        self.cloud_devices.clone_from(&snapshot.cloud_devices);
        self.cloud_notify.clone_from(&snapshot.cloud_notify);
        self.linked_devices = snapshot.linked_devices.len();
        self.daemon_connected = snapshot.daemon_connected;
        self.overlay_local_edits();
        self.last_error = None;
        cx.notify();
    }

    /// 连上 / 重连拿到全量快照：退出断线只读，并重新放行渲染期按需加载（系统集成状态、
    /// 网关 token 等）。首次连接失败、断线期间失败过的读取都靠这里在恢复后重发。
    fn reopen_load_gates(&mut self) {
        self.stale = false;
        self.load_attempts.clear();
        self.gateway_token_stale = true;
    }

    /// 投递日志：增量按 deliveryId 合并；清空只由显式事件表达。
    fn apply_webhook_event(&mut self, event: &DaemonEvent) {
        match event {
            DaemonEvent::WebhooksChanged(delta) => {
                fluxdown_protocol::merge_webhook_deliveries(&mut self.webhook_deliveries, delta);
            }
            DaemonEvent::WebhooksCleared => self.webhook_deliveries.clear(),
            _ => {}
        }
    }

    pub fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>) {
        let ServiceEvent::Agent(event) = event else {
            return;
        };
        match event {
            AgentEvent::Daemon(DaemonEvent::ConfigChanged(config)) => {
                self.daemon.clone_from(config);
                self.refresh_daemon_baselines();
                self.overlay_local_edits();
            }
            AgentEvent::Daemon(DaemonEvent::QueuesChanged(queues)) => {
                self.queues.clone_from(queues)
            }
            AgentEvent::Daemon(DaemonEvent::PluginsChanged(plugins)) => {
                self.plugins.clone_from(plugins)
            }
            AgentEvent::Daemon(DaemonEvent::BtGuardChanged) => {
                self.bt_guard_revision = self.bt_guard_revision.wrapping_add(1);
                self.load_attempts.remove(BT_GUARD_STATE_LOAD);
            }
            AgentEvent::Daemon(DaemonEvent::ComponentsChanged(components)) => {
                self.components.clone_from(components)
            }
            AgentEvent::Daemon(
                event @ (DaemonEvent::WebhooksChanged(_) | DaemonEvent::WebhooksCleared),
            ) => self.apply_webhook_event(event),
            AgentEvent::GatewayChanged(gateway) => {
                self.gateway.clone_from(gateway);
                self.gateway_token_stale = true;
            }
            AgentEvent::ShellChanged(shell) => self.shell.clone_from(shell),
            AgentEvent::PreferencesChanged(preferences) => {
                self.preferences.clone_from(preferences);
                self.refresh_pref_baselines();
                self.overlay_local_edits();
            }
            AgentEvent::SyncChanged(sync) => self.sync.clone_from(sync),
            AgentEvent::UpdateChanged(update) => self.update.clone_from(update),
            AgentEvent::SessionChanged(session) => {
                self.session.clone_from(session.as_ref());
                if session.is_none() {
                    // 账号维度的设备与云端推送概览 / 投递记录随会话结束失效。
                    self.clear_cloud_account();
                }
            }
            AgentEvent::CloudDevicesChanged(devices) => self.cloud_devices.clone_from(devices),
            AgentEvent::CloudNotifyChanged(state) => self.cloud_notify.clone_from(state),
            AgentEvent::LinkedDevicesChanged(devices) => self.linked_devices = devices.len(),
            AgentEvent::DaemonSnapshotReplaced(snapshot) => {
                self.daemon.clone_from(&snapshot.config);
                self.refresh_daemon_baselines();
                self.queues.clone_from(&snapshot.queues);
                self.plugins.clone_from(&snapshot.plugins);
                self.components.clone_from(&snapshot.components);
                self.webhook_deliveries
                    .clone_from(&snapshot.webhook_deliveries);
                self.overlay_local_edits();
                self.daemon_connected = true;
                self.load_attempts.clear();
            }
            AgentEvent::DaemonConnectionChanged(connected) => {
                self.daemon_connected = *connected;
                if *connected {
                    self.load_attempts.clear();
                }
            }
            _ => return,
        }
        cx.notify();
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        self.pending_daemon.clear();
        self.inflight_daemon.clear();
        self.pending_prefs.clear();
        self.inflight_prefs.clear();
        self.mutations.discard();
        self.last_error = Some(SettingsError {
            kind: SettingsErrorKind::Disconnected,
            detail: SharedString::default(),
        });
        cx.notify();
    }

    /// 记录编辑前的服务端值（仅首次；回执 / 回滚 / 新快照前保持不变）。
    /// `pref_key` 为空串表示不涉及偏好。
    fn capture_baselines(&mut self, pref_key: &str, daemon_key: Option<&str>) {
        if !pref_key.is_empty() && !self.baseline_prefs.contains_key(pref_key) {
            let old = self.preferences.values.get(pref_key).cloned();
            self.baseline_prefs.insert(pref_key.to_owned(), old);
        }
        if let Some(key) = daemon_key
            && !self.baseline_daemon.contains_key(key)
        {
            let old = self.daemon.values.get(key).cloned();
            self.baseline_daemon.insert(key.to_owned(), old);
        }
    }

    /// 新的服务端 daemon 配置到达：基线跟随服务端值。
    fn refresh_daemon_baselines(&mut self) {
        for (key, old) in &mut self.baseline_daemon {
            *old = self.daemon.values.get(key).cloned();
        }
    }

    /// 新的服务端偏好到达：基线跟随服务端值。
    fn refresh_pref_baselines(&mut self) {
        for (key, old) in &mut self.baseline_prefs {
            *old = self.preferences.values.get(key).cloned();
        }
    }

    /// 写回成功：已回执且无更新编辑的键不再需要基线。
    fn settle_inflight(&mut self) {
        let daemon = std::mem::take(&mut self.inflight_daemon);
        for key in daemon
            .keys()
            .filter(|k| !self.pending_daemon.contains_key(*k))
        {
            self.baseline_daemon.remove(key);
        }
        let prefs = std::mem::take(&mut self.inflight_prefs);
        for key in prefs
            .keys()
            .filter(|k| !self.pending_prefs.contains_key(*k))
        {
            self.baseline_prefs.remove(key);
            if let Some(spec) = setting_spec(key).filter(|s| s.owner == SettingOwner::Daemon) {
                self.baseline_daemon.remove(spec.storage_key);
            }
        }
    }

    /// 写回失败：把在途键恢复为编辑前的服务端值（已有更新的待发送编辑则保留）。
    fn rollback_inflight(&mut self) {
        let daemon = std::mem::take(&mut self.inflight_daemon);
        for key in daemon.into_keys() {
            if !self.pending_daemon.contains_key(&key) {
                Self::restore(
                    &mut self.daemon.values,
                    &key,
                    self.baseline_daemon.remove(&key),
                );
            }
        }
        let prefs = std::mem::take(&mut self.inflight_prefs);
        for key in prefs.into_keys() {
            if self.pending_prefs.contains_key(&key) {
                continue;
            }
            let old = self.baseline_prefs.remove(&key);
            match old {
                Some(Some(value)) => {
                    self.preferences.values.insert(key.clone(), value);
                }
                Some(None) => {
                    self.preferences.values.remove(&key);
                }
                None => {}
            }
            if let Some(spec) = setting_spec(&key).filter(|s| s.owner == SettingOwner::Daemon) {
                let old = self.baseline_daemon.remove(spec.storage_key);
                Self::restore(&mut self.daemon.values, spec.storage_key, old);
            }
        }
        self.conflict_retries = 0;
    }

    fn restore(values: &mut BTreeMap<String, String>, key: &str, old: Option<Option<String>>) {
        match old {
            Some(Some(value)) => {
                values.insert(key.to_owned(), value);
            }
            Some(None) => {
                values.remove(key);
            }
            None => {}
        }
    }

    /// 服务端快照到达时把尚未回执的本地编辑重新盖上去，避免输入框回跳。
    fn overlay_local_edits(&mut self) {
        self.capture_mutation_bases();
        for (key, value) in self
            .inflight_daemon
            .iter()
            .chain(self.pending_daemon.iter())
        {
            self.daemon.values.insert(key.clone(), value.clone());
        }
        for (key, (value, _)) in self.inflight_prefs.iter().chain(self.pending_prefs.iter()) {
            self.preferences.values.insert(key.clone(), value.clone());
            // daemon 拥有的键读侧是 daemon 快照，同样要盖回去。
            if let Some(spec) = setting_spec(key).filter(|s| s.owner == SettingOwner::Daemon)
                && let Ok(wire) = value_to_daemon_config(spec, value)
            {
                self.daemon.values.insert(spec.storage_key.to_owned(), wire);
            }
        }
        self.overlay_mutations();
    }

    // ───────────────────────── 只读投影 ─────────────────────────

    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.stale
    }
    #[must_use]
    pub fn daemon_connected(&self) -> bool {
        self.daemon_connected && !self.stale
    }
    #[must_use]
    pub fn gateway(&self) -> &GatewayStatusDto {
        &self.gateway
    }
    /// agent 托盘可用性与关闭 UI 后的驻留策略。
    #[must_use]
    pub fn shell(&self) -> &ShellStatusDto {
        &self.shell
    }
    #[must_use]
    pub fn sync_status(&self) -> &SyncStatusDto {
        &self.sync
    }

    /// 是否存在「其他设备」（云账号的其他设备或已配对的局域网设备）。
    #[must_use]
    pub fn has_other_devices(&self) -> bool {
        self.cloud_devices.iter().any(|device| !device.is_current) || self.linked_devices > 0
    }
    #[must_use]
    pub fn session(&self) -> Option<&fluxdown_protocol::AgentSessionDto> {
        self.session.as_ref()
    }
    #[must_use]
    pub fn queues(&self) -> &[QueueDto] {
        &self.queues
    }
    #[must_use]
    pub fn plugins(&self) -> &[PluginDto] {
        &self.plugins
    }
    #[must_use]
    pub fn components(&self) -> &[ComponentStatusDto] {
        &self.components
    }
    #[must_use]
    pub fn webhook_deliveries(&self) -> &[WebhookDeliveryDto] {
        &self.webhook_deliveries
    }
    #[must_use]
    pub fn integration(&self) -> Option<&PlatformIntegrationDto> {
        self.integration.as_ref()
    }
    #[must_use]
    pub fn diagnostics(&self) -> Option<&DiagnosticsReportDto> {
        self.diagnostics.as_ref()
    }
    #[must_use]
    pub fn site_auth(&self) -> &[SiteAuthEntryDto] {
        &self.site_auth
    }
    #[must_use]
    pub fn conn_policy(&self) -> Option<&ConnPolicySummaryDto> {
        self.conn_policy.as_ref()
    }
    #[must_use]
    pub fn system_proxy(&self) -> Option<&SystemProxyDto> {
        self.system_proxy.as_ref()
    }
    #[must_use]
    pub fn update_status(&self) -> &UpdateStatusDto {
        &self.update
    }
    #[must_use]
    pub fn transient(&self, key: &str) -> Option<&Value> {
        self.transient.get(key)
    }
    /// 用户 token 尚未读取，或网关状态变化后可能已过期。
    #[must_use]
    pub fn gateway_token_needs_reveal(&self) -> bool {
        self.gateway_token_stale || !self.transient.contains_key("gateway_user_token")
    }
    /// 渲染期是否应发起 token 读取：断线只读时不发；失败后本轮不再重发（避免
    /// 「失败 → notify → 重绘 → 再发」的循环），网关变化或重连 / 新快照后重新放行。
    #[must_use]
    pub fn should_reveal_gateway_token(&self) -> bool {
        !self.stale
            && !self.in_flight("gateway")
            && !self.in_flight("gatewayToken")
            && (self.gateway_token_stale
                || (!self.transient.contains_key("gateway_user_token")
                    && !self.load_attempts.contains("gatewayToken")))
    }
    /// 按需加载的「本轮已尝试」闸门：渲染期调用，返回 true 表示本轮首次，调用方随后发起加载。
    /// 失败后不会在下一次重绘里重发，直到重连 / 新快照 / 重新打开设置窗口重置。
    pub fn begin_load(&mut self, key: &'static str) -> bool {
        self.load_attempts.insert(key)
    }
    /// 本轮是否已尝试过 `key` 的按需加载（只读，渲染期先判定再决定是否 `update`）。
    pub fn load_attempted(&self, key: &'static str) -> bool {
        self.load_attempts.contains(key)
    }
    /// 清除指定加载标记，使下次渲染重新加载（如设置窗口重新打开）。
    pub fn reset_load(&mut self, key: &'static str) {
        self.load_attempts.remove(key);
    }
    pub fn set_transient(&mut self, key: &'static str, value: Value, cx: &mut Context<Self>) {
        self.transient.insert(key, value);
        cx.notify();
    }
    /// 渲染用的「进行中」：在途，或仍在最短可见期内（见 [`BusyHold`]）。
    #[must_use]
    pub fn is_busy(&self, action: &str) -> bool {
        self.in_flight(action) || self.busy_holds.get(action).is_some_and(BusyHold::is_held)
    }
    /// 请求是否真正在途：去重、自动加载等逻辑闸门用它，不受最短可见期影响。
    #[must_use]
    pub fn in_flight(&self, action: &str) -> bool {
        self.busy.contains(action)
    }
    /// `action` 进行中且由 `tag` 标记的按钮发起。
    #[must_use]
    pub fn is_busy_tagged(&self, action: &str, tag: &str) -> bool {
        self.is_busy(action) && self.busy_tags.get(action).is_some_and(|t| t == tag)
    }
    /// `action` 进行中且未打标签（区分同一 action 下其他按钮发起的调用）。
    #[must_use]
    pub fn is_busy_untagged(&self, action: &str) -> bool {
        self.is_busy(action) && !self.busy_tags.contains_key(action)
    }
    /// 给刚发起的 `action` 打上发起者标签，供对应按钮显示 loading；`action` 未在进行中则忽略。
    pub fn tag_busy(&mut self, action: &'static str, tag: impl Into<SharedString>) {
        if self.busy.contains(action) {
            self.busy_tags.insert(action, tag.into());
        }
    }
    /// 标记 `action` 开始：在途 + 最短可见期计时，并清掉上一次的发起者标签。
    fn begin_busy(&mut self, action: &'static str, cx: &mut Context<Self>) {
        self.busy.insert(action);
        self.busy_tags.remove(action);
        self.busy_holds.entry(action).or_default().start(cx);
    }
    #[must_use]
    pub fn last_error(&self) -> Option<&SettingsError> {
        self.last_error.as_ref()
    }
    #[must_use]
    pub fn last_notice(&self) -> Option<&'static str> {
        self.last_notice
    }
    pub fn clear_feedback(&mut self, cx: &mut Context<Self>) {
        if self.last_error.take().is_some() | self.last_notice.take().is_some() {
            cx.notify();
        }
    }

    // ───────────────────────── daemon 配置 ─────────────────────────

    /// daemon 原始字符串值；缺省取协议目录默认值。
    #[must_use]
    pub fn daemon_str(&self, key: &str) -> String {
        self.daemon
            .values
            .get(key)
            .cloned()
            .unwrap_or_else(|| fluxdown_protocol::daemon_config_default(key).to_owned())
    }
    #[must_use]
    pub fn daemon_bool(&self, key: &str) -> bool {
        matches!(self.daemon_str(key).as_str(), "true" | "1")
    }
    #[must_use]
    pub fn daemon_i64(&self, key: &str) -> i64 {
        self.daemon_str(key).trim().parse().unwrap_or_else(|_| {
            fluxdown_protocol::daemon_config_default(key)
                .parse()
                .unwrap_or(0)
        })
    }
    #[must_use]
    pub fn daemon_f64(&self, key: &str) -> f64 {
        self.daemon_str(key).trim().parse().unwrap_or_else(|_| {
            fluxdown_protocol::daemon_config_default(key)
                .parse()
                .unwrap_or(0.0)
        })
    }

    /// 写入一个 daemon 配置键（wire 字符串）。
    ///
    /// 键若在云同步目录内则经 `agent.preferences.patch` 走同步链路；
    /// 否则直接进入防抖合并的 `daemon.config.patch`。
    pub fn set_daemon(&mut self, key: &str, value: impl Into<String>, cx: &mut Context<Self>) {
        if self.stale {
            self.set_error(SettingsErrorKind::Disconnected, "", cx);
            return;
        }
        let value = value.into();
        let normalized = match fluxdown_protocol::normalize_daemon_config_value(key, &value) {
            Ok(normalized) => normalized,
            Err(error) => {
                self.set_error(SettingsErrorKind::InvalidArgument, error.to_string(), cx);
                return;
            }
        };
        if self.daemon.values.get(key) == Some(&normalized) {
            return;
        }
        let spec = fluxdown_protocol::SYNC_SETTING_SPECS
            .iter()
            .find(|spec| spec.owner == SettingOwner::Daemon && spec.storage_key == key);
        if let Some(spec) = spec {
            self.capture_baselines(spec.key, Some(key));
        } else {
            self.capture_baselines("", Some(key));
        }
        self.daemon
            .values
            .insert(key.to_owned(), normalized.clone());
        if let Some(spec) = spec {
            let json = daemon_string_to_json(spec.key, &normalized);
            self.pending_prefs.insert(spec.key.to_owned(), (json, true));
        } else {
            self.pending_daemon.insert(key.to_owned(), normalized);
        }
        self.schedule_flush(cx);
        cx.notify();
    }
    pub fn set_daemon_bool(&mut self, key: &str, value: bool, cx: &mut Context<Self>) {
        self.set_daemon(key, value.to_string(), cx);
    }
    pub fn set_daemon_i64(&mut self, key: &str, value: i64, cx: &mut Context<Self>) {
        self.set_daemon(key, value.to_string(), cx);
    }
    pub fn set_daemon_f64(&mut self, key: &str, value: f64, cx: &mut Context<Self>) {
        self.set_daemon(key, value.to_string(), cx);
    }

    // ───────────────────────── agent 偏好 ─────────────────────────

    /// 全部偏好：agent 快照 / 事件，再盖上本进程尚未回执的本地编辑。
    ///
    /// 这是 UI 进程内偏好的唯一读视图：由偏好派生的全局状态（主题、语言、活动栏）只从这里
    /// 投影，偏好写入也只经 [`Self::set_pref`]，二者因此不会互相回弹。
    #[must_use]
    pub fn preferences(&self) -> &BTreeMap<String, Value> {
        &self.preferences.values
    }
    #[must_use]
    pub fn pref(&self, key: &str) -> Option<&Value> {
        self.preferences.values.get(key)
    }
    #[must_use]
    pub fn pref_bool(&self, key: &str, default: bool) -> bool {
        self.pref(key).and_then(Value::as_bool).unwrap_or(default)
    }
    #[must_use]
    pub fn pref_str(&self, key: &str, default: &str) -> String {
        self.pref(key)
            .and_then(Value::as_str)
            .map_or_else(|| default.to_owned(), str::to_owned)
    }
    #[must_use]
    pub fn pref_i64(&self, key: &str, default: i64) -> i64 {
        self.pref(key).and_then(Value::as_i64).unwrap_or(default)
    }
    #[must_use]
    pub fn pref_f64(&self, key: &str, default: f64) -> f64 {
        self.pref(key).and_then(Value::as_f64).unwrap_or(default)
    }

    /// 写入偏好。云同步目录内的键自动进入同步；其余键为设备本地。
    /// `Value::Null` 是墓碑：agent 移除该偏好（同步键把删除同步给云端），不做值校验。
    pub fn set_pref(&mut self, key: &str, value: Value, cx: &mut Context<Self>) {
        if self.stale {
            self.set_error(SettingsErrorKind::Disconnected, "", cx);
            return;
        }
        let synced = preference_is_synced(key);
        if let Some(spec) = setting_spec(key) {
            if !value.is_null()
                && let Err(error) = fluxdown_protocol::validate_value(spec.key, &value)
            {
                self.set_error(SettingsErrorKind::InvalidArgument, error, cx);
                return;
            }
            if spec.owner == SettingOwner::Daemon {
                // daemon 键的读侧是 daemon 快照；写侧仍经同步链路。
                if let Ok(wire) = value_to_daemon_config(spec, &value) {
                    if self.preferences.values.get(key) != Some(&value) {
                        self.capture_baselines(key, Some(spec.storage_key));
                    }
                    self.daemon.values.insert(spec.storage_key.to_owned(), wire);
                }
            }
        }
        if self.preferences.values.get(key) == Some(&value) {
            return;
        }
        self.capture_baselines(key, None);
        self.preferences
            .values
            .insert(key.to_owned(), value.clone());
        self.pending_prefs.insert(key.to_owned(), (value, synced));
        self.schedule_flush(cx);
        cx.notify();
    }
    pub fn set_pref_bool(&mut self, key: &str, value: bool, cx: &mut Context<Self>) {
        self.set_pref(key, Value::Bool(value), cx);
    }
    pub fn set_pref_str(&mut self, key: &str, value: impl Into<String>, cx: &mut Context<Self>) {
        self.set_pref(key, Value::String(value.into()), cx);
    }
    pub fn set_pref_i64(&mut self, key: &str, value: i64, cx: &mut Context<Self>) {
        self.set_pref(key, Value::from(value), cx);
    }

    // ───────────────────────── 网关 ─────────────────────────

    pub fn patch_gateway(&mut self, patch: GatewayPatchParams, cx: &mut Context<Self>) {
        if self.in_flight("gateway") {
            return;
        }
        if self.stale {
            self.set_error(SettingsErrorKind::Disconnected, "", cx);
            return;
        }
        let port_changed = patch.port.is_some_and(|port| port != self.gateway.port);
        let params = match serde_json::to_value(patch) {
            Ok(params) => params,
            Err(_) => {
                self.set_error(SettingsErrorKind::Failed, "", cx);
                return;
            }
        };
        self.call_with(
            "gateway",
            method::AGENT_GATEWAY_PATCH,
            params,
            cx,
            move |this, result, cx| {
                if let Ok(value) = result {
                    match serde_json::from_value::<GatewayStatusDto>(value) {
                        Ok(gateway) => {
                            this.gateway = gateway;
                            this.reveal_gateway_token(cx);
                            if port_changed {
                                this.last_notice = Some("apiServiceRestarted");
                            }
                        }
                        Err(_) => this.set_error(SettingsErrorKind::Failed, "", cx),
                    }
                }
            },
        );
    }

    /// 用户 token 只在本机 UI 按需读取，不进快照；结果放入 `transient("gateway_user_token")`。
    pub fn reveal_gateway_token(&mut self, cx: &mut Context<Self>) {
        self.gateway_token_stale = false;
        self.load_attempts.insert("gatewayToken");
        self.call_with(
            "gatewayToken",
            method::AGENT_GATEWAY_REVEAL_TOKEN,
            json!({}),
            cx,
            |this, result, cx| {
                if let Ok(value) = result
                    && let Some(token) = value.get("userToken").and_then(Value::as_str)
                {
                    this.set_transient("gateway_user_token", Value::String(token.to_owned()), cx);
                }
            },
        );
    }

    // ───────────────────────── 通用动作 ─────────────────────────

    /// 发起一次 RPC；`action` 用于 `is_busy`，完成后回调更新状态。
    pub fn call_with(
        &mut self,
        action: &'static str,
        method: &'static str,
        params: Value,
        cx: &mut Context<Self>,
        on_done: impl FnOnce(&mut Self, Result<Value, RpcErrorData>, &mut Context<Self>) + 'static,
    ) {
        if self.stale {
            self.set_error(SettingsErrorKind::Disconnected, "", cx);
            return;
        }
        self.begin_busy(action, cx);
        self.last_error = None;
        self.last_notice = None;
        cx.notify();
        let directory = match method {
            method::AGENT_PLATFORM_OPEN_PATH
                if params.get("reveal") == Some(&Value::Bool(false)) =>
            {
                params.get("path").and_then(Value::as_str)
            }
            method::AGENT_DIAGNOSTICS_REPAIR
                if params.get("action").and_then(Value::as_str) == Some("open_log_dir") =>
            {
                params.get("target").and_then(Value::as_str)
            }
            _ => None,
        };
        let future = directory
            .filter(|path| !path.trim().is_empty())
            .and_then(|path| self.port.foreground_open_directory(path))
            .unwrap_or_else(|| self.port.call(method, params));
        cx.spawn(async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update(cx, |this, cx| {
                // 标签保留到最短可见期结束（下次发起时清除），转圈仍落在发起的那个按钮上。
                this.busy.remove(action);
                if let Err(error) = &result {
                    this.last_error = Some(SettingsError {
                        kind: SettingsErrorKind::from_rpc(error),
                        detail: SharedString::default(),
                    });
                }
                on_done(this, result, cx);
                cx.notify();
            }) else {
                // 设置视图或窗口已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    /// 无结果关心的动作；成功后展示 `notice` 提示（i18n 键）。
    pub fn call_simple(
        &mut self,
        action: &'static str,
        method: &'static str,
        params: Value,
        notice: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        self.call_with(action, method, params, cx, move |this, result, _| {
            if result.is_ok() {
                this.last_notice = notice;
            }
        });
    }

    pub fn raw_call(&self, method: &'static str, params: Value) -> PortFuture<Value> {
        self.port.call(method, params)
    }

    pub fn load_integration(&mut self, cx: &mut Context<Self>) {
        self.call_with(
            "integration",
            method::AGENT_PLATFORM_INTEGRATION_GET,
            json!({}),
            cx,
            Self::absorb_integration,
        );
    }
    pub fn set_autostart(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.call_with(
            "integration",
            method::AGENT_PLATFORM_SET_AUTOSTART,
            json!({ "enabled": enabled }),
            cx,
            Self::absorb_integration,
        );
    }
    pub fn set_file_association(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.call_with(
            "integration",
            method::AGENT_PLATFORM_SET_FILE_ASSOCIATION,
            json!({ "enabled": enabled }),
            cx,
            Self::absorb_integration,
        );
    }
    pub fn set_url_protocol(&mut self, scheme: &str, enabled: bool, cx: &mut Context<Self>) {
        self.call_with(
            "integration",
            method::AGENT_PLATFORM_SET_URL_PROTOCOL,
            json!({ "scheme": scheme, "enabled": enabled }),
            cx,
            Self::absorb_integration,
        );
    }
    fn absorb_integration(&mut self, result: Result<Value, RpcErrorData>, _cx: &mut Context<Self>) {
        if let Ok(value) = result
            && let Ok(dto) = serde_json::from_value::<PlatformIntegrationDto>(value)
        {
            self.integration = Some(dto);
        }
    }

    pub fn run_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.call_with(
            "diagnostics",
            method::AGENT_DIAGNOSTICS_RUN,
            json!({}),
            cx,
            |this, result, _| {
                if let Ok(value) = result
                    && let Ok(report) = serde_json::from_value::<DiagnosticsReportDto>(value)
                {
                    this.diagnostics = Some(report);
                }
            },
        );
    }
    /// 执行 Doctor 修复。结果（成功 / 已取消授权 / 修复后仍未通过…）记在
    /// [`DOCTOR_REPAIR_OUTCOME`] 由 Doctor 页就地展示，不走全局「操作失败」横幅；随后总是重新
    /// 诊断——修复可能只完成了一部分，列表要反映真实状态。
    pub fn repair_diagnostics(
        &mut self,
        params: fluxdown_protocol::DiagnosticRepairParams,
        cx: &mut Context<Self>,
    ) {
        let action = params.action.clone();
        let params = serde_json::to_value(params).unwrap_or_else(|_| json!({}));
        self.transient.remove(DOCTOR_REPAIR_OUTCOME);
        self.call_with(
            "diagnostics",
            method::AGENT_DIAGNOSTICS_REPAIR,
            params,
            cx,
            move |this, result, cx| {
                // 「打开目录」不是修复：成功时不提示、不重跑诊断；失败沿用全局错误横幅。
                if action == "open_log_dir" {
                    return;
                }
                let (key, ok) = crate::sections::doctor::repair_outcome(&action, &result);
                this.last_error = None;
                this.set_transient(DOCTOR_REPAIR_OUTCOME, json!({ "key": key, "ok": ok }), cx);
                this.run_diagnostics(cx);
            },
        );
    }

    /// 清除上一次修复的就地提示（重新运行诊断时）。
    pub fn clear_repair_outcome(&mut self, cx: &mut Context<Self>) {
        if self.transient.remove(DOCTOR_REPAIR_OUTCOME).is_some() {
            cx.notify();
        }
    }

    pub fn load_site_auth(&mut self, cx: &mut Context<Self>) {
        self.call_with(
            "siteAuth",
            method::DAEMON_SITE_AUTH_LIST,
            json!({}),
            cx,
            Self::absorb_site_auth,
        );
    }
    pub fn delete_site_auth(&mut self, site: &str, cx: &mut Context<Self>) {
        self.call_with(
            "siteAuth",
            method::DAEMON_SITE_AUTH_DELETE,
            json!({ "site": site }),
            cx,
            Self::absorb_site_auth,
        );
    }
    pub fn clear_site_auth(&mut self, cx: &mut Context<Self>) {
        self.call_with(
            "siteAuth",
            method::DAEMON_SITE_AUTH_CLEAR,
            json!({}),
            cx,
            Self::absorb_site_auth,
        );
    }
    fn absorb_site_auth(&mut self, result: Result<Value, RpcErrorData>, _cx: &mut Context<Self>) {
        if let Ok(value) = result
            && let Ok(entries) = serde_json::from_value::<Vec<SiteAuthEntryDto>>(value)
        {
            self.site_auth = entries;
        }
    }

    // ───────────────────────── BT 防吸血 ─────────────────────────

    #[must_use]
    pub fn bt_guard_state(&self) -> Option<&BtGuardStateDto> {
        self.bt_guard.as_ref()
    }
    #[must_use]
    pub fn bt_guard_revision(&self) -> u64 {
        self.bt_guard_revision
    }
    /// 本地动作（添加 / 解除 / 加白）成功后立即通知条目 / 事件面板与总览刷新，不依赖事件回流。
    pub fn bump_bt_guard_revision(&mut self, cx: &mut Context<Self>) {
        self.bt_guard_revision = self.bt_guard_revision.wrapping_add(1);
        self.load_attempts.remove(BT_GUARD_STATE_LOAD);
        cx.notify();
    }
    pub fn load_bt_guard_state(&mut self, cx: &mut Context<Self>) {
        self.load_attempts.insert(BT_GUARD_STATE_LOAD);
        self.call_with(
            "btGuardState",
            method::DAEMON_BT_GUARD_STATE,
            json!({}),
            cx,
            Self::absorb_bt_guard_state,
        );
    }
    fn absorb_bt_guard_state(
        &mut self,
        result: Result<Value, RpcErrorData>,
        cx: &mut Context<Self>,
    ) {
        // RPC 失败已由 `call_with` 记入 `last_error`；闸门保持置位，避免渲染期反复重发，
        // 由「重新加载」按钮或 `BtGuardChanged` 重新放行。
        let Ok(value) = result else { return };
        match decode_wire::<BtGuardStateDto>(value) {
            Some(state) => self.bt_guard = Some(state),
            None => self.set_error(SettingsErrorKind::Failed, "", cx),
        }
    }
    /// 立即刷新过滤文件订阅（慢方法：会拉取网络）；结果里的订阅状态直接并入总览。
    pub fn refresh_bt_guard_filter_sources(&mut self, cx: &mut Context<Self>) {
        self.call_with(
            "btGuardFilterRefresh",
            method::DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
            json!({}),
            cx,
            |this, result, cx| {
                let Ok(value) = result else { return };
                match decode_wire::<BtGuardFilterSourcesRefreshDto>(value) {
                    Some(refreshed) => {
                        if let Some(state) = &mut this.bt_guard {
                            state.filter_sources = refreshed.sources;
                        }
                    }
                    None => this.set_error(SettingsErrorKind::Failed, "", cx),
                }
            },
        );
    }

    pub fn load_conn_policy(&mut self, cx: &mut Context<Self>) {
        self.call_with(
            "connPolicy",
            method::DAEMON_CONFIG_CONN_POLICY,
            json!({}),
            cx,
            Self::absorb_conn_policy,
        );
    }
    pub fn clear_conn_policy(&mut self, cx: &mut Context<Self>) {
        self.call_with(
            "connPolicy",
            method::DAEMON_CONFIG_CLEAR_CONN_POLICY,
            json!({}),
            cx,
            Self::absorb_conn_policy,
        );
    }
    fn absorb_conn_policy(&mut self, result: Result<Value, RpcErrorData>, _cx: &mut Context<Self>) {
        if let Ok(value) = result
            && let Ok(summary) = serde_json::from_value::<ConnPolicySummaryDto>(value)
        {
            self.conn_policy = Some(summary);
        }
    }

    pub fn load_system_proxy(&mut self, cx: &mut Context<Self>) {
        self.call_with(
            "systemProxy",
            method::DAEMON_CONFIG_SYSTEM_PROXY,
            json!({}),
            cx,
            Self::absorb_system_proxy,
        );
    }
    fn absorb_system_proxy(
        &mut self,
        result: Result<Value, RpcErrorData>,
        _cx: &mut Context<Self>,
    ) {
        if let Ok(value) = result
            && let Ok(dto) = serde_json::from_value::<SystemProxyDto>(value)
        {
            self.system_proxy = Some(dto);
        }
    }

    pub fn check_update(&mut self, channel: Option<String>, cx: &mut Context<Self>) {
        self.call_update(
            "update",
            method::AGENT_UPDATE_CHECK,
            json!({ "channel": channel }),
            cx,
        );
    }

    /// 「更新并重启」：包未就绪时 agent 先下载再安装；成功后 agent 随即整体重启，连接会被
    /// `service-quit` 关闭，这类 `Unavailable` 属预期，不当作错误展示。
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        self.call_update("updateInstall", method::AGENT_UPDATE_INSTALL, json!({}), cx);
    }

    pub fn cancel_update(&mut self, cx: &mut Context<Self>) {
        self.call_update("updateCancel", method::AGENT_UPDATE_CANCEL, json!({}), cx);
    }

    fn call_update(
        &mut self,
        action: &'static str,
        method: &'static str,
        params: Value,
        cx: &mut Context<Self>,
    ) {
        self.call_with(action, method, params, cx, |this, result, _| match result {
            Ok(value) => {
                if let Ok(dto) = serde_json::from_value::<UpdateStatusDto>(value) {
                    this.update = dto;
                }
            }
            Err(error) if error.code == ApplicationErrorCode::Unavailable => {
                this.last_error = None;
            }
            Err(_) => {}
        });
    }

    /// 退出前把尚未发送的编辑立即打成 RPC future 交给调用方等待（不再走防抖）。
    #[must_use]
    pub fn drain_pending_calls(&mut self) -> Vec<PortFuture<Value>> {
        if self.stale || (self.pending_daemon.is_empty() && self.pending_prefs.is_empty()) {
            return Vec::new();
        }
        let daemon_values = std::mem::take(&mut self.pending_daemon);
        let prefs = std::mem::take(&mut self.pending_prefs);
        self.build_flush_calls(daemon_values, prefs)
    }

    // ───────────────────────── 写回泵 ─────────────────────────

    fn schedule_flush(&mut self, cx: &mut Context<Self>) {
        if self.flush_scheduled {
            return;
        }
        self.flush_scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLUSH_DEBOUNCE).await;

            let Ok(()) = this.update(cx, |this, cx| {
                this.flush_scheduled = false;
                this.flush(cx);
            }) else {
                // 设置视图或窗口已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    fn flush(&mut self, cx: &mut Context<Self>) {
        if self.flush_inflight || self.stale {
            return;
        }
        if self.pending_daemon.is_empty() && self.pending_prefs.is_empty() {
            return;
        }
        self.flush_inflight = true;
        let daemon_values = std::mem::take(&mut self.pending_daemon);
        let prefs = std::mem::take(&mut self.pending_prefs);
        self.inflight_daemon = daemon_values.clone();
        self.inflight_prefs = prefs.clone();

        let calls = self.build_flush_calls(daemon_values, prefs);

        cx.spawn(async move |this, cx| {
            let mut first_error: Option<RpcErrorData> = None;
            for call in calls {
                if let Err(error) = call.await
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }

            let Ok(()) =
                this.update(cx, |this, cx| {
                    this.flush_inflight = false;
                    match first_error {
                        None => {
                            this.settle_inflight();
                            this.conflict_retries = 0;
                            if this.last_error.as_ref().is_some_and(|error| {
                                error.kind != SettingsErrorKind::InvalidArgument
                            }) {
                                this.last_error = None;
                            }
                        }
                        Some(error)
                            if error.code == ApplicationErrorCode::Conflict
                                && this.conflict_retries < MAX_CONFLICT_RETRIES =>
                        {
                            this.conflict_retries += 1;
                            let inflight = std::mem::take(&mut this.inflight_daemon);
                            for (key, value) in inflight {
                                this.pending_daemon.entry(key).or_insert(value);
                            }
                            let inflight = std::mem::take(&mut this.inflight_prefs);
                            for (key, value) in inflight {
                                this.pending_prefs.entry(key).or_insert(value);
                            }
                            this.schedule_flush(cx);
                        }
                        Some(error) => {
                            this.rollback_inflight();
                            this.conflict_retries = 0;
                            this.last_error = Some(SettingsError {
                                kind: SettingsErrorKind::from_rpc(&error),
                                detail: SharedString::default(),
                            });
                        }
                    }
                    if !this.pending_daemon.is_empty() || !this.pending_prefs.is_empty() {
                        this.schedule_flush(cx);
                    }
                    cx.notify();
                })
            else {
                // 设置视图或窗口已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    fn build_flush_calls(
        &self,
        daemon_values: BTreeMap<String, String>,
        prefs: BTreeMap<String, (Value, bool)>,
    ) -> Vec<PortFuture<Value>> {
        let mut calls: Vec<PortFuture<Value>> = Vec::new();
        if !daemon_values.is_empty() {
            let patch = DaemonConfigPatch {
                expected_revision: self.daemon.revision,
                values: daemon_values,
            };
            let params = serde_json::to_value(patch).unwrap_or_else(|_| json!({}));
            calls.push(self.port.call(method::DAEMON_CONFIG_PATCH, params));
        }
        let (synced, local): (BTreeMap<_, _>, BTreeMap<_, _>) = prefs
            .into_iter()
            .partition::<BTreeMap<String, (Value, bool)>, _>(|(_, (_, synced))| *synced);
        if !synced.is_empty() {
            let values: serde_json::Map<String, Value> = synced
                .into_iter()
                .map(|(key, (value, _))| (key, value))
                .collect();
            calls.push(
                self.port
                    .call(method::AGENT_PREFERENCES_PATCH, json!({ "values": values })),
            );
        }
        if !local.is_empty() {
            let values: serde_json::Map<String, Value> = local
                .into_iter()
                .map(|(key, (value, _))| (key, value))
                .collect();
            calls.push(self.port.call(
                method::AGENT_PREFERENCES_PATCH,
                json!({ "values": values, "sync": false }),
            ));
        }
        calls
    }

    fn set_error(
        &mut self,
        kind: SettingsErrorKind,
        detail: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.last_error = Some(SettingsError {
            kind,
            detail: SharedString::from(detail.into()),
        });
        cx.notify();
    }
}

/// 偏好写入是否进入云同步：只有同步目录内（且未被排除）的键才上云；
/// 窗口边界、侧栏设备区显隐等设备本地键一律 `sync:false`。
fn preference_is_synced(key: &str) -> bool {
    setting_spec(key).is_some_and(|spec| spec.owner != SettingOwner::Excluded)
}

/// 解码 daemon 响应；形状不符返回 `None`（调用方据此显示错误，不得静默吞掉）。
fn decode_wire<T: serde::de::DeserializeOwned>(value: Value) -> Option<T> {
    serde_json::from_value(value).ok()
}

/// daemon wire 字符串 → 云同步目录键的 JSON 值。
fn daemon_string_to_json(spec_key: &str, wire: &str) -> Value {
    match setting_value_kind(spec_key) {
        fluxdown_protocol::SettingValueKind::Boolean => Value::Bool(matches!(wire, "true" | "1")),
        fluxdown_protocol::SettingValueKind::Integer => {
            wire.parse::<i64>().map_or(Value::Null, Value::from)
        }
        fluxdown_protocol::SettingValueKind::Float => wire
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
        fluxdown_protocol::SettingValueKind::String => Value::String(wire.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::{SettingsErrorKind, SettingsStore, preference_is_synced};
    use crate::port::{PortFuture, SettingsPort};

    struct NullPort;

    impl SettingsPort for NullPort {
        fn call(
            &self,
            _method: &'static str,
            _params: serde_json::Value,
        ) -> PortFuture<serde_json::Value> {
            Box::pin(async { Ok(serde_json::Value::Null) })
        }
    }

    #[test]
    fn gateway_failure_reason_takes_precedence_over_error_code() {
        use fluxdown_protocol::{ApplicationErrorCode, ErrorReason, RpcErrorData};

        for (reason, expected) in [
            (
                ErrorReason::GatewayPortInUse,
                SettingsErrorKind::GatewayPortInUse,
            ),
            (
                ErrorReason::GatewayRestartFailed,
                SettingsErrorKind::GatewayRestartFailed,
            ),
        ] {
            for code in [
                ApplicationErrorCode::Conflict,
                ApplicationErrorCode::Unavailable,
            ] {
                let error = RpcErrorData::new(code, false).with_reason(reason);
                assert_eq!(SettingsErrorKind::from_rpc(&error), expected);
            }
        }
    }

    fn delivery(id: &str, ts: i64) -> fluxdown_protocol::WebhookDeliveryDto {
        fluxdown_protocol::WebhookDeliveryDto {
            delivery_id: id.to_owned(),
            timestamp_ms: ts,
            event: String::new(),
            endpoint_id: String::new(),
            endpoint_name: String::new(),
            url: String::new(),
            request_headers: String::new(),
            request_body: String::new(),
            status_code: 200,
            response_body: String::new(),
            latency_ms: 0,
            attempts: 1,
            success: true,
            error: String::new(),
        }
    }

    #[test]
    fn webhook_delta_merges_and_clear_is_explicit() {
        use fluxdown_protocol::DaemonEvent;
        let mut store = SettingsStore::new(Arc::new(NullPort));
        store.apply_webhook_event(&DaemonEvent::WebhooksChanged(vec![
            delivery("a", 1),
            delivery("b", 2),
        ]));
        store.apply_webhook_event(&DaemonEvent::WebhooksChanged(vec![delivery("c", 3)]));
        let ids: Vec<&str> = store
            .webhook_deliveries()
            .iter()
            .map(|d| d.delivery_id.as_str())
            .collect();
        assert_eq!(ids, ["c", "b", "a"]);
        store.apply_webhook_event(&DaemonEvent::WebhooksChanged(Vec::new()));
        assert_eq!(store.webhook_deliveries().len(), 3);
        store.apply_webhook_event(&DaemonEvent::WebhooksCleared);
        assert!(store.webhook_deliveries().is_empty());
    }

    const PREF: &str = "download.max_concurrent_tasks";
    const WIRE: &str = "max_concurrent_tasks";

    fn store_with_server_value(server: &str) -> SettingsStore {
        let mut store = SettingsStore::new(Arc::new(NullPort));
        store
            .daemon
            .values
            .insert(WIRE.to_owned(), server.to_owned());
        store
    }

    #[test]
    fn load_gate_fires_once_until_reset() {
        let mut store = SettingsStore::new(Arc::new(NullPort));
        assert!(store.begin_load("connPolicy"));
        // 失败后重绘不得再次发起。
        assert!(!store.begin_load("connPolicy"));
        assert!(store.begin_load("siteAuth"));
        store.reset_load("connPolicy");
        assert!(store.begin_load("connPolicy"));
        assert!(!store.begin_load("siteAuth"));
    }

    /// 渲染期读取 token 失败后不得在下一次重绘里重发；断线时不发；网关变化后重新放行。
    #[test]
    fn gateway_token_reveal_does_not_loop_after_failure_or_while_stale() {
        let mut store = SettingsStore::new(Arc::new(NullPort));
        // 新建的 store 在首个快照前处于断线只读，不发起读取。
        assert!(!store.should_reveal_gateway_token());
        store.stale = false;
        assert!(store.should_reveal_gateway_token());
        // 模拟一次已发起但失败（没有写入 transient）的读取。
        store.gateway_token_stale = false;
        store.load_attempts.insert("gatewayToken");
        assert!(!store.should_reveal_gateway_token());
        // 网关变化（如重新生成 token）重新放行。
        store.gateway_token_stale = true;
        assert!(store.should_reveal_gateway_token());
        store.stale = true;
        assert!(!store.should_reveal_gateway_token());
    }

    /// 首次连接 agent 失败 → 连上后必须发起加载；连上后加载失败 → 本轮不重发；
    /// 断线重连拿到新快照 → 再次放行。
    #[test]
    fn load_gates_reopen_after_first_connect_and_every_reconnect() {
        let mut store = SettingsStore::new(Arc::new(NullPort));
        // 首个快照前（含首次连接失败、广播 Stale）：只读，渲染期不发任何加载。
        assert!(store.is_read_only());
        assert!(!store.should_reveal_gateway_token());
        store.reopen_load_gates();
        assert!(!store.is_read_only());
        assert!(!store.load_attempted("integration"));
        assert!(store.should_reveal_gateway_token());
        // 连上后两项读取都失败（busy 已清、未写入结果）：本轮不再重发。
        assert!(store.begin_load("integration"));
        store.gateway_token_stale = false;
        store.load_attempts.insert("gatewayToken");
        assert!(store.load_attempted("integration"));
        assert!(!store.should_reveal_gateway_token());
        // 断线（mark_stale 置只读）后重连：新快照重新放行。
        store.stale = true;
        store.reopen_load_gates();
        assert!(!store.load_attempted("integration"));
        assert!(store.should_reveal_gateway_token());
    }

    fn wire(store: &SettingsStore) -> Option<&str> {
        store.daemon.values.get(WIRE).map(String::as_str)
    }

    #[test]
    fn pending_daemon_owned_pref_survives_daemon_snapshot_replacement() {
        let mut store = store_with_server_value("3");
        store.capture_baselines(PREF, Some(WIRE));
        store.daemon.values.insert(WIRE.to_owned(), "8".to_owned());
        store
            .pending_prefs
            .insert(PREF.to_owned(), (json!(8), true));

        // 服务端快照（仍是旧值）整体替换 daemon 后重放本地编辑。
        store.daemon.values.insert(WIRE.to_owned(), "3".to_owned());
        store.overlay_local_edits();
        assert_eq!(wire(&store), Some("8"));
    }

    #[test]
    fn failed_writeback_restores_server_value() {
        let mut store = store_with_server_value("3");
        store.capture_baselines(PREF, Some(WIRE));
        store.daemon.values.insert(WIRE.to_owned(), "8".to_owned());
        store
            .inflight_prefs
            .insert(PREF.to_owned(), (json!(8), true));

        store.rollback_inflight();
        assert_eq!(wire(&store), Some("3"));
        assert!(store.inflight_prefs.is_empty());
    }

    #[test]
    fn failed_writeback_keeps_newer_pending_edit() {
        let mut store = store_with_server_value("3");
        store.capture_baselines(PREF, Some(WIRE));
        store.daemon.values.insert(WIRE.to_owned(), "9".to_owned());
        store
            .inflight_prefs
            .insert(PREF.to_owned(), (json!(8), true));
        store
            .pending_prefs
            .insert(PREF.to_owned(), (json!(9), true));

        store.rollback_inflight();
        assert_eq!(wire(&store), Some("9"));
    }

    #[test]
    fn catalog_keys_sync_and_device_local_keys_do_not() {
        // 侧栏设备区显隐、窗口边界是设备本地偏好，不得随账号同步到其他设备。
        assert!(!preference_is_synced("ui.show_sidebar_devices"));
        assert!(!preference_is_synced("desktop.window.main"));
        // 提示音偏好是设备本地键：每台设备的音频文件路径各不相同。
        for key in fluxdown_protocol::sound::SOUND_PREF_KEYS {
            assert!(!preference_is_synced(key), "{key}");
        }
        assert!(!preference_is_synced(fluxdown_ui_theme::FONT_FAMILY_KEY));
        assert!(!preference_is_synced("unknown.key"));
        // 同步目录内的键（含自定义分类与带每机属性的 BT / ED2K 开关）走同步链路，
        // 由「在此设备同步的范围」按需设为本机专属。
        assert!(preference_is_synced("appearance.theme_mode"));
        assert!(preference_is_synced("bt.enable_dht"));
        assert!(preference_is_synced("custom_categories"));
    }

    #[test]
    fn malformed_guard_state_is_detected_not_swallowed() {
        use super::decode_wire;
        assert!(decode_wire::<fluxdown_protocol::BtGuardStateDto>(json!("nope")).is_none());
        assert!(
            decode_wire::<fluxdown_protocol::BtGuardStateDto>(json!({"enabled": "x"})).is_none()
        );
        assert!(
            decode_wire::<fluxdown_protocol::BtGuardFilterSourcesRefreshDto>(json!([1])).is_none()
        );
    }
}
