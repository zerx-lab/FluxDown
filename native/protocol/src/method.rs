//! 本机服务 JSON-RPC 方法名与能力标识。

pub const SYSTEM_HELLO: &str = "system.hello";
pub const SYSTEM_PING: &str = "system.ping";
pub const SYSTEM_SNAPSHOT: &str = "system.snapshot";
/// 分页拉取 `system.snapshot` / 重载事件宣告「待载入」的任务表与队列位置表
/// （[`crate::TablePageParams`] → [`crate::TablePage`]）。与 `system.snapshot` 同权限、同服务：
/// 页应答自带服务端当时的 `(epoch, sequence)`，客户端按 [`crate::TableSync`] 载入并回放。
pub const SYSTEM_TASK_PAGE: &str = "system.taskPage";
/// 让服务优雅退出：daemon 关停引擎后退出；agent 先关停 daemon 再退出（完全退出）。
///
/// 允许作为连接首帧（握手前）调用：协议版本不兼容的新旧进程替换只能靠它完成。
pub const SYSTEM_SHUTDOWN: &str = "system.shutdown";

pub const DAEMON_TASK_LIST: &str = "daemon.task.list";
pub const DAEMON_TASK_GET: &str = "daemon.task.get";
pub const DAEMON_TASK_ACTIVITY: &str = "daemon.task.activity";
/// 单任务速度历史（`TaskSpeedHistoryQuery` → `Option<TaskSpeedHistoryDto>`）。
pub const DAEMON_TASK_SPEED_HISTORY: &str = "daemon.task.speedHistory";
pub const DAEMON_TASK_CREATE: &str = "daemon.task.create";
pub const DAEMON_TASK_PAUSE: &str = "daemon.task.pause";
pub const DAEMON_TASK_RESUME: &str = "daemon.task.resume";
pub const DAEMON_TASK_RENAME: &str = "daemon.task.rename";
/// 更换任务下载源地址（`{taskId, url}`，语义同旧 `PUT /api/v1/tasks/{id}/url`）。
pub const DAEMON_TASK_CHANGE_URL: &str = "daemon.task.changeUrl";
/// 修改任务线程数（`SetTaskSegmentsParams`），已下进度保留；完成或不存在的任务返回 `not-found`。
pub const DAEMON_TASK_SET_SEGMENTS: &str = "daemon.task.setSegments";
pub const DAEMON_TASK_DELETE: &str = "daemon.task.delete";
pub const DAEMON_TASK_PAUSE_ALL: &str = "daemon.task.pauseAll";
pub const DAEMON_TASK_RESUME_ALL: &str = "daemon.task.resumeAll";
pub const DAEMON_TASK_RESCAN: &str = "daemon.task.rescan";
pub const DAEMON_TASK_SET_SEED_LIMITS: &str = "daemon.task.setSeedLimits";
/// 修改 BT 任务的选片方式（`SetTaskPiecePickingParams`，缺省字段不变），活动任务热生效；
/// 不存在或非 BT 的任务返回 `notFound`。
pub const DAEMON_TASK_SET_PIECE_PICKING: &str = "daemon.task.setPiecePicking";
/// 已完成 BT 任务的完整性校验（`{taskId}` → `TaskVerifyResult`）：按种子 piece 表全量
/// 重哈希落盘数据，只读。慢方法：耗时与数据量成正比，同一任务同时只跑一个（`conflict`），
/// 非已完成 BT 任务返回 `invalidArgument`，任务不存在返回 `notFound`。
pub const DAEMON_TASK_VERIFY: &str = "daemon.task.verify";
/// 已完成任务落盘文件的哈希（`DaemonTaskHashParams` → `TaskHashResult`）：读整个文件计算
/// 摘要并记入任务的 `fileHash`（随任务行发布）。慢方法：耗时与文件大小成正比，同一任务同时
/// 只跑一个（`conflict`）；未完成 / 文件丢失 / 产物不是单个文件返回 `invalidArgument`，计算
/// 期间任务被重新下载返回 `taskStateConflict`，任务不存在返回 `notFound`。
pub const DAEMON_TASK_HASH: &str = "daemon.task.hash";
/// 本机做种（`DaemonSeedLocalParams` → `{taskId}`）：把 daemon 主机上已有的文件 / 文件夹
/// （可选现成 `.torrent`）登记为已完成并开始做种。无现成种子时需要为数据做哈希，慢方法；
/// BT 被禁用或路径 / 种子不匹配返回 `invalidArgument`。
pub const DAEMON_TASK_SEED_LOCAL: &str = "daemon.task.seedLocal";
/// BT 任务分享材料（`DaemonTorrentShareParams` → `TorrentShareDto`）：磁力链接与可选 `.torrent`
/// 内容。非 BT 任务 / 任务不存在返回 `invalidArgument` / `notFound`。
pub const DAEMON_TASK_TORRENT_SHARE: &str = "daemon.task.torrentShare";
/// BT 任务实况（`DaemonBtStatusParams` → `BtStatusDto`）：peer 列表与逐 tracker 汇报状态，
/// 供详情页轮询（对齐 qBittorrent 的 Peers / Trackers 页签）。非 BT 任务返回 `invalidArgument`。
pub const DAEMON_TASK_BT_STATUS: &str = "daemon.task.btStatus";
/// BT 任务的文件表（`DaemonBtStatusParams` 同形 `{taskId}` → `BtFilesDto`，不含 padding 文件），
/// 供详情页「文件」页签轮询。非 BT 任务返回 `notFound`。
pub const DAEMON_TASK_BT_FILES: &str = "daemon.task.btFiles";
/// 修改 BT 任务的文件优先级（`SetTaskFilePrioritiesParams`，部分更新）：`0` 不下载 / `1` 普通 /
/// `6` 高 / `7` 最高，下载中从下一次领片起生效。已完成任务返回 `taskStateConflict`；会导致没有
/// 任何文件要下载、下标越界或级别非法返回 `invalidArgument`。
pub const DAEMON_TASK_SET_FILE_PRIORITIES: &str = "daemon.task.setFilePriorities";
/// 重命名 BT 任务内的单个文件（`RenameBtFileParams`）：新路径是任务目录内的相对路径。
pub const DAEMON_TASK_RENAME_BT_FILE: &str = "daemon.task.renameBtFile";
/// 强制重新校验未完成的 BT 任务（`{taskId}`）：丢弃已有进度记录，按磁盘数据全量重哈希后回到
/// 原状态。已完成任务返回 `taskStateConflict`（改用只读的 `daemon.task.verify`）。
pub const DAEMON_TASK_RECHECK: &str = "daemon.task.recheck";
/// 移动 BT 任务的数据到新的保存目录（`MoveTaskStorageParams`）并更新任务位置，完成后任务回到
/// 原状态（下载 / 暂停 / 做种）。慢方法：跨卷移动耗时与数据量成正比；目标已有同名文件返回 `conflict`。
pub const DAEMON_TASK_MOVE_STORAGE: &str = "daemon.task.moveStorage";
/// 任务级 BT 限速（`SetTaskBtLimitsParams`，缺省字段不变，`0` = 不限），运行中立即生效。
pub const DAEMON_TASK_SET_BT_LIMITS: &str = "daemon.task.setBtLimits";
/// 超级做种开关（`SetTaskSuperSeedingParams`）：任务完成（做种）后才实际生效，可提前设置。
pub const DAEMON_TASK_SET_SUPER_SEEDING: &str = "daemon.task.setSuperSeeding";
/// 立即向 tracker 重新汇报（`{taskId}`）；任务当前不在运行返回 `taskStateConflict`。
pub const DAEMON_TASK_REANNOUNCE: &str = "daemon.task.reannounce";
/// eD2K 任务实况（`DaemonEd2kStatusParams` → `Ed2kStatusDto`）：源列表、查询过的服务器、Kad 节点
/// 与本机网络状态（HighID / LowID、UPnP），供详情页轮询。非 eD2K 任务返回 `invalidArgument`。
pub const DAEMON_TASK_ED2K_STATUS: &str = "daemon.task.ed2kStatus";
/// 批量暂停 / 恢复（`{taskIds}`）：整批只推一次任务快照；未知 id 忽略，空列表为空操作。
pub const DAEMON_TASK_PAUSE_MANY: &str = "daemon.task.pauseMany";
pub const DAEMON_TASK_RESUME_MANY: &str = "daemon.task.resumeMany";
/// 批量删除（`{taskIds, deleteFiles}`）：语义同批量暂停。
pub const DAEMON_TASK_DELETE_MANY: &str = "daemon.task.deleteMany";
/// 批量建任务（`DaemonCreateTasksParams`）：整块一个事务、一次 `TasksChanged` 事件，
/// 按 `batchId` 幂等；结果逐条对应（`DaemonCreateTasksResult`）。
pub const DAEMON_TASK_CREATE_MANY: &str = "daemon.task.createMany";

pub const DAEMON_QUEUE_LIST: &str = "daemon.queue.list";
pub const DAEMON_QUEUE_CREATE: &str = "daemon.queue.create";
pub const DAEMON_QUEUE_UPDATE: &str = "daemon.queue.update";
pub const DAEMON_QUEUE_DELETE: &str = "daemon.queue.delete";
pub const DAEMON_QUEUE_START: &str = "daemon.queue.start";
pub const DAEMON_QUEUE_STOP: &str = "daemon.queue.stop";
pub const DAEMON_QUEUE_SCHEDULE: &str = "daemon.queue.schedule";
pub const DAEMON_QUEUE_REORDER: &str = "daemon.queue.reorder";
pub const DAEMON_QUEUE_MOVE_TASK: &str = "daemon.queue.moveTask";
pub const DAEMON_QUEUE_BOOST: &str = "daemon.queue.boost";

pub const DAEMON_GROUP_LIST: &str = "daemon.group.list";
pub const DAEMON_GROUP_RESOLVE_PREVIEW: &str = "daemon.group.resolvePreview";
pub const DAEMON_GROUP_CREATE: &str = "daemon.group.create";
pub const DAEMON_GROUP_PAUSE: &str = "daemon.group.pause";
pub const DAEMON_GROUP_RESUME: &str = "daemon.group.resume";
pub const DAEMON_GROUP_DELETE: &str = "daemon.group.delete";

pub const DAEMON_CONFIG_GET: &str = "daemon.config.get";
pub const DAEMON_CONFIG_PATCH: &str = "daemon.config.patch";
pub const DAEMON_CONFIG_PROXY_TEST: &str = "daemon.config.proxyTest";
/// 站点代理规则草稿校验 + 「测试网址」解释（可能起 `scutil` 子进程，慢方法）。
pub const DAEMON_CONFIG_PROXY_ROUTE: &str = "daemon.config.proxyRoute";
pub const DAEMON_CONFIG_CONN_POLICY: &str = "daemon.config.connPolicy";
pub const DAEMON_CONFIG_CLEAR_CONN_POLICY: &str = "daemon.config.clearConnPolicy";
pub const DAEMON_CONFIG_SYSTEM_PROXY: &str = "daemon.config.systemProxy";
pub const DAEMON_SITE_AUTH_LIST: &str = "daemon.siteAuth.list";
pub const DAEMON_SITE_AUTH_DELETE: &str = "daemon.siteAuth.delete";
/// 读取单站点凭据详情（含明文密码；站点键会先按 `host` / `host:port` 归一化）。
pub const DAEMON_SITE_AUTH_GET: &str = "daemon.siteAuth.get";
/// 保存单站点 HTTP Basic 凭据，返回脱敏后的 `SiteAuthEntryDto`。
pub const DAEMON_SITE_AUTH_SAVE: &str = "daemon.siteAuth.save";
pub const DAEMON_SITE_AUTH_CLEAR: &str = "daemon.siteAuth.clear";
/// 按下载链接匹配已保存的站点凭据（含明文密码，仅供本机官方 UI 表单回填）。
pub const DAEMON_SITE_AUTH_MATCH: &str = "daemon.siteAuth.match";
pub const DAEMON_RUNTIME_STATS: &str = "daemon.runtime.stats";
pub const DAEMON_FS_LIST: &str = "daemon.fs.list";

pub const DAEMON_RSS_LIST_SOURCES: &str = "daemon.rss.listSources";
pub const DAEMON_RSS_GET_ITEMS: &str = "daemon.rss.getItems";
pub const DAEMON_RSS_CREATE_SOURCE: &str = "daemon.rss.createSource";
pub const DAEMON_RSS_UPDATE_SOURCE: &str = "daemon.rss.updateSource";
pub const DAEMON_RSS_DELETE_SOURCE: &str = "daemon.rss.deleteSource";
pub const DAEMON_RSS_REFRESH_SOURCE: &str = "daemon.rss.refreshSource";
pub const DAEMON_RSS_ITEM_ACTION: &str = "daemon.rss.itemAction";
pub const DAEMON_RSS_VALIDATE: &str = "daemon.rss.validate";
/// 剪贴板识别用的只读链接探测：daemon 以全局代理/UA 只发一次 HEAD（不发 GET），判定是否下载资源。
pub const DAEMON_LINK_PROBE: &str = "daemon.link.probe";

pub const DAEMON_PLUGIN_LIST: &str = "daemon.plugin.list";
/// 查询或手动刷新已签名的插件分类规则（可能访问网络）。
pub const DAEMON_PLUGIN_ARCHIVE_RULES: &str = "daemon.plugin.archiveRules";
pub const DAEMON_PLUGIN_SNIFF_RULES: &str = "daemon.plugin.sniffRules";
/// 插件页面脚本索引（`PluginPageScriptsDto`）：浏览器扩展据此注册 / 更新 / 注销用户脚本。
pub const DAEMON_PLUGIN_PAGE_SCRIPTS: &str = "daemon.plugin.pageScripts";
/// 读取单个页面脚本源码（`PluginPageScriptSourceParams` → `PluginPageScriptSourceDto`）。
pub const DAEMON_PLUGIN_PAGE_SCRIPT_SOURCE: &str = "daemon.plugin.pageScriptSource";
pub const DAEMON_PLUGIN_AUTH: &str = "daemon.plugin.auth";
pub const DAEMON_PLUGIN_SET_ENABLED: &str = "daemon.plugin.setEnabled";
pub const DAEMON_PLUGIN_UPDATE_SETTINGS: &str = "daemon.plugin.updateSettings";
pub const DAEMON_PLUGIN_INSTALL: &str = "daemon.plugin.install";
pub const DAEMON_PLUGIN_INSTALL_DEV: &str = "daemon.plugin.installDev";
/// 重新加载 dev 插件（`{identity}`）：重读 manifest + 源码并校验，失败保留登记。
pub const DAEMON_PLUGIN_RELOAD_DEV: &str = "daemon.plugin.reloadDev";
pub const DAEMON_PLUGIN_UNINSTALL: &str = "daemon.plugin.uninstall";
pub const DAEMON_PLUGIN_MARKET_LIST: &str = "daemon.plugin.marketList";
pub const DAEMON_PLUGIN_MARKET_INSTALL: &str = "daemon.plugin.marketInstall";
/// 安装 FluxCloud 私有商店的包（`PluginInstallPrivateParams`）：agent 已校验 sha256 的 blob，
/// daemon 再核对 identity / version / contentHash / 权限声明并标记来源 `private`，不经公开索引。
/// 仅 agent 自身调用（Gateway 拒绝透传）。
pub const DAEMON_PLUGIN_INSTALL_PRIVATE: &str = "daemon.plugin.installPrivate";
/// 按 FluxCloud 私有目录的撤回清单与授权范围 `scope` 禁用 / 解封已装私有插件（`PluginRevokePrivateParams`；
/// `scope` 缺省 = 只增不清）。仅 agent 自身调用（Gateway 拒绝透传）。
pub const DAEMON_PLUGIN_REVOKE_PRIVATE: &str = "daemon.plugin.revokePrivate";
pub const DAEMON_PLUGIN_IGNORE_RETRY: &str = "daemon.plugin.ignoreRetry";
/// 列出站点默认插件（多插件冲突的记忆选择，`Vec<PluginSiteDefaultDto>`）。
pub const DAEMON_PLUGIN_SITE_DEFAULTS: &str = "daemon.plugin.siteDefaults";
/// 确认 / 改选 / 删除站点默认插件（`PluginSetSiteDefaultParams`）。
pub const DAEMON_PLUGIN_SET_SITE_DEFAULT: &str = "daemon.plugin.setSiteDefault";
/// 授权插件请求的全部能力（`{identity, permissions, networkHosts}`，集合须与请求完全一致）。
pub const DAEMON_PLUGIN_CONSENT: &str = "daemon.plugin.consent";
/// 分页读取插件审计（`{identity?, beforeId?, limit?}`）。
pub const DAEMON_PLUGIN_AUDIT_LIST: &str = "daemon.plugin.audit.list";
/// 清空插件审计（`{identity?}`）。
pub const DAEMON_PLUGIN_AUDIT_CLEAR: &str = "daemon.plugin.audit.clear";
/// 客户端上报桌面脚本的审计记录（`{entries}`，origin 强制 desktop）。
pub const DAEMON_PLUGIN_AUDIT_RECORD: &str = "daemon.plugin.audit.record";
/// 打开可移植视图会话（`PluginViewOpenParams` → `PluginViewOpenedDto`）。慢方法：可能激活宿主。
pub const DAEMON_PLUGIN_VIEW_OPEN: &str = "daemon.plugin.view.open";
/// 投递视图事件（`PluginViewEventParams`）；入队即返回，处理结果经 `PluginViewFrame` 事件送出。
pub const DAEMON_PLUGIN_VIEW_EVENT: &str = "daemon.plugin.view.event";
/// 关闭视图会话（`{session}`）。
pub const DAEMON_PLUGIN_VIEW_CLOSE: &str = "daemon.plugin.view.close";
/// 取会话当前 `{seq, tree}`（序号断档 / 重连后重新同步）。
pub const DAEMON_PLUGIN_VIEW_SNAPSHOT: &str = "daemon.plugin.view.snapshot";
/// 会话续期（`{sessions: [...]}`）；90 s 无续期的会话会被关闭。
pub const DAEMON_PLUGIN_VIEW_KEEPALIVE: &str = "daemon.plugin.view.keepalive";
/// 执行插件命令（`{identity, command, taskIds}`）。慢方法：可能激活宿主。
pub const DAEMON_PLUGIN_COMMAND: &str = "daemon.plugin.command";
/// 调用插件 `flux.rpc.handle` 注册的处理器（`{identity, name, args}`）。慢方法。
pub const DAEMON_PLUGIN_INVOKE: &str = "daemon.plugin.invoke";
/// 页面脚本调用插件处理器（`PluginPageInvokeParams` → `PluginPageInvokeResultDto`）：只命中以
/// `{ page: true }` 注册的处理器，且插件须已授权 `pageScripts`。慢方法。
pub const DAEMON_PLUGIN_PAGE_INVOKE: &str = "daemon.plugin.pageInvoke";
pub const DAEMON_COMPONENT_GET: &str = "daemon.component.get";
pub const DAEMON_COMPONENT_LIST_VERSIONS: &str = "daemon.component.listVersions";
pub const DAEMON_COMPONENT_INSTALL: &str = "daemon.component.install";
pub const DAEMON_COMPONENT_UNINSTALL: &str = "daemon.component.uninstall";

pub const DAEMON_SELECTION_SUBSCRIBE: &str = "daemon.selection.subscribe";
pub const DAEMON_SELECTION_UNSUBSCRIBE: &str = "daemon.selection.unsubscribe";
pub const DAEMON_SELECTION_RESOLVE: &str = "daemon.selection.resolve";

pub const DAEMON_WEBHOOK_GET: &str = "daemon.webhook.get";
pub const DAEMON_WEBHOOK_CLEAR_DELIVERIES: &str = "daemon.webhook.clearDeliveries";
pub const DAEMON_WEBHOOK_SIMULATE: &str = "daemon.webhook.simulate";
pub const DAEMON_WEBHOOK_TEST: &str = "daemon.webhook.test";
pub const DAEMON_CDN_REPORTS_PEEK: &str = "daemon.cdnReports.peek";
pub const DAEMON_CDN_REPORTS_ACK: &str = "daemon.cdnReports.ack";
pub const DAEMON_CDN_CONFIG_APPLY: &str = "daemon.cdnConfig.apply";
/// 宿主自身外网请求的统一出口方案（`EgressPlanParams` → `EgressPlanDto`）：与下载任务同一套全局
/// 代理；`verifiedPayload` 为真时再按下载任务的 resolver 规则交给插件改写（如 GitHub 加速）。agent
/// 的自更新、规则包下载经它与 daemon 内的组件 / 插件包下载走同一出口。慢方法：插件改写可能实测加速源。
pub const DAEMON_EGRESS_PLAN: &str = "daemon.egress.plan";

pub const DAEMON_BT_TRACKER_SUBSCRIPTION_REFRESH: &str = "daemon.bt.trackerSubscription.refresh";
/// BT 监听端口与端口映射实况（无参 → `BtPortStatusDto`），供设置页轮询。
pub const DAEMON_BT_PORT_STATUS: &str = "daemon.bt.portStatus";
/// BT 防吸血状态总览（`BtGuardStateDto`）。
pub const DAEMON_BT_GUARD_STATE: &str = "daemon.bt.guard.state";
/// 过滤条目分页（`BtGuardEntriesListParams` → `BtGuardEntriesListDto`）。
pub const DAEMON_BT_GUARD_ENTRIES_LIST: &str = "daemon.bt.guard.entries.list";
/// 手动添加条目 / 白名单（`BtGuardEntriesAddParams` → `BtGuardEntryDto`）。
pub const DAEMON_BT_GUARD_ENTRIES_ADD: &str = "daemon.bt.guard.entries.add";
/// 按 id 移除条目（`BtGuardEntriesRemoveParams` → `BtGuardEntriesRemoveDto`）；云层条目拒绝。
pub const DAEMON_BT_GUARD_ENTRIES_REMOVE: &str = "daemon.bt.guard.entries.remove";
/// 事件日志分页（`BtGuardEventsListParams` → `BtGuardEventsListDto`）。
pub const DAEMON_BT_GUARD_EVENTS_LIST: &str = "daemon.bt.guard.events.list";
/// 导出条目文本（`BtGuardExportParams` → `BtGuardExportDto`）。慢方法：条目可达数万行。
pub const DAEMON_BT_GUARD_EXPORT: &str = "daemon.bt.guard.export";
/// 立即刷新过滤文件订阅（无参）。慢方法：会拉取网络。
pub const DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH: &str = "daemon.bt.guard.filterSources.refresh";
/// 请求立即检查云名单更新（无参 → `{}`）。daemon 只发出 `BtGuardCloudRefreshWanted`，拉取与验签由
/// agent 完成，结果经 `BtGuardChanged` 通知；云名单未启用时不发。BT 会话未运行时同样可用。
pub const DAEMON_BT_GUARD_CLOUD_REFRESH: &str = "daemon.bt.guard.cloud.refresh";
/// 仅 agent 调用：已验签的云名单整层替换（`BtGuardCloudApplyParams` → `BtGuardCloudApplyDto`）。
/// 慢方法：差异防护 + 整层重建 + 持久化。
pub const DAEMON_BT_GUARD_CLOUD_APPLY: &str = "daemon.bt.guard.cloud.apply";
/// 仅 agent 调用：弹出待上报证据（`BtGuardEvidenceDrainDto`）。
pub const DAEMON_BT_GUARD_EVIDENCE_DRAIN: &str = "daemon.bt.guard.evidence.drain";
pub const DAEMON_ED2K_SERVER_SUBSCRIPTION_REFRESH: &str = "daemon.ed2k.serverSubscription.refresh";
/// 立即刷新 Kad 节点（nodes.dat）订阅（无参 → `Ed2kNodesSubRefreshResponse`）。慢方法：会拉取网络。
pub const DAEMON_ED2K_NODES_SUBSCRIPTION_REFRESH: &str = "daemon.ed2k.nodesSubscription.refresh";
/// HighID / LowID 检测（`Ed2kIdCheckParams`，可省略 → `Ed2kNetworkDto`）：按目标报告当前会话，或用
/// 真实监听端口（启用 UPnP 时先等映射结果）登录服务器取得分配的 ID 后断开。慢方法：会连接网络。
pub const DAEMON_ED2K_ID_CHECK: &str = "daemon.ed2k.idCheck";
/// Kad 引导（`Ed2kKadBootstrapParams` → `Ed2kKadBootstrapDto`）：向指定节点 / 网址取到的节点 /
/// 已知节点发握手，报告应答情况并把存活节点并入已知节点缓存。慢方法：会连接网络。
pub const DAEMON_ED2K_KAD_BOOTSTRAP: &str = "daemon.ed2k.kadBootstrap";
pub const DAEMON_DIAGNOSTICS_DESCRIBE: &str = "daemon.diagnostics.describe";
pub const DAEMON_DIAGNOSTICS_PREPARE_LOG_EXPORT: &str = "daemon.diagnostics.prepareLogExport";
/// Doctor 动态探测：在 daemon 进程内真实写入各下载目录 / 数据目录并运行外部组件，
/// 验证下载链路实际拥有的权限（agent 只读配置判断不出 TCC、只读挂载、隔离属性等）。
pub const DAEMON_DIAGNOSTICS_PROBE: &str = "daemon.diagnostics.probe";
/// Doctor 修复：补上托管组件（daemon 数据目录内）缺失的执行权限并重新探测，返回 `ComponentProbeDto`。
pub const DAEMON_DIAGNOSTICS_FIX_COMPONENT: &str = "daemon.diagnostics.fixComponent";
pub const DAEMON_MIGRATION_LINK_EXPORT: &str = "daemon.migration.linkExport";
pub const DAEMON_MIGRATION_LINK_ACK: &str = "daemon.migration.linkAck";
pub const DAEMON_MIGRATION_GATEWAY_EXPORT: &str = "daemon.migration.gatewayExport";
pub const DAEMON_MIGRATION_GATEWAY_ACK: &str = "daemon.migration.gatewayAck";

pub const AGENT_SESSION_GET: &str = "agent.session.get";
pub const AGENT_AUTH_REGISTER: &str = "agent.auth.register";
pub const AGENT_AUTH_REGISTER_VERIFY: &str = "agent.auth.registerVerify";
pub const AGENT_AUTH_LOGIN: &str = "agent.auth.login";
pub const AGENT_AUTH_LOGIN_VERIFY: &str = "agent.auth.loginVerify";
pub const AGENT_AUTH_SEND_CODE: &str = "agent.auth.sendCode";
pub const AGENT_AUTH_VERIFY_CODE: &str = "agent.auth.verifyCode";
pub const AGENT_AUTH_LOGOUT: &str = "agent.auth.logout";
pub const AGENT_AUTH_REFRESH_PROFILE: &str = "agent.auth.refreshProfile";
pub const AGENT_AUTH_SEND_PASSWORD_RESET_CODE: &str = "agent.auth.sendPasswordResetCode";
pub const AGENT_AUTH_RESET_PASSWORD: &str = "agent.auth.resetPassword";
pub const AGENT_PROFILE_SEND_EMAIL_CODE: &str = "agent.profile.sendEmailCode";
pub const AGENT_PROFILE_SEND_NEW_EMAIL_CODE: &str = "agent.profile.sendNewEmailCode";
pub const AGENT_PROFILE_CHANGE_EMAIL: &str = "agent.profile.changeEmail";
pub const AGENT_PROFILE_RANDOM_ORIGIN_ID: &str = "agent.profile.randomOriginId";
pub const AGENT_PROFILE_CHECK_ORIGIN_ID: &str = "agent.profile.checkOriginId";
pub const AGENT_PROFILE_CHANGE_ORIGIN_ID: &str = "agent.profile.changeOriginId";
pub const AGENT_PROFILE_CHANGE_NICKNAME: &str = "agent.profile.changeNickname";
pub const AGENT_PROFILE_SEND_PASSWORD_CODE: &str = "agent.profile.sendPasswordCode";
pub const AGENT_PROFILE_CHANGE_PASSWORD: &str = "agent.profile.changePassword";
pub const AGENT_PROFILE_SEND_DELETE_CODE: &str = "agent.profile.sendDeleteCode";
pub const AGENT_PROFILE_DELETE_ACCOUNT: &str = "agent.profile.deleteAccount";

pub const AGENT_GATEWAY_GET: &str = "agent.gateway.get";
pub const AGENT_GATEWAY_PATCH: &str = "agent.gateway.patch";
/// 仅供本机官方 UI 展示/复制用户 token；结果 `{ "userToken": "..." }`（未配置为空串）。
pub const AGENT_GATEWAY_REVEAL_TOKEN: &str = "agent.gateway.revealToken";
pub const AGENT_DEVICE_LIST: &str = "agent.device.list";
/// 请求立即重连任务 SSE；返回 `{ "accepted": true }` 不表示已经连接成功。
pub const AGENT_REMOTE_RECONNECT: &str = "agent.remote.reconnect";
pub const AGENT_DEVICE_RENAME: &str = "agent.device.rename";
pub const AGENT_DEVICE_DELETE: &str = "agent.device.delete";
/// `{ "id": "<设备 id>" }`：让同账号的另一台在线设备下线释放在线名额（不删除设备）；
/// 不能对本机调用。成功响应 `{ "ok": true }`。
pub const AGENT_DEVICE_DISCONNECT: &str = "agent.device.disconnect";
pub const AGENT_PREFERENCES_PATCH: &str = "agent.preferences.patch";
pub const AGENT_SYNC_GET: &str = "agent.sync.get";
pub const AGENT_SYNC_ENABLE: &str = "agent.sync.enable";
pub const AGENT_SYNC_DISABLE: &str = "agent.sync.disable";
pub const AGENT_SYNC_NOW: &str = "agent.sync.now";
/// `SyncLocalOnlyParams` → `SyncStatusDto`：把同步目录键设为本设备专属 / 恢复同步。
pub const AGENT_SYNC_SET_LOCAL_ONLY: &str = "agent.sync.setLocalOnly";
/// FluxCloud 服务地址读取；正式构建 `editable=false`，地址恒为构建期固定值。
pub const AGENT_CLOUD_ENDPOINT_GET: &str = "agent.cloud.endpointGet";
/// 仅调试构建可用：覆盖/恢复 FluxCloud 服务地址，立即生效。
pub const AGENT_CLOUD_ENDPOINT_SET: &str = "agent.cloud.endpointSet";
pub const AGENT_REMOTE_LIST: &str = "agent.remote.list";
pub const AGENT_REMOTE_DISPATCH: &str = "agent.remote.dispatch";
pub const AGENT_REMOTE_COMMAND: &str = "agent.remote.command";
/// 局域网直连（L1）：展示本机配对码并开始广播 → `LinkPairingCodeDto`。
pub const AGENT_LINK_PAIRING_CODE: &str = "agent.link.pairingCode";
/// 停止展示配对码 / 停止配对广播。
pub const AGENT_LINK_STOP_PAIRING: &str = "agent.link.stopPairing";
/// `LinkDiscoveryParams`：开关 mDNS 发现；结果经 `LinkDiscoveredChanged` 推送。
pub const AGENT_LINK_DISCOVERY_SET: &str = "agent.link.discovery.set";
/// `LinkAddressParams` → `LinkDiscoveredPeer`：手动地址探测。
pub const AGENT_LINK_PROBE: &str = "agent.link.probe";
/// `LinkPairBeginParams` → `LinkPairBeginResponse`。
pub const AGENT_LINK_PAIR_BEGIN: &str = "agent.link.pairBegin";
/// `LinkPairFinishParams` → `LinkPairFinishResponse`。
pub const AGENT_LINK_PAIR_FINISH: &str = "agent.link.pairFinish";
/// `LinkApproveParams`：响应端确认 / 拒绝入站配对请求。
pub const AGENT_LINK_APPROVE: &str = "agent.link.approve";
/// `LinkDeviceParams`：解除配对（尽力通知在线对端同步解除）。
pub const AGENT_LINK_REMOVE: &str = "agent.link.remove";
/// `LinkRenameParams` → `LinkDeviceInfo`：设置 / 清除已配对设备的本机备注名。
pub const AGENT_LINK_RENAME: &str = "agent.link.rename";
/// 探测全部已配对设备在线状态 → `Vec<LinkDeviceInfo>`（同时推送 `LinkedDevicesChanged`）。
pub const AGENT_LINK_REFRESH: &str = "agent.link.refresh";
/// `LinkDeviceParams` → `LinkControlCredentialDto`：为「本机可控制」的已配对主机签发 `/rpc`
/// 设备控制凭据（仅 `controllable && !revoked` 的记录）。
pub const AGENT_LINK_CONTROL_CREDENTIAL: &str = "agent.link.controlCredential";
/// `LinkDispatchParams` → `LinkDispatchResult`：下发下载到已配对设备。
pub const AGENT_LINK_DISPATCH: &str = "agent.link.dispatch";

pub const AGENT_PLAN_LIST: &str = "agent.plan.list";
pub const AGENT_ORDER_CREATE: &str = "agent.order.create";
pub const AGENT_ORDER_GET: &str = "agent.order.get";
pub const AGENT_ORDER_LIST: &str = "agent.order.list";
pub const AGENT_REFERRAL_SUMMARY: &str = "agent.referral.summary";
pub const AGENT_REFERRAL_LIST_CODES: &str = "agent.referral.listCodes";
pub const AGENT_REFERRAL_CREATE_CODE: &str = "agent.referral.createCode";
pub const AGENT_REFERRAL_DELETE_CODE: &str = "agent.referral.deleteCode";
pub const AGENT_REFERRAL_LIST_RECORDS: &str = "agent.referral.listRecords";
pub const AGENT_REFERRAL_VALIDATE: &str = "agent.referral.validate";
/// 插件市场收藏：公开计数 + （已登录时）本人收藏 → `PluginFavoritesDto`。
pub const AGENT_PLUGIN_FAVORITE_LIST: &str = "agent.pluginFavorite.list";
/// 收藏 / 取消收藏单个插件 → `PluginFavoriteResultDto`。
pub const AGENT_PLUGIN_FAVORITE_SET: &str = "agent.pluginFavorite.set";
/// FluxCloud 私有插件商店：当前账号被授权的条目 → `PrivatePluginListDto`（未登录不发云请求）。
/// 成功后 agent 把撤回清单（`yanked=malicious`）与 `scope` 经 `daemon.plugin.revokePrivate` 交给 daemon 禁用 / 解封已装私有插件。
pub const AGENT_PRIVATE_PLUGIN_LIST: &str = "agent.privatePlugin.list";
/// `PrivatePluginInstallRequest` → `InstalledPlugin`：download 票据 → 拉包 → sha256 钉住 →
/// daemon blob → `daemon.plugin.installPrivate`。
pub const AGENT_PRIVATE_PLUGIN_INSTALL: &str = "agent.privatePlugin.install";

/// 云端推送状态 → `CloudNotifyStateDto`（读本机缓存；过期时后台刷新并经 `CloudNotifyChanged` 推送）。
pub const AGENT_CLOUD_NOTIFY_GET: &str = "agent.cloudNotify.get";
/// 立即向 FluxCloud 拉取概览 → `CloudNotifyStateDto`。
pub const AGENT_CLOUD_NOTIFY_REFRESH: &str = "agent.cloudNotify.refresh";
/// `CloudNotifyReportingParams` → `CloudNotifyStateDto`：本设备上报开关（设备本地）。
pub const AGENT_CLOUD_NOTIFY_SET_REPORTING: &str = "agent.cloudNotify.setReporting";
/// `CloudNotifyPrivacyParams` → `CloudNotifyStateDto`：上报附带字段（设备本地）。
pub const AGENT_CLOUD_NOTIFY_SET_PRIVACY: &str = "agent.cloudNotify.setPrivacy";
/// `CloudNotifyChannelCreateParams` → `CloudNotifyChannelDto`。
pub const AGENT_CLOUD_NOTIFY_CREATE_CHANNEL: &str = "agent.cloudNotify.createChannel";
/// `CloudNotifyChannelUpdateParams` → `CloudNotifyChannelDto`。
pub const AGENT_CLOUD_NOTIFY_UPDATE_CHANNEL: &str = "agent.cloudNotify.updateChannel";
/// `CloudNotifyChannelIdParams` → `{ "ok": true }`。
pub const AGENT_CLOUD_NOTIFY_DELETE_CHANNEL: &str = "agent.cloudNotify.deleteChannel";
/// `CloudNotifyChannelIdParams` → `CloudNotifyTestResult`（不计入额度）。
pub const AGENT_CLOUD_NOTIFY_TEST_CHANNEL: &str = "agent.cloudNotify.testChannel";
/// → `CloudNotifyTelegramBindDto`：生成一次性 Telegram 绑定码。
pub const AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_START: &str = "agent.cloudNotify.telegramBindStart";
/// `CloudNotifyTelegramBindStatusParams` → `CloudNotifyTelegramBindStatusDto`。
pub const AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_STATUS: &str = "agent.cloudNotify.telegramBindStatus";
/// `CloudNotifyDeliveriesParams` → `CloudNotifyDeliveriesPage`。
pub const AGENT_CLOUD_NOTIFY_DELIVERIES: &str = "agent.cloudNotify.deliveries";
/// `CloudNotifyEmailCodeParams` → `CloudNotifyEmailCodeResult`：向非账号邮箱发通知渠道验证码。
pub const AGENT_CLOUD_NOTIFY_SEND_EMAIL_CODE: &str = "agent.cloudNotify.sendEmailCode";
/// `CloudNotifyEmailVerifyParams` → `{ "ok": true }`：校验验证码，把地址记为本账号已验证的通知邮箱。
pub const AGENT_CLOUD_NOTIFY_VERIFY_EMAIL: &str = "agent.cloudNotify.verifyEmail";

pub const AGENT_PLATFORM_OPEN_TASK: &str = "agent.platform.openTask";
pub const AGENT_PLATFORM_REVEAL_TASK: &str = "agent.platform.revealTask";
/// `PlatformCopyTaskFilesParams` → `{ "ok": true }`：把已完成任务的产物以文件形式写入 agent 所在主机的
/// 系统剪贴板（可直接在文件管理器 / 聊天软件里粘贴）。仅对带 [`CAPABILITY_AGENT_COPY_TASK_FILES`] 的连接可用。
pub const AGENT_PLATFORM_COPY_TASK_FILES: &str = "agent.platform.copyTaskFiles";
pub const AGENT_PLATFORM_OPEN_PATH: &str = "agent.platform.openPath";
pub const AGENT_PLATFORM_INTEGRATION_GET: &str = "agent.platform.integrationGet";
pub const AGENT_PLATFORM_SET_AUTOSTART: &str = "agent.platform.setAutostart";
pub const AGENT_PLATFORM_SET_FILE_ASSOCIATION: &str = "agent.platform.setFileAssociation";
pub const AGENT_PLATFORM_SET_URL_PROTOCOL: &str = "agent.platform.setUrlProtocol";
/// 系统文件管理器为文件显示的图标（PNG）；参数见 `PlatformFileIconParams`。
pub const AGENT_PLATFORM_FILE_ICON: &str = "agent.platform.fileIcon";
/// `app_icon::PlatformSetAppIconParams` → `app_icon::AppIconStateDto`：切换 / 导入桌面应用图标，成功后
/// agent 写入偏好 `desktop.app_icon` 并同步托盘、窗口与快捷方式图标。
pub const AGENT_PLATFORM_SET_APP_ICON: &str = "agent.platform.setAppIcon";
/// `app_icon::PlatformAppIconImageParams` → `app_icon::PlatformAppIconImageDto`：当前自定义应用图标的 PNG。
pub const AGENT_PLATFORM_APP_ICON_IMAGE: &str = "agent.platform.appIconImage";
/// `{}` → `{ "ok": true }`：为桌面界面进程现有窗口补设当前应用图标（Windows / X11；其余平台无操作）。
pub const AGENT_PLATFORM_SYNC_WINDOW_ICONS: &str = "agent.platform.syncWindowIcons";
/// `SoundPreviewParams` → `{ "ok": true }`：由 agent 在本机播放一次提示音偏好值（`off` / `system` / `file:<绝对路径>`）。
pub const AGENT_SOUND_PREVIEW: &str = "agent.sound.preview";
pub const AGENT_CAPTURE_SUBMIT: &str = "agent.capture.submit";
/// 从本机 `.torrent` 文件建任务：agent 读文件、上传 daemon blob 后调用 `daemon.task.create`。
pub const AGENT_CAPTURE_SUBMIT_TORRENT_FILE: &str = "agent.capture.submitTorrentFile";
pub const AGENT_CAPTURE_LIST: &str = "agent.capture.list";
pub const AGENT_CAPTURE_RESOLVE: &str = "agent.capture.resolve";
/// 捕获上下文只读清单预解析，返回 `ResolvePreviewResponse`，不消费事务。
pub const AGENT_CAPTURE_PREVIEW: &str = "agent.capture.preview";
/// 用捕获上下文与最终清单选择建组，成功后消费事务，返回 `CreateGroupResponse`。
pub const AGENT_CAPTURE_CREATE_GROUP: &str = "agent.capture.createGroup";
/// 从本机插件包安装：agent 读文件、上传 daemon blob 后调用 `daemon.plugin.install`。
pub const AGENT_PLUGIN_INSTALL_FILE: &str = "agent.plugin.installFile";
pub const AGENT_DIAGNOSTICS_RUN: &str = "agent.diagnostics.run";
pub const AGENT_DIAGNOSTICS_REPAIR: &str = "agent.diagnostics.repair";
pub const AGENT_DIAGNOSTICS_LOG_PATHS: &str = "agent.diagnostics.logPaths";
pub const AGENT_DIAGNOSTICS_EXPORT_LOGS: &str = "agent.diagnostics.exportLogs";
/// 检查渠道最新版本：`{channel?}` → `UpdateStatusDto`（同时经 `UpdateChanged` 推送）。
pub const AGENT_UPDATE_CHECK: &str = "agent.update.check";
/// 后台下载并校验更新包（幂等）；不可一键更新时拒绝（`Unsupported`）。
pub const AGENT_UPDATE_DOWNLOAD: &str = "agent.update.download";
/// 一键「更新并重启」：包未就绪时先下载，就绪后替换程序并重启服务；返回 `UpdateStatusDto`。
pub const AGENT_UPDATE_INSTALL: &str = "agent.update.install";
/// 取消进行中的下载与待安装请求，返回 `UpdateStatusDto`。
pub const AGENT_UPDATE_CANCEL: &str = "agent.update.cancel";
/// 定时关机 / 睡眠：`{delaySecs, action}` → `{armed}`；没有任务时立即开始倒计时，
/// `delaySecs = 0` 只在有任务（活跃 / 排队 / 待重试）时接受，否则 `armed = false`。
pub const AGENT_POWER_ARM: &str = "agent.power.arm";
pub const AGENT_POWER_DISARM: &str = "agent.power.disarm";

pub const SERVICE_EVENT: &str = "service.event";

pub const CAPABILITY_DAEMON_TASKS: &str = "daemon.tasks";
pub const CAPABILITY_DAEMON_QUEUES: &str = "daemon.queues";
pub const CAPABILITY_DAEMON_GROUPS: &str = "daemon.groups";
pub const CAPABILITY_DAEMON_CONFIG: &str = "daemon.config";
pub const CAPABILITY_DAEMON_RSS: &str = "daemon.rss";
pub const CAPABILITY_DAEMON_PLUGINS: &str = "daemon.plugins";
pub const CAPABILITY_DAEMON_COMPONENTS: &str = "daemon.components";
pub const CAPABILITY_DAEMON_WEBHOOKS: &str = "daemon.webhooks";
pub const CAPABILITY_DAEMON_SELECTIONS: &str = "daemon.selections";
pub const CAPABILITY_DAEMON_FILES: &str = "daemon.files";
pub const CAPABILITY_AGENT_GATEWAY: &str = "agent.gateway";
pub const CAPABILITY_AGENT_AUTH: &str = "agent.auth";
pub const CAPABILITY_AGENT_SYNC: &str = "agent.sync";
pub const CAPABILITY_AGENT_REMOTE_TASKS: &str = "agent.remoteTasks";
pub const CAPABILITY_AGENT_BILLING: &str = "agent.billing";
pub const CAPABILITY_AGENT_REFERRALS: &str = "agent.referrals";
pub const CAPABILITY_AGENT_DEVICE_LINK: &str = "agent.deviceLink";
pub const CAPABILITY_AGENT_EXTERNAL_CAPTURE: &str = "agent.externalCapture";
/// 本连接可调用 `agent.platform.openTask` / `revealTask`，在 agent 所在主机上打开 / 定位任务
/// 产物。按连接下发：Windows / macOS / Linux 桌面宿主恒有，嵌入式移动宿主没有；headless
/// server 只给本机来源（见 agent `server_mode::ServerHandle::local_platform_permitted`）。
pub const CAPABILITY_AGENT_OPEN_TASK_FILES: &str = "agent.openTaskFiles";
/// 本连接可调用 `agent.platform.copyTaskFiles`，把任务产物作为文件写入 agent 所在主机的系统剪贴板。
/// 按连接下发：本机来源条件同 [`CAPABILITY_AGENT_OPEN_TASK_FILES`]，另要求本构建编入桌面剪贴板
/// （agent `desktop` 特性）且检测到图形会话；headless / NAS 构建与远程连接都没有（浏览器无法把文件
/// 写进系统剪贴板，客户端据此隐藏「复制文件」）。
pub const CAPABILITY_AGENT_COPY_TASK_FILES: &str = "agent.copyTaskFiles";
pub const CAPABILITY_CLIENT_SELECTIONS: &str = "client.selections";

/// 规范ALL_METHODS。
pub const ALL_METHODS: &[&str] = &[
    SYSTEM_HELLO,
    SYSTEM_PING,
    SYSTEM_SNAPSHOT,
    SYSTEM_TASK_PAGE,
    SYSTEM_SHUTDOWN,
    DAEMON_TASK_LIST,
    DAEMON_TASK_GET,
    DAEMON_TASK_ACTIVITY,
    DAEMON_TASK_SPEED_HISTORY,
    DAEMON_TASK_CREATE,
    DAEMON_TASK_CREATE_MANY,
    DAEMON_TASK_PAUSE,
    DAEMON_TASK_RESUME,
    DAEMON_TASK_RENAME,
    DAEMON_TASK_CHANGE_URL,
    DAEMON_TASK_SET_SEGMENTS,
    DAEMON_TASK_DELETE,
    DAEMON_TASK_PAUSE_ALL,
    DAEMON_TASK_RESUME_ALL,
    DAEMON_TASK_RESCAN,
    DAEMON_TASK_SET_SEED_LIMITS,
    DAEMON_TASK_SET_PIECE_PICKING,
    DAEMON_TASK_VERIFY,
    DAEMON_TASK_HASH,
    DAEMON_TASK_SEED_LOCAL,
    DAEMON_TASK_TORRENT_SHARE,
    DAEMON_TASK_BT_STATUS,
    DAEMON_TASK_BT_FILES,
    DAEMON_TASK_SET_FILE_PRIORITIES,
    DAEMON_TASK_RENAME_BT_FILE,
    DAEMON_TASK_RECHECK,
    DAEMON_TASK_MOVE_STORAGE,
    DAEMON_TASK_SET_BT_LIMITS,
    DAEMON_TASK_SET_SUPER_SEEDING,
    DAEMON_TASK_REANNOUNCE,
    DAEMON_TASK_ED2K_STATUS,
    DAEMON_TASK_PAUSE_MANY,
    DAEMON_TASK_RESUME_MANY,
    DAEMON_TASK_DELETE_MANY,
    DAEMON_QUEUE_LIST,
    DAEMON_QUEUE_CREATE,
    DAEMON_QUEUE_UPDATE,
    DAEMON_QUEUE_DELETE,
    DAEMON_QUEUE_START,
    DAEMON_QUEUE_STOP,
    DAEMON_QUEUE_SCHEDULE,
    DAEMON_QUEUE_REORDER,
    DAEMON_QUEUE_MOVE_TASK,
    DAEMON_QUEUE_BOOST,
    DAEMON_GROUP_LIST,
    DAEMON_GROUP_RESOLVE_PREVIEW,
    DAEMON_GROUP_CREATE,
    DAEMON_GROUP_PAUSE,
    DAEMON_GROUP_RESUME,
    DAEMON_GROUP_DELETE,
    DAEMON_CONFIG_GET,
    DAEMON_CONFIG_PATCH,
    DAEMON_CONFIG_PROXY_TEST,
    DAEMON_CONFIG_PROXY_ROUTE,
    DAEMON_CONFIG_CONN_POLICY,
    DAEMON_CONFIG_CLEAR_CONN_POLICY,
    DAEMON_CONFIG_SYSTEM_PROXY,
    DAEMON_SITE_AUTH_LIST,
    DAEMON_SITE_AUTH_DELETE,
    DAEMON_SITE_AUTH_GET,
    DAEMON_SITE_AUTH_SAVE,
    DAEMON_SITE_AUTH_CLEAR,
    DAEMON_SITE_AUTH_MATCH,
    DAEMON_RUNTIME_STATS,
    DAEMON_FS_LIST,
    DAEMON_RSS_LIST_SOURCES,
    DAEMON_RSS_GET_ITEMS,
    DAEMON_RSS_CREATE_SOURCE,
    DAEMON_RSS_UPDATE_SOURCE,
    DAEMON_RSS_DELETE_SOURCE,
    DAEMON_RSS_REFRESH_SOURCE,
    DAEMON_RSS_ITEM_ACTION,
    DAEMON_RSS_VALIDATE,
    DAEMON_LINK_PROBE,
    DAEMON_PLUGIN_LIST,
    DAEMON_PLUGIN_SNIFF_RULES,
    DAEMON_PLUGIN_ARCHIVE_RULES,
    DAEMON_PLUGIN_PAGE_SCRIPTS,
    DAEMON_PLUGIN_PAGE_SCRIPT_SOURCE,
    DAEMON_PLUGIN_AUTH,
    DAEMON_PLUGIN_SET_ENABLED,
    DAEMON_PLUGIN_UPDATE_SETTINGS,
    DAEMON_PLUGIN_INSTALL,
    DAEMON_PLUGIN_INSTALL_DEV,
    DAEMON_PLUGIN_RELOAD_DEV,
    DAEMON_PLUGIN_UNINSTALL,
    DAEMON_PLUGIN_MARKET_LIST,
    DAEMON_PLUGIN_MARKET_INSTALL,
    DAEMON_PLUGIN_INSTALL_PRIVATE,
    DAEMON_PLUGIN_REVOKE_PRIVATE,
    DAEMON_PLUGIN_IGNORE_RETRY,
    DAEMON_PLUGIN_SITE_DEFAULTS,
    DAEMON_PLUGIN_SET_SITE_DEFAULT,
    DAEMON_PLUGIN_CONSENT,
    DAEMON_PLUGIN_AUDIT_LIST,
    DAEMON_PLUGIN_AUDIT_CLEAR,
    DAEMON_PLUGIN_AUDIT_RECORD,
    DAEMON_PLUGIN_VIEW_OPEN,
    DAEMON_PLUGIN_VIEW_EVENT,
    DAEMON_PLUGIN_VIEW_CLOSE,
    DAEMON_PLUGIN_VIEW_SNAPSHOT,
    DAEMON_PLUGIN_VIEW_KEEPALIVE,
    DAEMON_PLUGIN_COMMAND,
    DAEMON_PLUGIN_INVOKE,
    DAEMON_PLUGIN_PAGE_INVOKE,
    DAEMON_COMPONENT_GET,
    DAEMON_COMPONENT_LIST_VERSIONS,
    DAEMON_COMPONENT_INSTALL,
    DAEMON_COMPONENT_UNINSTALL,
    DAEMON_SELECTION_SUBSCRIBE,
    DAEMON_SELECTION_UNSUBSCRIBE,
    DAEMON_SELECTION_RESOLVE,
    DAEMON_WEBHOOK_GET,
    DAEMON_WEBHOOK_CLEAR_DELIVERIES,
    DAEMON_WEBHOOK_SIMULATE,
    DAEMON_WEBHOOK_TEST,
    DAEMON_CDN_REPORTS_PEEK,
    DAEMON_CDN_REPORTS_ACK,
    DAEMON_CDN_CONFIG_APPLY,
    DAEMON_EGRESS_PLAN,
    DAEMON_BT_GUARD_STATE,
    DAEMON_BT_GUARD_ENTRIES_LIST,
    DAEMON_BT_GUARD_ENTRIES_ADD,
    DAEMON_BT_GUARD_ENTRIES_REMOVE,
    DAEMON_BT_GUARD_EVENTS_LIST,
    DAEMON_BT_GUARD_EXPORT,
    DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
    DAEMON_BT_GUARD_CLOUD_REFRESH,
    DAEMON_BT_GUARD_CLOUD_APPLY,
    DAEMON_BT_GUARD_EVIDENCE_DRAIN,
    DAEMON_BT_TRACKER_SUBSCRIPTION_REFRESH,
    DAEMON_BT_PORT_STATUS,
    DAEMON_ED2K_SERVER_SUBSCRIPTION_REFRESH,
    DAEMON_ED2K_NODES_SUBSCRIPTION_REFRESH,
    DAEMON_ED2K_ID_CHECK,
    DAEMON_ED2K_KAD_BOOTSTRAP,
    DAEMON_DIAGNOSTICS_DESCRIBE,
    DAEMON_DIAGNOSTICS_PREPARE_LOG_EXPORT,
    DAEMON_DIAGNOSTICS_PROBE,
    DAEMON_DIAGNOSTICS_FIX_COMPONENT,
    DAEMON_MIGRATION_LINK_EXPORT,
    DAEMON_MIGRATION_LINK_ACK,
    DAEMON_MIGRATION_GATEWAY_EXPORT,
    DAEMON_MIGRATION_GATEWAY_ACK,
    AGENT_SESSION_GET,
    AGENT_AUTH_REGISTER,
    AGENT_AUTH_REGISTER_VERIFY,
    AGENT_AUTH_LOGIN,
    AGENT_AUTH_LOGIN_VERIFY,
    AGENT_AUTH_SEND_CODE,
    AGENT_AUTH_VERIFY_CODE,
    AGENT_AUTH_LOGOUT,
    AGENT_AUTH_REFRESH_PROFILE,
    AGENT_AUTH_SEND_PASSWORD_RESET_CODE,
    AGENT_AUTH_RESET_PASSWORD,
    AGENT_PROFILE_SEND_EMAIL_CODE,
    AGENT_PROFILE_SEND_NEW_EMAIL_CODE,
    AGENT_PROFILE_CHANGE_EMAIL,
    AGENT_PROFILE_RANDOM_ORIGIN_ID,
    AGENT_PROFILE_CHECK_ORIGIN_ID,
    AGENT_PROFILE_CHANGE_ORIGIN_ID,
    AGENT_PROFILE_CHANGE_NICKNAME,
    AGENT_PROFILE_SEND_PASSWORD_CODE,
    AGENT_PROFILE_CHANGE_PASSWORD,
    AGENT_PROFILE_SEND_DELETE_CODE,
    AGENT_PROFILE_DELETE_ACCOUNT,
    AGENT_GATEWAY_GET,
    AGENT_GATEWAY_PATCH,
    AGENT_GATEWAY_REVEAL_TOKEN,
    AGENT_DEVICE_LIST,
    AGENT_REMOTE_RECONNECT,
    AGENT_DEVICE_RENAME,
    AGENT_DEVICE_DELETE,
    AGENT_DEVICE_DISCONNECT,
    AGENT_PREFERENCES_PATCH,
    AGENT_SYNC_GET,
    AGENT_SYNC_ENABLE,
    AGENT_SYNC_DISABLE,
    AGENT_SYNC_NOW,
    AGENT_SYNC_SET_LOCAL_ONLY,
    AGENT_CLOUD_ENDPOINT_GET,
    AGENT_CLOUD_ENDPOINT_SET,
    AGENT_REMOTE_LIST,
    AGENT_REMOTE_DISPATCH,
    AGENT_REMOTE_COMMAND,
    AGENT_LINK_PAIRING_CODE,
    AGENT_LINK_STOP_PAIRING,
    AGENT_LINK_DISCOVERY_SET,
    AGENT_LINK_PROBE,
    AGENT_LINK_PAIR_BEGIN,
    AGENT_LINK_PAIR_FINISH,
    AGENT_LINK_APPROVE,
    AGENT_LINK_REMOVE,
    AGENT_LINK_RENAME,
    AGENT_LINK_REFRESH,
    AGENT_LINK_CONTROL_CREDENTIAL,
    AGENT_LINK_DISPATCH,
    AGENT_PLAN_LIST,
    AGENT_ORDER_CREATE,
    AGENT_ORDER_GET,
    AGENT_ORDER_LIST,
    AGENT_REFERRAL_SUMMARY,
    AGENT_REFERRAL_LIST_CODES,
    AGENT_REFERRAL_CREATE_CODE,
    AGENT_REFERRAL_DELETE_CODE,
    AGENT_REFERRAL_LIST_RECORDS,
    AGENT_REFERRAL_VALIDATE,
    AGENT_PLUGIN_FAVORITE_LIST,
    AGENT_PLUGIN_FAVORITE_SET,
    AGENT_PRIVATE_PLUGIN_LIST,
    AGENT_PRIVATE_PLUGIN_INSTALL,
    AGENT_CLOUD_NOTIFY_GET,
    AGENT_CLOUD_NOTIFY_REFRESH,
    AGENT_CLOUD_NOTIFY_SET_REPORTING,
    AGENT_CLOUD_NOTIFY_SET_PRIVACY,
    AGENT_CLOUD_NOTIFY_CREATE_CHANNEL,
    AGENT_CLOUD_NOTIFY_UPDATE_CHANNEL,
    AGENT_CLOUD_NOTIFY_DELETE_CHANNEL,
    AGENT_CLOUD_NOTIFY_TEST_CHANNEL,
    AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_START,
    AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_STATUS,
    AGENT_CLOUD_NOTIFY_DELIVERIES,
    AGENT_CLOUD_NOTIFY_SEND_EMAIL_CODE,
    AGENT_CLOUD_NOTIFY_VERIFY_EMAIL,
    AGENT_PLATFORM_OPEN_TASK,
    AGENT_PLATFORM_REVEAL_TASK,
    AGENT_PLATFORM_COPY_TASK_FILES,
    AGENT_PLATFORM_OPEN_PATH,
    AGENT_PLATFORM_INTEGRATION_GET,
    AGENT_PLATFORM_SET_AUTOSTART,
    AGENT_PLATFORM_SET_FILE_ASSOCIATION,
    AGENT_PLATFORM_SET_URL_PROTOCOL,
    AGENT_PLATFORM_FILE_ICON,
    AGENT_PLATFORM_SET_APP_ICON,
    AGENT_PLATFORM_APP_ICON_IMAGE,
    AGENT_PLATFORM_SYNC_WINDOW_ICONS,
    AGENT_SOUND_PREVIEW,
    AGENT_CAPTURE_SUBMIT,
    AGENT_CAPTURE_SUBMIT_TORRENT_FILE,
    AGENT_CAPTURE_LIST,
    AGENT_CAPTURE_RESOLVE,
    AGENT_CAPTURE_PREVIEW,
    AGENT_CAPTURE_CREATE_GROUP,
    AGENT_PLUGIN_INSTALL_FILE,
    AGENT_DIAGNOSTICS_RUN,
    AGENT_DIAGNOSTICS_REPAIR,
    AGENT_DIAGNOSTICS_LOG_PATHS,
    AGENT_DIAGNOSTICS_EXPORT_LOGS,
    AGENT_UPDATE_CHECK,
    AGENT_UPDATE_DOWNLOAD,
    AGENT_UPDATE_INSTALL,
    AGENT_UPDATE_CANCEL,
    AGENT_POWER_ARM,
    AGENT_POWER_DISARM,
    SERVICE_EVENT,
];

/// 会等待网络、下载或外部 IO 的 daemon 方法。daemon 对这些方法按请求并发处理，
/// agent 网关也把它们放到独立于普通 daemon 命令的通道，避免长耗时调用堵住暂停/恢复/设置写入。
pub const SLOW_DAEMON_METHODS: &[&str] = &[
    DAEMON_COMPONENT_LIST_VERSIONS,
    DAEMON_COMPONENT_INSTALL,
    DAEMON_PLUGIN_AUTH,
    DAEMON_PLUGIN_ARCHIVE_RULES,
    DAEMON_PLUGIN_INSTALL,
    DAEMON_PLUGIN_INSTALL_DEV,
    DAEMON_PLUGIN_MARKET_LIST,
    DAEMON_PLUGIN_MARKET_INSTALL,
    DAEMON_PLUGIN_INSTALL_PRIVATE,
    DAEMON_PLUGIN_VIEW_OPEN,
    DAEMON_PLUGIN_COMMAND,
    DAEMON_PLUGIN_INVOKE,
    DAEMON_PLUGIN_PAGE_INVOKE,
    DAEMON_GROUP_RESOLVE_PREVIEW,
    DAEMON_RSS_VALIDATE,
    DAEMON_LINK_PROBE,
    DAEMON_CONFIG_PROXY_TEST,
    DAEMON_CONFIG_PROXY_ROUTE,
    DAEMON_WEBHOOK_TEST,
    DAEMON_BT_TRACKER_SUBSCRIPTION_REFRESH,
    DAEMON_ED2K_SERVER_SUBSCRIPTION_REFRESH,
    DAEMON_ED2K_NODES_SUBSCRIPTION_REFRESH,
    DAEMON_ED2K_ID_CHECK,
    DAEMON_ED2K_KAD_BOOTSTRAP,
    DAEMON_BT_GUARD_EXPORT,
    DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH,
    DAEMON_BT_GUARD_CLOUD_APPLY,
    DAEMON_FS_LIST,
    DAEMON_DIAGNOSTICS_PROBE,
    DAEMON_DIAGNOSTICS_FIX_COMPONENT,
    DAEMON_TASK_VERIFY,
    DAEMON_TASK_HASH,
    DAEMON_TASK_SEED_LOCAL,
    DAEMON_TASK_MOVE_STORAGE,
    DAEMON_EGRESS_PLAN,
];

/// 是否属于 [`SLOW_DAEMON_METHODS`]。
#[must_use]
pub fn is_slow_daemon_method(method_name: &str) -> bool {
    SLOW_DAEMON_METHODS.contains(&method_name)
}

/// 规范DAEMON_CAPABILITIES。
pub const DAEMON_CAPABILITIES: &[&str] = &[
    CAPABILITY_DAEMON_TASKS,
    CAPABILITY_DAEMON_QUEUES,
    CAPABILITY_DAEMON_GROUPS,
    CAPABILITY_DAEMON_CONFIG,
    CAPABILITY_DAEMON_RSS,
    CAPABILITY_DAEMON_PLUGINS,
    CAPABILITY_DAEMON_COMPONENTS,
    CAPABILITY_DAEMON_WEBHOOKS,
    CAPABILITY_DAEMON_SELECTIONS,
    CAPABILITY_DAEMON_FILES,
];

/// 规范 `AGENT_CAPABILITIES`。
///
/// 声明 agent 可能下发能力的规范全集（用于字面量断言与能力全景索引）。
/// 注意：[`CAPABILITY_AGENT_OPEN_TASK_FILES`] / [`CAPABILITY_AGENT_COPY_TASK_FILES`] 属于连接级条件能力，
/// 仅向本机来源或显式授权连接下发，服务级基线 hello 并不无条件包含它们。
pub const AGENT_CAPABILITIES: &[&str] = &[
    CAPABILITY_AGENT_GATEWAY,
    CAPABILITY_AGENT_AUTH,
    CAPABILITY_AGENT_SYNC,
    CAPABILITY_AGENT_REMOTE_TASKS,
    CAPABILITY_AGENT_BILLING,
    CAPABILITY_AGENT_REFERRALS,
    CAPABILITY_AGENT_DEVICE_LINK,
    CAPABILITY_AGENT_EXTERNAL_CAPTURE,
    CAPABILITY_AGENT_OPEN_TASK_FILES,
    CAPABILITY_AGENT_COPY_TASK_FILES,
];

/// 规范CLIENT_CAPABILITIES。
pub const CLIENT_CAPABILITIES: &[&str] = &[CAPABILITY_CLIENT_SELECTIONS];
