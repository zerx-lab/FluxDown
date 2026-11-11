// 各方法的 params / 非 DTO 结果类型（按 wire 名归类）。
// daemon 端的 id 参数在服务端带别名（`id` / `taskId` / `queueId` / `groupId` 等价），这里统一用语义名。

import type { PowerAction, RemoteTaskDto } from './agent';
import type { JsonValue } from './common';
import type { ComponentKind, ComponentStatusDto } from './plugin';
import type { ServiceHello } from './rpc';
import type { CreateTaskRequest, DownloadRequest } from './task';

// ── daemon.task ──

export interface TaskIdParams {
  taskId: string;
}

/** `daemon.task.seedLocal`：把守护进程所在机器上已有的文件 / 目录登记为已完成 BT 任务并做种。 */
export interface DaemonSeedLocalParams {
  /** 守护进程所在机器上的绝对路径（文件或目录）。 */
  path: string;
  /** 已有 .torrent 的绝对路径；省略 = 由守护进程生成。 */
  torrentPath?: string;
  /** 生成种子时写入的 announce 地址。 */
  trackers?: string[];
  /** true = 不因分享率 / 时长 / 不活跃限制而停止做种。 */
  unlimited: boolean;
  /** 生成的种子格式（缺省 `v1`）；给了 `torrentPath` 时忽略。 */
  version?: SeedTorrentVersion;
  /** 生成私有种子。 */
  private?: boolean;
  /** 分片大小（字节，2 的幂且 >= 16384）；0 / 缺省 = 自动。 */
  pieceLength?: number;
  /** 写入种子的 HTTP 源（BEP 19 `url-list`）。 */
  webSeeds?: string[];
  /** 写入种子的注释。 */
  comment?: string;
  /** 写入 info 字典的来源标记（会改变 info hash）。 */
  source?: string;
  /** 以超级做种模式做种。 */
  superSeeding?: boolean;
  /** 只在给了 `torrentPath` 时有意义：信任数据完整，跳过哈希校验。 */
  skipCheck?: boolean;
  /** 非空 = 同时把生成的 `.torrent` 写到守护进程主机的这个绝对路径。 */
  torrentOutput?: string;
  /** true = 只生成种子文件、不建任务（必须给 `torrentOutput`，结果 `taskId` 为空串）。 */
  createOnly?: boolean;
}

/** 本机做种生成的种子格式（`DaemonSeedLocalParams.version`）。 */
export type SeedTorrentVersion = 'v1' | 'v2' | 'hybrid';

/** `daemon.task.torrentShare`：取 BT 任务的磁力链接，可选同时带回 .torrent 字节。 */
export interface DaemonTorrentShareParams {
  taskId: string;
  includeTorrent: boolean;
}

export interface TaskRenameParams {
  taskId: string;
  /** 不含路径分隔符；引擎侧校验非法字符与状态。 */
  fileName: string;
}

export interface TaskChangeUrlParams {
  taskId: string;
  /** 新下载地址（http(s)/ftp，或待解封装的 `thunder://` 链接）。 */
  url: string;
}

/** `daemon.task.setSegments`：`segments <= 0` = 自动，上限 `MAX_TASK_SEGMENTS`（512）。 */
export interface TaskSetSegmentsParams {
  taskId: string;
  segments: number;
}

/** `daemon.task.setPiecePicking`：省略的字段保持不变。 */
export interface TaskSetPiecePickingParams {
  taskId: string;
  sequential?: boolean;
  firstLastPiecesFirst?: boolean;
}

/** BT 文件优先级（`BT_FILE_PRIORITY_*`，对齐 qBittorrent）：0 不下载 / 1 普通 / 6 高 / 7 最高。 */
export const BT_FILE_PRIORITY_SKIP = 0;
export const BT_FILE_PRIORITY_NORMAL = 1;
export const BT_FILE_PRIORITY_HIGH = 6;
export const BT_FILE_PRIORITY_MAXIMUM = 7;

/** `daemon.task.setFilePriorities` 的一项（`BtFilePriorityDto`）。 */
export interface BtFilePriorityDto {
  index: number;
  priority: number;
}

/** `daemon.task.setFilePriorities`：只改列出的文件，其余不变。 */
export interface SetTaskFilePrioritiesParams {
  taskId: string;
  priorities: BtFilePriorityDto[];
}

/** `daemon.task.renameBtFile`：`path` 是任务目录内的新相对路径（`/` 分隔）。 */
export interface RenameBtFileParams {
  taskId: string;
  index: number;
  path: string;
}

/** `daemon.task.moveStorage`：`saveDir` 是守护进程主机上的绝对路径。 */
export interface MoveTaskStorageParams {
  taskId: string;
  saveDir: string;
}

/** `daemon.task.setBtLimits`：字节/秒，0 = 不限；缺省字段保持不变。 */
export interface SetTaskBtLimitsParams {
  taskId: string;
  downloadLimitBps?: number;
  uploadLimitBps?: number;
}

/** `daemon.task.setSuperSeeding`。 */
export interface SetTaskSuperSeedingParams {
  taskId: string;
  enabled: boolean;
}

export interface TaskDeleteParams {
  taskId: string;
  /** 同时删除磁盘文件，缺省 false。 */
  deleteFiles?: boolean;
}

/** `daemon.task.pauseMany` / `daemon.task.resumeMany`：未知 id 忽略，空列表为空操作。 */
export interface TaskIdsParams {
  taskIds: string[];
}

/** `daemon.task.deleteMany`：语义同 {@link TaskIdsParams}。 */
export interface TaskDeleteManyParams {
  taskIds: string[];
  /** 同时删除磁盘文件，缺省 false。 */
  deleteFiles?: boolean;
}

/** `daemon.task.setSeedLimits` 全部字段必填；哨兵见 `SEED_LIMIT_INHERIT` / `SEED_LIMIT_UNLIMITED`。 */
export interface SetSeedLimitsParams {
  taskId: string;
  /** 总分享率上限（千分比）。 */
  ratioLimitMilli: number;
  /** 做种后分享率上限（千分比）。 */
  postRatioLimitMilli: number;
  seedTimeLimitMinutes: number;
  inactiveTimeLimitMinutes: number;
  /** 任务级做种上传限速（B/s，0 = 不限），下次 torrent add 时生效。 */
  uploadLimitBps: number;
}

// ── daemon.queue / daemon.group ──

export interface QueueIdParams {
  queueId: string;
}

export interface QueueUpdateParams {
  queueId: string;
  name: string;
  speedLimitKbps?: number;
  uploadLimitKbps?: number;
  maxConcurrent?: number;
  defaultSaveDir?: string;
  defaultSegments?: number;
  defaultUserAgent?: string;
}

export interface QueueScheduleParams {
  queueId: string;
  enabled: boolean;
  /** `HH:MM`，空 = 不定时启动。 */
  startTime?: string;
  /** `HH:MM`，空 = 不定时停止。 */
  stopTime?: string;
  /** 星期位掩码（bit0 = 周一 … bit6 = 周日）；0 / 缺省 = 每天。 */
  days?: number;
}

export interface QueueReorderParams {
  queueId: string;
  /** 队列内任务的完整新顺序。 */
  taskIds: string[];
}

export interface QueueMoveTaskParams {
  taskId: string;
  /** 空 = 默认队列。 */
  queueId?: string;
}

export interface GroupIdParams {
  groupId: string;
}

export interface GroupDeleteParams {
  groupId: string;
  deleteFiles?: boolean;
}

// ── daemon.config / siteAuth / fs ──

export interface SiteAuthDeleteParams {
  /** `host` 或 `host:port`。 */
  site: string;
}

export interface SiteAuthMatchParams {
  /** 下载链接；按其站点键匹配。 */
  url: string;
}

export interface SiteAuthGetParams {
  /** `host` / `host:port` 或完整 URL，服务端归一化后查询；无匹配结果为 null。 */
  site: string;
}

/** `daemon.siteAuth.save`：`user` 不可为空；返回脱敏后的 `SiteAuthEntryDto`。 */
export interface SiteAuthSaveParams {
  site: string;
  user: string;
  pass: string;
}

export interface FsListParams {
  /** 空 / 省略 = 默认保存目录。 */
  path?: string;
}

// ── daemon.rss ──

export interface RssSourceIdParams {
  sourceId: string;
}

export interface RssItemActionParams {
  sourceId: string;
  /** `action = "readAll"` 时忽略。 */
  guid?: string;
  /** `download`（绕过规则强制下载） / `ignore` / `readAll`。 */
  action: 'download' | 'ignore' | 'readAll';
}

/** `daemon.rss.createSource` 结果。 */
export interface RssCreateResult {
  sourceId: string;
}

// ── daemon.plugin / component ──

export interface PluginIdentityParams {
  identity: string;
}

export interface PluginSetEnabledParams {
  identity: string;
  enabled: boolean;
}

/** `daemon.plugin.setSiteDefault` 参数；`identity` 为 null = 移除条目（下次下载重新询问）。 */
export interface PluginSetSiteDefaultParams {
  site: string;
  identity: string | null;
}

export interface PluginUpdateSettingsParams {
  identity: string;
  /** 设置键 → 字符串值。 */
  entries: Record<string, string>;
}

/** `daemon.plugin.install`：`blobId` 是先经 daemon blob 上传端点得到的一次性引用。 */
export interface PluginInstallParams {
  blobId: string;
}

export interface PluginInstallDevParams {
  /** daemon 所在主机上的插件目录路径。 */
  dirPath: string;
}

export interface PluginMarketInstallParams {
  pluginId: string;
  /** 用户确认权限时看到的版本；最新可装版本不一致时返回 conflict（`marketVersionChanged`）。 */
  version?: string | null;
}

export interface ComponentParams {
  component: ComponentKind;
}

export interface ComponentInstallParams {
  component: ComponentKind;
  /** 钉住版本；省略 = 最新稳定版。 */
  version?: string | null;
}

// ── daemon.diagnostics ──

/** `daemon.diagnostics.describe` 结果。 */
export interface DaemonDiagnosticsDescribe {
  service: ServiceHello;
  /** 任务 / 队列 / 任务组数量。 */
  tasks: number;
  queues: number;
  groups: number;
  configRevision: number;
  logDir: string;
  components: ComponentStatusDto[];
}

/** `daemon.diagnostics.prepareLogExport` 结果：一次性 blob 引用，经 blob 下载端点取回。 */
export interface PrepareLogExportResult {
  exportId: string;
}

// ── agent.auth / profile ──

export interface RegisterParams {
  email: string;
  password: string;
  nickname?: string;
}

export interface RegisterVerifyParams {
  email: string;
  code: string;
}

/** `account` 接受邮箱或纯数字 Origin ID。 */
export interface LoginParams {
  account: string;
  password: string;
}

export interface LoginVerifyParams {
  account: string;
  password: string;
  code: string;
}

export interface SendCodeParams {
  email: string;
}

/** 验证码登录；邮箱不存在则自动注册（此时 `nickname` 生效）。 */
export interface VerifyCodeParams {
  email: string;
  code: string;
  nickname?: string;
}

/** 发送验证码类方法的结果。 */
export interface TtlResult {
  ttlSeconds: number;
}

export interface SendNewEmailCodeParams {
  /** 新邮箱。 */
  email: string;
  /** 原邮箱收到的验证码。 */
  code: string;
}

export interface ChangeEmailParams {
  email: string;
  oldCode: string;
  newCode: string;
}

export interface RandomOriginIdResult {
  originId: number;
}

export interface CheckOriginIdParams {
  value: number;
}

export interface ChangeOriginIdParams {
  /** >= 10000 的整数，全局仅可成功一次。 */
  originId: number;
}

export interface ChangeNicknameParams {
  /** 1-32 字符（服务端 trim 后校验）。 */
  nickname: string;
}

/** 已登录修改 / 设置密码：`currentPassword` 与 `code`（`sendPasswordCode` 发到绑定邮箱）二选一。 */
export interface ChangePasswordParams {
  /** 至少 8 位。 */
  newPassword: string;
  currentPassword?: string;
  code?: string;
}

/** 未登录重置密码：`sendPasswordResetCode` 发到该邮箱的验证码 + 新密码。 */
export interface ResetPasswordParams {
  email: string;
  code: string;
  /** 至少 8 位。 */
  newPassword: string;
}

// ── agent.gateway / device / preferences / power ──

export interface GatewayRevealTokenResult {
  /** 未配置为空串。 */
  userToken: string;
}

/**
 * `agent.gateway.patch` 参数（原子）。`apiEnabled` / `mcpEnabled` 由关转开且 token 仍为空时
 * 服务端自动生成 token；显式清空 token（且本次未开启上述开关）时服务端同时关闭二者。
 */
export interface GatewayPatchParams {
  takeoverEnabled?: boolean;
  jsonrpcEnabled?: boolean;
  apiEnabled?: boolean;
  mcpEnabled?: boolean;
  corsEnabled?: boolean;
  lanEnabled?: boolean;
  /** 1024..=65535；验证新 API/RPC 服务可用后立即切换，固定监听模式拒绝修改。 */
  port?: number;
  /** 空串 = 清除用户 token；省略 = 保持。 */
  userToken?: string;
  /** true 生成新的随机 token（优先于 `userToken`）。 */
  regenerateUserToken?: boolean;
}

export interface DeviceRenameParams {
  /** `CloudDevice.id`（不是 deviceId）。 */
  id: string;
  /** 1-64 字符。 */
  name: string;
}

export interface DeviceIdParams {
  id: string;
}

export interface PreferencesPatchParams {
  values: Record<string, JsonValue>;
  /** 是否参与云同步，缺省 true；false 仅写本机偏好。 */
  sync?: boolean;
}

/** `agent.preferences.patch` 结果：`revision` 为本次写入落定后的偏好版本。 */
export interface PreferencesPatchResult {
  ok: boolean;
  revision: number;
}

export interface PowerArmParams {
  /** 倒计时秒数：没有任务时立即开始，否则全部任务完成后开始（0 = 完成后立即，仅有任务时接受）。 */
  delaySecs: number;
  /** 缺省 = shutdown。 */
  action?: PowerAction;
}

export interface PowerArmResult {
  armed: boolean;
}

// ── agent.remote ──

export interface RemoteDispatchParams {
  /** 目标设备 ID（`CloudDevice.deviceId`）。 */
  toDevice: string;
  url: string;
  fileName?: string;
  /** 省略 = 目标设备默认目录；给出时须为目标路径风格的绝对路径。 */
  saveDir?: string;
}

export interface RemoteDispatchResult {
  task: RemoteTaskDto;
}

export interface RemoteCommandParams {
  taskId: string;
  action: 'pause' | 'resume' | 'delete' | 'cancel';
  /** 幂等键；省略时 agent 生成。 */
  commandId?: string;
  /** 仅 `delete`：目标设备同时删除已下载文件。 */
  deleteFiles?: boolean;
}

export interface SyncLocalOnlyParams {
  keys: string[];
  localOnly: boolean;
}

// ── agent.link ──

export interface LinkDiscoveryParams {
  enabled: boolean;
}

/** `agent.link.stopPairing`：带 `code` 时仅当它仍是当前配对码才作废（多客户端互不干扰）。 */
export interface LinkStopPairingParams {
  code?: string;
}

/** 接受 `host`、`host:port`、`http(s)://host[:port][/base]`。 */
export interface LinkAddressParams {
  address: string;
}

export interface LinkPairBeginParams {
  address: string;
  code: string;
  /** 请求「完全控制」对端；对端批准后其会签发设备控制凭据。 */
  requestControl?: boolean;
}

export interface LinkPairFinishParams {
  token: string;
  accept: boolean;
}

export interface LinkApproveParams {
  sessionId: string;
  accept: boolean;
}

export interface LinkDeviceParams {
  fingerprint: string;
}

export interface LinkRenameParams {
  fingerprint: string;
  /** 省略 / 空白 = 清除备注名。 */
  name?: string;
}

export interface LinkDispatchParams {
  fingerprint: string;
  url: string;
  fileName?: string;
  /** 省略 = 目标设备默认目录。 */
  saveDir?: string;
}

export interface LinkDispatchResult {
  taskId: string;
}

// ── agent.plan / order / referral ──

export interface OrderCreateParams {
  planCode: string;
  deviceId?: string;
  referralCode?: string;
}

export interface OrderGetParams {
  orderNo: string;
}

export interface PageParams {
  /** 从 1 开始，缺省 1。 */
  page?: number;
  /** 1..=100，缺省 20。 */
  pageSize?: number;
}

export interface ReferralListRecordsParams extends PageParams {
  /** 按买家昵称/邮箱子串过滤。 */
  search?: string;
}

export interface ReferralCreateCodeParams {
  /** 空 / 省略 = 服务端随机生成 8 位。 */
  code?: string;
}

export interface ReferralDeleteCodeParams {
  id: string;
}

export interface ReferralValidateParams {
  code: string;
  planCode: string;
}

// ── agent.capture ──

/** 声明请求来自系统默认处理程序的关联；用户已在设置中关闭该关联时请求被忽略。 */
export type OpenAssociation = 'torrent' | 'magnet' | 'ed2k';

export interface CaptureSubmitParams {
  /** 外部下载请求。 */
  request: DownloadRequest;
  /** true = 系统打开的链接：按「免打扰下载」偏好直接建任务，偏好关闭则排入确认队列；缺省 false = 恒排入确认队列。 */
  silent?: boolean;
  association?: OpenAssociation;
}

/**
 * `agent.capture.submit` 结果三选一：
 * `{ taskIds }` 直接建成（批量中途失败则整个调用报错，已建成的任务仍保留）、
 * `{ transactionIds }` 已排入确认队列、`{ ignored: true }` 关联被关闭而忽略。
 */
export type CaptureSubmitResult =
  | { taskIds: (string | null)[] }
  | { transactionIds: string[] }
  | { ignored: true };

export interface CaptureResolveParams {
  transactionId: string;
  accepted: boolean;
  /** 显式指定时，即使目录等于捕获表单的预填分类目录也禁止分类路由。 */
  disableCategoryRoute?: boolean;
  /** 确认时表单产出的建任务参数；省略 = 按捕获原请求建任务。 */
  request?: CreateTaskRequest | null;
}

/** 拒绝时 `{ accepted: false }`；接受时为 `daemon.task.create` 的 `{ taskId }`。 */
export type CaptureResolveResult = { accepted: false } | { taskId: string };

// ── agent.update ──

export interface UpdateCheckParams {
  /** `stable` | `frontier`；省略取偏好 `general.update_channel`。 */
  channel?: 'stable' | 'frontier' | null;
}
