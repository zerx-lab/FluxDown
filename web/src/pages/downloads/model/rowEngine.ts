// 下载页行引擎：任务行投影、筛选 / 搜索、排序与「行顺序稳定」的增量实现。
//
// 旧管线每帧（进度 / 速度 / 运行时任意变化）都对全部任务做一遍 `map` + `Map` + `filter` + `sort` + `Set`，
// 25 万任务时单帧数百毫秒。这里改为持久状态 + 输入差分：
//
// - 任务数组按引用逐项差分（原位替换 / 过滤删除 / 尾部追加都是 O(N) 的纯引用比较，不分配），
//   速度 / 运行时 / 队列键 / 优先任务只看被改动的键，所以一帧的开销 = O(改动数 · log N) + 引用扫描；
// - `all` 是全部行的严格有序数组，`shown` 是其中命中「筛选 + 搜索」的子序列（不筛选时与 `all` 同一对象）。
//   改动只在二分定位处摘 / 插，排序键没变的行原位换视图；切换筛选只需对 `all` 过滤，不必重排；
// - `applied` 是「保持期」内用户实际看到的顺序（语义同旧 `resolveRowOrder` / `mergeStable`）：
//   保持期内已有行不挪位置，新行落在 `shown` 给出的位置，离开的行消失；保持期结束后快照回 `shown`；
// - 侧栏计数、任务组汇总随视图增删改增量维护。
//
// 行对象（`TaskRow`）按任务稳定存在，`view` 随内容变化原位替换；数组身份只在顺序 / 成员变化时更换。

import { taskById, touchedTaskIds } from '../../../lib/rpc/apply'
import type { QueuePositionDto, TaskDto, TaskRuntimeDto } from '../../../lib/rpc'
import type { CategoryIndex } from './categories'
import { SidebarCounts } from './counts'
import { sameSelection, selectionMatches, selectsEverything } from './filters'
import type { SidebarSelection } from './filters'
import { isDynamicSortKey, isHolding } from './rowOrder'
import { buildLocalView, localKey } from './task'
import type { DownloadTaskView, RowKey } from './task'
import { compareViews, matchesQuery } from './viewPrefs'
import type { SortDir, ViewSortKey } from './viewPrefs'

/** 表格里的一行任务。`view` 随内容刷新原位替换（行对象本身稳定）。 */
export interface TaskRow {
  readonly type: 'task'
  readonly key: RowKey
  view: DownloadTaskView
}

/** 任务组汇总（本地任务按 `groupId` 累计）。 */
export interface GroupTally {
  total: number
  completed: number
  failed: number
  /** 下载中 + 准备中（引擎 status 5 已占用并发名额）。 */
  downloading: number
  /** 排队（不含准备中）。 */
  pending: number
  paused: number
  downloaded: number
  totalBytes: number
  /** 下载中成员的速度合计（B/s）。 */
  speed: number
}

export interface RowEngineInput {
  tasks: readonly TaskDto[]
  /** taskId → 下载速率 B/s。 */
  speeds: Readonly<Record<string, number>>
  /** taskId → BT 上传速率 B/s。 */
  uploads: Readonly<Record<string, number>>
  boostedId: string | null
  queuePositions: readonly QueuePositionDto[]
  runtimes: Readonly<Record<string, TaskRuntimeDto>>
  connected: boolean
  /** 已按设备过滤、建好视图的远程任务。 */
  remote: readonly DownloadTaskView[]
  selection: SidebarSelection
  /** 已 trim + 小写的搜索词。 */
  query: string
  /** 显示名命中搜索词的任务组 id：其成员即使自身不命中搜索也算命中（搜索为空时为空集）。 */
  groupMatches: ReadonlySet<string>
  categories: CategoryIndex
  sortKey: ViewSortKey
  sortDir: SortDir
}

/** 一次 `update` 的时钟与保持期输入。 */
export interface HoldClock {
  now: number
  lastPointerAt: number
  menuOpen: boolean
  /** 保持期结束后的强制重排。 */
  force: boolean
}

export interface RowSnapshot {
  /** 用户看到的行顺序；仅在顺序 / 成员变化时换新数组。 */
  rows: readonly TaskRow[]
  /** `rows` 顺序 / 成员变化计数。 */
  layoutVersion: number
  /** 任一行内容变化计数（行对象原位更新，靠它让消费者重新读取）。 */
  contentVersion: number
  /** 分组归属相关字段（状态 / 创建时间 / 名称 / 队列 / 来源 / 任务组）变化计数。 */
  groupingVersion: number
  /** 「命中筛选 + 搜索」集合变化计数。 */
  matchVersion: number
  /** 任务组汇总变化计数。 */
  tallyVersion: number
  counts: SidebarCounts
  /** 保持期内已应用顺序落后于排序结果：保持期结束后需强制重排。 */
  stale: boolean
  /** 上次真正改变已应用顺序的时刻（ms）。 */
  lastReorderAt: number
}

interface Entry extends TaskRow {
  /** 本地任务的最新 DTO；远程行为 undefined。 */
  task: TaskDto | undefined
  speed: number | null
  upload: number | null
  boosted: boolean
  queueKey: number | null
  runtime: TaskRuntimeDto | undefined
  connected: boolean
  /** 当前是否在 `shown` 里。 */
  matched: boolean
  /** 创建它的帧序号：本帧新建的行不再走差分刷新。 */
  born: number
}

interface Update {
  entry: Entry
  oldView: DownloadTaskView
  view: DownloadTaskView
}

type RowCompare = (left: DownloadTaskView, right: DownloadTaskView) => number

/** 摘 / 插不超过该数时逐个 splice（memmove）。 */
const SEQUENTIAL_LIMIT = 16
/** 摘出不超过该数时按二分定位的下标分段复制；更多（整表清空 / 重载）时单趟过滤。 */
const LOCATED_LIMIT = 4096

interface Membership {
  has(entry: Entry): boolean
}

interface Detached {
  removed: readonly Entry[]
  /** 顺序摘取：被摘行摘取前的下标。 */
  origin: ReadonlyMap<Entry, number> | null
  /** 批量摘取：摘取前的数组。 */
  before: readonly Entry[] | null
  gone: Membership
}

interface Attached {
  /** 有行进出（不含原样摘出又插回的行）。 */
  membershipChanged: boolean
  /** 原样摘出又插回的行数。 */
  moved: number
  /** 行的相对顺序变了（有成员变化时保守地为 true）。 */
  positionChanged: boolean
}

const NOTHING: Membership = { has: () => false }

function sameRows(left: readonly TaskRow[], right: readonly TaskRow[]): boolean {
  if (left.length !== right.length) return false
  for (let index = 0; index < left.length; index += 1) if (left[index] !== right[index]) return false
  return true
}

/** 去掉升序下标处的元素后的新数组（分段复制，不读元素内容）。 */
function copyWithout(rows: readonly Entry[], sortedIndexes: readonly number[]): Entry[] {
  const kept = new Array<Entry>(rows.length - sortedIndexes.length)
  let write = 0
  let from = 0
  for (let at = 0; at <= sortedIndexes.length; at += 1) {
    const until = at < sortedIndexes.length ? (sortedIndexes[at] as number) : rows.length
    for (let index = from; index < until; index += 1) kept[write++] = rows[index] as Entry
    from = until + 1
  }
  return kept
}

function lowerBound(rows: readonly Entry[], view: DownloadTaskView, compare: RowCompare, from: number): number {
  let low = from
  let high = rows.length
  while (low < high) {
    const mid = (low + high) >>> 1
    if (compare((rows[mid] as Entry).view, view) < 0) low = mid + 1
    else high = mid
  }
  return low
}

/** 按 `compare` 严格有序的行数组（比较器是全序：key 兜底）。 */
class SortedRows {
  constructor(
    private readonly compare: RowCompare,
    public rows: Entry[],
  ) {}

  /** 行的下标；全序保证二分命中，不命中（不变式被破坏）退回线性查找。 */
  indexOf(entry: Entry): number {
    const at = lowerBound(this.rows, entry.view, this.compare, 0)
    return this.rows[at] === entry ? at : this.rows.indexOf(entry)
  }

  /** 摘出 `removed`（按当前仍是旧值的 `view` 定位）。 */
  detach(removed: readonly Entry[]): Detached {
    if (removed.length === 0) return { removed, origin: null, before: null, gone: NOTHING }
    if (removed.length <= LOCATED_LIMIT) {
      const origin = new Map<Entry, number>()
      for (const entry of removed) origin.set(entry, this.indexOf(entry))
      const indexes = [...origin.values()].filter((index) => index >= 0).sort((left, right) => left - right)
      if (removed.length <= SEQUENTIAL_LIMIT) {
        for (let at = indexes.length - 1; at >= 0; at -= 1) this.rows.splice(indexes[at] as number, 1)
        return { removed, origin, before: null, gone: origin }
      }
      const before = this.rows
      this.rows = copyWithout(before, indexes)
      return { removed, origin: null, before, gone: origin }
    }
    const gone = new Set(removed)
    const before = this.rows
    this.rows = before.filter((entry) => !gone.has(entry))
    return { removed, origin: null, before, gone }
  }

  /** 按新 `view` 装入 `inserted`。 */
  attach(inserted: readonly Entry[], detached: Detached): Attached {
    const { removed, origin, before, gone } = detached
    let moved = 0
    for (const entry of inserted) if (gone.has(entry)) moved += 1
    const membershipChanged = removed.length !== moved || inserted.length !== moved

    if (inserted.length <= SEQUENTIAL_LIMIT) {
      for (const entry of inserted) this.rows.splice(lowerBound(this.rows, entry.view, this.compare, 0), 0, entry)
    } else if (inserted.length > 0) {
      const ordered = inserted.slice().sort((left, right) => this.compare(left.view, right.view))
      const base = this.rows
      const merged = new Array<Entry>(base.length + ordered.length)
      let write = 0
      let cursor = 0
      for (const entry of ordered) {
        const at = lowerBound(base, entry.view, this.compare, cursor)
        for (; cursor < at; cursor += 1) merged[write++] = base[cursor] as Entry
        merged[write++] = entry
      }
      for (; cursor < base.length; cursor += 1) merged[write++] = base[cursor] as Entry
      this.rows = merged
    }

    let positionChanged = membershipChanged
    if (!membershipChanged && moved > 0) {
      if (origin !== null) {
        for (const [entry, index] of origin) {
          if (this.indexOf(entry) !== index) {
            positionChanged = true
            break
          }
        }
      } else if (before !== null) {
        positionChanged = !sameRows(before, this.rows)
      }
    }
    return { membershipChanged, moved, positionChanged }
  }
}

function countsDiffer(left: DownloadTaskView, right: DownloadTaskView): boolean {
  return (
    left.state !== right.state ||
    left.queueId !== right.queueId ||
    left.toDevice !== right.toDevice ||
    left.name !== right.name
  )
}

function tallyDiffer(left: DownloadTaskView, right: DownloadTaskView): boolean {
  return (
    left.groupId !== right.groupId ||
    left.state !== right.state ||
    left.preparing !== right.preparing ||
    left.downloadedBytes !== right.downloadedBytes ||
    left.sizeBytes !== right.sizeBytes ||
    (left.groupId !== '' && left.speed !== right.speed)
  )
}

/** 分组桶归属读取的字段（状态 / 日期 / 分类 / 队列 / 站点 / 任务组）。 */
function groupingDiffer(left: DownloadTaskView, right: DownloadTaskView): boolean {
  return (
    left.state !== right.state ||
    left.createdAtSecs !== right.createdAtSecs ||
    left.name !== right.name ||
    left.queueId !== right.queueId ||
    left.url !== right.url ||
    left.referrer !== right.referrer ||
    left.protocol !== right.protocol ||
    left.saveDir !== right.saveDir ||
    left.groupId !== right.groupId
  )
}

export class RowEngine {
  private readonly entries = new Map<RowKey, Entry>()

  // 上一份输入（差分基准）。
  private prevTasks: readonly TaskDto[] = []
  private prevQueue: readonly QueuePositionDto[] = []
  private prevSpeeds: Readonly<Record<string, number>> = {}
  private prevUploads: Readonly<Record<string, number>> = {}
  private prevRuntimes: Readonly<Record<string, TaskRuntimeDto>> = {}
  private prevRemote: readonly DownloadTaskView[] = []
  private speeds: Readonly<Record<string, number>> = {}
  private uploads: Readonly<Record<string, number>> = {}
  private runtimes: Readonly<Record<string, TaskRuntimeDto>> = {}
  private boostedId: string | null = null
  private connected = false
  /** taskId → 引擎队列排序键（随 `queuePositions` 差分维护，不必每帧重建整张表）。 */
  private readonly queueKeys = new Map<string, number>()

  // 配置。
  private selection: SidebarSelection | null = null
  private query = ''
  private groupMatches: ReadonlySet<string> = new Set()
  private categories: CategoryIndex | null = null
  private sortKey: ViewSortKey = 'smart'
  private sortDir: SortDir = 'desc'
  private unfiltered = true
  private compare: RowCompare = () => 0

  // 顺序。
  private all = new SortedRows(() => 0, [])
  private shown: SortedRows = this.all
  /** 保持期内与 `shown` 分叉的已应用顺序；null = 与 `shown` 一致。 */
  private applied: Entry[] | null = null
  /** `applied` 与 `shown` 的相对顺序因行挪位而不同（保持期结束后需强制重排）。 */
  private appliedStale = false
  private published: readonly TaskRow[] = []
  private lastReorderAt = Number.NEGATIVE_INFINITY
  private initialized = false

  // 增量聚合。
  private countsLive!: SidebarCounts
  private countsPublished!: SidebarCounts
  private readonly tallies = new Map<string, GroupTally>()

  // 版本与快照。
  private frame = 0
  private layoutVersion = 0
  private contentVersion = 0
  private groupingVersion = 0
  private matchVersion = 0
  private tallyVersion = 0
  private cached: RowSnapshot | null = null

  // 一帧内的累计（`update` 开头清零）。
  private updates: Update[] = []
  private removedEntries: Entry[] = []
  private addedEntries: Entry[] = []
  private countsDirty = false
  private tallyDirty = false
  private groupingDirty = false
  private rebuildCounts = false

  /** 行键 → 当前视图。 */
  viewOf(key: RowKey): DownloadTaskView | undefined {
    return this.entries.get(key)?.view
  }

  /** 行是否命中当前筛选 + 搜索。 */
  isMatched(key: RowKey): boolean {
    return this.entries.get(key)?.matched ?? false
  }

  /** 全部视图（本地 + 远程，无序）；O(N)，只给按需路径（菜单动作）用。 */
  views(): DownloadTaskView[] {
    return Array.from(this.entries.values(), (entry) => entry.view)
  }

  groupTally(groupId: string): GroupTally | undefined {
    return this.tallies.get(groupId)
  }

  update(input: RowEngineInput, clock: HoldClock): RowSnapshot {
    this.frame += 1
    this.updates = []
    this.removedEntries = []
    this.addedEntries = []
    this.countsDirty = false
    this.tallyDirty = false
    this.groupingDirty = false

    const first = !this.initialized
    const categoriesChanged = first || this.categories !== input.categories
    const sortChanged = first || this.sortKey !== input.sortKey || this.sortDir !== input.sortDir
    const filterChanged =
      first ||
      categoriesChanged ||
      this.query !== input.query ||
      this.groupMatches !== input.groupMatches ||
      this.selection === null ||
      !sameSelection(this.selection, input.selection)
    this.rebuildCounts = categoriesChanged
    if (categoriesChanged) this.countsLive = new SidebarCounts([], input.categories)
    this.selection = input.selection
    this.query = input.query
    this.groupMatches = input.groupMatches
    this.categories = input.categories
    if (sortChanged) {
      this.sortKey = input.sortKey
      this.sortDir = input.sortDir
      const prefs = { sort_key: input.sortKey, sort_dir: input.sortDir }
      this.compare = (left, right) => compareViews(prefs, left, right)
    }
    this.unfiltered = selectsEverything(input.selection) && input.query === ''

    // ---- 1. 输入差分 → 行的增删改（视图在 commit 前保持旧值）----
    const connectedChanged = !first && this.connected !== input.connected
    this.connected = input.connected
    const touched = new Set<string>()
    this.syncQueue(input.queuePositions, touched)
    this.syncSpeeds(input.speeds, touched)
    this.syncUploads(input.uploads, touched)
    this.syncRuntimes(input.runtimes, touched)
    if (this.boostedId !== input.boostedId) {
      if (this.boostedId !== null) touched.add(this.boostedId)
      if (input.boostedId !== null) touched.add(input.boostedId)
      this.boostedId = input.boostedId
    }
    const replaced = new Map<string, TaskDto>()
    this.syncTasks(input.tasks, replaced)
    if (connectedChanged) {
      for (const entry of this.entries.values()) if (entry.task !== undefined) touched.add(entry.task.taskId)
    }
    for (const taskId of replaced.keys()) touched.add(taskId)
    for (const taskId of touched) {
      const entry = this.entries.get(localKey(taskId))
      if (entry === undefined || entry.born === this.frame || entry.task === undefined) continue
      this.refresh(entry, replaced.get(taskId) ?? entry.task)
    }
    this.syncRemote(input.remote)

    const membership = this.removedEntries.length > 0 || this.addedEntries.length > 0
    const content = membership || this.updates.length > 0

    // ---- 2. 顺序 ----
    const structural = sortChanged || filterChanged || membership
    const holding =
      this.initialized &&
      !structural &&
      !clock.force &&
      isHolding({
        now: clock.now,
        lastPointerAt: clock.lastPointerAt,
        menuOpen: clock.menuOpen,
        dynamicKey: isDynamicSortKey(this.sortKey),
        lastReorderAt: this.lastReorderAt,
      })
    let matchChanged = false
    let appliedChanged = false

    if (sortChanged) {
      for (const update of this.updates) update.entry.view = update.view
      const rows = Array.from(this.entries.values())
      rows.sort((left, right) => this.compare(left.view, right.view))
      this.all = new SortedRows(this.compare, rows)
    }
    if (sortChanged || filterChanged) {
      if (!sortChanged) this.applyToAll()
      this.rebuildShown()
      this.applied = null
      this.appliedStale = false
      matchChanged = true
      appliedChanged = !sameRows(this.published, this.shown.rows)
    } else {
      const result = this.applyIncremental(holding)
      matchChanged = result.membershipChanged
      appliedChanged = result.appliedChanged
    }

    if (this.rebuildCounts) {
      this.countsLive = new SidebarCounts(this.views(), input.categories)
      this.countsDirty = true
    }

    // ---- 3. 发布 ----
    this.initialized = true
    let layoutChanged = false
    if (appliedChanged || first) {
      this.published = (this.applied ?? this.shown.rows).slice()
      this.layoutVersion += 1
      this.lastReorderAt = clock.now
      layoutChanged = true
    }
    if (this.countsDirty) this.countsPublished = this.countsLive.clone()
    if (content) this.contentVersion += 1
    if (this.groupingDirty || sortChanged || filterChanged) this.groupingVersion += 1
    if (matchChanged) this.matchVersion += 1
    if (this.tallyDirty) this.tallyVersion += 1

    const stale = holding && this.applied !== null
    const previous = this.cached
    if (
      previous !== null &&
      !layoutChanged &&
      !content &&
      !matchChanged &&
      !this.countsDirty &&
      !this.tallyDirty &&
      previous.stale === stale
    ) {
      return previous
    }
    this.cached = {
      rows: this.published,
      layoutVersion: this.layoutVersion,
      contentVersion: this.contentVersion,
      groupingVersion: this.groupingVersion,
      matchVersion: this.matchVersion,
      tallyVersion: this.tallyVersion,
      counts: this.countsPublished,
      stale,
      lastReorderAt: this.lastReorderAt,
    }
    return this.cached
  }

  // ---- 输入差分 ----

  private syncQueue(next: readonly QueuePositionDto[], touched: Set<string>): void {
    const prev = this.prevQueue
    if (prev === next) return
    this.prevQueue = next
    let i = 0
    let j = 0
    while (i < prev.length && j < next.length) {
      const old = prev[i] as QueuePositionDto
      const fresh = next[j] as QueuePositionDto
      if (old === fresh) {
        i += 1
        j += 1
      } else if (old.taskId === fresh.taskId) {
        if (old.order !== fresh.order) {
          this.queueKeys.set(fresh.taskId, fresh.order)
          touched.add(fresh.taskId)
        }
        i += 1
        j += 1
      } else {
        this.queueKeys.delete(old.taskId)
        touched.add(old.taskId)
        i += 1
      }
    }
    for (; i < prev.length; i += 1) {
      const old = prev[i] as QueuePositionDto
      this.queueKeys.delete(old.taskId)
      touched.add(old.taskId)
    }
    for (; j < next.length; j += 1) {
      const fresh = next[j] as QueuePositionDto
      this.queueKeys.set(fresh.taskId, fresh.order)
      touched.add(fresh.taskId)
    }
  }

  private syncSpeeds(next: Readonly<Record<string, number>>, touched: Set<string>): void {
    const prev = this.prevSpeeds
    this.speeds = next
    if (prev === next) return
    this.prevSpeeds = next
    for (const taskId of Object.keys(next)) if (next[taskId] !== prev[taskId]) touched.add(taskId)
    for (const taskId of Object.keys(prev)) if (next[taskId] === undefined) touched.add(taskId)
  }

  private syncUploads(next: Readonly<Record<string, number>>, touched: Set<string>): void {
    const prev = this.prevUploads
    this.uploads = next
    if (prev === next) return
    this.prevUploads = next
    for (const taskId of Object.keys(next)) if (next[taskId] !== prev[taskId]) touched.add(taskId)
    for (const taskId of Object.keys(prev)) if (next[taskId] === undefined) touched.add(taskId)
  }

  private syncRuntimes(next: Readonly<Record<string, TaskRuntimeDto>>, touched: Set<string>): void {
    const prev = this.prevRuntimes
    this.runtimes = next
    if (prev === next) return
    this.prevRuntimes = next
    for (const taskId of Object.keys(next)) if (next[taskId] !== prev[taskId]) touched.add(taskId)
    for (const taskId of Object.keys(prev)) if (next[taskId] === undefined) touched.add(taskId)
  }

  /**
   * 任务数组差分。优先用 apply 层的变更日志：只对账被触及的 id，O(改动数)，不遍历数组；
   * 日志不可用（首帧、外部来源的数组、整表替换、落后太多帧）时退回逐项差分——原位替换（同 id 不同引用）、
   * 过滤删除、尾部追加按序一次走完，任何别的重排只会被解读成「删旧 + 增新」，结果集合仍然正确。
   */
  private syncTasks(next: readonly TaskDto[], replaced: Map<string, TaskDto>): void {
    const prev = this.prevTasks
    if (prev === next) return
    this.prevTasks = next
    const touchedIds = touchedTaskIds(prev, next)
    if (touchedIds !== null) {
      for (const taskId of touchedIds) {
        const task = taskById(next, taskId)
        const entry = this.entries.get(localKey(taskId))
        if (task === undefined) {
          if (entry !== undefined) this.removeEntry(entry.key)
        } else if (entry === undefined) {
          this.addLocal(task)
        } else if (entry.task !== task) {
          replaced.set(taskId, task)
        }
      }
      return
    }
    let i = 0
    let j = 0
    while (i < prev.length && j < next.length) {
      const old = prev[i] as TaskDto
      const fresh = next[j] as TaskDto
      if (old === fresh) {
        i += 1
        j += 1
      } else if (old.taskId === fresh.taskId) {
        replaced.set(fresh.taskId, fresh)
        i += 1
        j += 1
      } else {
        this.removeEntry(localKey(old.taskId))
        i += 1
      }
    }
    for (; i < prev.length; i += 1) this.removeEntry(localKey((prev[i] as TaskDto).taskId))
    for (; j < next.length; j += 1) this.addLocal(next[j] as TaskDto)
  }

  private syncRemote(next: readonly DownloadTaskView[]): void {
    const prev = this.prevRemote
    if (prev === next) return
    this.prevRemote = next
    const seen = new Set<RowKey>()
    for (const view of next) {
      seen.add(view.key)
      const entry = this.entries.get(view.key)
      if (entry === undefined) this.addEntry(view, undefined)
      else if (entry.view !== view) this.pushUpdate(entry, view)
    }
    for (const old of prev) if (!seen.has(old.key)) this.removeEntry(old.key)
  }

  // ---- 行的增删改 ----

  private addLocal(task: TaskDto): void {
    const taskId = task.taskId
    const speed = this.speeds[taskId] ?? null
    const upload = this.uploads[taskId] ?? null
    const boosted = this.boostedId === taskId
    const queueKey = this.queueKeys.get(taskId) ?? null
    const runtime = this.runtimes[taskId]
    const view = buildLocalView(task, speed, boosted, queueKey, runtime, this.connected, upload)
    const entry = this.addEntry(view, task)
    entry.speed = speed
    entry.upload = upload
    entry.boosted = boosted
    entry.queueKey = queueKey
    entry.runtime = runtime
    entry.connected = this.connected
  }

  private addEntry(view: DownloadTaskView, task: TaskDto | undefined): Entry {
    const entry: Entry = {
      type: 'task',
      key: view.key,
      view,
      task,
      speed: null,
      upload: null,
      boosted: false,
      queueKey: null,
      runtime: undefined,
      connected: false,
      matched: this.matches(view),
      born: this.frame,
    }
    this.entries.set(view.key, entry)
    this.addedEntries.push(entry)
    this.account(view, 1)
    return entry
  }

  private removeEntry(key: RowKey): void {
    const entry = this.entries.get(key)
    if (entry === undefined) return
    this.entries.delete(key)
    this.removedEntries.push(entry)
    this.account(entry.view, -1)
  }

  /** 按当前输入重算本地行的视图；输入都没变则什么也不做。 */
  private refresh(entry: Entry, task: TaskDto): void {
    const taskId = task.taskId
    const speed = this.speeds[taskId] ?? null
    const upload = this.uploads[taskId] ?? null
    const boosted = this.boostedId === taskId
    const queueKey = this.queueKeys.get(taskId) ?? null
    const runtime = this.runtimes[taskId]
    if (
      entry.task === task &&
      entry.speed === speed &&
      entry.upload === upload &&
      entry.boosted === boosted &&
      entry.queueKey === queueKey &&
      entry.runtime === runtime &&
      entry.connected === this.connected
    ) {
      return
    }
    entry.task = task
    entry.speed = speed
    entry.upload = upload
    entry.boosted = boosted
    entry.queueKey = queueKey
    entry.runtime = runtime
    entry.connected = this.connected
    this.pushUpdate(entry, buildLocalView(task, speed, boosted, queueKey, runtime, this.connected, upload))
  }

  private pushUpdate(entry: Entry, view: DownloadTaskView): void {
    const oldView = entry.view
    this.updates.push({ entry, oldView, view })
    if (countsDiffer(oldView, view)) {
      this.countsDirty = true
      if (!this.rebuildCounts) {
        this.countsLive.remove(oldView)
        this.countsLive.add(view)
      }
    }
    if (tallyDiffer(oldView, view)) {
      this.tallyDirty = true
      this.tally(oldView, -1)
      this.tally(view, 1)
    }
    if (groupingDiffer(oldView, view)) this.groupingDirty = true
  }

  /** 新增 / 删除一行对计数、任务组汇总、分组归属的影响。 */
  private account(view: DownloadTaskView, delta: 1 | -1): void {
    this.countsDirty = true
    this.groupingDirty = true
    if (!this.rebuildCounts) {
      if (delta === 1) this.countsLive.add(view)
      else this.countsLive.remove(view)
    }
    if (view.source === 'local' && view.groupId !== '') {
      this.tallyDirty = true
      this.tally(view, delta)
    }
  }

  private tally(view: DownloadTaskView, delta: 1 | -1): void {
    if (view.source !== 'local' || view.groupId === '') return
    let tally = this.tallies.get(view.groupId)
    if (tally === undefined) {
      tally = { total: 0, completed: 0, failed: 0, downloading: 0, pending: 0, paused: 0, downloaded: 0, totalBytes: 0, speed: 0 }
      this.tallies.set(view.groupId, tally)
    }
    tally.total += delta
    if (view.state === 'completed') tally.completed += delta
    else if (view.state === 'failed') tally.failed += delta
    else if (view.state === 'downloading') {
      tally.downloading += delta
      tally.speed += delta * (view.speed ?? 0)
    } else if (view.state === 'pending') {
      if (view.preparing) tally.downloading += delta
      else tally.pending += delta
    } else if (view.state === 'paused') tally.paused += delta
    tally.downloaded += delta * view.downloadedBytes
    tally.totalBytes += delta * view.sizeBytes
    if (tally.total === 0) this.tallies.delete(view.groupId)
  }

  private matches(view: DownloadTaskView): boolean {
    if (this.unfiltered) return true
    if (!selectionMatches(this.selection as SidebarSelection, view, this.categories as CategoryIndex)) return false
    return matchesQuery(view, this.query) || (view.groupId !== '' && this.groupMatches.has(view.groupId))
  }

  // ---- 顺序维护 ----

  /** 筛选变化但排序不变：先把本帧的行增删改并入 `all`（`shown` 随后整体重建）。 */
  private applyToAll(): void {
    const removals: Entry[] = [...this.removedEntries]
    const inserts: Entry[] = [...this.addedEntries]
    for (const update of this.updates) {
      if (this.compare(update.oldView, update.view) !== 0) {
        removals.push(update.entry)
        inserts.push(update.entry)
      }
    }
    const detached = this.all.detach(removals)
    for (const update of this.updates) update.entry.view = update.view
    this.all.attach(inserts, detached)
  }

  /** 以 `all` 为底按当前筛选 + 搜索重建 `shown`，并刷新所有行的命中标记。 */
  private rebuildShown(): void {
    const rows = this.all.rows
    if (this.unfiltered) {
      for (const entry of this.entries.values()) entry.matched = true
      this.shown = this.all
      return
    }
    // 先按创建顺序（与堆分配顺序一致，访问连续）刷新命中标记，再按排序顺序摘出命中行。
    for (const entry of this.entries.values()) entry.matched = this.matches(entry.view)
    const kept: Entry[] = []
    for (const entry of rows) if (entry.matched) kept.push(entry)
    this.shown = new SortedRows(this.compare, kept)
  }

  private applyIncremental(holding: boolean): { membershipChanged: boolean; appliedChanged: boolean } {
    const alias = this.shown === this.all
    const allRemovals: Entry[] = [...this.removedEntries]
    const allInserts: Entry[] = [...this.addedEntries]
    const shownRemovals: Entry[] = this.removedEntries.filter((entry) => entry.matched)
    const shownInserts: Entry[] = this.addedEntries.filter((entry) => entry.matched)
    for (const update of this.updates) {
      const entry = update.entry
      const moved = this.compare(update.oldView, update.view) !== 0
      const nowMatched = this.matches(update.view)
      if (moved) {
        allRemovals.push(entry)
        allInserts.push(entry)
      }
      if (entry.matched) {
        if (!nowMatched) shownRemovals.push(entry)
        else if (moved) {
          shownRemovals.push(entry)
          shownInserts.push(entry)
        }
      } else if (nowMatched) {
        shownInserts.push(entry)
      }
      entry.matched = nowMatched
    }

    const shownChanges = shownRemovals.length > 0 || shownInserts.length > 0
    if (holding && this.applied === null && shownChanges) this.applied = this.shown.rows.slice()

    const allDetached = this.all.detach(allRemovals)
    const shownDetached = alias ? allDetached : this.shown.detach(shownRemovals)
    for (const update of this.updates) update.entry.view = update.view
    const allAttached = this.all.attach(allInserts, allDetached)
    const shownAttached = alias ? allAttached : this.shown.attach(shownInserts, shownDetached)

    const membershipChanged = shownAttached.membershipChanged
    const applied = this.applied
    if (applied === null) return { membershipChanged, appliedChanged: membershipChanged || shownAttached.positionChanged }

    if (!holding) {
      this.applied = null
      this.appliedStale = false
      return { membershipChanged, appliedChanged: !sameRows(applied, this.shown.rows) }
    }

    // 保持期：成员变化并入已应用顺序，挪位留给保持期结束后的强制重排。
    const reinserted = new Set(shownInserts)
    const removedSet = new Set(shownRemovals)
    const gone = shownRemovals.filter((entry) => !reinserted.has(entry))
    const fresh = shownInserts.filter((entry) => !removedSet.has(entry))
    const kept = gone.length > 0 ? this.withoutRows(applied, gone) : applied
    if (fresh.length > 0) {
      const placed = fresh.map((entry) => ({ entry, at: this.shown.indexOf(entry) })).sort((left, right) => left.at - right.at)
      for (const { entry, at } of placed) kept.splice(Math.min(at, kept.length), 0, entry)
    }
    this.applied = kept
    if (shownAttached.moved > 0 && shownAttached.positionChanged) this.appliedStale = true
    // 没有行挪位：已应用顺序与排序结果一致，不再分叉。
    if (!this.appliedStale) this.applied = null
    return { membershipChanged, appliedChanged: gone.length > 0 || fresh.length > 0 }
  }

  private withoutRows(rows: Entry[], gone: readonly Entry[]): Entry[] {
    if (gone.length <= SEQUENTIAL_LIMIT) {
      for (const entry of gone) {
        const index = rows.indexOf(entry)
        if (index >= 0) rows.splice(index, 1)
      }
      return rows
    }
    const set = new Set(gone)
    return rows.filter((entry) => !set.has(entry))
  }
}
