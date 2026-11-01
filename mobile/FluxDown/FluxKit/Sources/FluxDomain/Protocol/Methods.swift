import Foundation

// 由 `native/protocol/src/method.rs` 一一生成（同 `web/src/lib/rpc/protocol/method.ts`）：
// 常量名 = Rust 常量名的 lowerCamelCase，值 = wire 方法名。改 Rust 常量后同步这里。

/// wire 方法名（`HostSession.call(_:params:)` 只放行 `daemon.*` / `agent.*`；`system.*` 属会话层，
/// 握手 / 快照 / 退出由 FluxBridge 内部处理，这里列出仅为与 Rust 常量表一一对应）。
public enum HostMethod {
    public static let systemHello = "system.hello"
    public static let systemPing = "system.ping"
    public static let systemSnapshot = "system.snapshot"
    /// 任务表 / 队列位置表分页（载入 `taskTotal` / `queueTotal` 加载态的快照头）：由 Rust 驱动（FluxBridge 之下）
    /// 消费，Swift 侧不直接调用；列出仅为与 Rust 常量表一一对应。
    public static let systemTaskPage = "system.taskPage"
    /// 让服务优雅退出：daemon 关停引擎后退出；agent 先关停 daemon 再退出（完全退出）。
    ///
    /// 允许作为连接首帧（握手前）调用：协议版本不兼容的新旧进程替换只能靠它完成。
    public static let systemShutdown = "system.shutdown"
    public static let daemonTaskList = "daemon.task.list"
    public static let daemonTaskGet = "daemon.task.get"
    public static let daemonTaskActivity = "daemon.task.activity"
    /// 任务整个生命周期的持久化速度曲线与统计（`{taskId}` → `TaskSpeedHistory?`，`null` = 无记录）。
    public static let daemonTaskSpeedHistory = "daemon.task.speedHistory"
    public static let daemonTaskCreate = "daemon.task.create"
    public static let daemonTaskPause = "daemon.task.pause"
    public static let daemonTaskResume = "daemon.task.resume"
    public static let daemonTaskRename = "daemon.task.rename"
    /// 更换任务下载源地址（`{taskId, url}`，语义同旧 `PUT /api/v1/tasks/{id}/url`）。
    public static let daemonTaskChangeUrl = "daemon.task.changeUrl"
    /// 修改任务线程数（`SetTaskSegmentsParams`），已下进度保留；完成或不存在的任务返回 `not-found`。
    public static let daemonTaskSetSegments = "daemon.task.setSegments"
    public static let daemonTaskDelete = "daemon.task.delete"
    public static let daemonTaskPauseAll = "daemon.task.pauseAll"
    public static let daemonTaskResumeAll = "daemon.task.resumeAll"
    public static let daemonTaskRescan = "daemon.task.rescan"
    public static let daemonTaskSetSeedLimits = "daemon.task.setSeedLimits"
    public static let daemonTaskVerify = "daemon.task.verify"
    /// 批量暂停 / 恢复（`{taskIds}`）：整批只推一次任务快照；未知 id 忽略，空列表为空操作。
    public static let daemonTaskPauseMany = "daemon.task.pauseMany"
    public static let daemonTaskResumeMany = "daemon.task.resumeMany"
    /// 批量删除（`{taskIds, deleteFiles}`）：语义同批量暂停。
    public static let daemonTaskDeleteMany = "daemon.task.deleteMany"
    /// 批量建任务（`DaemonCreateTasksParams`）：整块一个事务、一次 `tasksChanged` 事件，按 `batchId` 幂等。
    public static let daemonTaskCreateMany = "daemon.task.createMany"
    public static let daemonQueueList = "daemon.queue.list"
    public static let daemonQueueCreate = "daemon.queue.create"
    public static let daemonQueueUpdate = "daemon.queue.update"
    public static let daemonQueueDelete = "daemon.queue.delete"
    public static let daemonQueueStart = "daemon.queue.start"
    public static let daemonQueueStop = "daemon.queue.stop"
    public static let daemonQueueSchedule = "daemon.queue.schedule"
    public static let daemonQueueReorder = "daemon.queue.reorder"
    public static let daemonQueueMoveTask = "daemon.queue.moveTask"
    public static let daemonQueueBoost = "daemon.queue.boost"
    public static let daemonGroupList = "daemon.group.list"
    public static let daemonGroupResolvePreview = "daemon.group.resolvePreview"
    public static let daemonGroupCreate = "daemon.group.create"
    public static let daemonGroupPause = "daemon.group.pause"
    public static let daemonGroupResume = "daemon.group.resume"
    public static let daemonGroupDelete = "daemon.group.delete"
    public static let daemonConfigGet = "daemon.config.get"
    public static let daemonConfigPatch = "daemon.config.patch"
    public static let daemonConfigProxyTest = "daemon.config.proxyTest"
    public static let daemonConfigProxyRoute = "daemon.config.proxyRoute"
    public static let daemonConfigConnPolicy = "daemon.config.connPolicy"
    public static let daemonConfigClearConnPolicy = "daemon.config.clearConnPolicy"
    public static let daemonConfigSystemProxy = "daemon.config.systemProxy"
    public static let daemonSiteAuthList = "daemon.siteAuth.list"
    public static let daemonSiteAuthDelete = "daemon.siteAuth.delete"
    /// 读取单站点凭据详情（含明文密码；站点键会先按 `host` / `host:port` 归一化）。
    public static let daemonSiteAuthGet = "daemon.siteAuth.get"
    /// 保存单站点 HTTP Basic 凭据，返回脱敏后的 `SiteAuthEntryDto`。
    public static let daemonSiteAuthSave = "daemon.siteAuth.save"
    public static let daemonSiteAuthClear = "daemon.siteAuth.clear"
    /// 按下载链接匹配已保存的站点凭据（含明文密码，仅供本机官方 UI 表单回填）。
    public static let daemonSiteAuthMatch = "daemon.siteAuth.match"
    public static let daemonRuntimeStats = "daemon.runtime.stats"
    public static let daemonFsList = "daemon.fs.list"
    public static let daemonRssListSources = "daemon.rss.listSources"
    public static let daemonRssGetItems = "daemon.rss.getItems"
    public static let daemonRssCreateSource = "daemon.rss.createSource"
    public static let daemonRssUpdateSource = "daemon.rss.updateSource"
    public static let daemonRssDeleteSource = "daemon.rss.deleteSource"
    public static let daemonRssRefreshSource = "daemon.rss.refreshSource"
    public static let daemonRssItemAction = "daemon.rss.itemAction"
    public static let daemonRssValidate = "daemon.rss.validate"
    public static let daemonLinkProbe = "daemon.link.probe"
    public static let daemonPluginList = "daemon.plugin.list"
    public static let daemonPluginSniffRules = "daemon.plugin.sniffRules"
    public static let daemonPluginPageScripts = "daemon.plugin.pageScripts"
    public static let daemonPluginPageScriptSource = "daemon.plugin.pageScriptSource"
    public static let daemonPluginAuth = "daemon.plugin.auth"
    public static let daemonPluginSetEnabled = "daemon.plugin.setEnabled"
    public static let daemonPluginUpdateSettings = "daemon.plugin.updateSettings"
    public static let daemonPluginInstall = "daemon.plugin.install"
    public static let daemonPluginInstallDev = "daemon.plugin.installDev"
    /// 重新加载 dev 插件（`{identity}`）：重读 manifest + 源码并校验，失败保留登记。
    public static let daemonPluginReloadDev = "daemon.plugin.reloadDev"
    public static let daemonPluginUninstall = "daemon.plugin.uninstall"
    public static let daemonPluginMarketList = "daemon.plugin.marketList"
    public static let daemonPluginMarketInstall = "daemon.plugin.marketInstall"
    public static let daemonPluginIgnoreRetry = "daemon.plugin.ignoreRetry"
    /// 站点默认插件列表（无参）→ `[PluginSiteDefault]`。
    public static let daemonPluginSiteDefaults = "daemon.plugin.siteDefaults"
    /// `PluginSetSiteDefaultParams`：确认 / 修改 / 移除（identity 为 null）站点默认插件。
    public static let daemonPluginSetSiteDefault = "daemon.plugin.setSiteDefault"
    public static let daemonPluginConsent = "daemon.plugin.consent"
    public static let daemonPluginAuditList = "daemon.plugin.audit.list"
    public static let daemonPluginAuditClear = "daemon.plugin.audit.clear"
    public static let daemonPluginAuditRecord = "daemon.plugin.audit.record"
    public static let daemonPluginViewOpen = "daemon.plugin.view.open"
    public static let daemonPluginViewEvent = "daemon.plugin.view.event"
    public static let daemonPluginViewClose = "daemon.plugin.view.close"
    public static let daemonPluginViewSnapshot = "daemon.plugin.view.snapshot"
    public static let daemonPluginViewKeepalive = "daemon.plugin.view.keepalive"
    public static let daemonPluginCommand = "daemon.plugin.command"
    public static let daemonPluginInvoke = "daemon.plugin.invoke"
    public static let daemonPluginPageInvoke = "daemon.plugin.pageInvoke"
    public static let daemonComponentGet = "daemon.component.get"
    public static let daemonComponentListVersions = "daemon.component.listVersions"
    public static let daemonComponentInstall = "daemon.component.install"
    public static let daemonComponentUninstall = "daemon.component.uninstall"
    public static let daemonSelectionSubscribe = "daemon.selection.subscribe"
    public static let daemonSelectionUnsubscribe = "daemon.selection.unsubscribe"
    public static let daemonSelectionResolve = "daemon.selection.resolve"
    public static let daemonWebhookGet = "daemon.webhook.get"
    public static let daemonWebhookClearDeliveries = "daemon.webhook.clearDeliveries"
    public static let daemonWebhookSimulate = "daemon.webhook.simulate"
    public static let daemonWebhookTest = "daemon.webhook.test"
    public static let daemonCdnReportsPeek = "daemon.cdnReports.peek"
    public static let daemonCdnReportsAck = "daemon.cdnReports.ack"
    public static let daemonCdnConfigApply = "daemon.cdnConfig.apply"
    public static let daemonEgressPlan = "daemon.egress.plan"
    public static let daemonBtTrackerSubscriptionRefresh = "daemon.bt.trackerSubscription.refresh"
    public static let daemonEd2kServerSubscriptionRefresh = "daemon.ed2k.serverSubscription.refresh"
    /// BT 防吸血状态总览（`BtGuardState`）。
    public static let daemonBtGuardState = "daemon.bt.guard.state"
    /// 过滤条目分页（`BtGuardEntriesListParams` → `BtGuardEntriesPage`）。
    public static let daemonBtGuardEntriesList = "daemon.bt.guard.entries.list"
    /// 手动添加条目 / 白名单（`BtGuardEntriesAddParams` → `BtGuardEntry`）。
    public static let daemonBtGuardEntriesAdd = "daemon.bt.guard.entries.add"
    /// 按 id 移除条目（`BtGuardEntriesRemoveParams` → `BtGuardEntriesRemoveResult`）；云层条目被拒绝，应改为加白。
    public static let daemonBtGuardEntriesRemove = "daemon.bt.guard.entries.remove"
    /// 事件日志分页。
    public static let daemonBtGuardEventsList = "daemon.bt.guard.events.list"
    /// 导出条目文本（`BtGuardExportParams` → `BtGuardExportResult`）。慢方法。
    public static let daemonBtGuardExport = "daemon.bt.guard.export"
    /// 立即刷新过滤文件订阅（无参 → `BtGuardFilterSourcesRefresh`）。慢方法。
    public static let daemonBtGuardFilterSourcesRefresh = "daemon.bt.guard.filterSources.refresh"
    /// 仅 agent 调用：已验签的云名单整层替换。移动端不得调用（`native/mobile` 通用通道会拒绝）。
    public static let daemonBtGuardCloudApply = "daemon.bt.guard.cloud.apply"
    /// 仅 agent 调用：弹出待上报证据。移动端不得调用（`native/mobile` 通用通道会拒绝）。
    public static let daemonBtGuardEvidenceDrain = "daemon.bt.guard.evidence.drain"
    public static let daemonDiagnosticsDescribe = "daemon.diagnostics.describe"
    public static let daemonDiagnosticsPrepareLogExport = "daemon.diagnostics.prepareLogExport"
    /// Doctor 动态探测：在 daemon 进程内真实写入各下载目录 / 数据目录并运行外部组件，
    /// 验证下载链路实际拥有的权限（agent 只读配置判断不出 TCC、只读挂载、隔离属性等）。
    public static let daemonDiagnosticsProbe = "daemon.diagnostics.probe"
    /// Doctor 修复：补上托管组件（daemon 数据目录内）缺失的执行权限并重新探测，返回 `ComponentProbeDto`。
    public static let daemonDiagnosticsFixComponent = "daemon.diagnostics.fixComponent"
    public static let daemonMigrationLinkExport = "daemon.migration.linkExport"
    public static let daemonMigrationLinkAck = "daemon.migration.linkAck"
    public static let daemonMigrationGatewayExport = "daemon.migration.gatewayExport"
    public static let daemonMigrationGatewayAck = "daemon.migration.gatewayAck"
    public static let agentSessionGet = "agent.session.get"
    public static let agentAuthRegister = "agent.auth.register"
    public static let agentAuthRegisterVerify = "agent.auth.registerVerify"
    public static let agentAuthLogin = "agent.auth.login"
    public static let agentAuthLoginVerify = "agent.auth.loginVerify"
    public static let agentAuthSendCode = "agent.auth.sendCode"
    public static let agentAuthVerifyCode = "agent.auth.verifyCode"
    public static let agentAuthLogout = "agent.auth.logout"
    public static let agentAuthRefreshProfile = "agent.auth.refreshProfile"
    public static let agentAuthSendPasswordResetCode = "agent.auth.sendPasswordResetCode"
    public static let agentAuthResetPassword = "agent.auth.resetPassword"
    public static let agentProfileSendEmailCode = "agent.profile.sendEmailCode"
    public static let agentProfileSendNewEmailCode = "agent.profile.sendNewEmailCode"
    public static let agentProfileChangeEmail = "agent.profile.changeEmail"
    public static let agentProfileRandomOriginId = "agent.profile.randomOriginId"
    public static let agentProfileCheckOriginId = "agent.profile.checkOriginId"
    public static let agentProfileChangeOriginId = "agent.profile.changeOriginId"
    public static let agentProfileChangeNickname = "agent.profile.changeNickname"
    public static let agentProfileSendPasswordCode = "agent.profile.sendPasswordCode"
    public static let agentProfileChangePassword = "agent.profile.changePassword"
    public static let agentProfileSendDeleteCode = "agent.profile.sendDeleteCode"
    public static let agentProfileDeleteAccount = "agent.profile.deleteAccount"
    public static let agentGatewayGet = "agent.gateway.get"
    public static let agentGatewayPatch = "agent.gateway.patch"
    /// 仅供本机官方 UI 展示/复制用户 token；结果 `{ "userToken": "..." }`（未配置为空串）。
    public static let agentGatewayRevealToken = "agent.gateway.revealToken"
    public static let agentDeviceList = "agent.device.list"
    /// 请求立即重连任务 SSE；返回 `{ "accepted": true }` 不表示已经连接成功。
    public static let agentRemoteReconnect = "agent.remote.reconnect"
    public static let agentDeviceRename = "agent.device.rename"
    public static let agentDeviceDelete = "agent.device.delete"
    public static let agentDeviceDisconnect = "agent.device.disconnect"
    public static let agentPreferencesPatch = "agent.preferences.patch"
    public static let agentSyncGet = "agent.sync.get"
    public static let agentSyncEnable = "agent.sync.enable"
    public static let agentSyncDisable = "agent.sync.disable"
    public static let agentSyncNow = "agent.sync.now"
    /// `SyncLocalOnlyParams` → `SyncStatusDto`：把同步目录键设为本设备专属 / 恢复同步。
    public static let agentSyncSetLocalOnly = "agent.sync.setLocalOnly"
    /// FluxCloud 服务地址读取；正式构建 `editable=false`，地址恒为构建期固定值。
    public static let agentCloudEndpointGet = "agent.cloud.endpointGet"
    /// 仅调试构建可用：覆盖/恢复 FluxCloud 服务地址，立即生效。
    public static let agentCloudEndpointSet = "agent.cloud.endpointSet"
    public static let agentRemoteList = "agent.remote.list"
    public static let agentRemoteDispatch = "agent.remote.dispatch"
    public static let agentRemoteCommand = "agent.remote.command"
    /// 局域网直连（L1）：展示本机配对码并开始广播 → `LinkPairingCodeDto`。
    public static let agentLinkPairingCode = "agent.link.pairingCode"
    /// 停止展示配对码 / 停止配对广播。
    public static let agentLinkStopPairing = "agent.link.stopPairing"
    /// `LinkDiscoveryParams`：开关 mDNS 发现；结果经 `LinkDiscoveredChanged` 推送。
    public static let agentLinkDiscoverySet = "agent.link.discovery.set"
    /// `LinkAddressParams` → `LinkDiscoveredPeer`：手动地址探测。
    public static let agentLinkProbe = "agent.link.probe"
    /// `LinkPairBeginParams` → `LinkPairBeginResponse`。
    public static let agentLinkPairBegin = "agent.link.pairBegin"
    /// `LinkPairFinishParams` → `LinkPairFinishResponse`。
    public static let agentLinkPairFinish = "agent.link.pairFinish"
    /// `LinkApproveParams`：响应端确认 / 拒绝入站配对请求。
    public static let agentLinkApprove = "agent.link.approve"
    /// `LinkDeviceParams`：解除配对。
    public static let agentLinkRemove = "agent.link.remove"
    /// `LinkRenameParams` → `LinkDeviceInfo`：设置 / 清除本机备注名。
    public static let agentLinkRename = "agent.link.rename"
    /// 探测全部已配对设备在线状态 → `Vec<LinkDeviceInfo>`（同时推送 `LinkedDevicesChanged`）。
    public static let agentLinkRefresh = "agent.link.refresh"
    /// `LinkDispatchParams` → `LinkDispatchResult`：下发下载到已配对设备。
    public static let agentLinkDispatch = "agent.link.dispatch"
    /// `LinkDeviceParams` → `LinkControlCredentialDto`：向本机 agent 申请对某已配对主机的设备控制凭据（仅 `controllable && !revoked`）。
    public static let agentLinkControlCredential = "agent.link.controlCredential"
    public static let agentPlanList = "agent.plan.list"
    public static let agentOrderCreate = "agent.order.create"
    public static let agentOrderGet = "agent.order.get"
    public static let agentOrderList = "agent.order.list"
    public static let agentReferralSummary = "agent.referral.summary"
    public static let agentReferralListCodes = "agent.referral.listCodes"
    public static let agentReferralCreateCode = "agent.referral.createCode"
    public static let agentReferralDeleteCode = "agent.referral.deleteCode"
    public static let agentReferralListRecords = "agent.referral.listRecords"
    public static let agentReferralValidate = "agent.referral.validate"
    public static let agentPlatformOpenTask = "agent.platform.openTask"
    public static let agentPlatformRevealTask = "agent.platform.revealTask"
    /// 把任务产物以文件写入主机系统剪贴板；仅桌面主机本机来源，移动端不使用。
    public static let agentPlatformCopyTaskFiles = "agent.platform.copyTaskFiles"
    public static let agentPlatformOpenPath = "agent.platform.openPath"
    public static let agentPlatformIntegrationGet = "agent.platform.integrationGet"
    public static let agentPlatformSetAutostart = "agent.platform.setAutostart"
    public static let agentPlatformSetFileAssociation = "agent.platform.setFileAssociation"
    public static let agentPlatformSetUrlProtocol = "agent.platform.setUrlProtocol"
    /// 系统文件管理器为文件显示的图标（PNG）；参数见 `PlatformFileIconParams`。
    public static let agentPlatformFileIcon = "agent.platform.fileIcon"
    /// 切换 / 导入桌面应用图标；仅桌面，移动端不使用。
    public static let agentPlatformSetAppIcon = "agent.platform.setAppIcon"
    /// 当前自定义桌面应用图标的 PNG；仅桌面，移动端不使用。
    public static let agentPlatformAppIconImage = "agent.platform.appIconImage"
    /// 为桌面界面进程现有窗口补设应用图标；仅桌面，移动端不使用。
    public static let agentPlatformSyncWindowIcons = "agent.platform.syncWindowIcons"
    /// 由 agent 在本机播放一次提示音偏好值；仅桌面，移动端不使用。
    public static let agentSoundPreview = "agent.sound.preview"
    public static let agentCaptureSubmit = "agent.capture.submit"
    /// 从本机 `.torrent` 文件建任务：agent 读文件、上传 daemon blob 后调用 `daemon.task.create`。
    public static let agentCaptureSubmitTorrentFile = "agent.capture.submitTorrentFile"
    public static let agentCaptureList = "agent.capture.list"
    public static let agentCaptureResolve = "agent.capture.resolve"
    /// 捕获上下文只读清单预解析，返回 `ResolvePreviewResponse`，不消费事务。
    public static let agentCapturePreview = "agent.capture.preview"
    /// 用捕获上下文与最终清单选择建组，成功后消费事务，返回 `CreateGroupResponse`。
    public static let agentCaptureCreateGroup = "agent.capture.createGroup"
    /// 从本机插件包安装：agent 读文件、上传 daemon blob 后调用 `daemon.plugin.install`。
    public static let agentPluginInstallFile = "agent.plugin.installFile"
    public static let agentDiagnosticsRun = "agent.diagnostics.run"
    public static let agentDiagnosticsRepair = "agent.diagnostics.repair"
    public static let agentDiagnosticsLogPaths = "agent.diagnostics.logPaths"
    public static let agentDiagnosticsExportLogs = "agent.diagnostics.exportLogs"
    public static let agentUpdateCheck = "agent.update.check"
    public static let agentUpdateDownload = "agent.update.download"
    public static let agentUpdateInstall = "agent.update.install"
    public static let agentUpdateCancel = "agent.update.cancel"
    /// 完成后关机：`{delaySecs}`；无活跃任务时拒绝（`InvalidArgument`）。
    public static let agentPowerArm = "agent.power.arm"
    public static let agentPowerDisarm = "agent.power.disarm"
    public static let agentCloudNotifyGet = "agent.cloudNotify.get"
    public static let agentCloudNotifyRefresh = "agent.cloudNotify.refresh"
    public static let agentCloudNotifySetReporting = "agent.cloudNotify.setReporting"
    public static let agentCloudNotifySetPrivacy = "agent.cloudNotify.setPrivacy"
    public static let agentCloudNotifyCreateChannel = "agent.cloudNotify.createChannel"
    public static let agentCloudNotifyUpdateChannel = "agent.cloudNotify.updateChannel"
    public static let agentCloudNotifyDeleteChannel = "agent.cloudNotify.deleteChannel"
    public static let agentCloudNotifyTestChannel = "agent.cloudNotify.testChannel"
    public static let agentCloudNotifyTelegramBindStart = "agent.cloudNotify.telegramBindStart"
    public static let agentCloudNotifyTelegramBindStatus = "agent.cloudNotify.telegramBindStatus"
    public static let agentCloudNotifyDeliveries = "agent.cloudNotify.deliveries"
    public static let agentCloudNotifySendEmailCode = "agent.cloudNotify.sendEmailCode"
    public static let agentCloudNotifyVerifyEmail = "agent.cloudNotify.verifyEmail"
    public static let serviceEvent = "service.event"
}

/// 主机握手声明的能力标识（`HostState.has(_:)` / `HostInfo.has(_:)` 的参数）。
public enum HostCapability {
    public static let daemonTasks = "daemon.tasks"
    public static let daemonQueues = "daemon.queues"
    public static let daemonGroups = "daemon.groups"
    public static let daemonConfig = "daemon.config"
    public static let daemonRss = "daemon.rss"
    public static let daemonPlugins = "daemon.plugins"
    public static let daemonComponents = "daemon.components"
    public static let daemonWebhooks = "daemon.webhooks"
    public static let daemonSelections = "daemon.selections"
    public static let daemonFiles = "daemon.files"
    public static let agentGateway = "agent.gateway"
    public static let agentAuth = "agent.auth"
    public static let agentSync = "agent.sync"
    public static let agentRemoteTasks = "agent.remoteTasks"
    public static let agentBilling = "agent.billing"
    public static let agentReferrals = "agent.referrals"
    public static let agentDeviceLink = "agent.deviceLink"
    public static let agentExternalCapture = "agent.externalCapture"
    /// 按连接下发：本连接可在 agent 所在主机上打开 / 定位任务产物（`agent.platform.openTask` / `revealTask`）。
    public static let agentOpenTaskFiles = "agent.openTaskFiles"
    /// 按连接下发：本连接可把任务产物写入 agent 所在主机的系统剪贴板（`agent.platform.copyTaskFiles`）。
    public static let agentCopyTaskFiles = "agent.copyTaskFiles"
    public static let clientSelections = "client.selections"
}
