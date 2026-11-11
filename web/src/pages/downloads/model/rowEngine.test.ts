import { describe, expect, test } from 'bun:test'
import { applyDaemonEvents, installTasks } from '../../../lib/rpc/apply'
import type { DaemonEvent, DaemonSnapshot, QueuePositionDto, RemoteTaskDto, TaskDto, TaskRuntimeDto } from '../../../lib/rpc'
import { CategoryIndex, builtinDefaults } from './categories'
import { SidebarCounts } from './counts'
import { FILTER_ALL, LOCAL_DEVICE, STATUS_FILTERS, sameSelection, selectionMatches } from './filters'
import type { SidebarSelection } from './filters'
import { RowEngine } from './rowEngine'
import type { GroupTally, HoldClock, RowEngineInput, RowSnapshot } from './rowEngine'
import { DYNAMIC_HOLD_MS, POINTER_HOLD_MS, isDynamicSortKey } from './rowOrder'
import { resolveRowOrder } from './rowOrder.reference'
import type { OrderState } from './rowOrder.reference'
import { buildLocalView, buildRemoteView } from './task'
import type { DownloadTaskView } from './task'
import { compareViews, matchesQuery } from './viewPrefs'
import type { SortDir, ViewSortKey } from './viewPrefs'

// ---- 参照实现：旧的整表管线（逐帧 map + filter + sort，再走 `resolveRowOrder` 稳定合并），作为引擎的规格 ----

interface ReferenceFrame {
  rows: DownloadTaskView[]
  stale: boolean
  lastReorderAt: number
  views: DownloadTaskView[]
}

class Reference {
  private previous: OrderState | null = null
  private structure: {
    selection: SidebarSelection
    query: string
    categories: CategoryIndex
    sortKey: string
    sortDir: string
    keys: Set<string>
  } | null = null

  step(input: RowEngineInput, clock: HoldClock): ReferenceFrame {
    const queueKeys = new Map(input.queuePositions.map((entry) => [entry.taskId, entry.order]))
    const local = input.tasks.map((task) =>
      buildLocalView(
        task,
        input.speeds[task.taskId] ?? null,
        input.boostedId === task.taskId,
        queueKeys.get(task.taskId) ?? null,
        input.runtimes[task.taskId],
        input.connected,
      ),
    )
    const views = [...local, ...input.remote]
    const prefs = { sort_key: input.sortKey, sort_dir: input.sortDir }
    const sorted = views
      .filter((view) => selectionMatches(input.selection, view, input.categories) && matchesQuery(view, input.query))
      .sort((left, right) => compareViews(prefs, left, right))

    const keys = new Set(views.map((view) => view.key))
    const structure = this.structure
    const structural =
      structure === null ||
      !sameSelection(structure.selection, input.selection) ||
      structure.query !== input.query ||
      structure.categories !== input.categories ||
      structure.sortKey !== input.sortKey ||
      structure.sortDir !== input.sortDir ||
      structure.keys.size !== keys.size ||
      [...keys].some((key) => !structure.keys.has(key))
    const result = resolveRowOrder({
      sorted,
      previous: this.previous,
      structural,
      force: clock.force,
      now: clock.now,
      lastPointerAt: clock.lastPointerAt,
      menuOpen: clock.menuOpen,
      dynamicKey: isDynamicSortKey(input.sortKey),
    })
    this.previous = result.state
    this.structure = {
      selection: input.selection,
      query: input.query,
      categories: input.categories,
      sortKey: input.sortKey,
      sortDir: input.sortDir,
      keys,
    }
    return { rows: result.rows, stale: result.stale, lastReorderAt: result.state.lastReorderAt, views }
  }
}

// ---- 场景构造 ----

function mulberry(seed: number): () => number {
  let state = seed >>> 0
  return () => {
    state = (state + 0x6d2b79f5) >>> 0
    let value = state
    value = Math.imul(value ^ (value >>> 15), value | 1)
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61)
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296
  }
}

function task(taskId: string, patch: Partial<TaskDto> = {}): TaskDto {
  return {
    taskId,
    url: `https://host${taskId.length}.example.com/${taskId}.bin`,
    fileName: `${taskId}.zip`,
    saveDir: '/d',
    status: 2,
    downloadedBytes: 0,
    totalBytes: 1000,
    errorMessage: '',
    createdAt: '1700000000',
    queueId: 'main',
    completedAt: '',
    referrer: '',
    groupId: '',
    originUrl: '',
    fileMissing: false,
    queueOrder: 0,
    uploadedBytes: 0,
    seedingStatus: 0,
    ...patch,
  } as unknown as TaskDto
}

function runtime(taskId: string, sequence: number): TaskRuntimeDto {
  return { taskId, sampleSequence: sequence, activeTransfers: 1, segments: [] } as unknown as TaskRuntimeDto
}

const categories = new CategoryIndex(builtinDefaults())
const otherCategories = new CategoryIndex(builtinDefaults().slice(0, 3))
const SORT_KEYS: ViewSortKey[] = ['smart', 'created', 'name', 'size', 'progress', 'speed', 'status']
const SELECTIONS: SidebarSelection[] = [
  { kind: 'download', filter: FILTER_ALL },
  { kind: 'download', filter: { status: 'completed', category: null } },
  { kind: 'download', filter: { status: 'incomplete', category: null } },
  { kind: 'download', filter: { status: 'all', category: 'builtin_video' } },
  { kind: 'queue', queueId: 'later' },
  { kind: 'device', deviceId: LOCAL_DEVICE },
  { kind: 'device', deviceId: 'phone' },
]
const NAMES = ['alpha', 'Beta', 'gamma10', 'gamma2', 'delta', 'file01', 'file1', 'zeta']
const EXTENSIONS = ['zip', 'mp4', 'iso', 'txt']

function remoteTask(id: string, status: RemoteTaskDto['status'], toDevice: string, downloaded: number): RemoteTaskDto {
  return {
    id,
    fromDevice: 'pc',
    toDevice,
    fileName: `remote-${id}.mp4`,
    url: `https://remote.example.com/${id}`,
    status,
    downloadedBytes: downloaded,
    totalBytes: 1000,
    speed: downloaded,
    createdAt: '1700000500',
    saveDir: '/r',
    error: null,
  } as unknown as RemoteTaskDto
}

/** 测试里不涉及「任务组名命中搜索」。 */
const NO_GROUP_MATCHES: ReadonlySet<string> = new Set()

interface Scenario {
  tasks: TaskDto[]
  speeds: Record<string, number>
  runtimes: Record<string, TaskRuntimeDto>
  queue: QueuePositionDto[]
  boostedId: string | null
  connected: boolean
  remote: DownloadTaskView[]
  selection: SidebarSelection
  query: string
  categories: CategoryIndex
  sortKey: ViewSortKey
  sortDir: SortDir
}

function inputOf(scenario: Scenario): RowEngineInput {
  return {
    tasks: scenario.tasks,
    speeds: scenario.speeds,
    uploads: {},
    boostedId: scenario.boostedId,
    queuePositions: scenario.queue,
    runtimes: scenario.runtimes,
    connected: scenario.connected,
    remote: scenario.remote,
    selection: scenario.selection,
    query: scenario.query,
    groupMatches: NO_GROUP_MATCHES,
    categories: scenario.categories,
    sortKey: scenario.sortKey,
    sortDir: scenario.sortDir,
  }
}

function randomTask(random: () => number, id: string): TaskDto {
  const name = NAMES[Math.floor(random() * NAMES.length)] as string
  const ext = EXTENSIONS[Math.floor(random() * EXTENSIONS.length)] as string
  return task(id, {
    fileName: `${name}.${ext}`,
    status: Math.floor(random() * 6),
    downloadedBytes: Math.floor(random() * 1000),
    createdAt: String(1_700_000_000 + Math.floor(random() * 5)),
    queueId: random() < 0.3 ? 'later' : 'main',
    queueOrder: Math.floor(random() * 4),
    groupId: random() < 0.3 ? `g${Math.floor(random() * 3)}` : '',
  })
}

/** 视图比较忽略惰性搜索缓存（引擎与参照各自在不同时机填充）。 */
const plain = (view: DownloadTaskView | undefined): unknown => (view === undefined ? view : { ...view, searchFold: '' })

/** 与参照实现逐帧比较：行顺序、视图、计数、任务组汇总、命中标记、保持期状态。 */
function expectSame(engine: RowEngine, snapshot: RowSnapshot, scenario: Scenario, reference: ReferenceFrame, label: string): void {
  expect(snapshot.rows.map((row) => row.key)).toEqual(reference.rows.map((view) => view.key))
  for (const [index, row] of snapshot.rows.entries()) {
    expect({ label, key: row.key, view: plain(row.view) }).toEqual({ label, key: row.key, view: plain(reference.rows[index]) })
  }
  expect(snapshot.lastReorderAt).toBe(reference.lastReorderAt)
  if (reference.stale) expect(snapshot.stale).toBe(true)
  if (!snapshot.stale) {
    const sorted = reference.views
      .filter((view) => selectionMatches(scenario.selection, view, scenario.categories) && matchesQuery(view, scenario.query))
      .sort((left, right) => compareViews({ sort_key: scenario.sortKey, sort_dir: scenario.sortDir }, left, right))
    expect(snapshot.rows.map((row) => row.key)).toEqual(sorted.map((view) => view.key))
  }

  const expected = new SidebarCounts(reference.views, scenario.categories)
  for (const status of STATUS_FILTERS) {
    expect(snapshot.counts.filter({ status, category: null })).toBe(expected.filter({ status, category: null }))
    for (const rule of scenario.categories.rules) {
      const filter = { status, category: rule.dto.id }
      expect(snapshot.counts.filter(filter)).toBe(expected.filter(filter))
    }
  }
  for (const queueId of ['main', 'later']) expect(snapshot.counts.queue(queueId)).toBe(expected.queue(queueId))
  for (const device of [LOCAL_DEVICE, 'phone', 'tablet']) expect(snapshot.counts.device(device)).toBe(expected.device(device))

  const groups = new Map<string, GroupTally>()
  for (const view of reference.views) {
    if (view.source !== 'local' || view.groupId === '') continue
    const tally = groups.get(view.groupId) ?? { total: 0, completed: 0, failed: 0, downloading: 0, pending: 0, paused: 0, downloaded: 0, totalBytes: 0, speed: 0 }
    tally.total += 1
    if (view.state === 'completed') tally.completed += 1
    else if (view.state === 'failed') tally.failed += 1
    else if (view.state === 'downloading') {
      tally.downloading += 1
      tally.speed += view.speed ?? 0
    } else if (view.state === 'pending') {
      if (view.preparing) tally.downloading += 1
      else tally.pending += 1
    } else if (view.state === 'paused') tally.paused += 1
    tally.downloaded += view.downloadedBytes
    tally.totalBytes += view.sizeBytes
    groups.set(view.groupId, tally)
  }
  for (const groupId of ['g0', 'g1', 'g2']) expect(engine.groupTally(groupId)).toEqual(groups.get(groupId))

  const shownKeys = new Set(
    reference.views
      .filter((view) => selectionMatches(scenario.selection, view, scenario.categories) && matchesQuery(view, scenario.query))
      .map((view) => view.key),
  )
  expect(engine.views().length).toBe(reference.views.length)
  for (const view of reference.views) {
    expect(plain(engine.viewOf(view.key))).toEqual(plain(view))
    expect(engine.isMatched(view.key)).toBe(shownKeys.has(view.key))
  }
}

/**
 * `viaApply`：任务数组由 apply 层的草稿产生（带变更日志），引擎走「只对账被触及 id」的路径；
 * 否则任务数组是手工拼的（无日志），引擎走逐项差分的回退路径。
 */
function runScenario(seed: number, steps: number, viaApply: boolean): void {
  const random = mulberry(seed)
  let counter = 0
  const scenario: Scenario = {
    tasks: Array.from({ length: 40 }, () => randomTask(random, `t${String(counter++).padStart(4, '0')}`)),
    speeds: {},
    runtimes: {},
    queue: [],
    boostedId: null,
    connected: true,
    remote: [],
    selection: SELECTIONS[0] as SidebarSelection,
    query: '',
    categories,
    sortKey: 'smart',
    sortDir: 'desc',
  }
  let daemon = { tasks: scenario.tasks, taskRuntime: {}, pendingSelections: [], rssItemRevisions: {}, priority: [], plugins: [] } as unknown as DaemonSnapshot
  /** 经 apply 层应用一批任务事件（有时连应用几批再出帧，模拟消费者漏帧）。 */
  const applyTaskEvents = (events: DaemonEvent[]): void => {
    const cut = Math.floor(random() * (events.length + 1))
    daemon = applyDaemonEvents(daemon, events.slice(0, cut))
    daemon = applyDaemonEvents(daemon, events.slice(cut))
    scenario.tasks = daemon.tasks as TaskDto[]
  }
  const engine = new RowEngine()
  const reference = new Reference()
  let now = 1_000_000
  let lastPointerAt = Number.NEGATIVE_INFINITY
  let menuOpen = false
  let sequence = 1
  const pick = <T>(items: readonly T[]): T => items[Math.floor(random() * items.length)] as T

  // 初始帧。
  let force = false
  const frame = (label: string): void => {
    const clock: HoldClock = { now, lastPointerAt, menuOpen, force }
    force = false
    const input = inputOf(scenario)
    const snapshot = engine.update(input, clock)
    expectSame(engine, snapshot, scenario, reference.step(input, clock), label)
  }
  frame(`seed ${seed} init`)

  for (let step = 0; step < steps; step += 1) {
    const roll = random()
    let label = ''
    if (roll < 0.22) {
      // 改若干任务（进度 / 状态 / 名称 / 创建时间），新数组、未改的项保持引用。
      label = 'patch tasks'
      const next = scenario.tasks.slice()
      const changed: TaskDto[] = []
      for (let n = 0; n < 1 + Math.floor(random() * 3); n += 1) {
        const at = Math.floor(random() * next.length)
        if (next[at] === undefined) continue
        next[at] = { ...next[at], ...(random() < 0.5 ? { downloadedBytes: Math.floor(random() * 1000) } : randomTask(random, next[at].taskId)) } as TaskDto
        changed.push(next[at] as TaskDto)
      }
      if (viaApply) applyTaskEvents(changed.map((data) => ({ type: 'taskChanged', data }) as DaemonEvent))
      else scenario.tasks = next
    } else if (roll < 0.3) {
      label = 'append tasks'
      const fresh = Array.from({ length: 1 + Math.floor(random() * 20) }, () => randomTask(random, `t${String(counter++).padStart(4, '0')}`))
      if (viaApply) applyTaskEvents([{ type: 'tasksChanged', data: fresh } as DaemonEvent])
      else scenario.tasks = [...scenario.tasks, ...fresh]
    } else if (roll < 0.38) {
      label = 'remove tasks'
      const doomed = new Set(Array.from({ length: 1 + Math.floor(random() * 4) }, () => (pick(scenario.tasks) as TaskDto | undefined)?.taskId))
      if (viaApply) applyTaskEvents([{ type: 'tasksDeleted', data: { taskIds: [...doomed].filter((id): id is string => id !== undefined) } } as DaemonEvent])
      else scenario.tasks = scenario.tasks.filter((item) => !doomed.has(item.taskId))
    } else if (roll < 0.46) {
      label = 'speeds'
      const speeds = { ...scenario.speeds }
      const target = pick(scenario.tasks) as TaskDto | undefined
      if (target !== undefined) {
        if (random() < 0.3) delete speeds[target.taskId]
        else speeds[target.taskId] = Math.floor(random() * 500)
      }
      scenario.speeds = speeds
    } else if (roll < 0.52) {
      label = 'runtimes'
      const runtimes = { ...scenario.runtimes }
      const target = pick(scenario.tasks) as TaskDto | undefined
      if (target !== undefined) {
        if (random() < 0.3) delete runtimes[target.taskId]
        else runtimes[target.taskId] = runtime(target.taskId, sequence++)
      }
      scenario.runtimes = runtimes
    } else if (roll < 0.6) {
      label = 'queue'
      const members = scenario.tasks.filter(() => random() < 0.3).map((item) => item.taskId)
      scenario.queue = members.map((taskId, index) => ({ taskId, order: index * 10 + Math.floor(random() * 5), position: index + 1 }))
    } else if (roll < 0.64) {
      label = 'boost'
      scenario.boostedId = random() < 0.3 ? null : ((pick(scenario.tasks) as TaskDto | undefined)?.taskId ?? null)
    } else if (roll < 0.68) {
      label = 'connected'
      scenario.connected = !scenario.connected
    } else if (roll < 0.74) {
      label = 'remote'
      scenario.remote = Array.from({ length: Math.floor(random() * 4) }, (_, index) =>
        buildRemoteView(remoteTask(`r${index}`, pick(['pending', 'downloading', 'paused', 'completed', 'failed'] as const), pick(['phone', 'tablet']), Math.floor(random() * 1000))),
      )
    } else if (roll < 0.8) {
      label = 'selection'
      scenario.selection = pick(SELECTIONS)
    } else if (roll < 0.84) {
      label = 'query'
      scenario.query = pick(['', 'alpha', 'gamma', 'zip', 'host', 'remote'])
    } else if (roll < 0.88) {
      label = 'sort'
      scenario.sortKey = pick(SORT_KEYS)
      scenario.sortDir = pick(['asc', 'desc'] as const)
    } else if (roll < 0.9) {
      label = 'categories'
      scenario.categories = scenario.categories === categories ? otherCategories : categories
    } else if (roll < 0.92 && viaApply) {
      // 整表重载：载入的新数组（无日志）整体替换，引擎退回逐项差分。
      label = 'reload'
      daemon = installTasks(daemon, scenario.tasks.filter(() => random() < 0.8).map((item) => ({ ...item }) as TaskDto))
      scenario.tasks = daemon.tasks as TaskDto[]
    } else if (roll < 0.94) {
      label = 'pointer'
      lastPointerAt = now
      now += 50
      continue
    } else if (roll < 0.96) {
      label = 'menu'
      menuOpen = !menuOpen
      now += 10
      continue
    } else {
      label = 'tick'
      now += pick([100, 800, POINTER_HOLD_MS + 10, DYNAMIC_HOLD_MS + 10])
      force = random() < 0.5
    }
    now += pick([5, 30, 200])
    frame(`seed ${seed} step ${step} ${label}`)
  }
}

describe('RowEngine 与整表参照管线逐帧一致', () => {
  for (const seed of [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]) {
    test(`随机场景（手工数组，逐项差分）seed=${seed}`, () => {
      runScenario(seed, 220, false)
    })
    test(`随机场景（apply 变更日志）seed=${seed}`, () => {
      runScenario(seed, 220, true)
    })
  }
})

describe('RowEngine 基本语义', () => {
  const clock = (patch: Partial<HoldClock> = {}): HoldClock => ({
    now: 10_000,
    lastPointerAt: Number.NEGATIVE_INFINITY,
    menuOpen: false,
    force: false,
    ...patch,
  })
  const base = (tasks: TaskDto[], patch: Partial<RowEngineInput> = {}): RowEngineInput => ({
    tasks,
    speeds: {},
    uploads: {},
    boostedId: null,
    queuePositions: [],
    runtimes: {},
    connected: true,
    remote: [],
    selection: SELECTIONS[0] as SidebarSelection,
    query: '',
    groupMatches: NO_GROUP_MATCHES,
    categories,
    sortKey: 'created',
    sortDir: 'asc',
    ...patch,
  })
  const keys = (snapshot: RowSnapshot): string[] => snapshot.rows.map((row) => row.key)

  test('进度帧：行对象与数组身份不变，只换视图并递增内容版本', () => {
    const a = task('a', { status: 1, createdAt: '1' })
    const b = task('b', { status: 1, createdAt: '2' })
    const engine = new RowEngine()
    const first = engine.update(base([a, b]), clock())
    const progressed = engine.update(base([a, { ...b, downloadedBytes: 500 } as TaskDto]), clock({ now: 10_100 }))
    expect(progressed.rows).toBe(first.rows)
    expect(progressed.layoutVersion).toBe(first.layoutVersion)
    expect(progressed.contentVersion).toBe(first.contentVersion + 1)
    expect(progressed.rows[1]?.view.downloadedBytes).toBe(500)
    expect(progressed.rows[0]).toBe(first.rows[0])
  })

  test('保存目录变化使目录分组投影失效', () => {
    const a = task('a', { saveDir: '/first' })
    const engine = new RowEngine()
    const first = engine.update(base([a]), clock())
    const changed = engine.update(base([{ ...a, saveDir: '/second' } as TaskDto]), clock({ now: 10_100 }))
    expect(changed.groupingVersion).toBe(first.groupingVersion + 1)
  })

  test('没有任何输入变化：返回同一份快照', () => {
    const a = task('a')
    const input = base([a])
    const engine = new RowEngine()
    const first = engine.update(input, clock())
    expect(engine.update(input, clock({ now: 10_200 }))).toBe(first)
  })

  test('指针活动期内仅内容变化不改变相对顺序并标记过期；保持期结束后强制重排', () => {
    const a = task('a', { createdAt: '1' })
    const b = task('b', { createdAt: '2' })
    const c = task('c', { createdAt: '3' })
    const engine = new RowEngine()
    const first = engine.update(base([a, b, c]), clock())
    expect(keys(first)).toEqual(['l:a', 'l:b', 'l:c'])
    // a 的创建时间被改到最后：保持期内不挪位。
    const held = engine.update(base([{ ...a, createdAt: '9' } as TaskDto, b, c]), clock({ now: 10_100, lastPointerAt: 10_000 }))
    expect(keys(held)).toEqual(['l:a', 'l:b', 'l:c'])
    expect(held.stale).toBe(true)
    expect(held.lastReorderAt).toBe(first.lastReorderAt)
    // 保持期过后的强制重排。
    const forced = engine.update(base([{ ...a, createdAt: '9' } as TaskDto, b, c]), clock({ now: 10_000 + POINTER_HOLD_MS + 1, lastPointerAt: 10_000, force: true }))
    expect(keys(forced)).toEqual(['l:b', 'l:c', 'l:a'])
    expect(forced.stale).toBe(false)
  })

  test('保持期内离开筛选的行消失、进入筛选的新行落在排序位置、旧行相对顺序不变', () => {
    const completed: SidebarSelection = { kind: 'download', filter: { status: 'completed', category: null } }
    const a = task('a', { status: 3, createdAt: '1' })
    const b = task('b', { status: 3, createdAt: '3' })
    const c = task('c', { status: 3, createdAt: '5' })
    const d = task('d', { status: 2, createdAt: '2' })
    const engine = new RowEngine()
    const first = engine.update(base([a, b, c, d], { selection: completed }), clock())
    expect(keys(first)).toEqual(['l:a', 'l:b', 'l:c'])
    // a 的排序键后移、c 离开筛选：保持期内 a 仍占第一位。
    const moved = { ...a, createdAt: '9' } as TaskDto
    const leftFilter = { ...c, status: 1 } as TaskDto
    const held = engine.update(base([moved, b, leftFilter, d], { selection: completed }), clock({ now: 10_100, lastPointerAt: 10_000 }))
    expect(keys(held)).toEqual(['l:a', 'l:b'])
    expect(held.stale).toBe(true)
    // d 进入筛选：落在排序给出的位置（最前），旧行 a b 的相对顺序不变。
    const entered = { ...d, status: 3 } as TaskDto
    const heldAgain = engine.update(base([moved, b, leftFilter, entered], { selection: completed }), clock({ now: 10_200, lastPointerAt: 10_100 }))
    expect(keys(heldAgain)).toEqual(['l:d', 'l:a', 'l:b'])
    expect(heldAgain.stale).toBe(true)
  })

  test('动态排序键：距上次重排不足 2s 保持，超过后放行', () => {
    const a = task('a', { downloadedBytes: 100 })
    const b = task('b', { downloadedBytes: 200 })
    const engine = new RowEngine()
    const dynamic = (tasks: TaskDto[]) => base(tasks, { sortKey: 'progress', sortDir: 'desc' })
    const first = engine.update(dynamic([a, b]), clock({ now: 0 }))
    expect(keys(first)).toEqual(['l:b', 'l:a'])
    const next = [{ ...a, downloadedBytes: 900 } as TaskDto, b]
    const held = engine.update(dynamic(next), clock({ now: 500 }))
    expect(keys(held)).toEqual(['l:b', 'l:a'])
    expect(held.stale).toBe(true)
    const released = engine.update(dynamic([{ ...a, downloadedBytes: 901 } as TaskDto, b]), clock({ now: DYNAMIC_HOLD_MS + 1 }))
    expect(keys(released)).toEqual(['l:a', 'l:b'])
    expect(released.stale).toBe(false)
  })

  test('菜单打开期间一直保持', () => {
    const a = task('a', { createdAt: '1' })
    const b = task('b', { createdAt: '2' })
    const engine = new RowEngine()
    engine.update(base([a, b]), clock())
    const held = engine.update(base([{ ...a, createdAt: '9' } as TaskDto, b]), clock({ now: 99_999, menuOpen: true }))
    expect(keys(held)).toEqual(['l:a', 'l:b'])
    expect(held.stale).toBe(true)
  })

  test('大批量增删（逐段复制 / 整表过滤两条路径）与整表排序结果一致', () => {
    const random = mulberry(99)
    const all = Array.from({ length: 6000 }, (_, index) => randomTask(random, `b${String(index).padStart(5, '0')}`))
    const expected = (tasks: TaskDto[]): string[] =>
      tasks
        .map((item) => buildLocalView(item, null, false, null, undefined, true))
        .sort((left, right) => compareViews({ sort_key: 'smart', sort_dir: 'desc' }, left, right))
        .map((view) => view.key)
    const engine = new RowEngine()
    let snapshot = { tasks: all, taskRuntime: {}, pendingSelections: [], rssItemRevisions: {}, priority: [], plugins: [] } as unknown as DaemonSnapshot
    const smart = (tasks: readonly TaskDto[]): RowEngineInput => base(tasks as TaskDto[], { sortKey: 'smart', sortDir: 'desc' })
    expect(keys(engine.update(smart(snapshot.tasks), clock()))).toEqual(expected(snapshot.tasks as TaskDto[]))
    // 删 40 个（超过逐个 splice 上限、走分段复制）。
    snapshot = applyDaemonEvents(snapshot, [{ type: 'tasksDeleted', data: { taskIds: all.slice(100, 140).map((item) => item.taskId) } }])
    expect(keys(engine.update(smart(snapshot.tasks), clock({ now: 10_100 })))).toEqual(expected(snapshot.tasks as TaskDto[]))
    // 再删 5000 个（超过定位上限、走整表过滤）。
    snapshot = applyDaemonEvents(snapshot, [{ type: 'tasksDeleted', data: { taskIds: all.slice(500, 5500).map((item) => item.taskId) } }])
    expect(keys(engine.update(smart(snapshot.tasks), clock({ now: 10_200 })))).toEqual(expected(snapshot.tasks as TaskDto[]))
    // 一次新增 3000 个（走排序 + 归并）。
    const fresh = Array.from({ length: 3000 }, (_, index) => randomTask(random, `n${String(index).padStart(5, '0')}`))
    snapshot = applyDaemonEvents(snapshot, [{ type: 'tasksChanged', data: fresh }])
    expect(keys(engine.update(smart(snapshot.tasks), clock({ now: 10_300 })))).toEqual(expected(snapshot.tasks as TaskDto[]))
  })

  test('切换筛选不重排：命中集合变化递增 matchVersion，命中标记随之刷新', () => {
    const a = task('a', { status: 3 })
    const b = task('b', { status: 1 })
    const engine = new RowEngine()
    const all = engine.update(base([a, b]), clock())
    const completed = engine.update(
      base([a, b], { selection: { kind: 'download', filter: { status: 'completed', category: null } } }),
      clock({ now: 10_100 }),
    )
    expect(keys(completed)).toEqual(['l:a'])
    expect(completed.matchVersion).toBe(all.matchVersion + 1)
    expect(engine.isMatched('l:b')).toBe(false)
    expect(engine.viewOf('l:b')?.taskId).toBe('b')
  })
})
