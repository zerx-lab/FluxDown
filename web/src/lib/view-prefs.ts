// 任务列表视图系统 —— 偏好模型 + 持久化 store。
// 移植自 lib/src/models/view_prefs.dart（ViewPrefs/ViewPrefsStore）。
// 默认值 = 现状行为快照（列表·舒适·智能分组·智能排序·显示已完成·协议徽标开·
// 默认列为空——web 侧「列」简化为可选附加信息，见 task-columns.ts 顶部说明），
// 保证升级零感知。持久化用 localStorage 替代桌面 KvStore；全局一份 + 按状态
// 页签（all/downloading/completed/paused/error）独立覆盖层，语义对齐桌面：
// 未被用户改动过的页签恒回退出厂默认，不继承其它页签最近一次的改动。

import { t } from './i18n'
import { Store, useStore } from './ws'

export type ViewForm = 'list' | 'grid'
export type ViewDensity = 'comfortable' | 'compact'
export type ViewGroupBy = 'smart' | 'date' | 'status' | 'type' | 'queue' | 'site' | 'none'
export type ViewSortKey = 'smart' | 'created' | 'name' | 'size' | 'progress' | 'speed'
export type SortDir = 'asc' | 'desc'

/** 任务列「列」ID——web 侧仅保留 5 个可选附加信息列（大小/创建时间/协议/来源/队列），
 *  详见 task-columns.ts 顶部说明（进度/速度/剩余时间/状态在 web 卡片行里是结构性
 *  常驻信息，不纳入可关闭的列系统，与桌面 9 列表格的语义不同）。 */
export type TaskColumnId = 'size' | 'created' | 'protocol' | 'source' | 'queue'

/** 每个排序键切换时重置到的默认方向。 */
export const SORT_KEY_DEFAULT_DIR: Record<ViewSortKey, SortDir> = {
  smart: 'desc',
  created: 'desc',
  name: 'asc',
  size: 'desc',
  progress: 'desc',
  speed: 'desc',
}

/** 分组维度循环顺序（面板 chips 顺序 / `G` 快捷键循环）。 */
export const GROUP_BY_CYCLE: ViewGroupBy[] = ['smart', 'date', 'status', 'type', 'queue', 'site', 'none']

/** 排序键循环顺序（面板 chips 顺序 / `S` 快捷键循环）。 */
export const SORT_KEY_CYCLE: ViewSortKey[] = ['smart', 'created', 'name', 'size', 'progress', 'speed']

/** 列 canonical 顺序（勾选先后不影响列序）。 */
export const COLUMN_CANONICAL_ORDER: TaskColumnId[] = ['size', 'created', 'protocol', 'source', 'queue']

export interface ViewPrefs {
  form: ViewForm
  density: ViewDensity
  groupBy: ViewGroupBy
  sortKey: ViewSortKey
  sortDir: SortDir
  showCompleted: boolean
  protocolBadges: boolean
  columns: Set<TaskColumnId>
}

/** 出厂默认值。列默认含「创建时间」——任务行的时间显示统一由该列开关管理
 *  （行内不再硬编码时间，取消勾选即全局消失）。 */
export function defaultViewPrefs(): ViewPrefs {
  return {
    form: 'list',
    density: 'comfortable',
    groupBy: 'smart',
    sortKey: 'smart',
    sortDir: 'desc',
    showCompleted: true,
    protocolBadges: true,
    columns: new Set(['created']),
  }
}

function setEquals<T>(a: Set<T>, b: Set<T>): boolean {
  if (a.size !== b.size) return false
  for (const x of a) if (!b.has(x)) return false
  return true
}

/** 是否偏离出厂默认（顶栏圆点判定依据）。 */
export function isDefaultViewPrefs(p: ViewPrefs): boolean {
  const d = defaultViewPrefs()
  return (
    p.form === d.form &&
    p.density === d.density &&
    p.groupBy === d.groupBy &&
    p.sortKey === d.sortKey &&
    p.sortDir === d.sortDir &&
    p.showCompleted === d.showCompleted &&
    p.protocolBadges === d.protocolBadges &&
    setEquals(p.columns, d.columns)
  )
}

function toJson(p: ViewPrefs): Record<string, unknown> {
  // v:2 起空列数组是「用户显式清空」的有效状态；v1 存量的空数组只是旧默认（当时
  // 出厂列为空），读取时回退新默认，保证升级零感知。
  return { ...p, columns: Array.from(p.columns), v: 2 }
}

function enumFrom<T extends string>(values: readonly T[], raw: unknown, fallback: T): T {
  return typeof raw === 'string' && (values as readonly string[]).includes(raw) ? (raw as T) : fallback
}

/** 从 JSON 反序列化；字段缺失/类型不符/枚举值未知逐项回退默认值（schema 演进容错）。 */
function fromJson(json: Record<string, unknown>): ViewPrefs {
  const d = defaultViewPrefs()
  const rawColumns = json.columns
  const explicitEmpty = json.v === 2
  const columns =
    Array.isArray(rawColumns) && (rawColumns.length > 0 || explicitEmpty)
      ? new Set(rawColumns.filter((c): c is TaskColumnId => COLUMN_CANONICAL_ORDER.includes(c as TaskColumnId)))
      : d.columns
  return {
    form: enumFrom<ViewForm>(['list', 'grid'], json.form, d.form),
    density: enumFrom<ViewDensity>(['comfortable', 'compact'], json.density, d.density),
    groupBy: enumFrom<ViewGroupBy>(GROUP_BY_CYCLE, json.groupBy, d.groupBy),
    sortKey: enumFrom<ViewSortKey>(SORT_KEY_CYCLE, json.sortKey, d.sortKey),
    sortDir: enumFrom<SortDir>(['asc', 'desc'], json.sortDir, d.sortDir),
    showCompleted: typeof json.showCompleted === 'boolean' ? json.showCompleted : d.showCompleted,
    protocolBadges: typeof json.protocolBadges === 'boolean' ? json.protocolBadges : d.protocolBadges,
    columns,
  }
}

function decode(raw: string | null): ViewPrefs | null {
  if (!raw) return null
  try {
    const parsed = JSON.parse(raw)
    if (parsed && typeof parsed === 'object') return fromJson(parsed as Record<string, unknown>)
  } catch {
    // 损坏的 JSON 视作未设置，回退默认；不阻塞渲染。
  }
  return null
}

const GLOBAL_KEY = 'fluxdown.viewPrefs'
const tabKey = (tab: string) => `fluxdown.viewPrefs.${tab}`

/** 已知的状态页签 key 集合，构造时预加载覆盖层用。 */
const KNOWN_TABS = ['all', 'downloading', 'completed', 'paused', 'error']

interface ViewPrefsState {
  global: ViewPrefs
  overrides: Record<string, ViewPrefs>
}

function loadInitial(): ViewPrefsState {
  const global = decode(localStorage.getItem(GLOBAL_KEY)) ?? defaultViewPrefs()
  const overrides: Record<string, ViewPrefs> = {}
  for (const tab of KNOWN_TABS) {
    const decoded = decode(localStorage.getItem(tabKey(tab)))
    if (decoded) overrides[tab] = decoded
  }
  return { global, overrides }
}

const viewPrefsStore = new Store<ViewPrefsState>(loadInitial())

/** 解析指定页签的有效视图偏好：有覆盖层用覆盖层，否则回退全局默认。 */
export function resolveViewPrefs(tab: string): ViewPrefs {
  return viewPrefsStore.get().overrides[tab] ?? viewPrefsStore.get().global
}

/** 对指定页签应用一次偏好变更：写入该页签的覆盖层并持久化 + 广播（只影响
 *  当前页签，从不触碰全局或其它页签）。 */
export function updateViewPrefs(tab: string, updater: (current: ViewPrefs) => ViewPrefs) {
  const next = updater(resolveViewPrefs(tab))
  localStorage.setItem(tabKey(tab), JSON.stringify(toJson(next)))
  viewPrefsStore.set((prev) => ({ ...prev, overrides: { ...prev.overrides, [tab]: next } }))
}

/** 重置指定页签为出厂默认（清除该页签覆盖层，面板「重置为默认」用）。 */
export function resetViewPrefs(tab: string) {
  if (!(tab in viewPrefsStore.get().overrides)) return
  localStorage.removeItem(tabKey(tab))
  viewPrefsStore.set((prev) => {
    const overrides = { ...prev.overrides }
    delete overrides[tab]
    return { ...prev, overrides }
  })
}

/** 响应式读取指定页签当前有效视图偏好（页签切换/其它组件改动都会触发重渲染）。 */
export function useViewPrefs(tab: string): ViewPrefs {
  const state = useStore(viewPrefsStore)
  return state.overrides[tab] ?? state.global
}

/** 形态/密度/分组/排序 label（面板 chips/segmented + 状态栏回显共用）。 */
export function viewFormLabel(f: ViewForm): string {
  return f === 'list' ? t('view.formList') : t('view.formGrid')
}
export function viewDensityLabel(d: ViewDensity): string {
  return d === 'comfortable' ? t('view.densityComfortable') : t('view.densityCompact')
}
export function viewGroupByLabel(g: ViewGroupBy): string {
  const KEYS = {
    smart: 'view.groupSmart',
    date: 'view.groupDate',
    status: 'view.groupStatus',
    type: 'view.groupType',
    queue: 'view.groupQueue',
    site: 'view.groupSite',
    none: 'view.groupNone',
  } as const
  return t(KEYS[g])
}
export function viewSortKeyLabel(k: ViewSortKey): string {
  const KEYS = {
    smart: 'view.sortSmart',
    created: 'view.sortCreated',
    name: 'view.sortName',
    size: 'view.sortSize',
    progress: 'view.sortProgress',
    speed: 'view.sortSpeed',
  } as const
  return t(KEYS[k])
}

/** 组合视图状态文本（顶栏「显示选项」按钮 tooltip / 状态栏右端回显共用）：
 *  `<列表[· 密度]/网格> · 按<分组>分组 · <排序>排序`（网格形态无密度段）。 */
export function describeViewState(prefs: ViewPrefs): string {
  const formPart = prefs.form === 'list' ? `${viewFormLabel(prefs.form)} · ${viewDensityLabel(prefs.density)}` : viewFormLabel(prefs.form)
  return `${formPart} · ${t('view.groupedByLabel', { dim: viewGroupByLabel(prefs.groupBy) })} · ${t('view.sortedByLabel', { key: viewSortKeyLabel(prefs.sortKey) })}`
}
