import { GUARD_COLUMNS_MIN_WIDTH, peerGuardView } from '../../../lib/btGuard'
import type { PeerGuardView } from '../../../lib/btGuard'
import { normalizeCountry } from '../../../lib/flags/flags'
import type { BtPeerDto, BtPeerTransport, BtTrackerDto, BtTrackerStatus } from '../../../lib/rpc'
import type { TaskState } from '../model/task'
import type { Translate } from '../table/text'

/** 轮询间隔（与「速度」标签一致）。 */
export const BT_STATUS_POLL_MS = 2000

/** 连接列表渲染上限（服务端已按活跃度排序，超出部分是最不活跃的）。 */
export const MAX_PEER_ROWS = 200

// ── 相对时间 ──

export type RelativeUnit = 's' | 'm' | 'h' | 'd'

/** 秒数 → 最大整单位（<60s 秒，<60m 分，<24h 时，其余天）；负值按 0。 */
export function relativeSpan(secs: number): { unit: RelativeUnit; n: number } {
  const s = Math.max(0, Math.floor(Number.isFinite(secs) ? secs : 0))
  if (s < 60) return { unit: 's', n: s }
  if (s < 3600) return { unit: 'm', n: Math.floor(s / 60) }
  if (s < 86_400) return { unit: 'h', n: Math.floor(s / 3600) }
  return { unit: 'd', n: Math.floor(s / 86_400) }
}

function spanText(t: Translate, secs: number): string {
  const { unit, n } = relativeSpan(secs)
  switch (unit) {
    case 's':
      return t('etaSeconds', { n })
    case 'm':
      return t('etaMinutes', { n })
    case 'h':
      return t('etaHours', { n })
    case 'd':
      return `${n} ${t('timeUnitDays')}`
  }
}

/** 下次 announce：未知（0 / 缺省）→ null；已过期（<=0）显示「in 0s」。 */
export function nextAnnounceText(t: Translate, at: number | undefined, nowSecs: number): string | null {
  if (!at || at <= 0) return null
  return t('btTrackerIn', { time: spanText(t, at - nowSecs) })
}

/** 最近成功：从未（0 / 缺省）→ 「Never」。时钟略有偏差时按 0 秒前。 */
export function lastOkText(t: Translate, at: number | undefined, nowSecs: number): string {
  if (!at || at <= 0) return t('btTrackerNever')
  return t('btTrackerAgo', { time: spanText(t, nowSecs - at) })
}

// ── Tracker ──

const STATUS_KEY: Record<BtTrackerStatus, string> = {
  working: 'btTrackerStatusWorking',
  updating: 'btTrackerStatusUpdating',
  notWorking: 'btTrackerStatusNotWorking',
  notContacted: 'btTrackerStatusNotContacted',
  unknown: 'btTrackerStatusUnknown',
}

export const trackerStatusKey = (status: BtTrackerStatus): string => STATUS_KEY[status]

/** 状态点底色 / 文字色（均为主题令牌）。 */
export const TRACKER_DOT: Record<BtTrackerStatus, string> = {
  working: 'bg-success',
  updating: 'bg-muted-foreground animate-pulse',
  notWorking: 'bg-destructive',
  notContacted: 'bg-text-tertiary',
  unknown: 'bg-text-tertiary',
}

export const TRACKER_TEXT: Record<BtTrackerStatus, string> = {
  working: 'text-success',
  updating: 'text-muted-foreground',
  notWorking: 'text-destructive',
  notContacted: 'text-text-tertiary',
  unknown: 'text-text-tertiary',
}

/**
 * 「没有任何 tracker 工作」提示（GPUI `none_working` 同条件）：至少一个 tracker 失败（`notWorking`），
 * 没有 `working`，也没有 `unknown`（引擎未上报）。全部 `updating` / `notContacted` 不提示，避免首次
 * announce 期间误报。
 */
export function showNoneWorking(trackers: readonly Pick<BtTrackerDto, 'status'>[]): boolean {
  const any = (status: BtTrackerStatus) => trackers.some((tr) => tr.status === status)
  return trackers.length > 0 && !any('unknown') && !any('working') && any('notWorking')
}

/** 渲染行：保持服务端顺序，仅归一化可选字段。 */
export interface TrackerRow {
  key: string
  url: string
  source: BtTrackerDto['source']
  status: BtTrackerStatus
  peers: number | null
  message: string
  /** 原始错误文本；服务端未给时回退到 message。 */
  detail: string
  lastOkAt: number
  nextAnnounceAt: number
}

export function mapTrackerRows(trackers: readonly BtTrackerDto[]): TrackerRow[] {
  const seen = new Map<string, number>()
  return trackers.map((tr) => {
    const nth = seen.get(tr.url) ?? 0
    seen.set(tr.url, nth + 1)
    return {
      key: nth === 0 ? tr.url : `${tr.url}#${nth}`,
      url: tr.url,
      source: tr.source,
      status: tr.status,
      peers: typeof tr.peers === 'number' ? tr.peers : null,
      message: (tr.message ?? '').trim(),
      detail: (tr.detail ?? tr.message ?? '').trim(),
      lastOkAt: tr.lastOkAt ?? 0,
      nextAnnounceAt: tr.nextAnnounceAt ?? 0,
    }
  })
}

// ── Tracker 筛选 / 排序 ──
// 与 crates/downloads/src/model/bt_trackers.rs 对齐，共用夹具 crates/downloads/tests/fixtures/bt_tracker_view_cases.json。

export type TrackerFilter = 'all' | 'working' | 'updating' | 'notWorking' | 'notContacted'

export const TRACKER_FILTERS: readonly TrackerFilter[] = ['all', 'working', 'updating', 'notWorking', 'notContacted']

export type TrackerSortKey = 'url' | 'status' | 'peers' | 'nextAnnounce' | 'lastOk'

/** `null` = 服务端顺序。 */
export type TrackerSort = { key: TrackerSortKey; desc: boolean } | null

export type TrackerCounts = Record<TrackerFilter, number>

export function trackerCounts(rows: readonly Pick<TrackerRow, 'status'>[]): TrackerCounts {
  const counts: TrackerCounts = { all: rows.length, working: 0, updating: 0, notWorking: 0, notContacted: 0 }
  for (const row of rows) {
    if (row.status === 'working' || row.status === 'updating' || row.status === 'notWorking' || row.status === 'notContacted') counts[row.status] += 1
  }
  return counts
}

/** 升序：先看失败。 */
const STATUS_RANK: Record<BtTrackerStatus, number> = { notWorking: 0, updating: 1, working: 2, notContacted: 3, unknown: 4 }

/** 排序值；`null` = 缺省（无论升降序都排最后）。lastOk 取负使「最近」在升序最前。 */
function sortValue(row: TrackerRow, key: TrackerSortKey): number | string | null {
  switch (key) {
    case 'url':
      return row.url.toLowerCase()
    case 'status':
      return STATUS_RANK[row.status] ?? STATUS_RANK.unknown
    case 'peers':
      return row.peers
    case 'nextAnnounce':
      return row.nextAnnounceAt === 0 ? null : row.nextAnnounceAt
    case 'lastOk':
      return row.lastOkAt === 0 ? null : -row.lastOkAt
  }
}

/** 过滤 + 稳定排序，返回 `rows` 下标。 */
export function trackerView(rows: readonly TrackerRow[], filter: TrackerFilter, sort: TrackerSort): number[] {
  const indices: number[] = []
  rows.forEach((row, index) => {
    if (filter === 'all' || row.status === filter) indices.push(index)
  })
  if (sort === null) return indices
  const { key, desc } = sort
  const values = rows.map((row) => sortValue(row, key))
  return indices.sort((a, b) => {
    const va = values[a] ?? null
    const vb = values[b] ?? null
    if (va === null || vb === null) return va === vb ? a - b : va === null ? 1 : -1
    const cmp = va < vb ? -1 : va > vb ? 1 : 0
    return cmp === 0 ? a - b : desc ? -cmp : cmp
  })
}

/** 表头点击三态：未排序 → 升序 → 降序 → 未排序；换列从升序开始。 */
export function nextTrackerSort(current: TrackerSort, key: TrackerSortKey): TrackerSort {
  if (current === null || current.key !== key) return { key, desc: false }
  return current.desc ? null : { key, desc: true }
}

/** 中间省略：头部由 CSS 截断，尾部固定显示（保留域名后的路径末段 / 端口）。 */
export function splitMiddle(url: string, tailLen = 18): { head: string; tail: string } {
  if (url.length <= tailLen * 2) return { head: url, tail: '' }
  return { head: url.slice(0, url.length - tailLen), tail: url.slice(url.length - tailLen) }
}

export type TrackerLayout = 'card' | 'table' | 'wide'

/** 容器宽度 → Tracker 版式：<560 卡片式；<900 表格（消息在 URL 下方）；其余消息独立成列。 */
export function trackerLayout(width: number): TrackerLayout {
  if (width < 560) return 'card'
  if (width < 900) return 'table'
  return 'wide'
}

// ── Peers ──

export type PeerColumn = 'addr' | 'client' | 'transport' | 'downloaded' | 'uploaded' | 'downBps' | 'upBps' | 'groupDelivered' | 'suspicion' | 'action'

export type PeerLayout = 'card' | 'compact' | 'full'

/**
 * 容器宽度 → 连接版式：<480 卡片；<780 紧凑（隐藏客户端 / 上传量）；其余全列。
 * 带防吸血列（IP 组送达 / 嫌疑 / 处置，最小宽度合计见 `GUARD_COLUMNS_MIN_WIDTH`）时全列版式要求更宽，否则退回紧凑。
 */
export function peerLayout(width: number, guard = false): PeerLayout {
  if (width < 480) return 'card'
  if (width < (guard ? GUARD_COLUMNS_MIN_WIDTH : 780)) return 'compact'
  return 'full'
}

/** `guard` = 防吸血数据可用（任一 peer 带 IP 组）：追加 IP 组送达量 / 嫌疑 / 处置列。 */
export function peerColumns(layout: PeerLayout, guard = false): PeerColumn[] {
  switch (layout) {
    case 'card':
      return ['addr', 'transport', 'downBps', 'upBps']
    case 'compact':
      return guard ? ['addr', 'transport', 'downloaded', 'downBps', 'upBps', 'action'] : ['addr', 'transport', 'downloaded', 'downBps', 'upBps']
    case 'full':
      return guard
        ? ['addr', 'client', 'transport', 'downloaded', 'uploaded', 'downBps', 'upBps', 'groupDelivered', 'suspicion', 'action']
        : ['addr', 'client', 'transport', 'downloaded', 'uploaded', 'downBps', 'upBps']
  }
}

const TRANSPORT_LABEL: Record<BtPeerTransport, string> = { tcp: 'TCP', utp: 'uTP', socks: 'SOCKS' }

export const transportLabel = (transport: BtPeerTransport): string => TRANSPORT_LABEL[transport] ?? String(transport).toUpperCase()

export interface PeerRow {
  key: string
  addr: string
  client: string
  /** 大写国家码；未知 = `''`。 */
  country: string
  transport: BtPeerTransport
  downloaded: number
  uploaded: number
  downBps: number
  upBps: number
  /** 防吸血字段（旧 daemon / 未启用时为无嫌疑、放行）。 */
  guard: PeerGuardView
}

/** 是否有 peer 带防吸血 IP 组（决定是否显示防吸血列）。 */
export function hasGuardData(rows: readonly Pick<PeerRow, 'guard'>[]): boolean {
  return rows.some((row) => row.guard.ipGroup !== '')
}

/** 取前 `MAX_PEER_ROWS` 条（服务端顺序）；`key` = 地址，重复地址追加序号保证唯一。 */
export function mapPeerRows(peers: readonly BtPeerDto[]): { rows: PeerRow[]; total: number } {
  const seen = new Map<string, number>()
  const rows = peers.slice(0, MAX_PEER_ROWS).map((p) => {
    const nth = seen.get(p.addr) ?? 0
    seen.set(p.addr, nth + 1)
    return {
      key: nth === 0 ? p.addr : `${p.addr}#${nth}`,
      addr: p.addr,
      client: (p.client ?? '').trim(),
      country: normalizeCountry(p.country),
      transport: p.transport,
      downloaded: p.downloaded,
      uploaded: p.uploaded,
      downBps: p.downBps,
      upBps: p.upBps,
      guard: peerGuardView(p),
    }
  })
  return { rows, total: peers.length }
}

/** 空态：运行中（下载 / 做种）= `btPeersEmpty`，否则 `btPeersIdleHint`。 */
export function peersEmptyKey(state: TaskState, seeding: boolean): 'btPeersEmpty' | 'btPeersIdleHint' {
  return state === 'downloading' || seeding ? 'btPeersEmpty' : 'btPeersIdleHint'
}
