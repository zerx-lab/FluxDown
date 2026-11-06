// 下载页任务行投影（移植 crates/downloads/src/model/mod.rs 的 DownloadTaskView）。
// 本地任务来自 daemon 快照 + 引擎 taskProgress 的实时速度；远程任务来自 agent.remoteTasks。

import { fileKindOf } from '../../../lib/icon-pack'
import type { FileKind } from '../../../lib/icon-pack'
import type { RemoteTaskDto, TaskDto, TaskRuntimeDto } from '../../../lib/rpc'

export type TaskState = 'pending' | 'downloading' | 'paused' | 'completed' | 'failed'

/** 状态优先级（智能排序档位之外的 `status` 排序键与「按状态分组」的组顺序共用）：失败排在暂停前，因为失败需要处理。 */
export const STATE_RANK: Record<TaskState, number> = {
  downloading: 0,
  pending: 1,
  failed: 2,
  paused: 3,
  completed: 4,
}

export type TaskProtocol = 'http' | 'bt' | 'ed2k' | 'ftp' | 'hls'

export const PROTOCOL_LABEL: Record<TaskProtocol, string> = {
  http: 'HTTP',
  bt: 'BT',
  ed2k: 'ED2K',
  ftp: 'FTP',
  hls: 'HLS',
}

export type TaskSource = 'local' | 'remote'

/** 稳定行身份：`l:<taskId>`（本地）/ `r:<taskId>`（远程）。选中集合与详情用它。 */
export type RowKey = string

export const localKey = (taskId: string): RowKey => `l:${taskId}`
export const remoteKey = (taskId: string): RowKey => `r:${taskId}`
export const isLocalKey = (key: RowKey): boolean => key.startsWith('l:')
export const taskIdOfKey = (key: RowKey): string => key.slice(2)

/** 可「忽略插件重试」的插件失败错误消息前缀（二段解析任务失败用 `[解析插件]`，不匹配本前缀）。 */
export const PLUGIN_ERROR_PREFIX = '[插件]'

export interface DownloadTaskView {
  key: RowKey
  source: TaskSource
  taskId: string
  queueId: string
  name: string
  sizeBytes: number
  downloadedBytes: number
  /** 字节/秒；未收到实时采样为 null。 */
  speed: number | null
  /** 做种 / 下载中任务的上传速率（字节/秒）；未收到实时采样（或远程任务）为 null。 */
  uploadSpeed: number | null
  runtime: TaskRuntimeDto | undefined
  /** daemon 连接就绪（快照未过期）。 */
  runtimeConnected: boolean
  etaSeconds: number | null
  createdAtSecs: number
  completedAtSecs: number
  kind: FileKind
  protocol: TaskProtocol
  /** 0..1。 */
  progress: number
  state: TaskState
  /** 文件名尚未解析（元数据加载中）。 */
  metadataPending: boolean
  url: string
  originUrl: string
  site: string
  referrer: string
  saveDir: string
  groupId: string
  errorMessage: string
  fileMissing: boolean
  /** BT 按顺序下载；远程 / 非 BT 为 false。 */
  btSequential: boolean
  /** BT 先下载首尾分块；远程 / 非 BT 为 false。 */
  btFirstLastPiecesFirst: boolean
  /** 落盘文件哈希 `algo=hex`；空 = 尚未计算。远程任务恒空。 */
  fileHash: string
  seedingStatus: number
  uploadedBytes: number
  boosted: boolean
  /** 本地任务引擎原始 status == 5（准备中）；智能排序把它与下载中放进同一档。 */
  preparing: boolean
  /** 队列内插入序（每队列 MAX+1，用来打破同一秒批量添加的平局）；远程任务为 0。 */
  queueOrder: number
  /** 引擎队列排序键（升序即启动顺序，任务在队列期间不变）；null = 不在队列。排序用它，序号按键的名次求得。 */
  queueKey: number | null
  /** 大小写折叠后的名称，自然序比较用（建行时预计算，比较器内不分配）。 */
  nameFold: string
  /** 搜索用的小写「名称 / url / 站点」拼接；'' = 尚未计算（首次搜索时惰性填入，之后每次搜索只做一次 `includes`）。 */
  searchFold: string
  /** 远程任务目标设备 id（本地任务为空）。 */
  toDevice: string
  /** 远程任务的云端状态（云端命令是否可用的门控依据）；本地任务为 null。 */
  remoteStatus: RemoteTaskDto['status'] | null
  /** 本地任务原始 DTO（详情 / 重新下载需要）；远程任务为 undefined。 */
  dto: TaskDto | undefined
}

/** URL host（不含端口 / 凭据）；非 URL 返回空串。 */
export function urlHost(url: string): string {
  const split = url.indexOf('://')
  if (split < 0) return ''
  const rest = url.slice(split + 3)
  const authority = rest.split(/[/?#]/, 1)[0] ?? ''
  const at = authority.lastIndexOf('@')
  const host = at >= 0 ? authority.slice(at + 1) : authority
  if (host.startsWith('[')) {
    const end = host.indexOf(']')
    return end >= 0 ? host.slice(1, end) : host
  }
  const colon = host.indexOf(':')
  return colon >= 0 ? host.slice(0, colon) : host
}

export function detectProtocol(url: string, fileName: string): TaskProtocol {
  const lower = url.toLowerCase()
  if (lower.startsWith('magnet:') || lower.startsWith('torrent-file://')) return 'bt'
  if (lower.startsWith('ed2k://')) return 'ed2k'
  if (lower.startsWith('ftp://') || lower.startsWith('ftps://')) return 'ftp'
  if (lower.includes('.m3u8') || lower.includes('.mpd') || fileName.endsWith('.m3u8')) return 'hls'
  return 'http'
}

/** 文件扩展名（小写，无点）。 */
export function extensionOf(name: string): string | null {
  const dot = name.lastIndexOf('.')
  if (dot < 0) return null
  const ext = name.slice(dot + 1).toLowerCase()
  return ext === '' || ext.includes('/') ? null : ext
}

export function stateOfStatus(status: number): TaskState {
  switch (status) {
    case 0:
    case 5:
      return 'pending'
    case 1:
      return 'downloading'
    case 2:
      return 'paused'
    case 3:
      return 'completed'
    default:
      return 'failed'
  }
}

/** 做种相关判定所需的视图字段。 */
type SeedingFacts = Pick<DownloadTaskView, 'source' | 'state' | 'seedingStatus' | 'fileMissing'>

/** 本地已完成任务正在做种或排队等做种槽（seeding_status 1 / 8）；归入「做种中」文件夹，可暂停。 */
export function isSeeding(view: Pick<SeedingFacts, 'source' | 'state' | 'seedingStatus'>): boolean {
  return view.source === 'local' && view.state === 'completed' && (view.seedingStatus === 1 || view.seedingStatus === 8)
}

/** 「暂停」适用：下载中 / 排队 / 做种中，以及本地失败任务（撤销等待中的自动重试，#947）。 */
export function canPause(view: Pick<SeedingFacts, 'source' | 'state' | 'seedingStatus'>): boolean {
  return (
    view.state === 'downloading' ||
    view.state === 'pending' ||
    isSeeding(view) ||
    (view.source === 'local' && view.state === 'failed')
  )
}

/** 本地已完成任务因停止原因（seeding_status 2..=7）未在做种且文件仍在：可恢复做种。其余完成态任务不可继续（#935）。 */
export function canResumeSeeding(view: SeedingFacts): boolean {
  return (
    view.source === 'local' &&
    view.state === 'completed' &&
    view.seedingStatus >= 2 &&
    view.seedingStatus <= 7 &&
    !view.fileMissing
  )
}

const REMOTE_STATUS: Record<RemoteTaskDto['status'], number> = {
  pending: 0,
  accepted: 0,
  downloading: 1,
  paused: 2,
  completed: 3,
  failed: 4,
  canceled: 4,
  unknown: 0,
}

/**
 * 时间戳 → Unix 秒：本地任务是 Unix 秒字符串，远程任务（FluxCloud `DateTime<Utc>`）是 RFC3339；
 * 纯数字串按数值，否则按日期解析，都失败为 0。
 */
export function parseTimestampSecs(value: string): number {
  const text = value.trim()
  if (text === '') return 0
  if (/^\d+$/.test(text)) return Number(text)
  const ms = Date.parse(text)
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : 0
}

function compute(name: string, totalBytes: number, downloadedBytes: number, speed: number | null) {
  const sizeBytes = Math.max(0, totalBytes)
  const downloaded = Math.max(0, downloadedBytes)
  const progress = sizeBytes === 0 ? 0 : Math.min(1, Math.max(0, downloaded / sizeBytes))
  const etaSeconds =
    speed !== null && speed > 0 && downloaded < sizeBytes ? Math.floor((sizeBytes - downloaded) / speed) : null
  return { sizeBytes, downloaded, progress, etaSeconds, name }
}

export function buildLocalView(
  task: TaskDto,
  speed: number | null,
  boosted: boolean,
  queueKey: number | null,
  runtime: TaskRuntimeDto | undefined,
  runtimeConnected: boolean,
  uploadSpeed: number | null = null,
): DownloadTaskView {
  const live = speed === null ? null : Math.max(0, speed)
  const base = compute(task.fileName, task.totalBytes, task.downloadedBytes, live)
  return {
    key: localKey(task.taskId),
    source: 'local',
    taskId: task.taskId,
    queueId: task.queueId,
    name: task.fileName,
    sizeBytes: base.sizeBytes,
    downloadedBytes: base.downloaded,
    speed: live,
    uploadSpeed: uploadSpeed === null ? null : Math.max(0, uploadSpeed),
    runtime,
    runtimeConnected,
    etaSeconds: base.etaSeconds,
    createdAtSecs: parseTimestampSecs(task.createdAt),
    completedAtSecs: parseTimestampSecs(task.completedAt),
    kind: fileKindOf(task.fileName),
    protocol: detectProtocol(task.url, task.fileName),
    progress: base.progress,
    state: stateOfStatus(task.status),
    metadataPending: task.fileName.trim() === '',
    url: task.url,
    originUrl: task.originUrl,
    site: urlHost(task.url),
    referrer: task.referrer,
    saveDir: task.saveDir,
    groupId: task.groupId,
    errorMessage: task.errorMessage,
    fileMissing: task.fileMissing,
    btSequential: task.btSequential,
    btFirstLastPiecesFirst: task.btFirstLastPiecesFirst,
    fileHash: task.fileHash,
    seedingStatus: task.seedingStatus,
    uploadedBytes: task.uploadedBytes,
    boosted,
    preparing: task.status === 5,
    queueOrder: task.queueOrder,
    queueKey,
    nameFold: task.fileName.toLowerCase(),
    searchFold: '',
    toDevice: '',
    remoteStatus: null,
    dto: task,
  }
}

export function buildRemoteView(task: RemoteTaskDto): DownloadTaskView {
  const speed = Math.max(0, task.speed)
  const base = compute(task.fileName, task.totalBytes ?? 0, task.downloadedBytes, speed)
  return {
    key: remoteKey(task.id),
    source: 'remote',
    taskId: task.id,
    queueId: '',
    name: task.fileName,
    sizeBytes: base.sizeBytes,
    downloadedBytes: base.downloaded,
    speed,
    uploadSpeed: null,
    runtime: undefined,
    runtimeConnected: false,
    etaSeconds: base.etaSeconds,
    createdAtSecs: parseTimestampSecs(task.createdAt),
    completedAtSecs: 0,
    kind: fileKindOf(task.fileName),
    protocol: detectProtocol(task.url, task.fileName),
    progress: base.progress,
    state: stateOfStatus(REMOTE_STATUS[task.status] ?? 0),
    metadataPending: task.fileName.trim() === '',
    url: task.url,
    originUrl: '',
    site: urlHost(task.url),
    referrer: '',
    saveDir: task.saveDir ?? '',
    groupId: '',
    errorMessage: task.error ?? '',
    fileMissing: false,
    btSequential: false,
    btFirstLastPiecesFirst: false,
    fileHash: '',
    seedingStatus: 0,
    uploadedBytes: 0,
    boosted: false,
    preparing: false,
    queueOrder: 0,
    queueKey: null,
    nameFold: task.fileName.toLowerCase(),
    searchFold: '',
    toDevice: task.toDevice,
    remoteStatus: task.status,
    dto: undefined,
  }
}

/** 「复制链接」用：originUrl 优先，空则回退 url（torrent 任务的 url 是哨兵）。 */
export function shareUrl(view: DownloadTaskView): string {
  return view.originUrl === '' ? view.url : view.originUrl
}

/** 来源站点：referrer host 为主，空则 url host。 */
export function sourceSite(view: DownloadTaskView): string {
  const host = urlHost(view.referrer)
  return host === '' ? view.site : host
}

/**
 * 可在浏览器打开的来源页：仅 `http(s)://` 且有 host 的 referrer（与 GPUI `source_page_url` 一致）。
 * referrer 来自浏览器捕获 / API，不可信；其他协议（`javascript:`、`file:` …）只显示与复制。
 */
export function sourcePageUrl(view: Pick<DownloadTaskView, 'referrer'>): string | null {
  const split = view.referrer.indexOf('://')
  if (split < 0) return null
  const scheme = view.referrer.slice(0, split).toLowerCase()
  return (scheme === 'http' || scheme === 'https') && urlHost(view.referrer) !== '' ? view.referrer : null
}

/** 本地任务实时并发连接数（远程 / 未连接为 null）。 */
export function activeTransfers(view: DownloadTaskView): number | null {
  if (!view.runtimeConnected || view.source !== 'local') return null
  if (view.state !== 'downloading') return 0
  return view.runtime?.activeTransfers ?? null
}

/** 字节 → `1.5 MB`（与 GPUI `format_bytes` 一致：B/KB/MB/GB，1 位小数）。 */
export function formatBytes(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit + 1 < units.length) {
    value /= 1024
    unit += 1
  }
  return unit === 0 ? `${Math.floor(bytes)} B` : `${value.toFixed(1)} ${units[unit]}`
}

/** 完整本地时间 `YYYY-MM-DD HH:MM:SS`（Unix 秒）。 */
export function formatDateTime(timestampSecs: number): string {
  if (!timestampSecs || timestampSecs <= 0) return '—'
  const date = new Date(timestampSecs * 1000)
  if (Number.isNaN(date.getTime())) return '—'
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
}

/** 超过一天的剩余时间估算不可信，不显示。 */
export const MAX_ETA_SECS = 86_400

/** 整数百分比（向下取整；0~1% 显示 `<1%`）。 */
export function percentLabel(progress: number): string {
  const percent = Math.min(1, Math.max(0, progress)) * 100
  if (percent > 0 && percent < 1) return '<1%'
  return `${Math.floor(percent)}%`
}
