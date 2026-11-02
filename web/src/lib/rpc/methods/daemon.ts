// daemon.* 方法：每个包装器接收与 wire 完全一致的 params 对象，返回 wire 结果。
// 连到 agent 的 /rpc 时，agent 会把 `daemon.*` 原样转发给 daemon。

import { call } from '../client';
import { normalizeTask } from '../normalize';
import { METHOD } from '../protocol';
import type {
  BtGuardEntriesAddParams,
  BtGuardEntriesListDto,
  BtGuardEntriesListParams,
  BtGuardEntriesRemoveDto,
  BtGuardEntriesRemoveParams,
  BtGuardEntryDto,
  BtGuardEventsListDto,
  BtGuardEventsListParams,
  BtGuardExportDto,
  BtGuardExportParams,
  BtGuardFilterSourcesRefreshDto,
  BtGuardStateDto,
  ComponentInstallParams,
  ComponentParams,
  ComponentStatusDto,
  ComponentVersions,
  ConnPolicySummaryDto,
  CreateGroupRequest,
  CreateGroupResponse,
  CreatedTask,
  SeedLocalResult,
  DaemonConfigPatch,
  DaemonConfigSnapshot,
  DaemonCreateTasksParams,
  DaemonCreateTasksResult,
  DaemonCreateTaskParams,
  DaemonDiagnosticsDescribe,
  DaemonRuntimeStatsDto,
  Ed2kServerSubRefreshResponse,
  FsListParams,
  FsListResponse,
  GroupDeleteParams,
  GroupDto,
  GroupIdParams,
  InstalledPlugin,
  MarketEntryDto,
  OkResult,
  PluginAuthRequest,
  PluginAuthResponse,
  PluginDto,
  PluginAuditClearParams,
  PluginAuditListDto,
  PluginAuditListParams,
  PluginAuditRecordParams,
  PluginConsentParams,
  PluginSniffRulesDto,
  PluginCommandParams,
  PluginInvokeParams,
  PluginViewEventParams,
  PluginViewKeepaliveParams,
  PluginViewOpenParams,
  PluginViewOpenedDto,
  PluginViewSessionParams,
  PluginViewSnapshotDto,
  PluginIdentityParams,
  PluginInstallDevParams,
  PluginInstallParams,
  PluginMarketInstallParams,
  PluginSetEnabledParams,
  PluginSetSiteDefaultParams,
  PluginSiteDefaultDto,
  PluginUpdateSettingsParams,
  LinkProbeParams,
  LinkProbeResult,
  PrepareLogExportResult,
  ProxyRouteRequest,
  ProxyRouteResponse,
  ProxyTestRequest,
  ProxyTestResponse,
  QueueDto,
  QueueIdParams,
  QueueMoveTaskParams,
  QueueReorderParams,
  QueueScheduleParams,
  QueueUpdateParams,
  QueueInput,
  ResolvePreviewRequest,
  ResolvePreviewResponse,
  RssCreateResult,
  RssItemActionParams,
  RssItemDto,
  RssSourceDto,
  RssSourceIdParams,
  RssSourceInput,
  RssValidateRequest,
  RssValidateResponse,
  SelectionResolutionDto,
  SetSeedLimitsParams,
  TaskVerifyResult,
  TorrentShareDto,
  SiteAuthCredentialDto,
  SiteAuthDeleteParams,
  SiteAuthEntryDto,
  SiteAuthGetParams,
  SiteAuthMatchParams,
  SiteAuthSaveParams,
  SystemProxyDto,
  TaskActivityPage,
  TaskActivityQuery,
  TaskSpeedHistoryDto,
  TaskSpeedHistoryQuery,
  BtStatusDto,
  TaskDeleteManyParams,
  TaskDeleteParams,
  TaskChangeUrlParams,
  TaskSetSegmentsParams,
  TaskDtoWire,
  DaemonSeedLocalParams,
  DaemonTorrentShareParams,
  TaskIdParams,
  TaskIdsParams,
  TaskRenameParams,
  TrackerSubRefreshResponse,
  WebhookDeliveriesResponse,
  WebhookEndpointDraft,
  WebhookSimulateResponse,
  WebhookTestResponse,
} from '../protocol';

/** 服务端逐源串行拉取镜像（每源 20s），最坏远超默认 30s。 */
const SLOW_MIRROR_TIMEOUT_MS = 120_000;

/** BT 完整性校验需读盘比对全部分块，耗时随数据量增长。 */
const VERIFY_TIMEOUT_MS = 6 * 60 * 60 * 1000;

// `daemon.task.list` 在任务数超过 5000 时服务端报错（需分页），Web 不再封装：任务表一律来自
// store 的快照 + `system.taskPage` 载入。
const task = {
  get: async (params: TaskIdParams) =>
    normalizeTask(await call<TaskDtoWire>(METHOD.DAEMON_TASK_GET, params)),
  activity: (params: TaskActivityQuery) =>
    call<TaskActivityPage>(METHOD.DAEMON_TASK_ACTIVITY, params),
  speedHistory: (params: TaskSpeedHistoryQuery) =>
    call<TaskSpeedHistoryDto | null>(METHOD.DAEMON_TASK_SPEED_HISTORY, params),
  btStatus: (params: TaskIdParams) => call<BtStatusDto>(METHOD.DAEMON_TASK_BT_STATUS, params),
  create: (params: DaemonCreateTaskParams) => call<CreatedTask>(METHOD.DAEMON_TASK_CREATE, params),
  /** 批量建任务：一块（≤512 条）共享同一份选项，整块一个事务，按 `batchId` 幂等。 */
  createMany: (params: DaemonCreateTasksParams) =>
    call<DaemonCreateTasksResult>(METHOD.DAEMON_TASK_CREATE_MANY, params),
  pause: (params: TaskIdParams) => call<OkResult>(METHOD.DAEMON_TASK_PAUSE, params),
  resume: (params: TaskIdParams) => call<OkResult>(METHOD.DAEMON_TASK_RESUME, params),
  rename: (params: TaskRenameParams) => call<OkResult>(METHOD.DAEMON_TASK_RENAME, params),
  changeUrl: (params: TaskChangeUrlParams) =>
    call<OkResult>(METHOD.DAEMON_TASK_CHANGE_URL, params),
  /** 修改线程数：已下进度保留；下载中任务由引擎静默暂停 → 改配置 → 恢复。 */
  setSegments: (params: TaskSetSegmentsParams) =>
    call<OkResult>(METHOD.DAEMON_TASK_SET_SEGMENTS, params),
  delete: (params: TaskDeleteParams) => call<OkResult>(METHOD.DAEMON_TASK_DELETE, params),
  pauseAll: () => call<OkResult>(METHOD.DAEMON_TASK_PAUSE_ALL),
  resumeAll: () => call<OkResult>(METHOD.DAEMON_TASK_RESUME_ALL),
  /** 重新扫描已完成任务的目标文件是否仍存在。 */
  rescan: () => call<OkResult>(METHOD.DAEMON_TASK_RESCAN),
  setSeedLimits: (params: SetSeedLimitsParams) =>
    call<OkResult>(METHOD.DAEMON_TASK_SET_SEED_LIMITS, params),
  /** 本机做种：生成种子并哈希校验全部数据，耗时与数据量成正比；使用 6 小时超时，创建类提交不重试。 */
  seedLocal: (params: DaemonSeedLocalParams) =>
    call<SeedLocalResult>(METHOD.DAEMON_TASK_SEED_LOCAL, params, { timeoutMs: VERIFY_TIMEOUT_MS }),
  /** BT 任务的磁力链接（含 tracker）与可选 .torrent 字节（base64）。 */
  torrentShare: (params: DaemonTorrentShareParams) =>
    call<TorrentShareDto>(METHOD.DAEMON_TASK_TORRENT_SHARE, params),
  /** BT 完整性校验：耗时与数据量成正比，使用 6 小时超时。 */
  verify: (params: TaskIdParams) =>
    call<TaskVerifyResult>(METHOD.DAEMON_TASK_VERIFY, params, { timeoutMs: VERIFY_TIMEOUT_MS }),
  /** 批量操作：一次请求、整批只推一次任务快照（逐条调用会撞上 agent 的通道上限）。 */
  pauseMany: (taskIds: string[]) => {
    const params: TaskIdsParams = { taskIds };
    return call<OkResult>(METHOD.DAEMON_TASK_PAUSE_MANY, params);
  },
  resumeMany: (taskIds: string[]) => {
    const params: TaskIdsParams = { taskIds };
    return call<OkResult>(METHOD.DAEMON_TASK_RESUME_MANY, params);
  },
  deleteMany: (taskIds: string[], deleteFiles = false) => {
    const params: TaskDeleteManyParams = { taskIds, deleteFiles };
    return call<OkResult>(METHOD.DAEMON_TASK_DELETE_MANY, params);
  },
};

const queue = {
  list: () => call<QueueDto[]>(METHOD.DAEMON_QUEUE_LIST),
  create: (params: QueueInput) => call<OkResult>(METHOD.DAEMON_QUEUE_CREATE, params),
  update: (params: QueueUpdateParams) => call<OkResult>(METHOD.DAEMON_QUEUE_UPDATE, params),
  delete: (params: QueueIdParams) => call<OkResult>(METHOD.DAEMON_QUEUE_DELETE, params),
  start: (params: QueueIdParams) => call<OkResult>(METHOD.DAEMON_QUEUE_START, params),
  stop: (params: QueueIdParams) => call<OkResult>(METHOD.DAEMON_QUEUE_STOP, params),
  schedule: (params: QueueScheduleParams) => call<OkResult>(METHOD.DAEMON_QUEUE_SCHEDULE, params),
  reorder: (params: QueueReorderParams) => call<OkResult>(METHOD.DAEMON_QUEUE_REORDER, params),
  moveTask: (params: QueueMoveTaskParams) => call<OkResult>(METHOD.DAEMON_QUEUE_MOVE_TASK, params),
  /** 设为优先任务（Boost；wire 参数是 `{ id: 任务 ID }`，同 id 再次调用即取消）。 */
  boost: (params: { id: string }) => call<OkResult>(METHOD.DAEMON_QUEUE_BOOST, params),
};

const group = {
  list: () => call<GroupDto[]>(METHOD.DAEMON_GROUP_LIST),
  resolvePreview: (params: ResolvePreviewRequest) =>
    call<ResolvePreviewResponse>(METHOD.DAEMON_GROUP_RESOLVE_PREVIEW, params),
  create: (params: CreateGroupRequest) =>
    call<CreateGroupResponse>(METHOD.DAEMON_GROUP_CREATE, params),
  pause: (params: GroupIdParams) => call<OkResult>(METHOD.DAEMON_GROUP_PAUSE, params),
  resume: (params: GroupIdParams) => call<OkResult>(METHOD.DAEMON_GROUP_RESUME, params),
  delete: (params: GroupDeleteParams) => call<OkResult>(METHOD.DAEMON_GROUP_DELETE, params),
};

const config = {
  get: () => call<DaemonConfigSnapshot>(METHOD.DAEMON_CONFIG_GET),
  /** 乐观并发：`expectedRevision` 落后时 conflict，`error.data.revision` 为当前版本。 */
  patch: (params: DaemonConfigPatch) =>
    call<DaemonConfigSnapshot>(METHOD.DAEMON_CONFIG_PATCH, params),
  proxyTest: (params: ProxyTestRequest) =>
    call<ProxyTestResponse>(METHOD.DAEMON_CONFIG_PROXY_TEST, params),
  proxyRoute: (params: ProxyRouteRequest) =>
    call<ProxyRouteResponse>(METHOD.DAEMON_CONFIG_PROXY_ROUTE, params),
  connPolicy: () => call<ConnPolicySummaryDto>(METHOD.DAEMON_CONFIG_CONN_POLICY),
  clearConnPolicy: () => call<ConnPolicySummaryDto>(METHOD.DAEMON_CONFIG_CLEAR_CONN_POLICY),
  systemProxy: () => call<SystemProxyDto>(METHOD.DAEMON_CONFIG_SYSTEM_PROXY),
};

const siteAuth = {
  list: () => call<SiteAuthEntryDto[]>(METHOD.DAEMON_SITE_AUTH_LIST),
  /** 返回删除后的脱敏列表。 */
  delete: (params: SiteAuthDeleteParams) =>
    call<SiteAuthEntryDto[]>(METHOD.DAEMON_SITE_AUTH_DELETE, params),
  /** 含明文密码；无匹配返回 null。 */
  get: (params: SiteAuthGetParams) =>
    call<SiteAuthCredentialDto | null>(METHOD.DAEMON_SITE_AUTH_GET, params),
  /** 保存单站点凭据，返回脱敏后的条目。 */
  save: (params: SiteAuthSaveParams) =>
    call<SiteAuthEntryDto>(METHOD.DAEMON_SITE_AUTH_SAVE, params),
  /** 返回清空后的脱敏列表（空数组）。 */
  clear: () => call<SiteAuthEntryDto[]>(METHOD.DAEMON_SITE_AUTH_CLEAR),
  /** 含明文密码，仅供表单回填；无匹配返回 null。 */
  match: (params: SiteAuthMatchParams) =>
    call<SiteAuthCredentialDto | null>(METHOD.DAEMON_SITE_AUTH_MATCH, params),
};

const link = {
  /** 剪贴板识别用的只读 HEAD 探测（慢方法，上限 1.5s）。 */
  probe: (params: LinkProbeParams) => call<LinkProbeResult>(METHOD.DAEMON_LINK_PROBE, params),
};

const runtime = {
  stats: () => call<DaemonRuntimeStatsDto>(METHOD.DAEMON_RUNTIME_STATS),
};

const fs = {
  /** 列举服务端目录（仅子目录）；`params` 省略 = 默认保存目录。 */
  list: (params?: FsListParams) => call<FsListResponse>(METHOD.DAEMON_FS_LIST, params),
};

const rss = {
  listSources: () => call<RssSourceDto[]>(METHOD.DAEMON_RSS_LIST_SOURCES),
  getItems: (params: RssSourceIdParams) =>
    call<RssItemDto[]>(METHOD.DAEMON_RSS_GET_ITEMS, params),
  createSource: (params: RssSourceInput) =>
    call<RssCreateResult>(METHOD.DAEMON_RSS_CREATE_SOURCE, params),
  /** 必须带非空 `sourceId`。 */
  updateSource: (params: RssSourceInput & { sourceId: string }) =>
    call<OkResult>(METHOD.DAEMON_RSS_UPDATE_SOURCE, params),
  deleteSource: (params: RssSourceIdParams) =>
    call<OkResult>(METHOD.DAEMON_RSS_DELETE_SOURCE, params),
  refreshSource: (params: RssSourceIdParams) =>
    call<OkResult>(METHOD.DAEMON_RSS_REFRESH_SOURCE, params),
  itemAction: (params: RssItemActionParams) =>
    call<OkResult>(METHOD.DAEMON_RSS_ITEM_ACTION, params),
  validate: (params: RssValidateRequest) =>
    call<RssValidateResponse>(METHOD.DAEMON_RSS_VALIDATE, params),
};

const plugin = {
  list: () => call<PluginDto[]>(METHOD.DAEMON_PLUGIN_LIST),
  sniffRules: () => call<PluginSniffRulesDto>(METHOD.DAEMON_PLUGIN_SNIFF_RULES),
  auth: (params: PluginAuthRequest) =>
    call<PluginAuthResponse>(METHOD.DAEMON_PLUGIN_AUTH, params),
  setEnabled: (params: PluginSetEnabledParams) =>
    call<OkResult>(METHOD.DAEMON_PLUGIN_SET_ENABLED, params),
  updateSettings: (params: PluginUpdateSettingsParams) =>
    call<OkResult>(METHOD.DAEMON_PLUGIN_UPDATE_SETTINGS, params),
  /** `blobId` 需先经 daemon blob 上传端点获得。 */
  install: (params: PluginInstallParams) =>
    call<InstalledPlugin>(METHOD.DAEMON_PLUGIN_INSTALL, params),
  /** 从 daemon 主机上的目录安装开发版插件。 */
  installDev: (params: PluginInstallDevParams) =>
    call<InstalledPlugin>(METHOD.DAEMON_PLUGIN_INSTALL_DEV, params, { timeoutMs: SLOW_MIRROR_TIMEOUT_MS }),
  /** 重新加载开发版插件（重读 manifest 与源码并校验；失败保留登记）。 */
  reloadDev: (params: PluginIdentityParams) =>
    call<InstalledPlugin>(METHOD.DAEMON_PLUGIN_RELOAD_DEV, params, { timeoutMs: SLOW_MIRROR_TIMEOUT_MS }),
  uninstall: (params: PluginIdentityParams) =>
    call<OkResult>(METHOD.DAEMON_PLUGIN_UNINSTALL, params),
  marketList: () => call<MarketEntryDto[]>(METHOD.DAEMON_PLUGIN_MARKET_LIST, undefined, { timeoutMs: SLOW_MIRROR_TIMEOUT_MS }),
  marketInstall: (params: PluginMarketInstallParams) =>
    call<InstalledPlugin>(METHOD.DAEMON_PLUGIN_MARKET_INSTALL, params, { timeoutMs: SLOW_MIRROR_TIMEOUT_MS }),
  /** 忽略插件解析失败并重试（清除任务 resolver 后恢复任务）。 */
  ignoreRetry: (params: TaskIdParams) => call<OkResult>(METHOD.DAEMON_PLUGIN_IGNORE_RETRY, params),
  /** 站点默认插件列表（按 site 排序）。 */
  siteDefaults: () => call<PluginSiteDefaultDto[]>(METHOD.DAEMON_PLUGIN_SITE_DEFAULTS),
  /** 确认 / 变更站点默认插件；`identity: null` 移除条目。 */
  setSiteDefault: (params: PluginSetSiteDefaultParams) => call<OkResult>(METHOD.DAEMON_PLUGIN_SET_SITE_DEFAULT, params),
  /** 授权插件请求的全部能力；集合须与请求完全一致。 */
  consent: (params: PluginConsentParams) => call<OkResult>(METHOD.DAEMON_PLUGIN_CONSENT, params),
  auditList: (params: PluginAuditListParams = {}) =>
    call<PluginAuditListDto>(METHOD.DAEMON_PLUGIN_AUDIT_LIST, params),
  auditClear: (params: PluginAuditClearParams = {}) =>
    call<OkResult>(METHOD.DAEMON_PLUGIN_AUDIT_CLEAR, params),
  auditRecord: (params: PluginAuditRecordParams) =>
    call<OkResult>(METHOD.DAEMON_PLUGIN_AUDIT_RECORD, params),
  /** 打开可移植视图会话（可能激活常驻宿主，按慢方法超时）。 */
  viewOpen: (params: PluginViewOpenParams) =>
    call<PluginViewOpenedDto>(METHOD.DAEMON_PLUGIN_VIEW_OPEN, params, { timeoutMs: SLOW_MIRROR_TIMEOUT_MS }),
  /** 投递视图事件；入队即返回，结果经 `pluginViewFrame` 事件送出。 */
  viewEvent: (params: PluginViewEventParams) =>
    call<OkResult>(METHOD.DAEMON_PLUGIN_VIEW_EVENT, params),
  viewClose: (params: PluginViewSessionParams) =>
    call<OkResult>(METHOD.DAEMON_PLUGIN_VIEW_CLOSE, params),
  /** 取会话当前 `{seq, tree}`（序号断档 / 重连后重新同步）。 */
  viewSnapshot: (params: PluginViewSessionParams) =>
    call<PluginViewSnapshotDto>(METHOD.DAEMON_PLUGIN_VIEW_SNAPSHOT, params),
  /** 会话续期；90 s 无续期的会话会被关闭。 */
  viewKeepalive: (params: PluginViewKeepaliveParams) =>
    call<OkResult>(METHOD.DAEMON_PLUGIN_VIEW_KEEPALIVE, params),
  command: (params: PluginCommandParams) =>
    call<OkResult>(METHOD.DAEMON_PLUGIN_COMMAND, params, { timeoutMs: SLOW_MIRROR_TIMEOUT_MS }),
  invoke: (params: PluginInvokeParams) =>
    call<unknown>(METHOD.DAEMON_PLUGIN_INVOKE, params, { timeoutMs: SLOW_MIRROR_TIMEOUT_MS }),
};

const component = {
  get: (params: ComponentParams) =>
    call<ComponentStatusDto>(METHOD.DAEMON_COMPONENT_GET, params),
  listVersions: (params: ComponentParams) =>
    call<ComponentVersions>(METHOD.DAEMON_COMPONENT_LIST_VERSIONS, params),
  /** 长耗时；进度经 `componentProgress` / `componentResult` 引擎事件推送。 */
  install: (params: ComponentInstallParams) =>
    call<OkResult>(METHOD.DAEMON_COMPONENT_INSTALL, params),
  uninstall: (params: ComponentParams) => call<OkResult>(METHOD.DAEMON_COMPONENT_UNINSTALL, params),
};

const selection = {
  subscribe: () => call<OkResult>(METHOD.DAEMON_SELECTION_SUBSCRIBE),
  unsubscribe: () => call<OkResult>(METHOD.DAEMON_SELECTION_UNSUBSCRIBE),
  resolve: (params: SelectionResolutionDto) =>
    call<OkResult>(METHOD.DAEMON_SELECTION_RESOLVE, params),
};

const webhook = {
  get: () => call<WebhookDeliveriesResponse>(METHOD.DAEMON_WEBHOOK_GET),
  clearDeliveries: () => call<OkResult>(METHOD.DAEMON_WEBHOOK_CLEAR_DELIVERIES),
  simulate: () => call<WebhookSimulateResponse>(METHOD.DAEMON_WEBHOOK_SIMULATE),
  test: (params: WebhookEndpointDraft) =>
    call<WebhookTestResponse>(METHOD.DAEMON_WEBHOOK_TEST, params),
};

const bt = {
  trackerSubscription: {
    refresh: () =>
      call<TrackerSubRefreshResponse>(METHOD.DAEMON_BT_TRACKER_SUBSCRIPTION_REFRESH, undefined, { timeoutMs: SLOW_MIRROR_TIMEOUT_MS }),
  },
  /** 防吸血。`cloud.apply` / `evidence.drain` 是 agent 内部方法，这里刻意不封装。 */
  guard: {
    state: () => call<BtGuardStateDto>(METHOD.DAEMON_BT_GUARD_STATE),
    entries: {
      list: (params: BtGuardEntriesListParams) => call<BtGuardEntriesListDto>(METHOD.DAEMON_BT_GUARD_ENTRIES_LIST, params),
      add: (params: BtGuardEntriesAddParams) => call<BtGuardEntryDto>(METHOD.DAEMON_BT_GUARD_ENTRIES_ADD, params),
      remove: (params: BtGuardEntriesRemoveParams) => call<BtGuardEntriesRemoveDto>(METHOD.DAEMON_BT_GUARD_ENTRIES_REMOVE, params),
    },
    events: {
      list: (params: BtGuardEventsListParams) => call<BtGuardEventsListDto>(METHOD.DAEMON_BT_GUARD_EVENTS_LIST, params),
    },
    export: (params: BtGuardExportParams) => call<BtGuardExportDto>(METHOD.DAEMON_BT_GUARD_EXPORT, params),
    /** 立即抓取全部过滤文件订阅（逐个 HTTP 拉取，可能较慢）。 */
    filterSourcesRefresh: () =>
      call<BtGuardFilterSourcesRefreshDto>(METHOD.DAEMON_BT_GUARD_FILTER_SOURCES_REFRESH, undefined, { timeoutMs: SLOW_MIRROR_TIMEOUT_MS }),
  },
};

const ed2k = {
  serverSubscription: {
    refresh: () =>
      call<Ed2kServerSubRefreshResponse>(METHOD.DAEMON_ED2K_SERVER_SUBSCRIPTION_REFRESH, undefined, { timeoutMs: SLOW_MIRROR_TIMEOUT_MS }),
  },
};

const diagnostics = {
  describe: () => call<DaemonDiagnosticsDescribe>(METHOD.DAEMON_DIAGNOSTICS_DESCRIBE),
  prepareLogExport: () => call<PrepareLogExportResult>(METHOD.DAEMON_DIAGNOSTICS_PREPARE_LOG_EXPORT),
};

export const daemon = {
  link,
  task,
  queue,
  group,
  config,
  siteAuth,
  runtime,
  fs,
  rss,
  plugin,
  component,
  selection,
  webhook,
  bt,
  ed2k,
  diagnostics,
};
