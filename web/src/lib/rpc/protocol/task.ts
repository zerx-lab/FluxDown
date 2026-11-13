// 任务相关 DTO（daemon.rs TaskDto / CreateTaskRequest、task_activity.rs）。
import type { CategoryRouteDto } from './agent';
import type { RpcErrorData } from './error';

/** `TaskDto.status` 取值。 */
export const TASK_STATUS = {
  pending: 0,
  downloading: 1,
  paused: 2,
  completed: 3,
  error: 4,
  preparing: 5,
} as const;
export type TaskStatus = (typeof TASK_STATUS)[keyof typeof TASK_STATUS];

/** `TaskDto.seedingStatus` 取值：2..7 均为停止原因，具体见 `seedingMessage`。 */
export const SEEDING_STATUS = {
  none: 0,
  seeding: 1,
  ratioReached: 2,
  timeReached: 3,
  userStopped: 4,
  taskDeleted: 5,
  sessionReleased: 6,
  inactiveStopped: 7,
  /** 排队等待做种槽位。 */
  queued: 8,
} as const;

/** 任务级做种限制哨兵（分享率为千分比，1500 = 1.5）。 */
export const SEED_LIMIT_INHERIT = -2; // 跟随全局
export const SEED_LIMIT_UNLIMITED = -1; // 不限制（0 也视同不限制）

/** 加速路径累计字节（仅 CDN 多节点 / Auto 代理候选 / 多网卡；源站字节 = 已下载 − 三者之和）。 */
export interface TaskSourceBytesDto {
  cdnBytes: number;
  proxyBytes: number;
  nicBytes: number;
}

/** 任务信息。 */
export interface TaskDto {
  taskId: string;
  url: string;
  fileName: string;
  saveDir: string;
  /** 0 待处理 / 1 下载中 / 2 已暂停 / 3 已完成 / 4 出错 / 5 准备中，见 {@link TASK_STATUS}。 */
  status: number;
  downloadedBytes: number;
  totalBytes: number;
  errorMessage: string;
  /** Unix 秒级时间戳（字符串）。 */
  createdAt: string;
  /** 单任务代理 URL（空 = 使用全局代理）。 */
  proxyUrl: string;
  /** 命名队列 ID（空 = 默认队列）。 */
  queueId: string;
  /** `algo=hexhash`，空 = 跳过校验。 */
  checksum: string;
  ignoreTlsErrors: boolean;
  /** 已完成任务的目标文件是否已被删除/移动。 */
  fileMissing: boolean;
  /** 下载完成时刻 Unix 秒（字符串，空 = 尚未完成）。 */
  completedAt: string;
  /** 落盘文件哈希，格式 `algo=hex`（小写；空 = 尚未计算）。只对 status=3 有意义。 */
  fileHash: string;
  /** 浏览器扩展捕获的来源页面 URL（空 = 无）。 */
  referrer: string;
  /** 所属任务组 ID（空 = 不属于任何组）。 */
  groupId: string;
  /** 由哪条 RSS 订阅自动创建（空 = 非 RSS 来源）。 */
  rssSourceId: string;
  /** 展示用原始来源链接（空 = 用 `url`）；`.torrent` 任务的 `url` 是 `torrent-file://local` 哨兵。 */
  originUrl: string;
  /** Auto 路由或站点规则标签：`direct` / `proxy:cached` / `rule:custom` 等（空 = 非 Auto 且未命中规则）。 */
  autoRoute: string;
  /** 队列内启动顺序（0 = 未显式排序）。 */
  queueOrder: number;
  /** BT 已上传字节数（非 BT 恒 0）。 */
  uploadedBytes: number;
  /** 下载完成时刻的已上传字节数（做种后分享率基准）。 */
  uploadedAtCompletion: number;
  /** 见 {@link SEEDING_STATUS}。 */
  seedingStatus: number;
  seedingMessage: string;
  /** 累计做种秒数。 */
  seedingTimeSecs: number;
  /** 总分享率上限（千分比）；-2 跟随全局，-1 不限制，>=0 自定义。 */
  seedRatioLimitMilli: number;
  /** 做种后分享率上限（千分比），哨兵同上。 */
  seedPostRatioLimitMilli: number;
  /** 做种时长上限（分钟），哨兵同上。 */
  seedTimeLimitMinutes: number;
  /** 不活跃做种时长上限（分钟），哨兵同上。 */
  seedInactiveTimeLimitMinutes: number;
  /** 做种上传限速（字节/秒），0 = 未设置（跟随全局）。 */
  seedUploadLimitBps: number;
  /** BT 按顺序下载分块（缺省 false；非 BT 恒 false）。 */
  btSequential: boolean;
  /** BT 先下载首尾分块（缺省 false；非 BT 恒 false）。 */
  btFirstLastPiecesFirst: boolean;
  /** BT 任务级下载限速（字节/秒，`daemon.task.setBtLimits`）；0 = 不限。上传限速即 `seedUploadLimitBps`。 */
  btDownloadLimitBps: number;
  /** 超级做种（`daemon.task.setSuperSeeding`；缺省 false）。 */
  btSuperSeeding: boolean;
  /** 持久化的加速路径字节。 */
  sourceBytes: TaskSourceBytesDto;
}

/** 线上必带的 `TaskDto` 字段（其余默认值字段为 `skip_serializing_if`，缺省即默认值）。 */
export type TaskDtoRequiredKey =
  | 'taskId'
  | 'url'
  | 'fileName'
  | 'saveDir'
  | 'status'
  | 'downloadedBytes'
  | 'totalBytes'
  | 'createdAt';

/**
 * 线上形态的 `TaskDto`（协议 v17）：默认值字段不上线，解码必须经 `normalizeTask`
 * （见 `../normalize.ts`）补齐后才能进入 store。
 */
export type TaskDtoWire = Pick<TaskDto, TaskDtoRequiredKey> & Partial<Omit<TaskDto, TaskDtoRequiredKey>>;

/** 文件字节区间进度；`active` 未知（null）时不要推断其传输状态。 */
export interface TaskSegmentDto {
  index: number;
  startByte: number;
  endByte: number;
  downloadedBytes: number;
  active: boolean | null;
}

/** 任务最新传输采样；活跃传输数不是 socket 数、配置上限或分段数。 */
export interface TaskRuntimeDto {
  taskId: string;
  sampledAtMs: number;
  /** 源端单调采样序号；0 仅供旧客户端回退，不参与过期判定。 */
  sampleSequence: number;
  activeTransfers: number | null;
  connectedPeers: number | null;
  parallelismLimit: number | null;
  totalBytes: number;
  segments: TaskSegmentDto[];
  /** 实时累计（含在途）；null = 该协议/路径无归因。 */
  sourceBytes?: TaskSourceBytesDto | null;
}

/** 持久任务事件（`daemon.task.activity` 条目 / `taskActivityAdded` 事件）。 */
export interface TaskActivityDto {
  /** 同一数据库内单调递增。 */
  id: number;
  taskId: string;
  timestampMs: number;
  kind: string;
  message: string;
  status: number | null;
}

/** `daemon.task.activity` 参数：默认最近一页；`beforeId` 向前翻页，`afterId` 断线补齐，二者互斥。 */
export interface TaskActivityQuery {
  taskId: string;
  beforeId?: number | null;
  afterId?: number | null;
  /** 0 / 缺省 = 服务端默认值；服务端另设硬上限。 */
  limit?: number;
}

/** 页内按 ID 升序；`truncated` 表示历史已被清理。 */
export interface TaskActivityPage {
  entries: TaskActivityDto[];
  hasMore: boolean;
  oldestId: number | null;
  newestId: number | null;
  truncated: boolean;
}

/** 浏览器原始请求体（form-POST 触发的下载）。 */
export type RequestBody =
  | { kind: 'formData'; fields: Record<string, string[]> }
  /** 已序列化的 url-encoded 字符串，直接作为 body。 */
  | { kind: 'urlencoded'; raw: string }
  /** base64 二进制 body。 */
  | { kind: 'raw'; bytesB64: string; contentType?: string | null };

/** 创建任务请求（直接建任务，不经确认框）。除 `url` 外全部可省略。 */
export interface CreateTaskRequest {
  /** 使用 `torrentB64`/`torrentBlobId` 时允许为空占位。 */
  url: string;
  /** 空 = 从 URL / Content-Disposition 推断。 */
  fileName?: string;
  /** 空 = 全局默认保存目录。 */
  saveDir?: string;
  /** 0 = 由 segment_advisor 按文件大小决定。 */
  segments?: number;
  cookies?: string;
  referrer?: string;
  proxyUrl?: string;
  /** 空 = 全局 User-Agent。 */
  userAgent?: string;
  /** 命名队列 ID（空 = 默认队列）。 */
  queueId?: string;
  /** `algo=hexhash`。 */
  checksum?: string;
  ignoreTlsErrors?: boolean;
  headers?: Record<string, string> | null;
  /** base64 编码的 .torrent 内容。 */
  torrentB64?: string | null;
  method?: string | null;
  body?: RequestBody | null;
  /** 音频轨 URL：非空 = 视频轨+音频轨分别下载后 mux。 */
  audioUrl?: string | null;
  /** 稍后下载：建任务后以 paused 落库。 */
  startPaused?: boolean;
  /** HTTP Basic 用户名，非空时注入 Authorization 头。 */
  httpUser?: string;
  httpPassword?: string;
  /** 为该站点保存凭据（`httpUser` 非空时有意义）。 */
  saveSiteAuth?: boolean;
}

/** `daemon.task.create` 参数。 */
export interface DaemonCreateTaskParams {
  request: CreateTaskRequest;
  /** 一次性 blob 引用（与 `request.torrentB64` 互斥）。 */
  torrentBlobId?: string | null;
  unattended?: boolean;
  /** 已知文件大小（字节，>0 才生效）。 */
  hintFileSize?: number | null;
  /** 参照文件名（浏览器捕获的原始名）：显式名缺扩展名时借它的扩展名补全。 */
  nameHint?: string | null;
  /** 分类目录路由：`request.saveDir` 为未命中时的基准目录，引擎按初始名与完成时的最终文件名定落点。 */
  categoryRoute?: CategoryRouteDto | null;
}

/** `daemon.task.create` 结果。 */
export interface CreatedTask {
  taskId: string;
}

/** `daemon.task.seedLocal` 结果；`duplicate` 为真时未新建任务，`taskId` 是已有同 info hash 的任务。 */
export interface SeedLocalResult {
  taskId: string;
  duplicate?: boolean;
  existingName?: string;
  /** 写出的 `.torrent` 路径（请求带了 `torrentOutput` 时）；缺省 = 无。 */
  torrentPath?: string;
}

/** `daemon.task.torrentShare` 结果；`torrentB64` 仅在请求带回且库中有 .torrent 字节时出现。 */
export interface TorrentShareDto {
  infoHash: string;
  name: string;
  magnet: string;
  torrentB64?: string;
  /** v2 info hash（SHA-256，小写十六进制）；v1 种子 / 旧 daemon 缺省。 */
  infoHashV2?: string;
}

export type TaskVerifyStatus = 'intact' | 'damaged' | 'unverifiable';

/** `daemon.task.verify` 结果（BT 已完成任务的分块完整性校验）。 */
export interface TaskVerifyResult {
  status: TaskVerifyStatus;
  totalPieces: number;
  verifiedPieces: number;
  badPieces: number;
  skippedPieces: number;
  damagedFiles: string[];
}

/** `daemon.task.hash` 参数。`algorithm` 为 HASH_ALGORITHMS 之一。 */
export interface DaemonTaskHashParams {
  taskId: string;
  algorithm: string;
}

/** `daemon.task.hash` 结果。 */
export interface TaskHashResult {
  /** 记入任务的哈希，格式同 `TaskDto.fileHash`。 */
  fileHash: string;
}

/** `daemon.task.createMany` 单次调用的条目上限（`CREATE_TASKS_MAX_ITEMS`）；更大的导入须按块拆分。 */
export const CREATE_TASKS_MAX_ITEMS = 512;
/** `daemon.task.createMany` 幂等键 `batchId` 的最大长度（字节，`CREATE_TASKS_BATCH_ID_MAX_LEN`）。 */
export const CREATE_TASKS_BATCH_ID_MAX_LEN = 128;

/**
 * `daemon.task.createMany` 参数：一块链接共享同一份建任务选项，整块一个事务落库。
 *
 * 幂等：同一 `batchId` 已成功落库时，重放返回首次结果、不再建任务；连接中断等结果不确定的
 * 调用可以原样重发。
 */
export interface DaemonCreateTasksParams {
  /** 调用方为每块生成的唯一键（非空，不超过 {@link CREATE_TASKS_BATCH_ID_MAX_LEN} 字节）。 */
  batchId: string;
  /**
   * 整块共享的选项。`url` / `fileName` / `checksum` 由条目决定，这里必须为空；
   * 不支持种子、音轨与自定义 method / body（这些只走 `daemon.task.create`）。
   */
  common: CreateTaskRequest;
  /** 1..={@link CREATE_TASKS_MAX_ITEMS} 条。 */
  items: CreateTaskItem[];
  /** 分类目录路由：应用于整块每一条；`common.saveDir` 为未命中时的基准目录。 */
  categoryRoute?: CategoryRouteDto | null;
}

/** `daemon.task.createMany` 的单条链接。 */
export interface CreateTaskItem {
  url: string;
  /** 空 = 从 URL / Content-Disposition 推断；允许 `子目录/文件名`。 */
  fileName?: string;
  /** `algo=hexhash`（空 = 跳过校验）。 */
  checksum?: string;
}

/** `daemon.task.createMany` 结果：`results` 与请求 `items` 逐条同序对应。 */
export interface DaemonCreateTasksResult {
  results: CreateTaskOutcome[];
}

/** 单条建任务结果：成功带 `taskId`（开启 URL 去重时可能是已有任务），失败带 `error`。 */
export interface CreateTaskOutcome {
  taskId?: string;
  error?: RpcErrorData;
}

/** `agent.capture.submit` 的外部下载请求载荷（DownloadRequest）。除 `url` 外全部可省略。 */
export interface DownloadRequest {
  /** 多行 = 批量（按换行拆成多条）。 */
  url: string;
  filename?: string;
  /** 空 = 由宿主按分类匹配 / 默认目录决定。 */
  saveDir?: string;
  referrer?: string;
  cookies?: string;
  headers?: Record<string, string> | null;
  /** >0 已知大小；-1 未知但确认是下载资源（跳过 probe）；0/null 正常 probe。 */
  fileSize?: number | null;
  mimeType?: string | null;
  method?: string | null;
  body?: RequestBody | null;
  audioUrl?: string | null;
}

/** tracker 汇报状态（`BtTrackerStatus`）。 */
export type BtTrackerStatus = 'working' | 'updating' | 'notWorking' | 'notContacted' | 'unknown';

/** tracker 来源（`BtTrackerSource`）：种子 / 磁力自带，或会话级（设置 / 订阅）追加。 */
export type BtTrackerSource = 'torrent' | 'session';

/** `daemon.task.btStatus` 的单个 tracker（`BtTrackerDto`），顺序即服务端顺序。 */
export interface BtTrackerDto {
  url: string;
  source: BtTrackerSource;
  status: BtTrackerStatus;
  /** 最近一次成功 announce 返回的 peer 数；未知缺省。 */
  peers?: number;
  /** 失败原因的简短归类（如 `Timed out`）；正常为空。 */
  message?: string;
  /** 失败的原始错误文本（供复制）；正常为空。 */
  detail?: string;
  /** 最近一次成功的 unix 秒；0 / 缺省 = 从未。 */
  lastOkAt?: number;
  /** 下次 announce 的 unix 秒；0 / 缺省 = 未知。 */
  nextAnnounceAt?: number;
  /** 所在层级（BEP 12，从 0 起）；0 / 缺省 = 第一层。 */
  tier?: number;
  /** tracker 报告的做种数；未知缺省（0 是事实，与缺省区分）。 */
  seeders?: number;
  /** tracker 报告的下载中对端数；未知缺省。 */
  leechers?: number;
  /** tracker 统计的完成次数（scrape `downloaded`）；未知缺省。 */
  downloaded?: number;
}

/** 连接传输（`BtPeerDto.transport`）：`http` = 经 HTTP 代理，`i2p` = I2P 隧道。 */
export type BtPeerTransport = 'tcp' | 'utp' | 'socks' | 'http' | 'i2p';

/** `daemon.task.btStatus` 的单个 peer（`BtPeerDto`），已按活跃度排序。 */
export interface BtPeerDto {
  /** `ip:port`。 */
  addr: string;
  client?: string;
  transport: BtPeerTransport;
  downloaded: number;
  uploaded: number;
  downBps: number;
  upBps: number;
  /** ISO 3166-1 alpha-2 国家码（大写）；未知 / 私网 / 地理库未就绪时缺省。 */
  country?: string;
  /** 防吸血 IP 组文本（IPv4 `1.2.3.4`，IPv6 `2001:db8::/64`）；未启用 / 旧 daemon 缺省。 */
  ipGroup?: string;
  /** 该 IP 组在本任务累计送达给我方的字节；缺省 = 0。 */
  groupDeliveredBytes?: number;
  /** 嫌疑等级；未评估缺省。 */
  suspicion?: BtPeerSuspicion;
  /** 命中的规则 id（`BtGuardRule`）。 */
  rules?: string[];
  /** 处置：`allow` / `throttle` / `choke` / `banned`；缺省 = `allow`。 */
  action?: BtPeerAction;
  /** 命中规则但处于「仅观察」：`action` 为本应执行而未执行的动作。 */
  observeOnly?: boolean;
}

export type BtPeerSuspicion = 'none' | 'weak' | 'medium' | 'strong';
export type BtPeerAction = 'allow' | 'throttle' | 'choke' | 'banned';

/** peer 池各状态计数（`BtPeerCountsDto`）。 */
export interface BtPeerCountsDto {
  live: number;
  connecting: number;
  queued: number;
  seen: number;
  dead: number;
}

/** `daemon.task.btStatus` 结果（`BtStatusDto`）；非 BT 任务返回 `invalidArgument`。 */
export interface BtStatusDto {
  dhtEnabled: boolean;
  peerCounts: BtPeerCountsDto;
  trackers: BtTrackerDto[];
  peers: BtPeerDto[];
  /** info hash（小写十六进制）：v1 / 混合为 SHA-1，纯 v2 为截断 SHA-256；未知 / 旧 daemon 缺省。 */
  infoHash?: string;
  /** v2 info hash（SHA-256）；v1 种子 / 旧 daemon 缺省。 */
  infoHashV2?: string;
  /** HTTP 源（BEP 19 webseed）实况；没有则缺省。 */
  webSeeds?: BtWebSeedDto[];
}

/** HTTP 源状态（`BtWebSeedDto.state`）。 */
export type BtWebSeedState = 'idle' | 'downloading' | 'backingOff' | 'disabled';

/** 一个 HTTP 源（`BtWebSeedDto`）。 */
export interface BtWebSeedDto {
  url: string;
  /** {@link BtWebSeedState} 之一；未知取值按原样保留（新 daemon 可能扩展）。 */
  state: string;
  /** 从它下载的字节；0 / 缺省。 */
  downloaded?: number;
  /** 最近一次错误；无则缺省。 */
  message?: string;
}

/** `daemon.task.btFiles` 的一行（`BtTaskFileDto`）。 */
export interface BtTaskFileDto {
  /** 种子内文件下标（与 `BtFileDto.index` 同一编号）。 */
  index: number;
  /** 任务目录内的相对路径（`/` 分隔），已是重命名后的当前路径。 */
  path: string;
  size: number;
  /** 已下载并校验通过的字节。 */
  downloaded: number;
  /** 见 `BT_FILE_PRIORITY_*`。 */
  priority: number;
}

/** `daemon.task.btFiles` 结果（`BtFilesDto`，不含 padding 文件）。 */
export interface BtFilesDto {
  /** 能否改优先级 / 重命名：任务未完成且已取得元数据。 */
  editable: boolean;
  files: BtTaskFileDto[];
}

/** 端口映射状态（`BtPortMappingDto.state`）。 */
export type BtPortMappingState = 'pending' | 'mapped' | 'failed';

/** 一个端口的映射状态（`BtPortMappingDto`）；为 0 / 空串的可选字段不上线。 */
export interface BtPortMappingDto {
  /** `tcp` / `udp`。 */
  protocol: string;
  internalPort: number;
  /** 路由器上的外部端口；0 / 缺省 = 未知。 */
  externalPort?: number;
  /** `upnp` / `natPmp` / `pcp`；尚未成功缺省。 */
  method?: string;
  /** 外网地址；未知缺省。 */
  externalIp?: string;
  /** {@link BtPortMappingState} 之一；未知取值原样保留。 */
  state: string;
  /** 失败原因；非失败缺省。 */
  message?: string;
}

/** `daemon.bt.portStatus` 结果（`BtPortStatusDto`）。 */
export interface BtPortStatusDto {
  /** BT 会话是否在运行（没有 BT 任务时不创建会话）。 */
  running: boolean;
  /** 监听端口；0 / 缺省 = 未运行。 */
  listenPort?: number;
  mappings?: BtPortMappingDto[];
}

// ── eD2K 实况（native/protocol/src/daemon.rs `Ed2kStatusDto`）；标 `?` 的字段取默认值时不上线。──

export type Ed2kIdKind = 'none' | 'low' | 'high';
export type Ed2kUpnpStatus = 'disabled' | 'pending' | 'mapped' | 'failed';
export type Ed2kTaskPhase = 'idle' | 'searching' | 'downloading' | 'waiting' | 'verifying';
export type Ed2kPeerOrigin = 'server' | 'kad';
export type Ed2kPeerState = 'known' | 'connecting' | 'queued' | 'transferring' | 'connected' | 'cooldown' | 'failed' | 'banned';
export type Ed2kServerSource = 'manual' | 'subscription';
export type Ed2kServerStatus = 'querying' | 'working' | 'notWorking';
export type Ed2kKadPhase = 'idle' | 'bootstrapping' | 'findingNodes' | 'searching' | 'done';
export type Ed2kKadNodeOrigin = 'bootstrap' | 'discovered';
export type Ed2kKadNodeState = 'known' | 'queried' | 'responded';

/** 本机 eD2K 网络（`daemon.task.ed2kStatus` 的 `network`，亦为 `daemon.ed2k.idCheck` 结果）。 */
export interface Ed2kNetworkDto {
  id: Ed2kIdKind;
  /** 0 / 缺省 = 未登录。 */
  clientId?: number;
  /** `host:port`。 */
  server?: string;
  /** 0 / 缺省 = 监听器未启动。 */
  listenPort?: number;
  upnp: Ed2kUpnpStatus;
  /** `upnp == failed` 时映射器给出的原因（英文原文）；其余缺省。 */
  upnpError?: string;
  externalIp?: string;
  /** 全局设置 `ed2k_enable_upnp`。 */
  upnpEnabled: boolean;
  /** 全局设置 `ed2k_enable_kad`。 */
  kadEnabled: boolean;
  /** nodes.dat 缓存里的引导节点数。 */
  kadNodes: number;
  /** 可用服务器数（手填 + 订阅）。 */
  serverCount: number;
  /** 仅 idCheck 未能登录任何服务器时非空。 */
  error?: string;
}

export interface Ed2kPeerCountsDto {
  total: number;
  transferring: number;
  connected: number;
  queued: number;
  connecting: number;
  cooldown: number;
  failed: number;
  banned: number;
}

/** 单个来源；HighID 源有 `addr`，LowID 源有 `lowId`。 */
export interface Ed2kPeerDto {
  addr?: string;
  lowId?: number;
  origin: Ed2kPeerOrigin;
  state: Ed2kPeerState;
  queueRank?: number;
  client?: string;
  nickname?: string;
  downloaded?: number;
  downBps?: number;
  /** 对端持有的块数；缺省 = 未知。 */
  parts?: number;
  /** 正在下载的块序号（从 0 起）。 */
  part?: number;
  /** 最近一次失败原因（原始英文错误文本）。 */
  message?: string;
  /** 冷却结束 unix 秒。 */
  retryAt?: number;
}

export interface Ed2kServerDto {
  addr: string;
  source: Ed2kServerSource;
  status: Ed2kServerStatus;
  /** 本机当前登录的会话服务器。 */
  connected?: boolean;
  /** 最近一次查询返回的源数。 */
  sources?: number;
  message?: string;
  lastQueryAt?: number;
}

export interface Ed2kKadNodeDto {
  /** `ip:udpPort`。 */
  addr: string;
  /** 32 位小写 hex。 */
  id: string;
  origin: Ed2kKadNodeOrigin;
  state: Ed2kKadNodeState;
  tcpPort?: number;
  version?: number;
}

export interface Ed2kKadDto {
  phase: Ed2kKadPhase;
  bootstrapNodes: number;
  responded: number;
  discovered: number;
  sources: number;
  lastSearchAt?: number;
  /** 按与文件哈希的 XOR 距离升序，至多 100 个。 */
  nodes: Ed2kKadNodeDto[];
}

/** HighID / LowID 检测目标：auto = 有会话报告会话，否则同 list。 */
export type Ed2kIdCheckTarget = 'auto' | 'session' | 'list' | 'server';

/** `daemon.ed2k.idCheck` 参数；省略 / `{}` = auto。 */
export interface Ed2kIdCheckParams {
  target?: Ed2kIdCheckTarget;
  /** `target = server` 时的 `host:port`（IP 或域名）。 */
  server?: string;
}

/** Kad 引导来源：已知节点（nodes.dat 缓存）/ 网址 / 单个节点。 */
export type Ed2kKadBootstrapSource = 'known' | 'url' | 'node';

/** `daemon.ed2k.kadBootstrap` 参数。 */
export interface Ed2kKadBootstrapParams {
  source?: Ed2kKadBootstrapSource;
  /** `source = node`：IP 或域名。 */
  host?: string;
  /** `source = node`：Kad UDP 端口。 */
  port?: number;
  /** `source = url`：nodes.dat 地址；空 = 用已配置的订阅地址。 */
  url?: string;
}

/** `daemon.ed2k.kadBootstrap` 结果。 */
export interface Ed2kKadBootstrapDto {
  contacted: number;
  responded: number;
  discovered: number;
  /** 引导后已知节点缓存里的节点数。 */
  nodeCount: number;
  /** 本次联系与发现的节点，应答的在前，至多 100 个。 */
  nodes: Ed2kKadNodeDto[];
  /** 没有可联系的节点时的原因。 */
  error?: string;
}

/** `daemon.task.ed2kStatus` 结果；任务未在运行时 `phase = idle`、列表为空，`network` 仍有效。 */
export interface Ed2kStatusDto {
  phase: Ed2kTaskPhase;
  /** unix 秒；0 / 缺省 = 未知。 */
  nextSearchAt?: number;
  fileHash: string;
  partsTotal: number;
  partsDone: number;
  /** 当前生效的同时连接来源数 1..=8。 */
  maxSources: number;
  /** 任务的线程数设置（`daemon.task.setSegments` 写入值）；0 / 缺省 = 自动。 */
  segments?: number;
  network: Ed2kNetworkDto;
  peerCounts: Ed2kPeerCountsDto;
  /** 服务端已按活跃度排序。 */
  peers: Ed2kPeerDto[];
  servers: Ed2kServerDto[];
  kad: Ed2kKadDto;
}

/** `daemon.task.speedHistory` 参数。 */
export interface TaskSpeedHistoryQuery {
  taskId: string;
}

/** 任务全生命周期速度曲线与统计；结果为 null = 无记录。平均速度 = transferredBytes / activeSecs。 */
export interface TaskSpeedHistoryDto {
  taskId: string;
  /** 每点覆盖秒数（1、2、4…，服务端点数封顶后相邻合并、宽度翻倍）。 */
  bucketSecs: number;
  /** 每点平均 bytes/s，最旧在前；末点可能是尚在累计的不完整桶。 */
  samples: number[];
  /** 处于下载态的总秒数（暂停 / 排队 / 做种不计）。 */
  activeSecs: number;
  transferredBytes: number;
  /** 逐秒峰值速度。 */
  peakBps: number;
}
