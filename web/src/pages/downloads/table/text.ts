// 任务表 / 卡片共用的文案与映射（状态文案、并发详情、类别名）。

import type { FileKind } from '../../../lib/icon-pack'
import { activeTransfers, formatBytes, isSeeding, MAX_ETA_SECS, sourceSite } from '../model/task'
import { localizeTaskError } from '../model/taskError'
import type { DownloadTaskView, TaskState } from '../model/task'
import { stateLabel } from '../state'

export type Translate = (key: string, params?: Record<string, string | number>) => string

const KIND_LABEL_KEY: Record<FileKind, string> = {
  video: 'categoryVideo',
  audio: 'categoryAudio',
  document: 'categoryDocument',
  image: 'categoryImage',
  archive: 'categoryArchive',
  diskImage: 'categoryArchive',
  application: 'categoryProgram',
  mobile: 'categoryProgram',
  other: 'categoryOther',
}

export const kindLabel = (t: Translate, kind: FileKind): string => t(KIND_LABEL_KEY[kind])

export const STATUS_TEXT: Record<TaskState, string> = {
  downloading: 'text-status-downloading',
  failed: 'text-status-failed',
  paused: 'text-status-paused',
  pending: 'text-status-queued',
  completed: 'text-status-completed',
}

/** 任务行状态文案颜色：做种中 / 排队做种用成功色（绿），其余按状态。 */
export const taskStatusClass = (view: Pick<DownloadTaskView, 'source' | 'state' | 'seedingStatus'>): string =>
  isSeeding(view) ? 'text-success' : STATUS_TEXT[view.state]

/** 速度列的上传速率：做种中且有采样（> 0）时返回「↑ 速率/s」，否则 null。 */
export function seedingUploadLabel(view: Pick<DownloadTaskView, 'source' | 'state' | 'seedingStatus' | 'uploadSpeed'>): string | null {
  if (view.seedingStatus !== 1 || !isSeeding(view)) return null
  return view.uploadSpeed !== null && view.uploadSpeed > 0 ? `↑ ${formatBytes(view.uploadSpeed)}/s` : null
}

export function formatEta(t: Translate, seconds: number): string {
  if (seconds < 60) return t('etaSeconds', { n: seconds })
  if (seconds < 3600) return t('etaMinutes', { n: Math.floor(seconds / 60) })
  return t('etaHours', { n: (seconds / 3600).toFixed(1) })
}

/**
 * 任务的状态名：引擎 status 5（探测 / 插件解析）为「准备中」而非「等待中」，对齐 GPUI
 * `task_state_label` 的准备中分支。按任务展示状态名的地方都走这里，不直接用 `stateLabel`。
 */
export function taskStateLabel(
  t: Translate,
  view: Pick<DownloadTaskView, 'state' | 'preparing' | 'source' | 'seedingStatus'>,
): string {
  if (view.state === 'pending' && view.preparing) return t('statusPreparing')
  // 做种中 / 排队做种覆盖「已完成」；停止做种的完成态保持普通已完成文案。
  if (isSeeding(view)) return t(view.seedingStatus === 8 ? 'seedingStatusQueued' : 'statusSeeding')
  return stateLabel(t, view.state)
}

/** 活动日志里只有引擎原始 status 的状态名：5 为「准备中」（GPUI `status_code_label`）。 */
export function statusCodeLabel(t: Translate, status: number): string {
  switch (status) {
    case 0:
      return stateLabel(t, 'pending')
    case 1:
      return stateLabel(t, 'downloading')
    case 2:
      return stateLabel(t, 'paused')
    case 3:
      return stateLabel(t, 'completed')
    case 5:
      return t('statusPreparing')
    default:
      return stateLabel(t, 'failed')
  }
}

/** 状态列主文案：下载中显示「速度 · 剩余时间」（只显示已知部分），其余为状态名。 */
export function statusLabel(t: Translate, view: DownloadTaskView): string {
  if (view.state !== 'downloading') return taskStateLabel(t, view)
  const speed = view.speed !== null && view.speed > 0 ? `${formatBytes(view.speed)}/s` : null
  const eta = view.etaSeconds !== null && view.etaSeconds <= MAX_ETA_SECS ? formatEta(t, view.etaSeconds) : null
  if (speed && eta) return `${speed} · ${eta}`
  return speed ?? eta ?? t('statusDownloading')
}

function bytesProgress(view: DownloadTaskView): string {
  return view.sizeBytes > 0
    ? `${formatBytes(view.downloadedBytes)} / ${formatBytes(view.sizeBytes)}`
    : formatBytes(view.downloadedBytes)
}

/** 实时并发：本地分段连接数与 BT 已连节点数。 */
function transferDetail(t: Translate, view: DownloadTaskView): string | null {
  const active = activeTransfers(view)
  const peers = view.runtimeConnected && view.protocol === 'bt' ? (view.runtime?.connectedPeers ?? null) : null
  if (active !== null && peers !== null) {
    return `${active} ${t('taskActiveTransfers')} · ${peers} ${t('taskConnectedPeers')}`
  }
  if (active !== null) return `${active} ${t('taskActiveTransfers')}`
  if (peers !== null) return `${peers} ${t('taskConnectedPeers')}`
  return null
}

/** 状态列第二行 / 紧凑密度悬停提示：已下 / 总量、并发连接、失败原因首行。 */
export function statusDetail(t: Translate, view: DownloadTaskView): string | null {
  switch (view.state) {
    case 'downloading': {
      const bytes = bytesProgress(view)
      const transfers = transferDetail(t, view)
      return transfers ? `${bytes} · ${transfers}` : bytes
    }
    case 'paused':
      return bytesProgress(view)
    case 'failed': {
      const line = view.errorMessage.split('\n', 1)[0]?.trim() ?? ''
      return line === '' ? null : localizeTaskError(t, line)
    }
    default:
      return null
  }
}

/**
 * 宽松密度文件名列副标题（移植 task_table.rs 的 `relaxed_meta`，旧 Flutter 默认布局）：已完成为
 * 「大小 · 类别 · 站点」；未完成为「已下 / 总量 [· 速度（仅下载中且 > 0）] · 站点」（总量未知只给已下）。
 * 未知字段直接省略；百分比与剩余时间在独立列里，不在这里重复。`hideSource`（设置「隐藏任务来源」）
 * 为真时去掉站点，其余字段保留。
 */
export function relaxedMeta(t: Translate, view: DownloadTaskView, hideSource = false): string {
  const parts: string[] = []
  if (view.state === 'completed') {
    if (view.sizeBytes > 0) parts.push(formatBytes(view.sizeBytes))
    parts.push(kindLabel(t, view.kind))
  } else {
    parts.push(bytesProgress(view))
    if (view.state === 'downloading' && view.speed !== null && view.speed > 0) parts.push(`${formatBytes(view.speed)}/s`)
  }
  const site = shownSourceSite(view, hideSource)
  if (site !== '') parts.push(site)
  return parts.join(' · ')
}

/** 副标题里展示的来源站点：用户选择隐藏来源（设置「隐藏任务来源」）时为空串（对应 GPUI `shown_source_site`）。 */
export function shownSourceSite(view: DownloadTaskView, hideSource: boolean): string {
  return hideSource ? '' : sourceSite(view)
}

/** 非宽松密度文件名列副标题：「类别 · 站点」，隐藏来源时只留类别（对应 GPUI `file_cell_meta`）。 */
export function fileCellMeta(t: Translate, view: DownloadTaskView, hideSource: boolean): string {
  const category = kindLabel(t, view.kind)
  const site = shownSourceSite(view, hideSource)
  return site === '' ? category : `${category} · ${site}`
}

/** 移动端卡片副标题：类别 · 站点 · 大小，空字段省略；隐藏来源时去掉站点。 */
export function cardMeta(t: Translate, view: DownloadTaskView, hideSource: boolean): string {
  return [kindLabel(t, view.kind), shownSourceSite(view, hideSource), view.sizeBytes > 0 ? formatBytes(view.sizeBytes) : '']
    .filter((part) => part !== '')
    .join(' · ')
}

