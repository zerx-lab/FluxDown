// RSS 页的纯展示逻辑：与 GPUI `crates/rss/src/{view,controller}.rs` 的文案/状态判定逐项对齐。

import { byteUnit, type FileSizeUnits } from '../../lib/fileSizeUnits'
import type { TFunction } from '../../i18n'
import type { RssItemDto, RssSourceDto } from '../../lib/rpc'
import { LATER_QUEUE_ID, MAIN_QUEUE_ID, RSS_ITEM_STATUS } from '../../lib/rpc'

/** 引擎默认抓取间隔（`intervalMinutes = 0` 时）。 */
export const DEFAULT_INTERVAL_MINUTES = 30
/** 引擎默认每轮最多新建任务数（`maxPerFetch = 0` 时）。 */
export const DEFAULT_MAX_PER_FETCH = 20

/** `YYYY-MM-DD HH:mm UTC`；无时间戳返回空串（GPUI `date_text`）。 */
export function dateText(timestamp: number): string {
  if (timestamp <= 0) return ''
  const iso = new Date(timestamp * 1000).toISOString()
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)} UTC`
}

/** 体积展示（1024 进制，1 位小数）；未知返回空串（GPUI `bytes_text`）。 */
export function bytesText(bytes: number, units?: FileSizeUnits): string {
  if (bytes <= 0) return ''
  let size = bytes
  let unit = 0
  for (; unit < 4 && size >= 1024; unit += 1) size /= 1024
  return unit === 0 ? `${bytes} ${byteUnit(unit, units)}` : `${size.toFixed(1)} ${byteUnit(unit, units)}`
}

export function effectiveInterval(source: Pick<RssSourceDto, 'intervalMinutes'>): number {
  return source.intervalMinutes > 0 ? source.intervalMinutes : DEFAULT_INTERVAL_MINUTES
}

/** 「每 N 分钟 / 每 N 小时」（整点小时用小时）。 */
export function intervalLabel(t: TFunction, minutes: number): string {
  return minutes > 0 && minutes % 60 === 0 ? t('rssEveryHours', { n: minutes / 60 }) : t('rssEveryMinutes', { n: minutes })
}

/** 订阅源状态行：间隔 · 抓取状态 · 自动下载/收集模式。 */
export function sourceStatusLine(t: TFunction, source: RssSourceDto, refreshing: boolean, nowSeconds: number): string {
  const parts = [t('rssEveryMinutes', { n: effectiveInterval(source) })]
  if (refreshing) {
    parts.push(t('rssRefreshing'))
  } else if (source.lastError !== '') {
    parts.push(t('rssFailedTimes', { n: source.failCount }), source.lastError)
  } else if (source.lastSuccessAt > 0) {
    const age = Math.max(0, nowSeconds - source.lastSuccessAt)
    const when =
      age < 60
        ? t('rssJustNow')
        : age < 3600
          ? t('rssMinutesAgo', { n: Math.floor(age / 60) })
          : age < 86_400
            ? t('rssHoursAgo', { n: Math.floor(age / 3600) })
            : t('rssDaysAgo', { n: Math.floor(age / 86_400) })
    parts.push(t('rssLastFetch', { when }))
  } else {
    parts.push(t('rssNeverFetched'))
  }
  parts.push(t(source.autoDownload ? 'rssAutoDownloadOn' : 'rssCollectMode'))
  return parts.join(' · ')
}

/** 关联任务的紧凑状态：`status * 2 + (fileMissing ? 1 : 0)`。 */
export type LinkedTaskStates = Readonly<Record<string, number>>

export function encodeTaskState(status: number, fileMissing: boolean): number {
  return status * 2 + (fileMissing ? 1 : 0)
}

/** 任务表仍在载入：关联任务是否存在未知（不能判定为「已删除」）。 */
export const TASK_STATE_UNKNOWN = -1

/** 条目状态徽标文案键（GPUI `status_key`）：已建任务以真实任务状态为准。 */
export function itemStatusKey(item: RssItemDto, tasks: LinkedTaskStates): string {
  switch (item.status) {
    case RSS_ITEM_STATUS.downloaded: {
      const state = item.taskId === '' ? undefined : tasks[item.taskId]
      if (state === undefined) return 'rssTaskMissing'
      if (state === TASK_STATE_UNKNOWN) return 'rssTaskCreated'
      const status = Math.floor(state / 2)
      const missing = state % 2 === 1
      switch (status) {
        case 0:
          return 'statusPending'
        case 1:
          return 'statusDownloading'
        case 2:
          return 'statusPaused'
        case 3:
          return missing ? 'statusIncomplete' : 'statusCompleted'
        case 4:
          return 'statusError'
        case 5:
          return 'statusPreparing'
        default:
          return 'rssTaskCreated'
      }
    }
    case RSS_ITEM_STATUS.ignored:
      return 'rssStatusIgnored'
    case RSS_ITEM_STATUS.filtered:
      return 'rssStatusFiltered'
    case RSS_ITEM_STATUS.duplicateEpisode:
      return 'rssStatusDuplicate'
    case RSS_ITEM_STATUS.seeded:
      return 'rssStatusHistory'
    default:
      return 'rssStatusNew'
  }
}

/**
 * 可见条目下标（GPUI `visible_indices`）：标题包含（小写）→ 无发布时间的沉底 →
 * 按发布时间排序 → 稳定回退到原顺序。
 */
export function visibleIndices(items: readonly RssItemDto[], query: string, oldestFirst: boolean): number[] {
  const needle = query.trim().toLowerCase()
  const indices: number[] = []
  items.forEach((item, index) => {
    if (item.title.toLowerCase().includes(needle)) indices.push(index)
  })
  return indices.sort((ai, bi) => {
    const a = items[ai]
    const b = items[bi]
    if (!a || !b) return 0
    const missing = Number(a.pubDate <= 0) - Number(b.pubDate <= 0)
    if (missing !== 0) return missing
    const date = oldestFirst ? a.pubDate - b.pubDate : b.pubDate - a.pubDate
    return date !== 0 ? date : ai - bi
  })
}

/** 队列显示名：内置主队列 / 稍后队列走 i18n，其余用自定义名称。 */
export function queueLabel(t: TFunction, queueId: string, name: string): string {
  if (queueId === MAIN_QUEUE_ID) return t('mainQueue')
  if (queueId === LATER_QUEUE_ID) return t('laterQueue')
  return name !== '' ? name : queueId
}
