package com.fluxdown.core.protocol

/**
 * 通用分区键（`HostState.sections` / `HostEvent.SectionChanged.name`），与 Rust `native/mobile/src/sections.rs`
 * 的 `pub const` 及 iOS `FluxDomain/Protocol/HostSections.swift` 一一对应；值为协议 serde wire 的 JSON。
 */
object HostSection {
    /** `Option<AgentSessionDto>`：未登录为 `null`。 */
    const val agentSession = "agent.session"
    /** `SyncStatusDto`。 */
    const val agentSync = "agent.sync"
    /** `CloudConnectionDto`。 */
    const val agentCloudConnection = "agent.cloudConnection"
    /** `AgentPreferencesDto`（完整偏好）。 */
    const val agentPreferences = "agent.preferences"
    /** `GatewayStatusDto`。 */
    const val agentGateway = "agent.gateway"
    /** `Vec<RemoteTaskDto>`。 */
    const val agentRemoteTasks = "agent.remoteTasks"
    /** `Vec<PendingCaptureDto>`。 */
    const val agentPendingCaptures = "agent.pendingCaptures"
    /** `Vec<LinkPairingRequestDto>`。 */
    const val agentLinkPairingRequests = "agent.linkPairingRequests"
    /** `Vec<LinkDiscoveredPeer>`。 */
    const val agentLinkDiscovered = "agent.linkDiscovered"
    /** `ShellStatusDto`。 */
    const val agentShell = "agent.shell"
    /** `PowerStatusDto`。 */
    const val agentPower = "agent.power"
    /** `CloudNotifyStateDto`：云端推送概览（渠道 / 用量 / 本机上报偏好）；移动端暂不展示。 */
    const val agentCloudNotify = "agent.cloudNotify"
    /** `BTreeMap<String, u64>`：sourceId → RSS 条目流修订号。 */
    const val daemonRssItemRevisions = "daemon.rssItemRevisions"
    /** `Vec<PluginDto>`。 */
    const val daemonPlugins = "daemon.plugins"
    /** `Vec<ComponentStatusDto>`。 */
    const val daemonComponents = "daemon.components"
    /** `Vec<WebhookDeliveryDto>`：投递日志（时间降序，封顶 1000）。 */
    const val daemonWebhookDeliveries = "daemon.webhookDeliveries"
}

/** 一次性通知名（`HostEvent.Notice.name`），与 `sections.rs` 的 `NOTICE_*` 一一对应。 */
object HostNotice {
    /** 载荷为 `ErrorReason` 的 wire 名字符串。 */
    const val sessionRevoked = "sessionRevoked"
    const val captureTasksStarted = "captureTasksStarted"
    const val taskActivityAdded = "taskActivityAdded"
    const val pluginAutoDisabled = "pluginAutoDisabled"
    const val duplicateTorrent = "duplicateTorrent"
    const val pluginHookActivity = "pluginHookActivity"
    const val rssItemsChanged = "rssItemsChanged"
    const val rssFeedValidated = "rssFeedValidated"
    const val componentProgress = "componentProgress"
    const val componentResult = "componentResult"
    const val linkIncomingPairing = "linkIncomingPairing"
    const val taskCdnEvent = "taskCdnEvent"
    /** `DaemonEvent::BtGuardChanged`：BT 防吸血条目 / 状态变化（载荷恒为 `null`），收到后重新拉取 `daemon.bt.guard.*`。 */
    const val btGuardChanged = "btGuardChanged"
}
