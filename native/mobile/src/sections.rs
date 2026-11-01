//! 通用「分区 / 通知」通道的契约：除了已有类型化 DTO 之外，客户端要渲染的其余
//! `AgentSnapshot` / `DaemonSnapshot` 字段，都以 **一条 JSON 字符串** 暴露（形状 = 协议 serde
//! wire，camelCase，与 `web/src/lib/rpc/protocol/*.ts` 镜像逐字一致）。
//!
//! - 键：`"agent.<serde 字段名>"` / `"daemon.<serde 字段名>"`（本模块的 `pub const`）；
//! - 初值：`HostSnapshotDto.sections`（连接 / 重同步时整表下发）；
//! - 增量：事件使某分区的序列化结果变化时下发 `HostEventDto::SectionChanged { name, json }`，
//!   序列化结果不变（空操作）不下发；
//! - 一次性通知（不是状态）：`HostEventDto::Notice { name, json }`，`name` 为 serde 变体名
//!   （`NOTICE_*` 常量）。`AgentEvent` / `DaemonEvent` 通知的 `json` 是变体载荷；`WsServerMsg`
//!   通知的 `json` 是整条消息对象（扁平 camelCase，含 `type` 判别）。
//!
//! 已有类型化 DTO 覆盖的字段（`daemonConnected`、`tasks`、`taskRuntime`、`queues`、
//! `queuePositions`、`groups`、`config`、`rssSources`、`priority`、`runtimeStats`、
//! `pendingSelections`、`cloudDevices`、`linkedDevices`）不在此列。

use fluxdown_protocol::{AgentEvent, AgentSnapshot, DaemonEvent, WsServerMsg};
use serde::Serialize;

/// `Option<AgentSessionDto>`：未登录为 `null`。
pub const AGENT_SESSION: &str = "agent.session";
/// `SyncStatusDto`。
pub const AGENT_SYNC: &str = "agent.sync";
/// `CloudConnectionDto`。
pub const AGENT_CLOUD_CONNECTION: &str = "agent.cloudConnection";
/// `AgentPreferencesDto`（完整偏好；分类另有类型化投影）。
pub const AGENT_PREFERENCES: &str = "agent.preferences";
/// `GatewayStatusDto`。
pub const AGENT_GATEWAY: &str = "agent.gateway";
/// `Vec<RemoteTaskDto>`。
pub const AGENT_REMOTE_TASKS: &str = "agent.remoteTasks";
/// `Vec<PendingCaptureDto>`。
pub const AGENT_PENDING_CAPTURES: &str = "agent.pendingCaptures";
/// `Vec<LinkPairingRequestDto>`：等待本机确认的入站局域网配对请求。
pub const AGENT_LINK_PAIRING_REQUESTS: &str = "agent.linkPairingRequests";
/// `Vec<LinkDiscoveredPeer>`：局域网发现到的设备。
pub const AGENT_LINK_DISCOVERED: &str = "agent.linkDiscovered";
/// `ShellStatusDto`。
pub const AGENT_SHELL: &str = "agent.shell";
/// `PowerStatusDto`。
pub const AGENT_POWER: &str = "agent.power";
/// `CloudNotifyStateDto`：云端推送状态（原生移动端暂不展示，仅作通用通道完整性）。
pub const AGENT_CLOUD_NOTIFY: &str = "agent.cloudNotify";
/// `BTreeMap<String, u64>`：sourceId → RSS 条目流修订号（变化即应重新 `daemon.rss.getItems`）。
pub const DAEMON_RSS_ITEM_REVISIONS: &str = "daemon.rssItemRevisions";
/// `Vec<PluginDto>`。
pub const DAEMON_PLUGINS: &str = "daemon.plugins";
/// `Vec<ComponentStatusDto>`。
pub const DAEMON_COMPONENTS: &str = "daemon.components";
/// `Vec<WebhookDeliveryDto>`：投递日志（时间降序，封顶 1000）。
pub const DAEMON_WEBHOOK_DELIVERIES: &str = "daemon.webhookDeliveries";

/// `AgentEvent::SessionRevoked(ErrorReason)`：载荷为 `ErrorReason` 的 wire 名字符串。
pub const NOTICE_SESSION_REVOKED: &str = "sessionRevoked";
/// `AgentEvent::CaptureTasksStarted(Vec<String>)`：载荷为任务 id 数组。
pub const NOTICE_CAPTURE_TASKS_STARTED: &str = "captureTasksStarted";
/// `DaemonEvent::TaskActivityAdded(TaskActivityDto)`：载荷为 `TaskActivityDto`。
pub const NOTICE_TASK_ACTIVITY_ADDED: &str = "taskActivityAdded";
/// `WsServerMsg::PluginAutoDisabled`。
pub const NOTICE_PLUGIN_AUTO_DISABLED: &str = "pluginAutoDisabled";
/// `WsServerMsg::DuplicateTorrent`。
pub const NOTICE_DUPLICATE_TORRENT: &str = "duplicateTorrent";
/// `WsServerMsg::PluginHookActivity`。
pub const NOTICE_PLUGIN_HOOK_ACTIVITY: &str = "pluginHookActivity";
/// `WsServerMsg::RssItemsChanged`（含 `notifyTitles`）。
pub const NOTICE_RSS_ITEMS_CHANGED: &str = "rssItemsChanged";
/// `WsServerMsg::RssFeedValidated`（按 `requestId` 对应 `daemon.rss.validate`）。
pub const NOTICE_RSS_FEED_VALIDATED: &str = "rssFeedValidated";
/// `WsServerMsg::ComponentProgress`。
pub const NOTICE_COMPONENT_PROGRESS: &str = "componentProgress";
/// `WsServerMsg::ComponentResult`。
pub const NOTICE_COMPONENT_RESULT: &str = "componentResult";
/// `WsServerMsg::LinkIncomingPairing`。
pub const NOTICE_LINK_INCOMING_PAIRING: &str = "linkIncomingPairing";
/// `WsServerMsg::TaskCdnEvent`。
pub const NOTICE_TASK_CDN_EVENT: &str = "taskCdnEvent";
/// `DaemonEvent::BtGuardChanged`：BT 防吸血条目 / 状态变化，载荷恒为 `null`，宿主端据此重新拉取 `daemon.bt.guard.*`。
pub const NOTICE_BT_GUARD_CHANGED: &str = "btGuardChanged";

/// 分区：与上方键一一对应，顺序即 [`Section::index`]。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Section {
    AgentSession,
    AgentSync,
    AgentCloudConnection,
    AgentPreferences,
    AgentGateway,
    AgentRemoteTasks,
    AgentPendingCaptures,
    AgentLinkPairingRequests,
    AgentLinkDiscovered,
    AgentShell,
    AgentPower,
    AgentCloudNotify,
    DaemonRssItemRevisions,
    DaemonPlugins,
    DaemonComponents,
    DaemonWebhookDeliveries,
}

impl Section {
    pub(crate) const ALL: [Self; 16] = [
        Self::AgentSession,
        Self::AgentSync,
        Self::AgentCloudConnection,
        Self::AgentPreferences,
        Self::AgentGateway,
        Self::AgentRemoteTasks,
        Self::AgentPendingCaptures,
        Self::AgentLinkPairingRequests,
        Self::AgentLinkDiscovered,
        Self::AgentShell,
        Self::AgentPower,
        Self::AgentCloudNotify,
        Self::DaemonRssItemRevisions,
        Self::DaemonPlugins,
        Self::DaemonComponents,
        Self::DaemonWebhookDeliveries,
    ];

    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::AgentSession => AGENT_SESSION,
            Self::AgentSync => AGENT_SYNC,
            Self::AgentCloudConnection => AGENT_CLOUD_CONNECTION,
            Self::AgentPreferences => AGENT_PREFERENCES,
            Self::AgentGateway => AGENT_GATEWAY,
            Self::AgentRemoteTasks => AGENT_REMOTE_TASKS,
            Self::AgentPendingCaptures => AGENT_PENDING_CAPTURES,
            Self::AgentLinkPairingRequests => AGENT_LINK_PAIRING_REQUESTS,
            Self::AgentLinkDiscovered => AGENT_LINK_DISCOVERED,
            Self::AgentShell => AGENT_SHELL,
            Self::AgentPower => AGENT_POWER,
            Self::AgentCloudNotify => AGENT_CLOUD_NOTIFY,
            Self::DaemonRssItemRevisions => DAEMON_RSS_ITEM_REVISIONS,
            Self::DaemonPlugins => DAEMON_PLUGINS,
            Self::DaemonComponents => DAEMON_COMPONENTS,
            Self::DaemonWebhookDeliveries => DAEMON_WEBHOOK_DELIVERIES,
        }
    }

    /// 该分区在快照中的当前 JSON。
    pub(crate) fn json(self, snapshot: &AgentSnapshot) -> String {
        match self {
            Self::AgentSession => to_json(self.key(), &snapshot.session),
            Self::AgentSync => to_json(self.key(), &snapshot.sync),
            Self::AgentCloudConnection => to_json(self.key(), &snapshot.cloud_connection),
            Self::AgentPreferences => to_json(self.key(), &snapshot.preferences),
            Self::AgentGateway => to_json(self.key(), &snapshot.gateway),
            Self::AgentRemoteTasks => to_json(self.key(), &snapshot.remote_tasks),
            Self::AgentPendingCaptures => to_json(self.key(), &snapshot.pending_captures),
            Self::AgentLinkPairingRequests => to_json(self.key(), &snapshot.link_pairing_requests),
            Self::AgentLinkDiscovered => to_json(self.key(), &snapshot.link_discovered),
            Self::AgentShell => to_json(self.key(), &snapshot.shell),
            Self::AgentPower => to_json(self.key(), &snapshot.power),
            Self::AgentCloudNotify => to_json(self.key(), &snapshot.cloud_notify),
            Self::DaemonRssItemRevisions => {
                to_json(self.key(), &snapshot.daemon.rss_item_revisions)
            }
            Self::DaemonPlugins => to_json(self.key(), &snapshot.daemon.plugins),
            Self::DaemonComponents => to_json(self.key(), &snapshot.daemon.components),
            Self::DaemonWebhookDeliveries => {
                to_json(self.key(), &snapshot.daemon.webhook_deliveries)
            }
        }
    }
}

/// 序列化协议 DTO。这些类型只含字符串键映射，序列化不会失败；万一失败记录日志并
/// 退化为 `null`（客户端按「无数据」处理），不让投影因此中断。
fn to_json<T: Serialize>(what: &str, value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|error| {
        tracing::error!("encode section {what} failed: {error:#}");
        "null".to_owned()
    })
}

/// 全部分区的当前 JSON（按 [`Section::index`] 排列）。
pub(crate) fn render_all(snapshot: &AgentSnapshot) -> Vec<String> {
    Section::ALL
        .iter()
        .map(|section| section.json(snapshot))
        .collect()
}

const SESSION_GROUP: &[Section] = &[
    Section::AgentSession,
    Section::AgentCloudConnection,
    Section::AgentRemoteTasks,
];

/// 事件可能改变的分区。整表替换（`SnapshotReplaced` / `DaemonSnapshotReplaced`）由投影改发
/// 完整 `Snapshot` 信号，这里不列出。
pub(crate) fn affected(event: &AgentEvent) -> &'static [Section] {
    match event {
        AgentEvent::Daemon(daemon) => match daemon {
            DaemonEvent::RssChanged { .. } => &[Section::DaemonRssItemRevisions],
            DaemonEvent::PluginsChanged(_) => &[Section::DaemonPlugins],
            DaemonEvent::ComponentsChanged(_) => &[Section::DaemonComponents],
            DaemonEvent::WebhooksChanged(_) | DaemonEvent::WebhooksCleared => {
                &[Section::DaemonWebhookDeliveries]
            }
            DaemonEvent::Engine(message) => match message {
                WsServerMsg::RssItemsChanged { .. } => &[Section::DaemonRssItemRevisions],
                WsServerMsg::WebhookDeliveriesChanged { .. } => &[Section::DaemonWebhookDeliveries],
                WsServerMsg::PluginAutoDisabled { .. } => &[Section::DaemonPlugins],
                _ => &[],
            },
            DaemonEvent::TaskRuntimeChanged(_)
            | DaemonEvent::TaskActivityAdded(_)
            // 云端推送事件只由 agent 消费，不应到达投影；到达也不改变任何分区。
            | DaemonEvent::TaskNotice(_)
            // 插件可移植视图事件：移动端 v1 不渲染，不改变任何分区。
            | DaemonEvent::PluginViewFrame(_)
            | DaemonEvent::PluginViewClosed { .. }
            | DaemonEvent::PluginToast { .. }
            | DaemonEvent::SnapshotReplaced(_)
            // 整表重载宣告：任务 / 队列位置表不属于任何分区，由完整快照信号承载。
            | DaemonEvent::TablesReloading { .. }
            | DaemonEvent::TaskChanged(_)
            | DaemonEvent::TasksChanged(_)
            | DaemonEvent::TaskDeleted { .. }
            | DaemonEvent::TasksDeleted { .. }
            | DaemonEvent::QueuesChanged(_)
            | DaemonEvent::GroupsChanged(_)
            | DaemonEvent::ConfigChanged(_)
            | DaemonEvent::RuntimeStatsChanged(_)
            | DaemonEvent::SelectionPending(_)
            | DaemonEvent::SelectionResolved { .. }
            // 云名单刷新 / 证据上报只由 agent 消费；`BtGuardChanged` 走通知而非分区。
            | DaemonEvent::BtGuardChanged
            | DaemonEvent::BtGuardCloudRefreshWanted { .. }
            | DaemonEvent::BtGuardEvidenceReady { .. } => &[],
        },
        AgentEvent::SessionChanged(_) => SESSION_GROUP,
        AgentEvent::SyncChanged(_) => &[Section::AgentSync],
        AgentEvent::CloudConnectionChanged(_) => &[Section::AgentCloudConnection],
        AgentEvent::PreferencesChanged(_) => &[Section::AgentPreferences],
        AgentEvent::GatewayChanged(_) => &[Section::AgentGateway],
        AgentEvent::RemoteTasksChanged(_) => &[Section::AgentRemoteTasks],
        AgentEvent::PendingCapturesChanged(_) => &[Section::AgentPendingCaptures],
        AgentEvent::LinkPairingRequestsChanged(_) => &[Section::AgentLinkPairingRequests],
        AgentEvent::LinkDiscoveredChanged(_) => &[Section::AgentLinkDiscovered],
        AgentEvent::ShellChanged(_) => &[Section::AgentShell],
        AgentEvent::PowerChanged(_) => &[Section::AgentPower],
        AgentEvent::CloudNotifyChanged(_) => &[Section::AgentCloudNotify],
        AgentEvent::DaemonSnapshotReplaced(_)
        | AgentEvent::DaemonConnectionChanged(_)
        | AgentEvent::CloudDevicesChanged(_)
        | AgentEvent::LinkedDevicesChanged(_)
        | AgentEvent::CaptureTasksStarted(_)
        // 外部恢复任务只驱动桌面提示窗口，移动端既无分区也不发通知。
        | AgentEvent::CaptureTasksResumed(_)
        // 远端主机的应用内更新只在其桌面 / Web 界面操作，移动端不暴露分区。
        | AgentEvent::UpdateChanged(_)
        | AgentEvent::SessionRevoked(_) => &[],
    }
}

/// 一次性通知：`(name, json)`。不是通知的事件返回 `None`。
pub(crate) fn notice(event: &AgentEvent) -> Option<(String, String)> {
    match event {
        AgentEvent::SessionRevoked(reason) => Some(variant_notice(
            NOTICE_SESSION_REVOKED,
            serde_json::to_string(reason),
        )),
        AgentEvent::CaptureTasksStarted(task_ids) => Some(variant_notice(
            NOTICE_CAPTURE_TASKS_STARTED,
            serde_json::to_string(task_ids),
        )),
        AgentEvent::Daemon(DaemonEvent::TaskActivityAdded(activity)) => Some(variant_notice(
            NOTICE_TASK_ACTIVITY_ADDED,
            serde_json::to_string(activity),
        )),
        AgentEvent::Daemon(DaemonEvent::BtGuardChanged) => {
            Some((NOTICE_BT_GUARD_CHANGED.to_owned(), "null".to_owned()))
        }
        AgentEvent::Daemon(DaemonEvent::Engine(message)) => engine_notice(message),
        _ => None,
    }
}

fn variant_notice(name: &str, json: Result<String, serde_json::Error>) -> (String, String) {
    let json = json.unwrap_or_else(|error| {
        tracing::error!("encode notice {name} failed: {error:#}");
        "null".to_owned()
    });
    (name.to_owned(), json)
}

/// 客户端需要、且不是状态（或状态之外还带一次性信息）的引擎消息。进度 / 分段 / 列表类
/// 消息已由类型化事件或分区覆盖；`Pong` 与旧版选择请求（走 `SelectionPending`）不转发。
fn engine_notice(message: &WsServerMsg) -> Option<(String, String)> {
    let name = match message {
        WsServerMsg::PluginAutoDisabled { .. } => NOTICE_PLUGIN_AUTO_DISABLED,
        WsServerMsg::DuplicateTorrent { .. } => NOTICE_DUPLICATE_TORRENT,
        WsServerMsg::PluginHookActivity { .. } => NOTICE_PLUGIN_HOOK_ACTIVITY,
        WsServerMsg::RssItemsChanged { .. } => NOTICE_RSS_ITEMS_CHANGED,
        WsServerMsg::RssFeedValidated { .. } => NOTICE_RSS_FEED_VALIDATED,
        WsServerMsg::ComponentProgress { .. } => NOTICE_COMPONENT_PROGRESS,
        WsServerMsg::ComponentResult { .. } => NOTICE_COMPONENT_RESULT,
        WsServerMsg::LinkIncomingPairing { .. } => NOTICE_LINK_INCOMING_PAIRING,
        WsServerMsg::TaskCdnEvent { .. } => NOTICE_TASK_CDN_EVENT,
        _ => return None,
    };
    Some(variant_notice(name, serde_json::to_string(message)))
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::AgentSnapshot;

    use super::{Section, render_all};

    #[test]
    fn keys_are_unique_prefixed_and_indexed_in_order() {
        let mut seen = std::collections::HashSet::new();
        for (position, section) in Section::ALL.iter().enumerate() {
            assert_eq!(section.index(), position);
            assert!(
                section.key().starts_with("agent.") || section.key().starts_with("daemon."),
                "{}",
                section.key()
            );
            assert!(seen.insert(section.key()), "duplicate {}", section.key());
        }
    }

    #[test]
    fn every_section_serializes_to_valid_json_on_the_default_snapshot() {
        let rendered = render_all(&AgentSnapshot::default());
        assert_eq!(rendered.len(), Section::ALL.len());
        for (section, json) in Section::ALL.iter().zip(&rendered) {
            serde_json::from_str::<serde_json::Value>(json)
                .unwrap_or_else(|error| panic!("{}: {error}", section.key()));
        }
        assert_eq!(rendered[Section::AgentSession.index()], "null");
    }

    #[test]
    fn bt_guard_changed_is_a_null_notice_and_touches_no_section() {
        use fluxdown_protocol::{AgentEvent, DaemonEvent};

        let event = AgentEvent::Daemon(DaemonEvent::BtGuardChanged);
        assert!(super::affected(&event).is_empty());
        assert_eq!(
            super::notice(&event),
            Some((super::NOTICE_BT_GUARD_CHANGED.to_owned(), "null".to_owned()))
        );
        for agent_only in [
            DaemonEvent::BtGuardCloudRefreshWanted { have_sequence: 1 },
            DaemonEvent::BtGuardEvidenceReady { count: 2 },
        ] {
            let event = AgentEvent::Daemon(agent_only);
            assert!(super::affected(&event).is_empty());
            assert!(super::notice(&event).is_none());
        }
    }
}
