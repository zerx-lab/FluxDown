// Wire 契约 —— 与 native/api `types.rs` + native/server `wire.rs` 一一对应（camelCase）。

/** 任务状态码：0=pending 1=downloading 2=paused 3=completed 4=error 5=preparing */
export type TaskStatus = 0 | 1 | 2 | 3 | 4 | 5

export interface TaskDto {
  taskId: string
  url: string
  fileName: string
  saveDir: string
  status: TaskStatus
  downloadedBytes: number
  totalBytes: number
  errorMessage: string
  /** Unix 秒时间戳（字符串） */
  createdAt: string
  proxyUrl: string
  queueId: string
  checksum: string
  /** 当前任务是否显式接受无效 HTTPS 证书 */
  ignoreTlsErrors: boolean
  /** 文件跟踪：completed 任务的目标文件是否已丢失（被删除/移动）。默认 false */
  fileMissing?: boolean
  /** Unix 秒时间戳（字符串），任务完成时刻；未完成为空串 */
  completedAt?: string
}

export interface QueueDto {
  queueId: string
  name: string
  speedLimitKbps: number
  maxConcurrent: number
  defaultSaveDir: string
  position: number
  defaultSegments: number
  defaultUserAgent: string
  /** 队列是否处于运行态；停止时队列内任务全部暂停，不会被调度器恢复。 */
  isRunning: boolean
  /** 是否启用每日定时启停 */
  scheduleEnabled: boolean
  /** 每日启动时间 "HH:MM"，为空表示未设置 */
  scheduleStart: string
  /** 每日停止时间 "HH:MM"，为空表示未设置 */
  scheduleStop: string
  /** 生效星期位掩码：bit0=周一 … bit6=周日，127=每天 */
  scheduleDays: number
}

export interface CreateTaskRequest {
  url: string
  fileName?: string
  saveDir?: string
  segments?: number
  cookies?: string
  referrer?: string
  proxyUrl?: string
  userAgent?: string
  queueId?: string
  checksum?: string
  /** true = 仅为此任务忽略 HTTPS 证书错误；默认 false */
  ignoreTlsErrors?: boolean
  headers?: Record<string, string>
  /** true = 稍后下载：任务以 paused 状态创建，不自动启动 */
  startPaused?: boolean
}

export interface CreatedTask {
  taskId: string
}

export interface ApiInfo {
  name: string
  version: string
}

export interface PingInfo {
  success: boolean
  app: string
  version: string
  message: string
  /** 服务器默认语言（FLUXDOWN_LANG / config `web_language`），未配置时缺省。 */
  language?: string
}

export interface SegmentDetail {
  index: number
  startByte: number
  endByte: number
  downloadedBytes: number
}

export interface HlsQualityOption {
  index: number
  bandwidth: number
  width: number
  height: number
}

export interface BtFileEntry {
  index: number
  path: string
  size: number
}

export interface ResolveVariantOption {
  index: number
  label: string
  container: string
  bandwidth: number
  width: number
  height: number
  totalBytes: number
}

// ---- WS 服务端 → 客户端（tag = type） ----

export type WsServerMsg =
  | ({ type: 'taskProgress' } & TaskProgressMsg)
  | { type: 'tasksSnapshot'; tasks: TaskDto[] }
  | ({ type: 'segmentProgress' } & SegmentProgressMsg)
  | ({ type: 'segmentSplit' } & SegmentSplitMsg)
  | { type: 'taskMetaProbed'; taskId: string; fileName: string; totalBytes: number }
  | { type: 'queuesChanged'; queues: QueueDto[] }
  | { type: 'taskQueueChanged'; taskId: string; queueId: string }
  | { type: 'queuePositionsChanged'; positions: { taskId: string; position: number }[] }
  | { type: 'priorityTaskChanged'; priorityTaskId: string; autoPausedCount: number }
  | { type: 'hlsSelectionRequest'; taskId: string; options: HlsQualityOption[] }
  | { type: 'btSelectionRequest'; taskId: string; files: BtFileEntry[] }
  | { type: 'resolveVariantRequest'; taskId: string; defaultIndex: number; options: ResolveVariantOption[] }
  | { type: 'pluginsChanged' }
  | { type: 'pluginAutoDisabled'; identity: string; reason: string }
  | { type: 'pluginHookActivity'; taskId: string; pluginId: string; running: boolean }
  | { type: 'componentProgress'; component: string; downloadedBytes: number; totalBytes: number }
  | { type: 'componentResult'; component: string; ok: boolean; message: string }
  | { type: 'pong' }

export interface TaskProgressMsg {
  taskId: string
  status: TaskStatus
  downloadedBytes: number
  totalBytes: number
  speed: number
  fileName: string
  saveDir: string
  url: string
  errorMessage: string
}

export interface SegmentProgressMsg {
  taskId: string
  totalBytes: number
  segmentCount: number
  segments: SegmentDetail[]
}

export interface SegmentSplitMsg {
  taskId: string
  parentIndex: number
  parentNewEnd: number
  childIndex: number
  childStart: number
  childEnd: number
  isProactive: boolean
  totalSegments: number
}

// ---- WS 客户端 → 服务端 ----

export type WsClientMsg =
  | { type: 'hlsSelection'; taskId: string; selectedIndex: number }
  | { type: 'btSelection'; taskId: string; selectedIndices: number[] }
  | { type: 'selectVariant'; taskId: string; selectedIndex: number }
  | { type: 'ping' }

// ---- 扩展 REST ----

export interface ProxyTestRequest {
  proxyType: string
  host: string
  port: string
  username?: string
  password?: string
}

export interface ProxyTestResponse {
  latencyMs: number
}

export interface TrackerSubRefreshResponse {
  success: boolean
  trackerCount: number
  okSources: number
  totalSources: number
  updatedAt: number
  error: string
}

export interface CreateQueueRequest {
  name: string
  speedLimitKbps?: number
  maxConcurrent?: number
  defaultSaveDir?: string
  defaultSegments?: number
  defaultUserAgent?: string
}

export interface QueueScheduleRequest {
  enabled: boolean
  startTime: string
  stopTime: string
  days: number
}

export interface QueueOrderRequest {
  taskIds: string[]
}

export interface FsEntry {
  name: string
  path: string
}

export interface FsListResponse {
  path: string
  parent: string | null
  dirs: FsEntry[]
}

export interface StatsResponse {
  diskFreeBytes: number | null
  saveDir: string
  serverVersion: string
  wsClients: number
  /** 演示模式开关（服务器以 FLUXDOWN_DEMO_URL 启动时为 true）。 */
  demoMode: boolean
  /** 演示模式下唯一允许下载的 URL；非演示模式为空串。 */
  demoUrl: string
}

export interface TokenResponse {
  token: string
  note: string
}

export interface LogFileDto {
  name: string
  size: number
}

export interface LogsResponse {
  /** 日志目录绝对路径（服务器文件系统）。 */
  dir: string
  files: LogFileDto[]
}

// ---- 组件（ffmpeg / yt-dlp） ----

/** ffmpeg 路径来源：manual=手动指定 managed=托管安装 system=系统 PATH none=未找到。 */
export type FfmpegSource = 'manual' | 'managed' | 'system' | 'none'

export interface ComponentFfmpegStatus {
  source: FfmpegSource
  /** 当前平台是否提供托管安装（macOS 等为 false）。 */
  managedSupported: boolean
  path: string
  version: string
  managedVersion: string
  systemPath: string
}

export interface ComponentYtdlpStatus {
  source: FfmpegSource
  /** 当前平台是否提供托管安装（yt-dlp 全平台均支持，通常为 true）。 */
  managedSupported: boolean
  path: string
  version: string
  managedVersion: string
  systemPath: string
}

export interface ComponentVersions {
  versions: string[]
  latestStable: string
}

export interface InstallFfmpegRequest {
  version?: string
}

export type ConfigMap = Record<string, string>

// ---- 插件系统 ----

export type SettingValueType = 'string' | 'number' | 'boolean'
export type SettingWidget = 'text' | 'password' | 'textarea' | 'select' | 'toggle' | 'number' | 'folder'
export type PluginDisabledReason = 'None' | 'Manual' | 'CircuitBreaker'

export interface SettingOptionDto {
  value: string
  label: string
}

export interface SettingFieldDto {
  key: string
  title: string
  description: string
  type: SettingValueType
  widget: SettingWidget
  options: SettingOptionDto[]
  default: string | null
  required: boolean
  min: number | null
  max: number | null
  pattern: string | null
  /** 辅助脚本（非空时字段旁渲染复制按钮，仅复制文本、绝不执行）。旧服务端可能缺省。 */
  helperScript?: string | null
  /** 辅助脚本按钮文案（空则用默认文案）。 */
  helperLabel?: string | null
}

export interface PluginDto {
  identity: string
  name: string
  version: string
  description: string
  homepage: string
  enabled: boolean
  devMode: boolean
  disabledReason: PluginDisabledReason
  settings: SettingFieldDto[]
  settingsValues: Record<string, string>
  /** manifest 声明的能力权限（如 ["ffmpeg"]），旧服务端可能缺省。 */
  permissions?: string[]
}

export interface InstalledPlugin {
  identity: string
  /** 插件声明权限所需但尚未安装的基础组件（"ffmpeg"/"ytdlp"），提醒式。 */
  missingComponents?: string[]
}

/** 插件市场索引条目（浏览/安装用）。yanked 值域：none/deprecated/vulnerable/malicious。 */
export interface MarketEntry {
  pluginId: string
  version: string
  sequence: number
  contentHash: string
  minAppVersion: string
  name: string
  description: string
  author: string
  homepage: string
  mirrors: string[]
  publishTime: string
  yanked: string
  tags: string[]
  /** manifest 声明的能力权限（如 ["ffmpeg"]），旧索引可能缺省。 */
  permissions?: string[]
}
