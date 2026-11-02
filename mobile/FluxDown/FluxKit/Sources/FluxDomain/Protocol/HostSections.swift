import Foundation

// 通用「分区 / 通知」通道的键（与 Rust `native/mobile/src/sections.rs` 的 `pub const` 一一对应）。
// 分区值 = 协议 serde wire 的 JSON（camelCase，同 `web/src/lib/rpc/protocol/*.ts`）：
// `HostState.section(_:as:)` 解码，`HostSnapshot.sections` / `HostEvent.sectionChanged` 整值下发。

/// `HostState.sections` / `HostEvent.sectionChanged` 的分区键。已有类型化模型覆盖的字段
/// （`daemonConnected`、`tasks`、`taskRuntime`、`queues`、`queuePositions`、`groups`、`config`、
/// `rssSources`、`priority`、`runtimeStats`、`pendingSelections`、`cloudDevices`、`linkedDevices`）
/// 不在此列。
public enum HostSection {
    /// `AgentSessionDto?`：未登录为 JSON `null`。
    public static let agentSession = "agent.session"
    /// `SyncStatusDto`。
    public static let agentSync = "agent.sync"
    /// `CloudConnectionDto`。
    public static let agentCloudConnection = "agent.cloudConnection"
    /// `AgentPreferencesDto`（完整偏好；分类另有类型化投影 `HostState.categories`）。
    public static let agentPreferences = "agent.preferences"
    /// `GatewayStatusDto`。
    public static let agentGateway = "agent.gateway"
    /// `[RemoteTaskDto]`。
    public static let agentRemoteTasks = "agent.remoteTasks"
    /// `[PendingCaptureDto]`。
    public static let agentPendingCaptures = "agent.pendingCaptures"
    /// `[LinkPairingRequestDto]`：等待本机确认的入站局域网配对请求。
    public static let agentLinkPairingRequests = "agent.linkPairingRequests"
    /// `[LinkDiscoveredPeer]`：局域网发现到的设备。
    public static let agentLinkDiscovered = "agent.linkDiscovered"
    /// `ShellStatusDto`。
    public static let agentShell = "agent.shell"
    /// `PowerStatusDto`。
    public static let agentPower = "agent.power"
    /// `CloudNotifyStateDto`：云端推送概览（渠道 / 用量 / 本机上报偏好）；移动端暂不展示。
    public static let agentCloudNotify = "agent.cloudNotify"
    /// `[String: UInt64]`：sourceId → RSS 条目流修订号（变化即应重新 `daemon.rss.getItems`）。
    public static let daemonRssItemRevisions = "daemon.rssItemRevisions"
    /// `[PluginDto]`。
    public static let daemonPlugins = "daemon.plugins"
    /// `[ComponentStatusDto]`。
    public static let daemonComponents = "daemon.components"
    /// `[WebhookDeliveryDto]`：投递日志（时间降序，封顶 1000）。
    public static let daemonWebhookDeliveries = "daemon.webhookDeliveries"

    /// 全部键（快照必须携带每一个）。
    public static let all: [String] = [
        agentSession, agentSync, agentCloudConnection, agentPreferences, agentGateway,
        agentRemoteTasks, agentPendingCaptures, agentLinkPairingRequests, agentLinkDiscovered,
        agentShell, agentPower, agentCloudNotify, daemonRssItemRevisions, daemonPlugins, daemonComponents,
        daemonWebhookDeliveries,
    ]
}

/// `HostEvent.notice` / `HostNotice.name`：一次性通知（不进快照）的 serde 变体名。
/// `AgentEvent` / `DaemonEvent` 通知的 JSON 是变体载荷；`WsServerMsg` 通知的 JSON 是整条消息对象
/// （扁平 camelCase，含 `type` 判别）。
public enum HostNoticeName {
    /// 载荷：`ErrorReason` wire 名字符串（`deviceUntrusted` / `sessionExpired` / `accountDisabled`）。
    public static let sessionRevoked = "sessionRevoked"
    /// 载荷：任务 id 数组。
    public static let captureTasksStarted = "captureTasksStarted"
    /// 载荷：`TaskActivityDto`。
    public static let taskActivityAdded = "taskActivityAdded"
    /// `WsServerMsg::PluginAutoDisabled { identity, reason }`。
    public static let pluginAutoDisabled = "pluginAutoDisabled"
    /// `WsServerMsg::DuplicateTorrent { taskId, existingTaskId, existingName }`。
    public static let duplicateTorrent = "duplicateTorrent"
    /// `WsServerMsg::PluginHookActivity { taskId, pluginId, running }`。
    public static let pluginHookActivity = "pluginHookActivity"
    /// `WsServerMsg::RssItemsChanged { sourceId, items, notifyTitles }`。
    public static let rssItemsChanged = "rssItemsChanged"
    /// `WsServerMsg::RssFeedValidated { requestId, url, feedTitle, items, error }`。
    public static let rssFeedValidated = "rssFeedValidated"
    /// `WsServerMsg::ComponentProgress { component, downloadedBytes, totalBytes }`。
    public static let componentProgress = "componentProgress"
    /// `WsServerMsg::ComponentResult { component, ok, message }`。
    public static let componentResult = "componentResult"
    /// `WsServerMsg::LinkIncomingPairing { sessionId, sas, name, platform }`。
    public static let linkIncomingPairing = "linkIncomingPairing"
    /// `WsServerMsg::TaskCdnEvent { taskId, kind, host, nodes, ip, reason, … }`。
    public static let taskCdnEvent = "taskCdnEvent"
    /// `DaemonEvent::BtGuardChanged`：防吸血条目 / 状态变化，载荷恒为 `null`，据此重新拉取 `daemon.bt.guard.*`。
    public static let btGuardChanged = "btGuardChanged"
}
