package com.fluxdown.core.protocol

/*
 * 由 `native/protocol/src/method.rs` 一一生成（同 `web/src/lib/rpc/protocol/method.ts` 与
 * iOS `FluxDomain/Protocol/Methods.swift`）：常量名 = Rust 常量名的 lowerCamelCase，值 = wire 方法名。
 * `HostSession.call` 只放行 `daemon.*` / `agent.*`；`system.*` 由 Rust 会话层内部处理，列出仅为一一对应。
 */

/** wire 方法名。 */
object HostMethod {
    const val systemHello = "system.hello"
    const val systemPing = "system.ping"
    const val systemSnapshot = "system.snapshot"
    /** 任务表 / 队列位置表分页（协议 v17）：`{section:"tasks"|"queue", cursor?, limit?}`。分页载入由 Rust 会话完成，Kotlin 不直接调用。 */
    const val systemTaskPage = "system.taskPage"
    /** 让服务优雅退出：daemon 关停引擎后退出；agent 先关停 daemon 再退出（完全退出）。允许作为连接首帧（握手前）调用。 */
    const val systemShutdown = "system.shutdown"
    const val daemonTaskList = "daemon.task.list"
    const val daemonTaskGet = "daemon.task.get"
    const val daemonTaskActivity = "daemon.task.activity"
    /** 任务全生命周期速度曲线与统计（`TaskSpeedHistoryQuery` → `TaskSpeedHistoryDto | null`）；旧主机无此方法。 */
    const val daemonTaskSpeedHistory = "daemon.task.speedHistory"
    const val daemonTaskCreate = "daemon.task.create"
    const val daemonTaskPause = "daemon.task.pause"
    const val daemonTaskResume = "daemon.task.resume"
    const val daemonTaskRename = "daemon.task.rename"
    /** 更换任务下载源地址（`{taskId, url}`，语义同旧 `PUT /api/v1/tasks/{id}/url`）。 */
    const val daemonTaskChangeUrl = "daemon.task.changeUrl"
    /** 修改任务线程数（`SetTaskSegmentsParams`），已下进度保留；完成或不存在的任务返回 `not-found`。 */
    const val daemonTaskSetSegments = "daemon.task.setSegments"
    /** 切换 BT 选片（`SetTaskPiecePickingParams`）：省略的字段保持不变；非 BT 或不存在返回 `not-found`。 */
    const val daemonTaskSetPiecePicking = "daemon.task.setPiecePicking"
    const val daemonTaskDelete = "daemon.task.delete"
    const val daemonTaskPauseAll = "daemon.task.pauseAll"
    const val daemonTaskResumeAll = "daemon.task.resumeAll"
    const val daemonTaskRescan = "daemon.task.rescan"
    const val daemonTaskSetSeedLimits = "daemon.task.setSeedLimits"
    const val daemonTaskVerify = "daemon.task.verify"
    /** 已完成任务落盘文件的哈希（`{taskId, algorithm}` → `{fileHash}`），结果同时记入任务 `fileHash`。 */
    const val daemonTaskHash = "daemon.task.hash"
    /** eD2K 任务实况（`{taskId}` → `Ed2kStatusDto`）；非 eD2K 任务返回 `invalidArgument`。移动端暂不消费，列出仅为一一对应。 */
    const val daemonTaskEd2kStatus = "daemon.task.ed2kStatus"
    /** 批量暂停 / 恢复（`{taskIds}`）：整批只推一次任务快照；未知 id 忽略，空列表为空操作。 */
    const val daemonTaskPauseMany = "daemon.task.pauseMany"
    const val daemonTaskResumeMany = "daemon.task.resumeMany"
    /** 批量删除（`{taskIds, deleteFiles}`）：语义同批量暂停。 */
    const val daemonTaskDeleteMany = "daemon.task.deleteMany"
    /** 批量建任务（`DaemonCreateTasksParams`）：整块一个事务、一次 `tasksChanged` 事件，按 `batchId` 幂等。 */
    const val daemonTaskCreateMany = "daemon.task.createMany"
    const val daemonQueueList = "daemon.queue.list"
    const val daemonQueueCreate = "daemon.queue.create"
    const val daemonQueueUpdate = "daemon.queue.update"
    const val daemonQueueDelete = "daemon.queue.delete"
    const val daemonQueueStart = "daemon.queue.start"
    const val daemonQueueStop = "daemon.queue.stop"
    const val daemonQueueSchedule = "daemon.queue.schedule"
    const val daemonQueueReorder = "daemon.queue.reorder"
    const val daemonQueueMoveTask = "daemon.queue.moveTask"
    const val daemonQueueBoost = "daemon.queue.boost"
    const val daemonGroupList = "daemon.group.list"
    const val daemonGroupResolvePreview = "daemon.group.resolvePreview"
    const val daemonGroupCreate = "daemon.group.create"
    const val daemonGroupPause = "daemon.group.pause"
    const val daemonGroupResume = "daemon.group.resume"
    const val daemonGroupDelete = "daemon.group.delete"
    const val daemonConfigGet = "daemon.config.get"
    const val daemonConfigPatch = "daemon.config.patch"
    const val daemonConfigProxyTest = "daemon.config.proxyTest"
    const val daemonConfigProxyRoute = "daemon.config.proxyRoute"
    const val daemonConfigConnPolicy = "daemon.config.connPolicy"
    const val daemonConfigClearConnPolicy = "daemon.config.clearConnPolicy"
    const val daemonConfigSystemProxy = "daemon.config.systemProxy"
    const val daemonSiteAuthList = "daemon.siteAuth.list"
    const val daemonSiteAuthDelete = "daemon.siteAuth.delete"
    /** 读取单站点凭据详情（含明文密码；站点键会先按 `host` / `host:port` 归一化）。 */
    const val daemonSiteAuthGet = "daemon.siteAuth.get"
    /** 保存单站点 HTTP Basic 凭据，返回脱敏后的 `SiteAuthEntryDto`。 */
    const val daemonSiteAuthSave = "daemon.siteAuth.save"
    const val daemonSiteAuthClear = "daemon.siteAuth.clear"
    /** 按下载链接匹配已保存的站点凭据（含明文密码，仅供本机官方 UI 表单回填）。 */
    const val daemonSiteAuthMatch = "daemon.siteAuth.match"
    const val daemonRuntimeStats = "daemon.runtime.stats"
    const val daemonFsList = "daemon.fs.list"
    const val daemonRssListSources = "daemon.rss.listSources"
    const val daemonRssGetItems = "daemon.rss.getItems"
    const val daemonRssCreateSource = "daemon.rss.createSource"
    const val daemonRssUpdateSource = "daemon.rss.updateSource"
    const val daemonRssDeleteSource = "daemon.rss.deleteSource"
    const val daemonRssRefreshSource = "daemon.rss.refreshSource"
    const val daemonRssItemAction = "daemon.rss.itemAction"
    const val daemonRssValidate = "daemon.rss.validate"
    const val daemonLinkProbe = "daemon.link.probe"
    const val daemonPluginList = "daemon.plugin.list"
    const val daemonPluginSniffRules = "daemon.plugin.sniffRules"
    const val daemonPluginPageScripts = "daemon.plugin.pageScripts"
    const val daemonPluginPageScriptSource = "daemon.plugin.pageScriptSource"
    const val daemonPluginAuth = "daemon.plugin.auth"
    const val daemonPluginSetEnabled = "daemon.plugin.setEnabled"
    const val daemonPluginUpdateSettings = "daemon.plugin.updateSettings"
    const val daemonPluginInstall = "daemon.plugin.install"
    const val daemonPluginInstallDev = "daemon.plugin.installDev"
    /** 重新加载 dev 插件（`{identity}`）：重读 manifest + 源码并校验，失败保留登记。 */
    const val daemonPluginReloadDev = "daemon.plugin.reloadDev"
    const val daemonPluginUninstall = "daemon.plugin.uninstall"
    const val daemonPluginMarketList = "daemon.plugin.marketList"
    const val daemonPluginMarketInstall = "daemon.plugin.marketInstall"
    const val daemonPluginIgnoreRetry = "daemon.plugin.ignoreRetry"
    const val daemonPluginSiteDefaults = "daemon.plugin.siteDefaults"
    const val daemonPluginSetSiteDefault = "daemon.plugin.setSiteDefault"
    const val daemonPluginConsent = "daemon.plugin.consent"
    const val daemonPluginAuditList = "daemon.plugin.audit.list"
    const val daemonPluginAuditClear = "daemon.plugin.audit.clear"
    const val daemonPluginAuditRecord = "daemon.plugin.audit.record"
    const val daemonPluginViewOpen = "daemon.plugin.view.open"
    const val daemonPluginViewEvent = "daemon.plugin.view.event"
    const val daemonPluginViewClose = "daemon.plugin.view.close"
    const val daemonPluginViewSnapshot = "daemon.plugin.view.snapshot"
    const val daemonPluginViewKeepalive = "daemon.plugin.view.keepalive"
    const val daemonPluginCommand = "daemon.plugin.command"
    const val daemonPluginInvoke = "daemon.plugin.invoke"
    const val daemonPluginPageInvoke = "daemon.plugin.pageInvoke"
    const val daemonComponentGet = "daemon.component.get"
    const val daemonComponentListVersions = "daemon.component.listVersions"
    const val daemonComponentInstall = "daemon.component.install"
    const val daemonComponentUninstall = "daemon.component.uninstall"
    const val daemonSelectionSubscribe = "daemon.selection.subscribe"
    const val daemonSelectionUnsubscribe = "daemon.selection.unsubscribe"
    const val daemonSelectionResolve = "daemon.selection.resolve"
    const val daemonWebhookGet = "daemon.webhook.get"
    const val daemonWebhookClearDeliveries = "daemon.webhook.clearDeliveries"
    const val daemonWebhookSimulate = "daemon.webhook.simulate"
    const val daemonWebhookTest = "daemon.webhook.test"
    const val daemonCdnReportsPeek = "daemon.cdnReports.peek"
    const val daemonCdnReportsAck = "daemon.cdnReports.ack"
    const val daemonCdnConfigApply = "daemon.cdnConfig.apply"
    const val daemonEgressPlan = "daemon.egress.plan"
    const val daemonBtTrackerSubscriptionRefresh = "daemon.bt.trackerSubscription.refresh"
    const val daemonBtGuardState = "daemon.bt.guard.state"
    const val daemonBtGuardEntriesList = "daemon.bt.guard.entries.list"
    const val daemonBtGuardEntriesAdd = "daemon.bt.guard.entries.add"
    const val daemonBtGuardEntriesRemove = "daemon.bt.guard.entries.remove"
    const val daemonBtGuardEventsList = "daemon.bt.guard.events.list"
    const val daemonBtGuardExport = "daemon.bt.guard.export"
    const val daemonBtGuardFilterSourcesRefresh = "daemon.bt.guard.filterSources.refresh"
    /** 仅 agent 内部调用（已验签云名单整层替换）；UI 不得使用，原生桥通用通道会拒绝。 */
    const val daemonBtGuardCloudApply = "daemon.bt.guard.cloud.apply"
    /** 仅 agent 内部调用（弹出待上报证据）；UI 不得使用，原生桥通用通道会拒绝。 */
    const val daemonBtGuardEvidenceDrain = "daemon.bt.guard.evidence.drain"
    const val daemonEd2kServerSubscriptionRefresh = "daemon.ed2k.serverSubscription.refresh"
    /** 立即刷新 Kad 节点（nodes.dat）订阅（无参 → `Ed2kNodesSubRefreshResponse`）。慢方法。 */
    const val daemonEd2kNodesSubscriptionRefresh = "daemon.ed2k.nodesSubscription.refresh"
    /** HighID / LowID 检测（无参 → `Ed2kNetworkDto`）。慢方法（可达 ~30 s）。 */
    const val daemonEd2kIdCheck = "daemon.ed2k.idCheck"
    const val daemonDiagnosticsDescribe = "daemon.diagnostics.describe"
    const val daemonDiagnosticsPrepareLogExport = "daemon.diagnostics.prepareLogExport"
    /** Doctor 动态探测：在 daemon 进程内真实写入各下载目录 / 数据目录并运行外部组件， 验证下载链路实际拥有的权限（agent 只读配置判断不出 TCC、只读挂载、隔离属性等）。 */
    const val daemonDiagnosticsProbe = "daemon.diagnostics.probe"
    /** Doctor 修复：补上托管组件（daemon 数据目录内）缺失的执行权限并重新探测，返回 `ComponentProbeDto`。 */
    const val daemonDiagnosticsFixComponent = "daemon.diagnostics.fixComponent"
    const val daemonMigrationLinkExport = "daemon.migration.linkExport"
    const val daemonMigrationLinkAck = "daemon.migration.linkAck"
    const val daemonMigrationGatewayExport = "daemon.migration.gatewayExport"
    const val daemonMigrationGatewayAck = "daemon.migration.gatewayAck"
    const val agentSessionGet = "agent.session.get"
    const val agentAuthRegister = "agent.auth.register"
    const val agentAuthRegisterVerify = "agent.auth.registerVerify"
    const val agentAuthLogin = "agent.auth.login"
    const val agentAuthLoginVerify = "agent.auth.loginVerify"
    const val agentAuthSendCode = "agent.auth.sendCode"
    const val agentAuthVerifyCode = "agent.auth.verifyCode"
    const val agentAuthLogout = "agent.auth.logout"
    const val agentAuthRefreshProfile = "agent.auth.refreshProfile"
    const val agentAuthSendPasswordResetCode = "agent.auth.sendPasswordResetCode"
    const val agentAuthResetPassword = "agent.auth.resetPassword"
    const val agentProfileSendEmailCode = "agent.profile.sendEmailCode"
    const val agentProfileSendNewEmailCode = "agent.profile.sendNewEmailCode"
    const val agentProfileChangeEmail = "agent.profile.changeEmail"
    const val agentProfileRandomOriginId = "agent.profile.randomOriginId"
    const val agentProfileCheckOriginId = "agent.profile.checkOriginId"
    const val agentProfileChangeOriginId = "agent.profile.changeOriginId"
    const val agentProfileChangeNickname = "agent.profile.changeNickname"
    const val agentProfileSendPasswordCode = "agent.profile.sendPasswordCode"
    const val agentProfileChangePassword = "agent.profile.changePassword"
    const val agentProfileSendDeleteCode = "agent.profile.sendDeleteCode"
    const val agentProfileDeleteAccount = "agent.profile.deleteAccount"
    const val agentGatewayGet = "agent.gateway.get"
    const val agentGatewayPatch = "agent.gateway.patch"
    /** 仅供本机官方 UI 展示/复制用户 token；结果 `{ "userToken": "..." }`（未配置为空串）。 */
    const val agentGatewayRevealToken = "agent.gateway.revealToken"
    const val agentDeviceList = "agent.device.list"
    /** 请求立即重连任务 SSE；返回 `{ "accepted": true }` 不表示已经连接成功。 */
    const val agentRemoteReconnect = "agent.remote.reconnect"
    const val agentDeviceRename = "agent.device.rename"
    const val agentDeviceDelete = "agent.device.delete"
    const val agentDeviceDisconnect = "agent.device.disconnect"
    const val agentPreferencesPatch = "agent.preferences.patch"
    const val agentSyncGet = "agent.sync.get"
    const val agentSyncEnable = "agent.sync.enable"
    const val agentSyncDisable = "agent.sync.disable"
    const val agentSyncNow = "agent.sync.now"
    /** `SyncLocalOnlyParams` → `SyncStatusDto`：把同步目录键设为本设备专属 / 恢复同步。 */
    const val agentSyncSetLocalOnly = "agent.sync.setLocalOnly"
    /** FluxCloud 服务地址读取；正式构建 `editable=false`，地址恒为构建期固定值。 */
    const val agentCloudEndpointGet = "agent.cloud.endpointGet"
    /** 仅调试构建可用：覆盖/恢复 FluxCloud 服务地址，立即生效。 */
    const val agentCloudEndpointSet = "agent.cloud.endpointSet"
    const val agentRemoteList = "agent.remote.list"
    const val agentRemoteDispatch = "agent.remote.dispatch"
    const val agentRemoteCommand = "agent.remote.command"
    /** 局域网直连（L1）：展示本机配对码并开始广播 → `LinkPairingCodeDto`。 */
    const val agentLinkPairingCode = "agent.link.pairingCode"
    /** 停止展示配对码 / 停止配对广播。 */
    const val agentLinkStopPairing = "agent.link.stopPairing"
    /** `LinkDiscoveryParams`：开关 mDNS 发现；结果经 `LinkDiscoveredChanged` 推送。 */
    const val agentLinkDiscoverySet = "agent.link.discovery.set"
    /** `LinkAddressParams` → `LinkDiscoveredPeer`：手动地址探测。 */
    const val agentLinkProbe = "agent.link.probe"
    /** `LinkPairBeginParams` → `LinkPairBeginResponse`。 */
    const val agentLinkPairBegin = "agent.link.pairBegin"
    /** `LinkPairFinishParams` → `LinkPairFinishResponse`。 */
    const val agentLinkPairFinish = "agent.link.pairFinish"
    /** `LinkApproveParams`：响应端确认 / 拒绝入站配对请求。 */
    const val agentLinkApprove = "agent.link.approve"
    /** `LinkDeviceParams`：解除配对。 */
    const val agentLinkRemove = "agent.link.remove"
    /** `LinkRenameParams` → `LinkDeviceInfo`：设置 / 清除已配对设备的本机备注名（省略 / 空白 = 清除）。 */
    const val agentLinkRename = "agent.link.rename"
    /** 探测全部已配对设备在线状态 → `Vec<LinkDeviceInfo>`（同时推送 `LinkedDevicesChanged`）。 */
    const val agentLinkRefresh = "agent.link.refresh"
    /** `LinkDispatchParams` → `LinkDispatchResult`：下发下载到已配对设备。 */
    const val agentLinkDispatch = "agent.link.dispatch"
    /** `LinkDeviceParams` → `LinkControlCredentialDto`：本机对已配对且可控制的主机签发设备专属控制凭据（仅发起方）。 */
    const val agentLinkControlCredential = "agent.link.controlCredential"
    const val agentPlanList = "agent.plan.list"
    const val agentOrderCreate = "agent.order.create"
    const val agentOrderGet = "agent.order.get"
    const val agentOrderList = "agent.order.list"
    const val agentReferralSummary = "agent.referral.summary"
    const val agentReferralListCodes = "agent.referral.listCodes"
    const val agentReferralCreateCode = "agent.referral.createCode"
    const val agentReferralDeleteCode = "agent.referral.deleteCode"
    const val agentReferralListRecords = "agent.referral.listRecords"
    const val agentReferralValidate = "agent.referral.validate"
    const val agentPlatformOpenTask = "agent.platform.openTask"
    const val agentPlatformRevealTask = "agent.platform.revealTask"
    /** 把任务产物以文件写入主机系统剪贴板；仅桌面主机本机来源，移动端不使用。 */
    const val agentPlatformCopyTaskFiles = "agent.platform.copyTaskFiles"
    const val agentPlatformOpenPath = "agent.platform.openPath"
    const val agentPlatformIntegrationGet = "agent.platform.integrationGet"
    const val agentPlatformSetAutostart = "agent.platform.setAutostart"
    const val agentPlatformSetFileAssociation = "agent.platform.setFileAssociation"
    const val agentPlatformSetUrlProtocol = "agent.platform.setUrlProtocol"
    /** 系统文件管理器为文件显示的图标（PNG）；参数见 `PlatformFileIconParams`。 */
    const val agentPlatformFileIcon = "agent.platform.fileIcon"
    /** 切换 / 导入桌面应用图标；仅桌面，移动端不使用。 */
    const val agentPlatformSetAppIcon = "agent.platform.setAppIcon"
    /** 当前自定义桌面应用图标的 PNG；仅桌面，移动端不使用。 */
    const val agentPlatformAppIconImage = "agent.platform.appIconImage"
    /** 为桌面界面进程现有窗口补设应用图标；仅桌面，移动端不使用。 */
    const val agentPlatformSyncWindowIcons = "agent.platform.syncWindowIcons"
    /** 由 agent 在本机播放一次提示音偏好值；仅桌面，移动端不使用。 */
    const val agentSoundPreview = "agent.sound.preview"
    const val agentCaptureSubmit = "agent.capture.submit"
    /** 从本机 `.torrent` 文件建任务：agent 读文件、上传 daemon blob 后调用 `daemon.task.create`。 */
    const val agentCaptureSubmitTorrentFile = "agent.capture.submitTorrentFile"
    const val agentCaptureList = "agent.capture.list"
    const val agentCaptureResolve = "agent.capture.resolve"
    /** 捕获上下文只读清单预解析，返回 `ResolvePreviewResponse`，不消费事务。 */
    const val agentCapturePreview = "agent.capture.preview"
    /** 用捕获上下文与最终清单选择建组，成功后消费事务，返回 `CreateGroupResponse`。 */
    const val agentCaptureCreateGroup = "agent.capture.createGroup"
    /** 从本机插件包安装：agent 读文件、上传 daemon blob 后调用 `daemon.plugin.install`。 */
    const val agentPluginInstallFile = "agent.plugin.installFile"
    const val agentDiagnosticsRun = "agent.diagnostics.run"
    const val agentDiagnosticsRepair = "agent.diagnostics.repair"
    const val agentDiagnosticsLogPaths = "agent.diagnostics.logPaths"
    const val agentDiagnosticsExportLogs = "agent.diagnostics.exportLogs"
    const val agentUpdateCheck = "agent.update.check"
    const val agentUpdateDownload = "agent.update.download"
    const val agentUpdateInstall = "agent.update.install"
    const val agentUpdateCancel = "agent.update.cancel"
    /** 定时关机 / 睡眠：`{delaySecs, action}` → `{armed}`；没有任务时立即倒计时，`delaySecs = 0` 仅有任务时受理。 */
    const val agentPowerArm = "agent.power.arm"
    const val agentPowerDisarm = "agent.power.disarm"
    const val agentCloudNotifyGet = "agent.cloudNotify.get"
    const val agentCloudNotifyRefresh = "agent.cloudNotify.refresh"
    const val agentCloudNotifySetReporting = "agent.cloudNotify.setReporting"
    const val agentCloudNotifySetPrivacy = "agent.cloudNotify.setPrivacy"
    const val agentCloudNotifyCreateChannel = "agent.cloudNotify.createChannel"
    const val agentCloudNotifyUpdateChannel = "agent.cloudNotify.updateChannel"
    const val agentCloudNotifyDeleteChannel = "agent.cloudNotify.deleteChannel"
    const val agentCloudNotifyTestChannel = "agent.cloudNotify.testChannel"
    const val agentCloudNotifyTelegramBindStart = "agent.cloudNotify.telegramBindStart"
    const val agentCloudNotifyTelegramBindStatus = "agent.cloudNotify.telegramBindStatus"
    const val agentCloudNotifyDeliveries = "agent.cloudNotify.deliveries"
    const val agentCloudNotifySendEmailCode = "agent.cloudNotify.sendEmailCode"
    const val agentCloudNotifyVerifyEmail = "agent.cloudNotify.verifyEmail"
    const val serviceEvent = "service.event"
}

/** 主机握手声明的能力标识（`HostInfo.capabilities` 的成员）。 */
object HostCapability {
    const val daemonTasks = "daemon.tasks"
    const val daemonQueues = "daemon.queues"
    const val daemonGroups = "daemon.groups"
    const val daemonConfig = "daemon.config"
    const val daemonRss = "daemon.rss"
    const val daemonPlugins = "daemon.plugins"
    const val daemonComponents = "daemon.components"
    const val daemonWebhooks = "daemon.webhooks"
    const val daemonSelections = "daemon.selections"
    const val daemonFiles = "daemon.files"
    const val agentGateway = "agent.gateway"
    const val agentAuth = "agent.auth"
    const val agentSync = "agent.sync"
    const val agentRemoteTasks = "agent.remoteTasks"
    const val agentBilling = "agent.billing"
    const val agentReferrals = "agent.referrals"
    const val agentDeviceLink = "agent.deviceLink"
    const val agentExternalCapture = "agent.externalCapture"
    /** 按连接下发：本连接可在 agent 所在主机上打开 / 定位任务产物（`agent.platform.openTask` / `revealTask`）。 */
    const val agentOpenTaskFiles = "agent.openTaskFiles"
    /** 按连接下发：本连接可把任务产物写入 agent 所在主机的系统剪贴板（`agent.platform.copyTaskFiles`）。 */
    const val agentCopyTaskFiles = "agent.copyTaskFiles"
    const val clientSelections = "client.selections"
}
