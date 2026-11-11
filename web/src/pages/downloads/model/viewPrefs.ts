// 下载页视图偏好（移植 crates/downloads/src/model/view_prefs.rs）。
// 全局单套、设备本地（`sync:false`），偏好键 `desktop.downloads.view`，JSON 形状与 GPUI 一致（snake_case）。

import { STATE_RANK } from './task'
import type { DownloadTaskView, TaskState } from './task'

export const VIEW_PREFS_KEY = 'desktop.downloads.view'
/** 设备本地偏好（缺省关，GPUI 与 Web 共用）：下载列表 / 详情副标题不展示来源站点（对应 GPUI `HIDE_TASK_SOURCE_PREF`）。 */
export const HIDE_TASK_SOURCE_PREF = 'ui.hide_task_source'

export type ViewDensity = 'comfortable' | 'compact' | 'relaxed'
export type ViewGroupBy = 'none' | 'status' | 'date' | 'type' | 'queue' | 'site' | 'directory'
export type ViewSortKey = 'smart' | 'created' | 'name' | 'size' | 'progress' | 'speed' | 'status'
export type SortDir = 'asc' | 'desc'
export type DetailPlacement = 'bottom' | 'right'

export const GROUP_BY_OPTIONS: readonly ViewGroupBy[] = ['none', 'status', 'date', 'type', 'queue', 'site', 'directory']
export const SORT_KEY_OPTIONS: readonly ViewSortKey[] = ['smart', 'created', 'name', 'size', 'progress', 'speed', 'status']

export type ColumnKind =
  | 'file_name'
  | 'progress'
  | 'size'
  | 'speed'
  | 'eta'
  | 'status'
  | 'created'
  | 'protocol'
  | 'source'
  | 'queue'

/** 默认列顺序（`columns` 偏好为空或缺列时按此补齐）。 */
export const ALL_COLUMNS: readonly ColumnKind[] = [
  'file_name',
  'size',
  'progress',
  'status',
  'created',
  'speed',
  'eta',
  'protocol',
  'source',
  'queue',
]

/** 列名 i18n 键。 */
export const COLUMN_LABEL_KEY: Record<ColumnKind, string> = {
  file_name: 'colFileName',
  progress: 'colProgress',
  size: 'colSize',
  speed: 'colSpeed',
  eta: 'colEta',
  status: 'colStatus',
  created: 'colCreated',
  protocol: 'colProtocol',
  source: 'colSource',
  queue: 'colQueue',
}

const DEFAULT_VISIBLE = new Set<ColumnKind>(['file_name', 'size', 'progress', 'status', 'created'])

const DEFAULT_WIDTH: Record<ColumnKind, number> = {
  file_name: 240,
  progress: 150,
  size: 84,
  speed: 90,
  eta: 84,
  status: 140,
  // `YYYY-MM-DD HH:MM:SS` + 左右内边距；与 GPUI `DownloadColumnKind` 对齐。
  created: 164,
  protocol: 64,
  source: 148,
  queue: 88,
}

export const MIN_WIDTH: Record<ColumnKind, number> = {
  file_name: 160,
  progress: 110,
  size: 64,
  speed: 72,
  eta: 64,
  status: 84,
  created: 152,
  protocol: 56,
  source: 96,
  queue: 72,
}

export const MAX_COLUMN_WIDTH = 480
export const FILE_NAME_MAX_WIDTH = 1600

/** 点击表头切换到的排序键（「智能排序」不对应任何列）；「状态」列按状态优先级排序。 */
export const COLUMN_SORT_KEY: Partial<Record<ColumnKind, ViewSortKey>> = {
  file_name: 'name',
  progress: 'progress',
  size: 'size',
  speed: 'speed',
  created: 'created',
  status: 'status',
}

export interface ColumnPref {
  key: ColumnKind
  visible: boolean
  width: number
}

/** 任务表表头文字对齐（视图菜单 › 显示）；默认左对齐，单元格内容对齐不受影响。 */
export type HeaderAlign = 'left' | 'center' | 'right'

export interface ViewPrefs {
  density: ViewDensity
  header_align: HeaderAlign
  group_by: ViewGroupBy
  sort_key: ViewSortKey
  sort_dir: SortDir
  /** 空 = 使用默认列集。 */
  columns: ColumnPref[]
  /** 文件名列拖过后的固定宽度；null = 吸收表格剩余宽度。 */
  file_name_width: number | null
  detail_placement: DetailPlacement
  detail_open: boolean
  detail_size: number
  sidebar_width: number
  sidebar_collapsed: boolean
  collapsed_groups: string[]
}

export const DEFAULT_SIDEBAR_WIDTH = 200
export const SIDEBAR_WIDTH_RANGE = [176, 300] as const
export const DETAIL_SIZE_RANGE: Record<DetailPlacement, readonly [number, number]> = {
  bottom: [160, 480],
  right: [220, 560],
}

export function defaultViewPrefs(): ViewPrefs {
  return {
    density: 'comfortable',
    header_align: 'left',
    group_by: 'none',
    sort_key: 'smart',
    sort_dir: 'desc',
    columns: [],
    file_name_width: null,
    detail_placement: 'bottom',
    detail_open: false,
    detail_size: 260,
    sidebar_width: DEFAULT_SIDEBAR_WIDTH,
    sidebar_collapsed: false,
    collapsed_groups: [],
  }
}

function oneOf<T extends string>(values: readonly T[], raw: unknown, fallback: T): T {
  return typeof raw === 'string' && (values as readonly string[]).includes(raw) ? (raw as T) : fallback
}

function finite(raw: unknown, fallback: number): number {
  return typeof raw === 'number' && Number.isFinite(raw) ? raw : fallback
}

/** 反序列化；偏好为字符串（JSON 文本）或对象都接受，逐字段容错，非对象返回默认值。 */
export function parseViewPrefs(value: unknown): ViewPrefs {
  const defaults = defaultViewPrefs()
  let raw: unknown = value
  if (typeof raw === 'string') {
    try {
      raw = JSON.parse(raw)
    } catch {
      return defaults
    }
  }
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return defaults
  const json = raw as Record<string, unknown>
  const columns: ColumnPref[] = Array.isArray(json.columns)
    ? json.columns.flatMap((item): ColumnPref[] => {
        if (typeof item !== 'object' || item === null) return []
        const entry = item as Record<string, unknown>
        if (typeof entry.key !== 'string' || !(ALL_COLUMNS as readonly string[]).includes(entry.key)) return []
        return [{ key: entry.key as ColumnKind, visible: entry.visible !== false, width: finite(entry.width, 0) }]
      })
    : []
  return {
    density: oneOf(['comfortable', 'compact', 'relaxed'], json.density, defaults.density),
    header_align: oneOf(['left', 'center', 'right'], json.header_align, defaults.header_align),
    group_by: oneOf(GROUP_BY_OPTIONS, json.group_by, defaults.group_by),
    sort_key: oneOf(SORT_KEY_OPTIONS, json.sort_key, defaults.sort_key),
    sort_dir: oneOf(['asc', 'desc'], json.sort_dir, defaults.sort_dir),
    columns,
    file_name_width: typeof json.file_name_width === 'number' ? json.file_name_width : null,
    detail_placement: oneOf(['bottom', 'right'], json.detail_placement, defaults.detail_placement),
    detail_open: json.detail_open === true,
    detail_size: finite(json.detail_size, defaults.detail_size),
    sidebar_width: finite(json.sidebar_width, defaults.sidebar_width),
    sidebar_collapsed: json.sidebar_collapsed === true,
    collapsed_groups: Array.isArray(json.collapsed_groups)
      ? json.collapsed_groups.filter((item): item is string => typeof item === 'string')
      : [],
  }
}

/** 解析后的列配置（补齐缺列、至少一列可见）——GPUI `apply_column_prefs`。 */
export interface ResolvedColumn {
  kind: ColumnKind
  width: number
  visible: boolean
}

export function resolveColumns(prefs: readonly ColumnPref[]): ResolvedColumn[] {
  const make = (kind: ColumnKind): ResolvedColumn => ({
    kind,
    width: DEFAULT_WIDTH[kind],
    visible: DEFAULT_VISIBLE.has(kind),
  })
  if (prefs.length === 0) return ALL_COLUMNS.map(make)
  const columns = prefs.map((pref) => {
    const column = make(pref.key)
    column.visible = pref.visible
    if (pref.width >= MIN_WIDTH[pref.key]) column.width = pref.width
    return column
  })
  for (const kind of ALL_COLUMNS) {
    if (!columns.some((column) => column.kind === kind)) columns.push(make(kind))
  }
  if (!columns.some((column) => column.visible)) columns[0] = { ...(columns[0] as ResolvedColumn), visible: true }
  return columns
}

export function toColumnPrefs(columns: readonly ResolvedColumn[]): ColumnPref[] {
  return columns.map((column) => ({ key: column.kind, visible: column.visible, width: column.width }))
}

/** 排序键首次选中时的方向：名称 A→Z，数值类从大到新。 */
export function defaultSortDir(key: ViewSortKey): SortDir {
  return key === 'name' ? 'asc' : 'desc'
}

/**
 * 表头点击三档循环：列的默认方向 → 反方向 → 回到智能排序（`sort_dir` 置回 desc）。
 * 切到别的列从该列默认方向开始。
 */
export function nextHeaderSort(current: ViewPrefs, key: ViewSortKey): ViewPrefs {
  const initial = defaultSortDir(key)
  if (current.sort_key !== key) return { ...current, sort_key: key, sort_dir: initial }
  if (current.sort_dir === initial) return { ...current, sort_dir: initial === 'asc' ? 'desc' : 'asc' }
  return { ...current, sort_key: 'smart', sort_dir: 'desc' }
}

/**
 * 智能排序档位：
 * 0 优先下载（boosted 且 下载中/排队）→ 1 下载中或准备中 → 2 排队 → 3 失败 → 4 暂停 → 5 已完成。
 */
export function smartTier(view: DownloadTaskView): number {
  const active = view.state === 'downloading' || view.state === 'pending'
  if (view.boosted && active) return 0
  if (view.state === 'downloading' || (view.state === 'pending' && view.preparing)) return 1
  switch (view.state) {
    case 'pending':
      return 2
    case 'failed':
      return 3
    case 'paused':
      return 4
    default:
      return 5
  }
}

const keyOrder = (left: DownloadTaskView, right: DownloadTaskView): number =>
  left.key < right.key ? -1 : left.key > right.key ? 1 : 0

/** 添加顺序正序：先加的在前。`createdAtSecs` → `queueOrder`（打破同一秒批量添加）→ key。 */
function addedAsc(left: DownloadTaskView, right: DownloadTaskView): number {
  return left.createdAtSecs - right.createdAtSecs || left.queueOrder - right.queueOrder || keyOrder(left, right)
}

/** 添加顺序倒序：最新在前（key 仍升序，保证全序）。 */
function addedDesc(left: DownloadTaskView, right: DownloadTaskView): number {
  return right.createdAtSecs - left.createdAtSecs || right.queueOrder - left.queueOrder || keyOrder(left, right)
}

const isDigit = (code: number): boolean => code >= 48 && code <= 57

/** 逐码元比较 `a[from, aEnd)` 与 `b[bFrom, bEnd)`；前缀相同则短者在前。不分配。 */
function compareRange(a: string, aFrom: number, aEnd: number, b: string, bFrom: number, bEnd: number): number {
  const length = Math.min(aEnd - aFrom, bEnd - bFrom)
  for (let k = 0; k < length; k++) {
    const diff = a.charCodeAt(aFrom + k) - b.charCodeAt(bFrom + k)
    if (diff !== 0) return diff < 0 ? -1 : 1
  }
  return aEnd - aFrom === bEnd - bFrom ? 0 : aEnd - aFrom < bEnd - bFrom ? -1 : 1
}

/**
 * 自然序：切成连续 ASCII 数字段 / 非数字段交替比较。两边都是数字段时去前导 0，先比长度再逐字符；
 * 否则按普通字符串比较该段；一边先耗尽则短者在前。全部相等返回 0（调用方再用原始串定序）。
 * 入参应是已大小写折叠的名称。
 */
export function compareNatural(a: string, b: string): number {
  let i = 0
  let j = 0
  while (i < a.length && j < b.length) {
    const aDigit = isDigit(a.charCodeAt(i))
    const bDigit = isDigit(b.charCodeAt(j))
    let iEnd = i + 1
    while (iEnd < a.length && isDigit(a.charCodeAt(iEnd)) === aDigit) iEnd++
    let jEnd = j + 1
    while (jEnd < b.length && isDigit(b.charCodeAt(jEnd)) === bDigit) jEnd++
    let order: number
    if (aDigit && bDigit) {
      let iStart = i
      while (iStart < iEnd && a.charCodeAt(iStart) === 48) iStart++
      let jStart = j
      while (jStart < jEnd && b.charCodeAt(jStart) === 48) jStart++
      const aLength = iEnd - iStart
      const bLength = jEnd - jStart
      order = aLength !== bLength ? (aLength < bLength ? -1 : 1) : compareRange(a, iStart, iEnd, b, jStart, jEnd)
    } else {
      order = compareRange(a, i, iEnd, b, j, jEnd)
    }
    if (order !== 0) return order
    i = iEnd
    j = jEnd
  }
  if (i < a.length) return 1
  if (j < b.length) return -1
  return 0
}

function compareNames(left: DownloadTaskView, right: DownloadTaskView): number {
  const natural = compareNatural(left.nameFold, right.nameFold)
  if (natural !== 0) return natural
  // 与 GPUI `natural_cmp` 一致：各段等价（如 `a01` / `a1`）时按折叠后的原串定序。
  return left.nameFold < right.nameFold ? -1 : left.nameFold > right.nameFold ? 1 : 0
}

/** 智能排序：先档位；档内 0/1 添加顺序正序，2 按引擎队列位置（未知在后）再添加正序，3/4/5 添加顺序倒序。 */
function compareSmart(left: DownloadTaskView, right: DownloadTaskView): number {
  const tier = smartTier(left)
  const diff = tier - smartTier(right)
  if (diff !== 0) return diff
  if (tier <= 1) return addedAsc(left, right)
  if (tier === 2) {
    const l = left.queueKey ?? Infinity
    const r = right.queueKey ?? Infinity
    return l === r ? addedAsc(left, right) : l < r ? -1 : 1
  }
  return addedDesc(left, right)
}

/**
 * 排序比较（与 GPUI `view_prefs.rs` 一致）：
 * - `smart` 忽略 `sort_dir`，见 `smartTier` 与档内顺序；
 * - 其他键主键按 `sort_dir`（`status` 用 `STATE_RANK`，`name` 用自然序），相等回落「添加顺序倒序」。
 */
export function compareViews(prefs: Pick<ViewPrefs, 'sort_key' | 'sort_dir'>, left: DownloadTaskView, right: DownloadTaskView): number {
  if (prefs.sort_key === 'smart') return compareSmart(left, right)
  let order = 0
  switch (prefs.sort_key) {
    case 'created':
      order = left.createdAtSecs - right.createdAtSecs
      break
    case 'name':
      order = compareNames(left, right)
      break
    case 'size':
      order = left.sizeBytes - right.sizeBytes
      break
    case 'progress':
      order = left.progress - right.progress
      break
    case 'status':
      order = STATE_RANK[left.state] - STATE_RANK[right.state]
      break
    case 'speed':
      order = (left.speed ?? 0) - (right.speed ?? 0)
      break
  }
  return (prefs.sort_dir === 'asc' ? order : -order) || addedDesc(left, right)
}

/** 日期分组桶（本地日期）。顺序即分组顺序。 */
export type DateBucket = 'today' | 'yesterday' | 'this_week' | 'this_month' | 'older'
export const DATE_BUCKET_ORDER: Record<DateBucket, number> = {
  today: 0,
  yesterday: 1,
  this_week: 2,
  this_month: 3,
  older: 4,
}

function isoWeekKey(date: Date): string {
  // ISO 周：以周四所在年份 + 周序号标识。
  const d = new Date(Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()))
  const dayNum = d.getUTCDay() || 7
  d.setUTCDate(d.getUTCDate() + 4 - dayNum)
  const yearStart = Date.UTC(d.getUTCFullYear(), 0, 1)
  const week = Math.ceil(((d.getTime() - yearStart) / 86_400_000 + 1) / 7)
  return `${d.getUTCFullYear()}-${week}`
}

export function dateBucketOf(createdAtSecs: number, now: Date = new Date()): DateBucket {
  if (!createdAtSecs || createdAtSecs <= 0) return 'older'
  const created = new Date(createdAtSecs * 1000)
  const startOf = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
  const today = startOf(now)
  const day = startOf(created)
  if (day >= today) return 'today'
  const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1).getTime()
  if (day === yesterday) return 'yesterday'
  if (isoWeekKey(created) === isoWeekKey(now)) return 'this_week'
  if (created.getMonth() === now.getMonth() && created.getFullYear() === now.getFullYear()) return 'this_month'
  return 'older'
}

/** 状态分组键（稳定，用于折叠记忆）。 */
export const stateGroupKey = (state: TaskState): string => state

/** 搜索：名称 / url / 站点（小写包含）。 */
export function matchesQuery(view: DownloadTaskView, query: string): boolean {
  if (query === '') return true
  // 三个字段以换行拼接（搜索框是单行输入，查询词里不会有换行，不会跨字段误命中）。
  if (view.searchFold === '') view.searchFold = `${view.nameFold}\n${view.url.toLowerCase()}\n${view.site.toLowerCase()}`
  return view.searchFold.includes(query)
}
