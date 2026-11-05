// 方法名与能力标识（native/protocol/src/method.rs）。

/** wire 方法名；键与 Rust 常量名一致。 */
export const METHOD = {
  SYSTEM_HELLO: 'system.hello',
  SYSTEM_PING: 'system.ping',
  SYSTEM_SNAPSHOT: 'system.snapshot',
  /** 任务表 / 队列位置表分页（快照头带 `taskTotal` / `queueTotal` 时用它载入）。 */
  SYSTEM_TASK_PAGE: 'system.taskPage',
  /** 允许作为连接首帧（握手前）调用。 */
  SYSTEM_SHUTDOWN: 'system.shutdown',

  DAEMON_TASK_LIST: 'daemon.task.list',
  DAEMON_TASK_GET: 'daemon.task.get',
  DAEMON_TASK_ACTIVITY: 'daemon.task.activity',
  DAEMON_TASK_SPEED_HISTORY: 'daemon.task.speedHistory',
  /** BT 任务实况（`TaskIdParams` → `BtStatusDto`）：详情页 Tracker / 连接标签轮询；非 BT 任务 `invalidArgument`。 */
  DAEMON_TASK_BT_STATUS: 'daemon.task.btStatus',
  DAEMON_TASK_CREATE: 'daemon.task.create',
  /** 批量建任务（`DaemonCreateTasksParams`）：整块一个事务、一次 `tasksChanged` 事件，按 `batchId` 幂等。 */
  DAEMON_TASK_CREATE_MANY: 'daemon.task.createMany',
  DAEMON_TASK_PAUSE: 'daemon.task.pause',
  DAEMON_TASK_RESUME: 'daemon.task.resume',
  DAEMON_TASK_RENAME: 'daemon.task.rename',
  DAEMON_TASK_CHANGE_URL: 'daemon.task.changeUrl',
  /** 修改任务线程数（`{taskId, segments}`，≤0 = 自动），已下进度保留；完成或不存在的任务返回 not-found。 */
  DAEMON_TASK_SET_SEGMENTS: 'daemon.task.setSegments',
  DAEMON_TASK_DELETE: 'daemon.task.delete',
  DAEMON_TASK_PAUSE_ALL: 'daemon.task.pauseAll',
  DAEMON_TASK_RESUME_ALL: 'daemon.task.resumeAll',
  DAEMON_TASK_RESCAN: 'daemon.task.rescan',
  DAEMON_TASK_SET_SEED_LIMITS: 'daemon.task.setSeedLimits',
  DAEMON_TASK_VERIFY: 'daemon.task.verify',
  /** 已完成任务落盘文件的哈希（`DaemonTaskHashParams` → `TaskHashResult`）。慢方法。 */
  DAEMON_TASK_HASH: 'daemon.task.hash',
  DAEMON_TASK_SEED_LOCAL: 'daemon.task.seedLocal',
  DAEMON_TASK_TORRENT_SHARE: 'daemon.task.torrentShare',
  DAEMON_TASK_PAUSE_MANY: 'daemon.task.pauseMany',
  DAEMON_TASK_RESUME_MANY: 'daemon.task.resumeMany',
  DAEMON_TASK_DELETE_MANY: 'daemon.task.deleteMany',

  DAEMON_QUEUE_LIST: 'daemon.queue.list',
  DAEMON_QUEUE_CREATE: 'daemon.queue.create',
  DAEMON_QUEUE_UPDATE: 'daemon.queue.update',
  DAEMON_QUEUE_DELETE: 'daemon.queue.delete',
  DAEMON_QUEUE_START: 'daemon.queue.start',
  DAEMON_QUEUE_STOP: 'daemon.queue.stop',
  DAEMON_QUEUE_SCHEDULE: 'daemon.queue.schedule',
  DAEMON_QUEUE_REORDER: 'daemon.queue.reorder',
  DAEMON_QUEUE_MOVE_TASK: 'daemon.queue.moveTask',
  DAEMON_QUEUE_BOOST: 'daemon.queue.boost',

  DAEMON_GROUP_LIST: 'daemon.group.list',
  DAEMON_GROUP_RESOLVE_PREVIEW: 'daemon.group.resolvePreview',
  DAEMON_GROUP_CREATE: 'daemon.group.create',
  DAEMON_GROUP_PAUSE: 'daemon.group.pause',
  DAEMON_GROUP_RESUME: 'daemon.group.resume',
  DAEMON_GROUP_DELETE: 'daemon.group.delete',

  DAEMON_CONFIG_GET: 'daemon.config.get',
  DAEMON_CONFIG_PATCH: 'daemon.config.patch',
  DAEMON_CONFIG_PROXY_TEST: 'daemon.config.proxyTest',
  DAEMON_CONFIG_PROXY_ROUTE: 'daemon.config.proxyRoute',
  DAEMON_CONFIG_CONN_POLICY: 'daemon.config.connPolicy',
  DAEMON_CONFIG_CLEAR_CONN_POLICY: 'daemon.config.clearConnPolicy',
  DAEMON_CONFIG_SYSTEM_PROXY: 'daemon.config.systemProxy',
  DAEMON_SITE_AUTH_LIST: 'daemon.siteAuth.list',
  DAEMON_SITE_AUTH_DELETE: 'daemon.siteAuth.delete',
  /** 结果含明文密码；`site` 服务端会归一化。 */
  DAEMON_SITE_AUTH_GET: 'daemon.siteAuth.get',
  DAEMON_SITE_AUTH_SAVE: 'daemon.siteAuth.save',
  DAEMON_SITE_AUTH_CLEAR: 'daemon.siteAuth.clear',
  /** 结果含明文密码，仅供本机官方 UI 表单回填。 */
  DAEMON_SITE_AUTH_MATCH: 'daemon.siteAuth.match',
  DAEMON_RUNTIME_STATS: 'daemon.runtime.stats',
  DAEMON_FS_LIST: 'daemon.fs.list',

  DAEMON_RSS_LIST_SOURCES: 'daemon.rss.listSources',
  DAEMON_RSS_GET_ITEMS: 'daemon.rss.getItems',
  DAEMON_RSS_CREATE_SOURCE: 'daemon.rss.createSource',
  DAEMON_RSS_UPDATE_SOURCE: 'daemon.rss.updateSource',
  DAEMON_RSS_DELETE_SOURCE: 'daemon.rss.deleteSource',
  DAEMON_RSS_REFRESH_SOURCE: 'daemon.rss.refreshSource',
  DAEMON_RSS_ITEM_ACTION: 'daemon.rss.itemAction',
  DAEMON_RSS_VALIDATE: 'daemon.rss.validate',
  DAEMON_LINK_PROBE: 'daemon.link.probe',

  DAEMON_PLUGIN_LIST: 'daemon.plugin.list',
  DAEMON_PLUGIN_ARCHIVE_RULES: 'daemon.plugin.archiveRules',
  DAEMON_PLUGIN_SNIFF_RULES: 'daemon.plugin.sniffRules',
  DAEMON_PLUGIN_PAGE_SCRIPTS: 'daemon.plugin.pageScripts',
  DAEMON_PLUGIN_PAGE_SCRIPT_SOURCE: 'daemon.plugin.pageScriptSource',
  DAEMON_PLUGIN_AUTH: 'daemon.plugin.auth',
  DAEMON_PLUGIN_SET_ENABLED: 'daemon.plugin.setEnabled',
  DAEMON_PLUGIN_UPDATE_SETTINGS: 'daemon.plugin.updateSettings',
  DAEMON_PLUGIN_INSTALL: 'daemon.plugin.install',
  DAEMON_PLUGIN_INSTALL_DEV: 'daemon.plugin.installDev',
  DAEMON_PLUGIN_RELOAD_DEV: 'daemon.plugin.reloadDev',
  DAEMON_PLUGIN_UNINSTALL: 'daemon.plugin.uninstall',
  DAEMON_PLUGIN_MARKET_LIST: 'daemon.plugin.marketList',
  DAEMON_PLUGIN_MARKET_INSTALL: 'daemon.plugin.marketInstall',
  // 以下两条 agent 专用边界方法，Web 不使用：仅为与 ALL_METHODS 对齐而保留。
  DAEMON_PLUGIN_INSTALL_PRIVATE: 'daemon.plugin.installPrivate',
  DAEMON_PLUGIN_REVOKE_PRIVATE: 'daemon.plugin.revokePrivate',
  DAEMON_PLUGIN_IGNORE_RETRY: 'daemon.plugin.ignoreRetry',
  DAEMON_PLUGIN_SITE_DEFAULTS: 'daemon.plugin.siteDefaults',
  DAEMON_PLUGIN_SET_SITE_DEFAULT: 'daemon.plugin.setSiteDefault',
  DAEMON_PLUGIN_CONSENT: 'daemon.plugin.consent',
  DAEMON_PLUGIN_AUDIT_LIST: 'daemon.plugin.audit.list',
  DAEMON_PLUGIN_AUDIT_CLEAR: 'daemon.plugin.audit.clear',
  DAEMON_PLUGIN_AUDIT_RECORD: 'daemon.plugin.audit.record',
  DAEMON_PLUGIN_VIEW_OPEN: 'daemon.plugin.view.open',
  DAEMON_PLUGIN_VIEW_EVENT: 'daemon.plugin.view.event',
  DAEMON_PLUGIN_VIEW_CLOSE: 'daemon.plugin.view.close',
  DAEMON_PLUGIN_VIEW_SNAPSHOT: 'daemon.plugin.view.snapshot',
  DAEMON_PLUGIN_VIEW_KEEPALIVE: 'daemon.plugin.view.keepalive',
  DAEMON_PLUGIN_COMMAND: 'daemon.plugin.command',
  DAEMON_PLUGIN_INVOKE: 'daemon.plugin.invoke',
  DAEMON_PLUGIN_PAGE_INVOKE: 'daemon.plugin.pageInvoke',
  DAEMON_COMPONENT_GET: 'daemon.component.get',
  DAEMON_COMPONENT_LIST_VERSIONS: 'daemon.component.listVersions',
  DAEMON_COMPONENT_INSTALL: 'daemon.component.install',
  DAEMON_COMPONENT_UNINSTALL: 'daemon.component.uninstall',

  DAEMON_SELECTION_SUBSCRIBE: 'daemon.selection.subscribe',
  DAEMON_SELECTION_UNSUBSCRIBE: 'daemon.selection.unsubscribe',
  DAEMON_SELECTION_RESOLVE: 'daemon.selection.resolve',

  DAEMON_WEBHOOK_GET: 'daemon.webhook.get',
  DAEMON_WEBHOOK_CLEAR_DELIVERIES: 'daemon.webhook.clearDeliveries',
  DAEMON_WEBHOOK_SIMULATE: 'daemon.webhook.simulate',
  DAEMON_WEBHOOK_TEST: 'daemon.webhook.test',
  // 以下 agent 专用边界方法，Web 不使用：仅为与 ALL_METHODS 对齐而保留。
  DAEMON_CDN_REPORTS_PEEK: 'daemon.cdnReports.peek',
  DAEMON_CDN_REPORTS_ACK: 'daemon.cdnReports.ack',
  DAEMON_CDN_CONFIG_APPLY: 'daemon.cdnConfig.apply',
  DAEMON_EGRESS_PLAN: 'daemon.egress.plan',

  DAEMON_BT_TRACKER_SUBSCRIPTION_REFRESH: 'daemon.bt.trackerSubscription.refresh',
  DAEMON_BT_GUARD_STATE: 'daemon.bt.guard.state',
  DAEMON_BT_GUARD_ENTRIES_LIST: 'daemon.bt.guard.entries.list',
  DAEMON_BT_GUARD_ENTRIES_ADD: 'daemon.bt.guard.entries.add',
  DAEMON_BT_GUARD_ENTRIES_REMOVE: 'daemon.bt.guard.entries.remove',
  DAEMON_BT_GUARD_EVENTS_LIST: 'daemon.bt.guard.events.list',
  DAEMON_BT_GUARD_EXPORT: 'daemon.bt.guard.export',
  DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH: 'daemon.bt.guard.filterSources.refresh',
  // 以下 agent 内部方法，Web 不得调用。
  DAEMON_BT_GUARD_CLOUD_APPLY: 'daemon.bt.guard.cloud.apply',
  DAEMON_BT_GUARD_EVIDENCE_DRAIN: 'daemon.bt.guard.evidence.drain',
  DAEMON_ED2K_SERVER_SUBSCRIPTION_REFRESH: 'daemon.ed2k.serverSubscription.refresh',
  DAEMON_DIAGNOSTICS_DESCRIBE: 'daemon.diagnostics.describe',
  DAEMON_DIAGNOSTICS_PREPARE_LOG_EXPORT: 'daemon.diagnostics.prepareLogExport',
  /** agent Doctor 专用：daemon 内真实写入下载 / 数据目录并运行外部组件。 */
  DAEMON_DIAGNOSTICS_PROBE: 'daemon.diagnostics.probe',
  /** agent Doctor 专用：补上托管组件缺失的执行权限并重新探测。 */
  DAEMON_DIAGNOSTICS_FIX_COMPONENT: 'daemon.diagnostics.fixComponent',
  DAEMON_MIGRATION_LINK_EXPORT: 'daemon.migration.linkExport',
  DAEMON_MIGRATION_LINK_ACK: 'daemon.migration.linkAck',
  DAEMON_MIGRATION_GATEWAY_EXPORT: 'daemon.migration.gatewayExport',
  DAEMON_MIGRATION_GATEWAY_ACK: 'daemon.migration.gatewayAck',

  AGENT_SESSION_GET: 'agent.session.get',
  AGENT_AUTH_REGISTER: 'agent.auth.register',
  AGENT_AUTH_REGISTER_VERIFY: 'agent.auth.registerVerify',
  AGENT_AUTH_LOGIN: 'agent.auth.login',
  AGENT_AUTH_LOGIN_VERIFY: 'agent.auth.loginVerify',
  AGENT_AUTH_SEND_CODE: 'agent.auth.sendCode',
  AGENT_AUTH_VERIFY_CODE: 'agent.auth.verifyCode',
  AGENT_AUTH_LOGOUT: 'agent.auth.logout',
  AGENT_AUTH_REFRESH_PROFILE: 'agent.auth.refreshProfile',
  AGENT_AUTH_SEND_PASSWORD_RESET_CODE: 'agent.auth.sendPasswordResetCode',
  AGENT_AUTH_RESET_PASSWORD: 'agent.auth.resetPassword',
  AGENT_PROFILE_SEND_EMAIL_CODE: 'agent.profile.sendEmailCode',
  AGENT_PROFILE_SEND_NEW_EMAIL_CODE: 'agent.profile.sendNewEmailCode',
  AGENT_PROFILE_CHANGE_EMAIL: 'agent.profile.changeEmail',
  AGENT_PROFILE_RANDOM_ORIGIN_ID: 'agent.profile.randomOriginId',
  AGENT_PROFILE_CHECK_ORIGIN_ID: 'agent.profile.checkOriginId',
  AGENT_PROFILE_CHANGE_ORIGIN_ID: 'agent.profile.changeOriginId',
  AGENT_PROFILE_CHANGE_NICKNAME: 'agent.profile.changeNickname',
  AGENT_PROFILE_SEND_PASSWORD_CODE: 'agent.profile.sendPasswordCode',
  AGENT_PROFILE_CHANGE_PASSWORD: 'agent.profile.changePassword',
  AGENT_PROFILE_SEND_DELETE_CODE: 'agent.profile.sendDeleteCode',
  AGENT_PROFILE_DELETE_ACCOUNT: 'agent.profile.deleteAccount',

  AGENT_GATEWAY_GET: 'agent.gateway.get',
  AGENT_GATEWAY_PATCH: 'agent.gateway.patch',
  AGENT_GATEWAY_REVEAL_TOKEN: 'agent.gateway.revealToken',
  AGENT_DEVICE_LIST: 'agent.device.list',
  AGENT_REMOTE_RECONNECT: 'agent.remote.reconnect',
  AGENT_DEVICE_RENAME: 'agent.device.rename',
  AGENT_DEVICE_DELETE: 'agent.device.delete',
  AGENT_DEVICE_DISCONNECT: 'agent.device.disconnect',
  AGENT_PREFERENCES_PATCH: 'agent.preferences.patch',
  AGENT_SYNC_GET: 'agent.sync.get',
  AGENT_SYNC_ENABLE: 'agent.sync.enable',
  AGENT_SYNC_DISABLE: 'agent.sync.disable',
  AGENT_SYNC_NOW: 'agent.sync.now',
  AGENT_SYNC_SET_LOCAL_ONLY: 'agent.sync.setLocalOnly',
  AGENT_LINK_PAIRING_CODE: 'agent.link.pairingCode',
  AGENT_LINK_STOP_PAIRING: 'agent.link.stopPairing',
  AGENT_LINK_DISCOVERY_SET: 'agent.link.discovery.set',
  AGENT_LINK_PROBE: 'agent.link.probe',
  AGENT_LINK_PAIR_BEGIN: 'agent.link.pairBegin',
  AGENT_LINK_PAIR_FINISH: 'agent.link.pairFinish',
  AGENT_LINK_APPROVE: 'agent.link.approve',
  AGENT_LINK_REMOVE: 'agent.link.remove',
  AGENT_LINK_RENAME: 'agent.link.rename',
  AGENT_LINK_REFRESH: 'agent.link.refresh',
  AGENT_LINK_CONTROL_CREDENTIAL: 'agent.link.controlCredential',
  AGENT_LINK_DISPATCH: 'agent.link.dispatch',
  AGENT_CLOUD_ENDPOINT_GET: 'agent.cloud.endpointGet',
  AGENT_CLOUD_ENDPOINT_SET: 'agent.cloud.endpointSet',
  AGENT_REMOTE_LIST: 'agent.remote.list',
  AGENT_REMOTE_DISPATCH: 'agent.remote.dispatch',
  AGENT_REMOTE_COMMAND: 'agent.remote.command',

  AGENT_PLAN_LIST: 'agent.plan.list',
  AGENT_ORDER_CREATE: 'agent.order.create',
  AGENT_ORDER_GET: 'agent.order.get',
  AGENT_ORDER_LIST: 'agent.order.list',
  AGENT_REFERRAL_SUMMARY: 'agent.referral.summary',
  AGENT_REFERRAL_LIST_CODES: 'agent.referral.listCodes',
  AGENT_REFERRAL_CREATE_CODE: 'agent.referral.createCode',
  AGENT_REFERRAL_DELETE_CODE: 'agent.referral.deleteCode',
  AGENT_REFERRAL_LIST_RECORDS: 'agent.referral.listRecords',
  AGENT_REFERRAL_VALIDATE: 'agent.referral.validate',
  AGENT_PLUGIN_FAVORITE_LIST: 'agent.pluginFavorite.list',
  AGENT_PLUGIN_FAVORITE_SET: 'agent.pluginFavorite.set',
  AGENT_PRIVATE_PLUGIN_LIST: 'agent.privatePlugin.list',
  AGENT_PRIVATE_PLUGIN_INSTALL: 'agent.privatePlugin.install',

  // 以下方法读写宿主机路径 / 桌面集成，按连接能力门控（多数 Web 不使用）。
  AGENT_PLATFORM_OPEN_TASK: 'agent.platform.openTask',
  AGENT_PLATFORM_REVEAL_TASK: 'agent.platform.revealTask',
  /** 把任务产物以文件写入 agent 所在主机的系统剪贴板；仅当 hello 带 `agent.copyTaskFiles` 能力时可用。 */
  AGENT_PLATFORM_COPY_TASK_FILES: 'agent.platform.copyTaskFiles',
  AGENT_PLATFORM_OPEN_PATH: 'agent.platform.openPath',
  AGENT_PLATFORM_INTEGRATION_GET: 'agent.platform.integrationGet',
  AGENT_PLATFORM_SET_AUTOSTART: 'agent.platform.setAutostart',
  AGENT_PLATFORM_SET_FILE_ASSOCIATION: 'agent.platform.setFileAssociation',
  AGENT_PLATFORM_SET_URL_PROTOCOL: 'agent.platform.setUrlProtocol',
  AGENT_PLATFORM_FILE_ICON: 'agent.platform.fileIcon',
  /** 桌面应用图标（仅 GPUI 使用）：切换 / 导入、读取自定义图标、为现有窗口补设图标。 */
  AGENT_PLATFORM_SET_APP_ICON: 'agent.platform.setAppIcon',
  AGENT_PLATFORM_APP_ICON_IMAGE: 'agent.platform.appIconImage',
  AGENT_PLATFORM_SYNC_WINDOW_ICONS: 'agent.platform.syncWindowIcons',
  AGENT_SOUND_PREVIEW: 'agent.sound.preview',
  AGENT_CAPTURE_SUBMIT: 'agent.capture.submit',
  AGENT_CAPTURE_SUBMIT_TORRENT_FILE: 'agent.capture.submitTorrentFile',
  AGENT_CAPTURE_LIST: 'agent.capture.list',
  AGENT_CAPTURE_RESOLVE: 'agent.capture.resolve',
  AGENT_CAPTURE_PREVIEW: 'agent.capture.preview',
  AGENT_CAPTURE_CREATE_GROUP: 'agent.capture.createGroup',
  AGENT_PLUGIN_INSTALL_FILE: 'agent.plugin.installFile',
  AGENT_DIAGNOSTICS_RUN: 'agent.diagnostics.run',
  AGENT_DIAGNOSTICS_REPAIR: 'agent.diagnostics.repair',
  AGENT_DIAGNOSTICS_LOG_PATHS: 'agent.diagnostics.logPaths',
  AGENT_DIAGNOSTICS_EXPORT_LOGS: 'agent.diagnostics.exportLogs',
  AGENT_UPDATE_CHECK: 'agent.update.check',
  AGENT_UPDATE_DOWNLOAD: 'agent.update.download',
  AGENT_UPDATE_INSTALL: 'agent.update.install',
  AGENT_UPDATE_CANCEL: 'agent.update.cancel',
  /** 定时关机 / 睡眠 `{delaySecs, action}` → `{armed}`；没有任务时立即倒计时，`delaySecs = 0` 仅有任务时受理。 */
  AGENT_POWER_ARM: 'agent.power.arm',
  AGENT_POWER_DISARM: 'agent.power.disarm',
  AGENT_CLOUD_NOTIFY_GET: 'agent.cloudNotify.get',
  AGENT_CLOUD_NOTIFY_REFRESH: 'agent.cloudNotify.refresh',
  AGENT_CLOUD_NOTIFY_SET_REPORTING: 'agent.cloudNotify.setReporting',
  AGENT_CLOUD_NOTIFY_SET_PRIVACY: 'agent.cloudNotify.setPrivacy',
  AGENT_CLOUD_NOTIFY_CREATE_CHANNEL: 'agent.cloudNotify.createChannel',
  AGENT_CLOUD_NOTIFY_UPDATE_CHANNEL: 'agent.cloudNotify.updateChannel',
  AGENT_CLOUD_NOTIFY_DELETE_CHANNEL: 'agent.cloudNotify.deleteChannel',
  AGENT_CLOUD_NOTIFY_TEST_CHANNEL: 'agent.cloudNotify.testChannel',
  AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_START: 'agent.cloudNotify.telegramBindStart',
  AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_STATUS: 'agent.cloudNotify.telegramBindStatus',
  AGENT_CLOUD_NOTIFY_DELIVERIES: 'agent.cloudNotify.deliveries',
  AGENT_CLOUD_NOTIFY_SEND_EMAIL_CODE: 'agent.cloudNotify.sendEmailCode',
  AGENT_CLOUD_NOTIFY_VERIFY_EMAIL: 'agent.cloudNotify.verifyEmail',

  /** 服务端推送通知，params 为 EventFrame。 */
  SERVICE_EVENT: 'service.event',
} as const;

export type MethodName = (typeof METHOD)[keyof typeof METHOD];

export const CAPABILITY_DAEMON_TASKS = 'daemon.tasks';
export const CAPABILITY_DAEMON_QUEUES = 'daemon.queues';
export const CAPABILITY_DAEMON_GROUPS = 'daemon.groups';
export const CAPABILITY_DAEMON_CONFIG = 'daemon.config';
export const CAPABILITY_DAEMON_RSS = 'daemon.rss';
export const CAPABILITY_DAEMON_PLUGINS = 'daemon.plugins';
export const CAPABILITY_DAEMON_COMPONENTS = 'daemon.components';
export const CAPABILITY_DAEMON_WEBHOOKS = 'daemon.webhooks';
export const CAPABILITY_DAEMON_SELECTIONS = 'daemon.selections';
export const CAPABILITY_DAEMON_FILES = 'daemon.files';
export const CAPABILITY_AGENT_GATEWAY = 'agent.gateway';
export const CAPABILITY_AGENT_AUTH = 'agent.auth';
export const CAPABILITY_AGENT_SYNC = 'agent.sync';
export const CAPABILITY_AGENT_REMOTE_TASKS = 'agent.remoteTasks';
export const CAPABILITY_AGENT_BILLING = 'agent.billing';
export const CAPABILITY_AGENT_REFERRALS = 'agent.referrals';
export const CAPABILITY_AGENT_DEVICE_LINK = 'agent.deviceLink';
export const CAPABILITY_AGENT_EXTERNAL_CAPTURE = 'agent.externalCapture';
/** 按连接下发：本连接可在 agent 所在主机上打开 / 定位任务产物（`agent.platform.openTask` / `revealTask`）。 */
export const CAPABILITY_AGENT_OPEN_TASK_FILES = 'agent.openTaskFiles';
/**
 * 按连接下发：本连接可把任务产物写入 agent 所在主机的系统剪贴板（`agent.platform.copyTaskFiles`）。
 * 只在本机来源 + 桌面构建 + 有图形会话时下发；远程 / NAS 连接没有（浏览器无法把文件写进系统剪贴板）。
 */
export const CAPABILITY_AGENT_COPY_TASK_FILES = 'agent.copyTaskFiles';
/** 客户端能力：可处理交互选择（HLS/BT/变体），放入握手 capabilities。 */
export const CAPABILITY_CLIENT_SELECTIONS = 'client.selections';
