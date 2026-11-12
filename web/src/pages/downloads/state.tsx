// 下载页共享状态：任务行投影、筛选/搜索、视图偏好（300ms 防抖写回）、选中集、详情面板绑定。
// 页面内组件（侧栏 / 标题栏 / 表格 / 选择条 / 详情）通过 `useDownloads()` 读取；状态栏小部件直接读全局 store，不经此处。

import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../i18n'
import { useIsMobile } from '../../ui'
import {
  rpc,
  useAgent,
  useConfigValue,
  useDaemon,
  usePref,
  useTaskTableStatus,
  useTasks,
} from '../../lib/rpc'
import type {
  AgentSessionDto,
  CloudDevice,
  GroupDto,
  JsonValue,
  LinkDeviceInfo,
  QueueDto,
  QueuePositionDto,
  RemoteTaskDto,
  TaskRuntimeDto,
} from '../../lib/rpc'
import { CategoryIndex, categoriesFromPreference } from './model/categories'
import { RescanThrottle } from '../../lib/rescanThrottle'
import { currentDeviceId, visibleRemoteTasks } from './model/devices'
import { SELECTION_ALL } from './model/filters'
import type { SidebarSelection } from './model/filters'
import { remoteCan } from './model/batchPlan'
import { isDownloadable } from './model/actions'
import type { SidebarCounts } from './model/counts'
import { useLiveSpeeds } from './model/liveSpeeds'
import { buildVisibleRows, dirCollapseKey, dominantCategory, groupFullySelected, groupRowsByDirectory, groupsMatchingQuery, keysInVisualSpan, rowIndexOfKey, selectGroupOnClick, selectionIsGroup } from './model/groupRows'
import type { BucketInfo, GroupFolderRow, VisibleRow, VisibleRowsInput } from './model/groupRows'
import type { DirectoryItem } from './model/directoryTree'
import { makeAggregate } from './model/groupTree'
import type { GroupAggregate, GroupCounts } from './model/groupTree'
import { buildRemoteView, canPause, canResumeSeeding, isLocalKey, sourceSite, STATE_RANK } from './model/task'
import type { DownloadTaskView, RowKey } from './model/task'
import { useRowEngine } from './model/useRowEngine'
import { queueLabelById } from './dialogs/utils'
import { useDaemonReady } from './statusbar/widget'
import { compareViews, dateBucketOf, DATE_BUCKET_ORDER, parseViewPrefs, VIEW_PREFS_KEY } from './model/viewPrefs'
import type { ViewPrefs } from './model/viewPrefs'

// ── 类型 ──

/** 按键读取当前视图（背后是行引擎的持久索引，不随进度帧重建）。 */
export interface ViewLookup {
  get: (key: RowKey) => DownloadTaskView | undefined
}

/** 选中集合投影（选择条 / 工具栏）：只统计仍存在的选中任务。 */
export interface SelectionSummary {
  count: number
  any: boolean
  /** 含本地任务。 */
  anyLocal: boolean
  /** 全部为本地已完成任务（可下载文件）。 */
  allDownloadable: boolean
  /** 含下载中 / 排队（可暂停）。 */
  anyActive: boolean
  /** 含暂停 / 失败（可继续）。 */
  anyResumable: boolean
}

export interface ClickModifiers {
  shift: boolean
  /** Ctrl（Win/Linux）或 Cmd（macOS）。 */
  secondary: boolean
}

export interface DownloadsContextValue {
  /** daemon 连接就绪（false = 只读快照）。 */
  connected: boolean
  /**
   * 任务表仍在分页载入（`daemon.taskTotal` / `queueTotal` 存在）：本地任务不完整，不得当作完整列表
   * （计数、空态、批量操作范围、「任务已消失」推断都要跳过）；此时 `selectedViews` 恒为空。
   */
  tasksLoading: boolean
  /** 服务端任务总数（载入期间取快照头里的 `taskTotal`）。 */
  taskTotal: number
  /** 已载入的任务行数。 */
  loadedTasks: number
  /** 全部视图（本地 + 远程）：O(N) 现取，只给菜单动作用，渲染路径不要碰。 */
  allViews: () => readonly DownloadTaskView[]
  byKey: ViewLookup
  /** 侧栏计数（随任务增删改增量维护；载入期间不完整）。 */
  counts: SidebarCounts
  /** 任一行内容变化时递增（行对象原位更新，靠它让消费者重新读取）。 */
  contentVersion: number
  categories: CategoryIndex
  queues: readonly QueueDto[]
  queueName: (queueId: string) => string
  groups: readonly GroupDto[]
  /** 任务组全部成员（不受侧栏筛选 / 搜索影响）的计数与聚合；没有成员返回 undefined。 */
  groupTotals: (groupId: string) => GroupAggregate | undefined
  cloudDevices: readonly CloudDevice[]
  linkedDevices: readonly LinkDeviceInfo[]

  sidebarSelection: SidebarSelection
  setSidebarSelection: (selection: SidebarSelection) => void
  query: string
  setQuery: (query: string) => void

  prefs: ViewPrefs
  /** 修改视图偏好；300ms 防抖写回 `desktop.downloads.view`（sync:false）。返回原对象 = 无变化。 */
  updatePrefs: (mutate: (prefs: ViewPrefs) => ViewPrefs) => void

  /** 当前筛选 + 搜索 + 分组 + 排序后的可见行。 */
  rows: readonly VisibleRow[]
  /** 可见任务数（不含折叠分组内的任务）。 */
  visibleCount: number
  /** 选中集里仍可见的任务数（表头全选框三态用）。 */
  visibleSelected: number
  selected: ReadonlySet<RowKey>
  selectedViews: readonly DownloadTaskView[]
  summary: SelectionSummary
  /** 显式多选模式：Ctrl/Cmd+点击、行首复选框、右键「选择 / 全选」或全选进入，普通单击 / 清空选择退出（与 GPUI `multi_select` 对齐）。 */
  multiSelect: boolean
  /** 点击行：shift 连选，ctrl/cmd 切换，否则单选。 */
  clickSelect: (key: RowKey, modifiers: ClickModifiers) => void
  /**
   * 划选：按下行 `press` 到当前行 `current` 的可见行闭区间。
   * `fromAnchor` 时起点改为选区锚点所在行；`additive` 时并入 `base`（按下时的选区）。
   * 区间含多个任务，或并入已有选区时，进入显式多选。
   */
  sweepSelect: (
    press: number,
    current: number,
    gesture: { additive: boolean; fromAnchor: boolean; base: ReadonlySet<RowKey> | null },
  ) => void
  /** 只切换该行选中状态（复选框 / 移动端点选）。 */
  toggleSelected: (key: RowKey) => void
  /** 桌面表格行首复选框（与 GPUI `toggle_multi_select` 对齐）：未处于多选模式时以该行开启多选，否则切换该行。 */
  toggleMultiSelect: (key: RowKey) => void
  /** 右键「选择」：保留当前选区并进入显式多选模式。 */
  enterMultiSelect: () => void
  /** 右键 / 长按：未选中则单选它，已选中保持当前选区。 */
  contextSelect: (key: RowKey) => void
  selectAll: () => void
  clearSelection: () => void
  /** 折叠 / 展开分组方式的桶标题（偏好 `collapsed_groups`）。 */
  toggleBucketCollapsed: (bucketKey: string) => void
  /** 组行点击（普通 / Cmd·Ctrl / Shift 的选区语义见 `selectGroupOnClick`）。 */
  clickGroup: (row: GroupFolderRow, modifiers: ClickModifiers) => void
  /** 组行首格复选框：切换全部通过成员（与任务的 toggle 多选同语义）。 */
  toggleGroupSelected: (row: GroupFolderRow) => void
  /** 组行右键：成员未全选时先执行普通单击选择。 */
  contextSelectGroup: (row: GroupFolderRow) => void
  /** 切换组展开（运行期状态，不持久化）。 */
  toggleGroupExpanded: (groupId: string) => void
  /** 切换组内目录折叠（运行期状态）。 */
  toggleDirCollapsed: (groupId: string, dir: string) => void
  /** 停靠面板正在显示「组概览」的组 id（聚焦组且选区恰好等于其通过成员）。 */
  overviewGroupId: string | null
  /** 聚焦该组（选中其通过成员）并打开详情面板。 */
  showGroupDetail: (row: GroupFolderRow) => void

  /** 详情面板是否打开：桌面取偏好 `detail_open`，移动端（Sheet）为页面本地状态。 */
  detailOpen: boolean
  /** 详情面板当前承载的本地任务 id。 */
  detailTaskId: string | null
  /** 打开详情面板并切换到该任务（同时把它设为唯一选中项）。 */
  showDetail: (taskId: string) => void
  closeDetail: () => void

  /** 移动端侧栏抽屉。 */
  sidebarOpen: boolean
  setSidebarOpen: (open: boolean) => void
}

const Context = createContext<DownloadsContextValue | null>(null)

export function useDownloads(): DownloadsContextValue {
  const value = useContext(Context)
  if (!value) throw new Error('useDownloads must be used inside <DownloadsProvider>')
  return value
}

const EMPTY_RUNTIME: Readonly<Record<string, TaskRuntimeDto>> = {}
const EMPTY_PRIORITY: readonly string[] = []
const EMPTY_REMOTE: readonly RemoteTaskDto[] = []
const EMPTY_QUEUES: readonly QueueDto[] = []
const EMPTY_QUEUE_POSITIONS: readonly QueuePositionDto[] = []
const EMPTY_GROUPS: readonly GroupDto[] = []
const EMPTY_CLOUD: readonly CloudDevice[] = []
const EMPTY_LINKED: readonly LinkDeviceInfo[] = []
const EMPTY_SELECTED: ReadonlySet<RowKey> = new Set()
const EMPTY_VIEWS: readonly DownloadTaskView[] = []
const EMPTY_STRINGS: ReadonlySet<string> = new Set()
const PREFS_DEBOUNCE_MS = 300

/** 集合切换一个成员（返回新集合）。 */
function toggledSet(current: ReadonlySet<string>, item: string): ReadonlySet<string> {
  const next = new Set(current)
  if (!next.delete(item)) next.add(item)
  return next
}

/** 只保留满足条件的成员；没有变化返回原集合（避免无谓重渲染）。 */
function pruneSet(current: ReadonlySet<string>, keep: (item: string) => boolean): ReadonlySet<string> {
  if (current.size === 0) return current
  const kept = [...current].filter(keep)
  return kept.length === current.size ? current : new Set(kept)
}

const selectRuntime = (daemon: { taskRuntime: Record<string, TaskRuntimeDto> }) => daemon.taskRuntime
const selectPriority = (daemon: { priority: string[] }) => daemon.priority
const selectQueuePositions = (daemon: { queuePositions: QueuePositionDto[] }) => daemon.queuePositions
const selectQueues = (daemon: { queues: QueueDto[] }) => daemon.queues
const selectGroups = (daemon: { groups: GroupDto[] }) => daemon.groups
const selectRemote = (snapshot: { remoteTasks: RemoteTaskDto[] }) => snapshot.remoteTasks
const selectSession = (snapshot: { session: AgentSessionDto | null }) => snapshot.session
const selectCloud = (snapshot: { cloudDevices: CloudDevice[] }) => snapshot.cloudDevices
const selectLinked = (snapshot: { linkedDevices: LinkDeviceInfo[] }) => snapshot.linkedDevices

// ── 视图偏好：同步 + 防抖写回 ──

function useViewPrefsState(): { prefs: ViewPrefs; updatePrefs: DownloadsContextValue['updatePrefs'] } {
  const serverValue = usePref<unknown>(VIEW_PREFS_KEY)
  const [prefs, setPrefs] = useState<ViewPrefs>(() => parseViewPrefs(serverValue))
  const prefsRef = useRef(prefs)
  /** 最近一次应用 / 写出的规范化序列化值：自己写出的值回流不重载。 */
  const applied = useRef(serverValue === undefined ? undefined : JSON.stringify(parseViewPrefs(serverValue)))
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const flush = useCallback(() => {
    if (timer.current === null) return
    clearTimeout(timer.current)
    timer.current = null
    const current = prefsRef.current
    applied.current = JSON.stringify(current)
    rpc.agent.preferences
      .patch({ values: { [VIEW_PREFS_KEY]: current as unknown as JsonValue }, sync: false })
      .catch(() => undefined)
  }, [])

  useEffect(() => {
    if (serverValue === undefined) return
    const normalized = JSON.stringify(parseViewPrefs(serverValue))
    if (normalized === applied.current) return
    applied.current = normalized
    const next = parseViewPrefs(serverValue)
    prefsRef.current = next
    setPrefs(next)
  }, [serverValue])

  useEffect(() => flush, [flush])

  const updatePrefs = useCallback<DownloadsContextValue['updatePrefs']>(
    (mutate) => {
      const next = mutate(prefsRef.current)
      if (next === prefsRef.current) return
      prefsRef.current = next
      setPrefs(next)
      if (timer.current !== null) clearTimeout(timer.current)
      timer.current = setTimeout(flush, PREFS_DEBOUNCE_MS)
    },
    [flush],
  )
  return { prefs, updatePrefs }
}

// ── Provider ──

export function DownloadsProvider({ children }: { children: ReactNode }) {
  const t = useT()
  const tasks = useTasks()
  const tableStatus = useTaskTableStatus()
  const tasksLoading = tableStatus.loading
  const runtimes = useDaemon(selectRuntime, EMPTY_RUNTIME)
  const priority = useDaemon(selectPriority, EMPTY_PRIORITY as string[])
  const queuePositions = useDaemon(selectQueuePositions, EMPTY_QUEUE_POSITIONS as QueuePositionDto[])
  const queues = useDaemon(selectQueues, EMPTY_QUEUES as QueueDto[])
  const groups = useDaemon(selectGroups, EMPTY_GROUPS as GroupDto[])
  const remoteTasks = useAgent(selectRemote, EMPTY_REMOTE as RemoteTaskDto[])
  const session = useAgent(selectSession, null)
  const cloudDevices = useAgent(selectCloud, EMPTY_CLOUD as CloudDevice[])
  const linkedDevices = useAgent(selectLinked, EMPTY_LINKED as LinkDeviceInfo[])
  const configuredDefaultRoot = useConfigValue('default_save_dir') ?? ''
  const runtimeSaveDir = useDaemon((daemon) => daemon.runtimeStats.saveDir, '')
  const directoryDefaultRoot = configuredDefaultRoot.trim() !== '' ? configuredDefaultRoot : runtimeSaveDir
  const categoriesPref = usePref<unknown>('custom_categories')
  const live = useLiveSpeeds()
  const connected = useDaemonReady()

  // 页面可见 / 连接就绪时重扫已完成文件（daemon 空闲不再定时扫描）；节流镜像 GPUI RescanThrottle。
  const rescanThrottle = useRef(new RescanThrottle())
  const rescanTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    if (!connected) return
    const fire = () => {
      rpc.daemon.task.rescan().catch(() => {})
    }
    const request = () => {
      if (document.visibilityState !== 'visible') return
      const decision = rescanThrottle.current.request(Date.now())
      if (decision.kind === 'now') fire()
      else if (decision.kind === 'after') {
        rescanTimer.current = setTimeout(() => {
          rescanTimer.current = null
          rescanThrottle.current.trailingFired(Date.now())
          fire()
        }, decision.delayMs)
      }
    }
    request()
    document.addEventListener('visibilitychange', request)
    return () => document.removeEventListener('visibilitychange', request)
  }, [connected])
  useEffect(
    () => () => {
      if (rescanTimer.current !== null) clearTimeout(rescanTimer.current)
    },
    [],
  )

  const { prefs, updatePrefs } = useViewPrefsState()
  const [sidebarSelection, setSidebarSelectionState] = useState<SidebarSelection>(SELECTION_ALL)
  const [query, setQueryState] = useState('')
  const [selected, setSelected] = useState<ReadonlySet<RowKey>>(EMPTY_SELECTED)
  const [multiSelectMode, setMultiSelectMode] = useState(false)
  const anchor = useRef<RowKey | null>(null)
  /** 面板「组概览」聚焦的组（普通单击组行设置；任何任务点选 / 清空选区清除）。 */
  const [focusedGroup, setFocusedGroup] = useState<string | null>(null)
  const [detailTaskId, setDetailTaskId] = useState<string | null>(null)
  const mobile = useIsMobile()
  const [mobileDetailOpen, setMobileDetailOpen] = useState(false)
  const detailOpen = mobile ? mobileDetailOpen : prefs.detail_open
  const [sidebarOpen, setSidebarOpen] = useState(false)

  const categories = useMemo(() => new CategoryIndex(categoriesFromPreference(categoriesPref)), [categoriesPref])

  const boostedId = priority[0] ?? null
  const currentId = useMemo(() => currentDeviceId(session, cloudDevices), [session, cloudDevices])
  const remoteViews = useMemo(
    () => visibleRemoteTasks(remoteTasks, currentId).map(buildRemoteView),
    [remoteTasks, currentId],
  )
  const normalizedQuery = useMemo(() => query.trim().toLowerCase(), [query])
  const groupMatches = useMemo(() => groupsMatchingQuery(groups, normalizedQuery), [groups, normalizedQuery])

  // 行引擎：任务行投影 / 筛选 / 搜索 / 排序 / 行顺序稳定 / 侧栏计数 / 任务组汇总都按输入差分增量维护，
  // 一帧进度只触及变化的行（见 model/rowEngine.ts）。
  const { engine, snapshot } = useRowEngine({
    tasks,
    speeds: live.download,
    uploads: live.upload,
    boostedId,
    queuePositions,
    runtimes,
    connected,
    remote: remoteViews,
    selection: sidebarSelection,
    query: normalizedQuery,
    groupMatches,
    categories,
    sortKey: prefs.sort_key,
    sortDir: prefs.sort_dir,
  })
  const byKey = useMemo<ViewLookup>(() => ({ get: (key) => engine.viewOf(key) }), [engine])
  const allViews = useCallback(() => engine.views(), [engine])
  const groupTotals = useCallback<DownloadsContextValue['groupTotals']>(
    (groupId) => {
      const tally = engine.groupTally(groupId)
      if (tally === undefined) return undefined
      const counts: GroupCounts = {
        total: tally.total,
        done: tally.completed,
        downloading: tally.downloading,
        pending: tally.pending,
        paused: tally.paused,
        failed: tally.failed,
      }
      return makeAggregate(counts, tally.totalBytes, tally.downloaded, tally.speed, false)
    },
    [engine],
  )

  const queueName = useCallback((queueId: string) => queueLabelById(t, queues, queueId), [t, queues])

  // 组展开 / 目录折叠：运行期状态，不持久化；组消失时清理。
  const [expandedGroups, setExpandedGroups] = useState<ReadonlySet<string>>(EMPTY_STRINGS)
  const [dirCollapsed, setDirCollapsed] = useState<ReadonlySet<string>>(EMPTY_STRINGS)
  useEffect(() => {
    const live = new Set(groups.map((group) => group.groupId))
    setExpandedGroups((current) => pruneSet(current, (id) => live.has(id)))
    setDirCollapsed((current) => pruneSet(current, (key) => live.has(key.slice(0, key.indexOf('\u0000')))))
  }, [groups])

  const compare = useMemo<VisibleRowsInput['compare']>(() => {
    const sort = { sort_key: prefs.sort_key, sort_dir: prefs.sort_dir }
    return (left, right) => compareViews(sort, left, right)
  }, [prefs.sort_key, prefs.sort_dir])
  const collapsedBuckets = useMemo(() => new Set(prefs.collapsed_groups), [prefs.collapsed_groups])

  // 分组方式的桶归属：任务按自身视图，组行按聚合代理视图（「类型」取通过成员里占多数的分类）。
  const bucketOf = useMemo<VisibleRowsInput['bucketOf']>(() => {
    const groupBy = prefs.group_by
    if (groupBy === 'none' || groupBy === 'directory') return null
    const queueIndex = new Map<string, number>()
    if (groupBy === 'queue') {
      queues.forEach((queue, index) => {
        if (!queueIndex.has(queue.queueId)) queueIndex.set(queue.queueId, index)
      })
    }
    const categoryOrder = (id: string): number => {
      const index = categories.indexOf(id)
      return index < 0 ? Number.MAX_SAFE_INTEGER : index
    }
    return (view, groupMembers): BucketInfo => {
      switch (groupBy) {
        case 'status':
          return { key: `status:${view.state}`, label: stateLabel(t, view.state), order: STATE_RANK[view.state] }
        case 'date': {
          const bucket = dateBucketOf(view.createdAtSecs)
          return { key: `date:${bucket}`, label: t(DATE_LABEL_KEY[bucket]), order: DATE_BUCKET_ORDER[bucket] }
        }
        case 'type': {
          const own = categories.categoryOf(view)
          const id =
            groupMembers === null
              ? own
              : (dominantCategory(groupMembers, (member) => categories.categoryOf(member), categoryOrder) ?? own)
          const index = categories.indexOf(id)
          const rule = categories.rules[index]
          return {
            key: `type:${id}`,
            label: rule ? categoryLabel(t, rule.dto) : t('categoryOther'),
            order: rule ? index : Number.MAX_SAFE_INTEGER,
          }
        }
        case 'queue': {
          if (view.source === 'remote') {
            return { key: 'queue:remote', label: t('remoteTasksGroup'), order: Number.MAX_SAFE_INTEGER }
          }
          const index = queueIndex.get(view.queueId) ?? -1
          return {
            key: `queue:${view.queueId}`,
            label: queueLabelById(t, queues, view.queueId),
            order: index >= 0 ? index : Number.MAX_SAFE_INTEGER - 1,
          }
        }
        case 'site': {
          const site = sourceSite(view)
          const label = site !== '' ? site : view.protocol === 'bt' ? t('viewSiteBt') : '—'
          return { key: `site:${label}`, label, order: 0 }
        }
      }
    }
  }, [prefs.group_by, queues, categories, t])

  // 可见行：任务组并成文件夹行（按聚合代理排序并入）、分桶、展开成员 / 目录行（O(N)，只在布局或分组归属
  // 字段变化时重算，进度帧不触发；组行聚合在渲染时读成员行的最新视图）。
  const built = useMemo(
    () =>
      buildVisibleRows({
        rows: snapshot.rows,
        groups,
        compare,
        bucketOf,
        collapsedBuckets,
        expanded: expandedGroups,
        dirCollapsed,
        searching: normalizedQuery !== '',
      }),
    // 行对象原位更新：内容变化不重排，只有布局 / 分组归属变化（版本号）才重算。
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `snapshot.rows` 随 layoutVersion 换身份，groupingVersion 覆盖分组归属字段
    [snapshot.rows, snapshot.groupingVersion, groups, compare, bucketOf, collapsedBuckets, expandedGroups, dirCollapsed, normalizedQuery],
  )
  const directoryItems = useMemo<DirectoryItem[]>(() => {
    if (prefs.group_by !== 'directory') return []
    const roots = new Map<string, { root: string; label: string }>()
    for (const device of cloudDevices) roots.set(device.deviceId, { root: device.defaultSaveDir ?? '', label: device.name || device.deviceId })
    for (const device of linkedDevices) {
      const id = device.cloudDeviceId || device.fingerprint
      const known = roots.get(id)
      roots.set(id, {
        root: known?.root || device.defaultSaveDir || '',
        label: known?.label || device.customName || device.name || id,
      })
    }
    const items: DirectoryItem[] = []
    for (const row of built.rows) {
      if (row.type === 'task') {
        const view = row.view
        const device = view.source === 'remote' ? roots.get(view.toDevice) : undefined
        items.push({
          path: view.saveDir,
          root: view.source === 'local' ? directoryDefaultRoot : device?.root ?? '',
          scope: view.source === 'remote' ? view.toDevice || 'remote' : '',
          scope_label: view.source === 'remote' ? device?.label || view.toDevice || t('remoteTasksGroup') : '',
          count: 1,
        })
      } else if (row.type === 'group') {
        items.push({ path: row.group.saveDir, root: directoryDefaultRoot, scope: '', scope_label: '', count: row.members.length })
      }
    }
    return items
  }, [built.rows, prefs.group_by, cloudDevices, linkedDevices, directoryDefaultRoot, t])
  const directoryCollapsed = useMemo(() => new Set(prefs.collapsed_groups), [prefs.collapsed_groups])
  const directoryRows = useMemo(
    () => prefs.group_by === 'directory' ? groupRowsByDirectory(built.rows, directoryItems, directoryCollapsed) : built.rows,
    [prefs.group_by, built.rows, directoryItems, directoryCollapsed],
  )
  const rows: readonly VisibleRow[] = directoryRows
  const visibleCount = prefs.group_by === 'directory'
    ? rows.reduce((count, row) => count + (row.type === 'task' || row.type === 'member' ? 1 : row.type === 'group' && !row.expanded ? row.members.length : 0), 0)
    : built.visibleCount

  // 选中集里仍可见的任务数：没有组参与且不分桶时可见集 = 命中集；否则按可见任务序列（折叠桶 / 折叠目录内的不算）。
  const visibleSelected = useMemo(() => {
    if (selected.size === 0) return 0
    let count = 0
    if (prefs.group_by === 'none' && built.folders.size === 0) {
      for (const key of selected) if (engine.isMatched(key)) count += 1
    } else {
      const visible = prefs.group_by === 'directory'
        ? new Set(directoryRows.flatMap((row) =>
            row.type === 'task' || row.type === 'member' ? [row.key] :
              row.type === 'group' && !row.expanded ? [...row.orderedKeys()] : [],
          ))
        : new Set(built.sequence())
      for (const key of selected) if (visible.has(key)) count += 1
    }
    return count
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 命中集随 matchVersion 变化
  }, [selected, engine, built, directoryRows, snapshot.matchVersion, prefs.group_by])

  // 选中集只保留当前筛选 + 搜索下仍在视图内的任务（折叠分组内的仍属于当前视图）。
  // 任务表载入期间本地任务不完整，「不在视图内」不代表任务被删：跳过，载入完成后再收敛。
  useEffect(() => {
    if (tasksLoading) return
    setSelected((current) => {
      if (current.size === 0) return current
      const kept = [...current].filter((key) => engine.isMatched(key))
      if (kept.length === current.size) return current
      if (anchor.current !== null && !kept.includes(anchor.current)) anchor.current = null
      return kept.length === 0 ? EMPTY_SELECTED : new Set(kept)
    })
  }, [engine, snapshot.matchVersion, tasksLoading])

  const setSidebarSelection = useCallback((selection: SidebarSelection) => {
    setSidebarSelectionState((current) => (JSON.stringify(current) === JSON.stringify(selection) ? current : selection))
  }, [])

  // 队列被删除后不能继续筛选已不存在的队列。
  useEffect(() => {
    if (sidebarSelection.kind === 'queue' && !queues.some((queue) => queue.queueId === sidebarSelection.queueId)) {
      setSidebarSelectionState(SELECTION_ALL)
    }
  }, [queues, sidebarSelection])

  // 搜索 150ms 防抖由输入框侧完成，这里直接存值。
  const setQuery = useCallback((value: string) => setQueryState(value), [])

  const selectedRef = useRef(selected)
  selectedRef.current = selected
  const multiModeRef = useRef(multiSelectMode)
  multiModeRef.current = multiSelectMode

  const rowsRef = useRef(rows)
  rowsRef.current = rows

  const sweepSelect = useCallback(
    (
      press: number,
      current: number,
      gesture: { additive: boolean; fromAnchor: boolean; base: ReadonlySet<RowKey> | null },
    ) => {
      const visible = rowsRef.current
      let from = press
      if (gesture.fromAnchor && anchor.current !== null) {
        const anchored = rowIndexOfKey(visible, anchor.current)
        if (anchored >= 0) from = anchored
      }
      const keys = keysInVisualSpan(visible, from, current)
      setFocusedGroup(null)
      const next = gesture.additive ? new Set(gesture.base) : new Set<RowKey>()
      for (const key of keys) next.add(key)
      setSelected(next.size === 0 ? EMPTY_SELECTED : next)
      setMultiSelectMode(keys.length > 1 || (gesture.additive && next.size > 0))
      anchor.current = keys[keys.length - 1] ?? anchor.current
    },
    [],
  )

  const clickSelect = useCallback(
    (key: RowKey, modifiers: ClickModifiers) => {
      setFocusedGroup(null)
      if (modifiers.shift && anchor.current !== null) {
        const keys = built.sequence()
        const from = keys.indexOf(anchor.current)
        const to = keys.indexOf(key)
        if (from >= 0 && to >= 0) {
          const [start, end] = from <= to ? [from, to] : [to, from]
          setSelected(new Set(keys.slice(start, end + 1)))
          anchor.current = key
          return
        }
      }
      setSelected((current) => {
        if (modifiers.secondary) {
          const next = new Set(current)
          if (!next.delete(key)) next.add(key)
          return next
        }
        return new Set([key])
      })
      setMultiSelectMode(modifiers.secondary)
      anchor.current = key
    },
    [built],
  )

  const toggleSelected = useCallback((key: RowKey) => {
    setFocusedGroup(null)
    setSelected((current) => {
      const next = new Set(current)
      if (!next.delete(key)) next.add(key)
      return next
    })
    anchor.current = key
  }, [])

  const contextSelect = useCallback((key: RowKey) => {
    setSelected((current) => {
      if (current.has(key)) return current
      setMultiSelectMode(false)
      setFocusedGroup(null)
      return new Set([key])
    })
    anchor.current = key
  }, [])

  const selectAll = useCallback(() => {
    if (tasksLoading) return
    setSelected(new Set(built.sequence()))
    setMultiSelectMode(true)
    setFocusedGroup(null)
    anchor.current = null
  }, [built, tasksLoading])

  const clearSelection = useCallback(() => {
    setSelected(EMPTY_SELECTED)
    setMultiSelectMode(false)
    setFocusedGroup(null)
    anchor.current = null
  }, [])

  const multiSelect = multiSelectMode && selected.size > 0

  const toggleMultiSelect = useCallback(
    (key: RowKey) => {
      setFocusedGroup(null)
      setSelected((current) => {
        if (!multiSelect) return new Set([key])
        const next = new Set(current)
        if (!next.delete(key)) next.add(key)
        return next
      })
      setMultiSelectMode(true)
      anchor.current = key
    },
    [multiSelect],
  )

  const enterMultiSelect = useCallback(() => setMultiSelectMode(true), [])

  const clickGroup = useCallback(
    (row: GroupFolderRow, modifiers: ClickModifiers) => {
      const result = selectGroupOnClick(
        { selected: selectedRef.current, multiSelect: multiModeRef.current, anchor: anchor.current },
        row.groupId,
        row.orderedKeys(),
        built.sequence(),
        modifiers,
      )
      setSelected(result.selected.size === 0 ? EMPTY_SELECTED : result.selected)
      setMultiSelectMode(result.multiSelect)
      setFocusedGroup(result.focusedGroup)
      anchor.current = result.anchor
    },
    [built],
  )

  const toggleGroupSelected = useCallback(
    (row: GroupFolderRow) => {
      const keys = row.orderedKeys()
      setFocusedGroup(null)
      setSelected((current) => {
        if (!multiSelect) return new Set(keys)
        const next = new Set(current)
        if (groupFullySelected(current, keys)) for (const key of keys) next.delete(key)
        else for (const key of keys) next.add(key)
        return next
      })
      setMultiSelectMode(true)
      anchor.current = keys[keys.length - 1] ?? null
    },
    [multiSelect],
  )

  const contextSelectGroup = useCallback(
    (row: GroupFolderRow) => {
      if (!groupFullySelected(selectedRef.current, row.orderedKeys())) clickGroup(row, { shift: false, secondary: false })
    },
    [clickGroup],
  )

  const toggleGroupExpanded = useCallback((groupId: string) => {
    setExpandedGroups((current) => toggledSet(current, groupId))
  }, [])

  const toggleDirCollapsed = useCallback((groupId: string, dir: string) => {
    setDirCollapsed((current) => toggledSet(current, dirCollapseKey(groupId, dir)))
  }, [])

  const toggleBucketCollapsed = useCallback(
    (bucketKey: string) =>
      updatePrefs((current) => ({
        ...current,
        collapsed_groups: current.collapsed_groups.includes(bucketKey)
          ? current.collapsed_groups.filter((item) => item !== bucketKey)
          : [...current.collapsed_groups, bucketKey],
      })),
    [updatePrefs],
  )

  const selectedViews = useMemo(
    () =>
      tasksLoading
        ? EMPTY_VIEWS
        : [...selected].map((key) => byKey.get(key)).filter((view): view is DownloadTaskView => view !== undefined),
    // 行内容原位更新：靠 contentVersion 让选中视图随进度刷新。
    // eslint-disable-next-line react-hooks/exhaustive-deps -- contentVersion 是视图内容的失效信号
    [selected, byKey, snapshot.contentVersion, tasksLoading],
  )

  const summary = useMemo<SelectionSummary>(() => {
    let anyLocal = false
    let anyActive = false
    let anyResumable = false
    let allDownloadable = selectedViews.length > 0
    for (const view of selectedViews) {
      anyLocal ||= view.source === 'local'
      if (canPause(view)) anyActive ||= remoteCan(view, 'pause')
      if (view.state === 'paused' || view.state === 'failed' || canResumeSeeding(view)) anyResumable ||= remoteCan(view, 'resume')
      if (!isDownloadable(view)) allDownloadable = false
    }
    return {
      count: selectedViews.length,
      any: selectedViews.length > 0,
      anyLocal,
      allDownloadable,
      anyActive,
      anyResumable,
    }
  }, [selectedViews])

  const overviewGroupId = useMemo(() => {
    if (focusedGroup === null) return null
    const folder = built.folders.get(focusedGroup)
    return folder !== undefined && selectionIsGroup(selected, folder.members) ? focusedGroup : null
  }, [focusedGroup, built, selected])

  // 详情面板打开时跟随「恰好一个本地选中任务」。
  const singleLocal = selected.size === 1 ? [...selected][0] : undefined
  useEffect(() => {
    if (detailOpen && singleLocal !== undefined && isLocalKey(singleLocal)) {
      setDetailTaskId(singleLocal.slice(2))
    }
  }, [detailOpen, singleLocal])

  const showDetail = useCallback(
    (taskId: string) => {
      setDetailTaskId(taskId)
      setFocusedGroup(null)
      setSelected(new Set([`l:${taskId}`]))
      anchor.current = `l:${taskId}`
      if (mobile) setMobileDetailOpen(true)
      else updatePrefs((current) => (current.detail_open ? current : { ...current, detail_open: true }))
    },
    [updatePrefs, mobile],
  )

  const showGroupDetail = useCallback(
    (row: GroupFolderRow) => {
      const keys = row.orderedKeys()
      setSelected(new Set(keys))
      setMultiSelectMode(false)
      setFocusedGroup(row.groupId)
      anchor.current = keys[keys.length - 1] ?? null
      if (mobile) setMobileDetailOpen(true)
      else updatePrefs((current) => (current.detail_open ? current : { ...current, detail_open: true }))
    },
    [updatePrefs, mobile],
  )

  const closeDetail = useCallback(() => {
    setDetailTaskId(null)
    if (mobile) setMobileDetailOpen(false)
    else updatePrefs((current) => (current.detail_open ? { ...current, detail_open: false } : current))
  }, [updatePrefs, mobile])

  const value = useMemo<DownloadsContextValue>(
    () => ({
      connected,
      tasksLoading,
      taskTotal: tableStatus.total,
      loadedTasks: tableStatus.loaded,
      allViews,
      byKey,
      counts: snapshot.counts,
      contentVersion: snapshot.contentVersion,
      categories,
      queues,
      queueName,
      groups,
      groupTotals,
      cloudDevices,
      linkedDevices,
      sidebarSelection,
      setSidebarSelection,
      query,
      setQuery,
      prefs,
      updatePrefs,
      rows,
      visibleCount,
      visibleSelected,
      selected,
      selectedViews,
      summary,
      multiSelect,
      clickSelect,
      sweepSelect,
      toggleSelected,
      toggleMultiSelect,
      enterMultiSelect,
      contextSelect,
      selectAll,
      clearSelection,
      toggleBucketCollapsed,
      clickGroup,
      toggleGroupSelected,
      contextSelectGroup,
      toggleGroupExpanded,
      toggleDirCollapsed,
      overviewGroupId,
      showGroupDetail,
      detailOpen,
      detailTaskId,
      showDetail,
      closeDetail,
      sidebarOpen,
      setSidebarOpen,
    }),
    [
      connected,
      tasksLoading,
      tableStatus.total,
      tableStatus.loaded,
      allViews,
      byKey,
      snapshot.counts,
      snapshot.contentVersion,
      categories,
      queues,
      queueName,
      groups,
      groupTotals,
      cloudDevices,
      linkedDevices,
      sidebarSelection,
      setSidebarSelection,
      query,
      setQuery,
      prefs,
      updatePrefs,
      rows,
      visibleCount,
      visibleSelected,
      selected,
      selectedViews,
      summary,
      multiSelect,
      clickSelect,
      sweepSelect,
      toggleSelected,
      toggleMultiSelect,
      enterMultiSelect,
      contextSelect,
      selectAll,
      clearSelection,
      toggleBucketCollapsed,
      clickGroup,
      toggleGroupSelected,
      contextSelectGroup,
      toggleGroupExpanded,
      toggleDirCollapsed,
      overviewGroupId,
      showGroupDetail,
      detailOpen,
      detailTaskId,
      showDetail,
      closeDetail,
      sidebarOpen,
    ],
  )
  return <Context.Provider value={value}>{children}</Context.Provider>
}

// ── 分组标签辅助（i18n 键与 GPUI strings.rs 一致） ──

type Translate = (key: string, params?: Record<string, string | number>) => string

const DATE_LABEL_KEY = {
  today: 'today',
  yesterday: 'yesterday',
  this_week: 'thisWeek',
  this_month: 'thisMonth',
  older: 'older',
} as const

const STATE_LABEL_KEY = {
  pending: 'statusPending',
  downloading: 'statusDownloading',
  paused: 'statusPaused',
  completed: 'statusCompleted',
  failed: 'statusError',
} as const

export const stateLabel = (t: Translate, state: keyof typeof STATE_LABEL_KEY): string => t(STATE_LABEL_KEY[state])

const BUILTIN_LABEL_KEY: Record<string, string> = {
  all: 'tabAll',
  video: 'categoryVideo',
  audio: 'categoryAudio',
  document: 'categoryDocument',
  image: 'categoryImage',
  program: 'categoryProgram',
  archive: 'categoryArchive',
  other: 'categoryOther',
}

/** 分类显示名：内置项走 i18n（categoryVideo…），自定义项用 name。 */
export function categoryLabel(t: Translate, dto: { builtinType: string | null; name: string }): string {
  const key = dto.builtinType ? BUILTIN_LABEL_KEY[dto.builtinType] : undefined
  return key ? t(key) : dto.name
}
