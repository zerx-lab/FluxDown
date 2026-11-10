// 事件应用：`native/protocol/src/event.rs` 的 `apply_agent_event` / `apply_daemon_event` /
// `apply_engine_message` 逐条移植。全部为不可变更新（返回新对象，未变化的分支保持引用不变），
// 以便 React 选择器按引用比较。一批事件走 `DaemonDraft`：大集合整批只复制一次，见该类说明。

import type {
  AgentEvent,
  AgentSnapshot,
  DaemonEvent,
  DaemonSnapshot,
  QueueOrderEntryDto,
  QueuePositionDto,
  ServiceEvent,
  TaskDto,
  TaskRuntimeDto,
  WebhookDeliveryDto,
  WsServerMsg,
} from './protocol'

/** 状态 1（下载中）/ 5（准备中）以外的任务视为不活跃。 */
function isActiveStatus(status: number): boolean {
  return status === 1 || status === 5
}

/**
 * 任务运行时是否仍有可见用途（对应 Rust `runtime_retained`）：已完成任务（3）的进度条退化为满条、
 * 来源构成读持久化的 `TaskDto.sourceBytes`，其运行时不保留，快照头不随历史跑过的任务数增长；
 * 暂停 / 失败任务保留（分段图仍按真实字节区间绘制）。
 */
export function runtimeRetained(status: number): boolean {
  return status !== 3
}

/** 投递日志保留上限（对应 Rust `WEBHOOK_DELIVERY_LIMIT`）。 */
export const WEBHOOK_DELIVERY_LIMIT = 1000

/**
 * 按 `deliveryId` 合并投递记录增量（对应 Rust `merge_webhook_deliveries`）：同 id 以增量为准，
 * 结果按 `timestampMs` 降序并截到上限。增量为空时返回原数组引用——清空由 `webhooksCleared` 表达。
 */
export function mergeWebhookDeliveries(
  current: WebhookDeliveryDto[],
  delta: readonly WebhookDeliveryDto[],
): WebhookDeliveryDto[] {
  if (delta.length === 0) return current
  const incoming = new Set(delta.map((delivery) => delivery.deliveryId))
  const merged = current.filter((delivery) => !incoming.has(delivery.deliveryId)).concat(delta)
  merged.sort((a, b) => b.timestampMs - a.timestampMs)
  return merged.slice(0, WEBHOOK_DELIVERY_LIMIT)
}

/** 读数已清零时为 true，可复用原引用。 */
function isRuntimeCleared(runtime: TaskRuntimeDto): boolean {
  return (
    runtime.activeTransfers === 0 &&
    runtime.connectedPeers === 0 &&
    runtime.segments.every((segment) => !segment.active)
  )
}

/** 清零传输活跃读数（对应 Rust `clear_active_runtime`）。 */
function clearedRuntime(runtime: TaskRuntimeDto): TaskRuntimeDto {
  if (isRuntimeCleared(runtime)) return runtime
  return {
    ...runtime,
    activeTransfers: 0,
    connectedPeers: 0,
    segments: runtime.segments.map((segment) => ({ ...segment, active: false })),
  }
}

// ---- 任务下标索引 ---------------------------------------------------------------------------

/**
 * 任务数组的下标索引（对应 Rust `DaemonSnapshot` 的任务下标缓存）。
 *
 * 逐条 `findIndex` 是 O(N)，每次删除后整表重建 Map 也要十几毫秒（25 万任务）。这里给任务发「槽号」：
 * 槽号单调分配，数组始终按槽号升序（原位替换不改变槽号，追加取新槽号，删除保序），所以
 * 数组位置 = 槽号 − 「已删除且槽号更小」的个数。删除只往 `removed`（升序槽号表）里并入几项，
 * 查找多一次二分，不再有 O(N) 的重建。
 *
 * `slots` 在快照链的各代数组之间共享且只增不改：同一个 id 在链上永远是同一个槽号，所以祖先 / 兄弟分支
 * 写进去的别家条目要么槽号 ≥ 自己的 `next`，要么落在 `removed` 里，要么按位置读回来 id 对不上，
 * 都会被识别并按「重建自己的那一份」处理；追加遇到共享表里已有的 id（删后重建同 id / 兄弟分支同时追加）
 * 也走重建，绝不覆盖共享条目。
 */
interface TaskIndex {
  slots: Map<string, number>
  /** 升序的已删除槽号。 */
  removed: readonly number[]
  /** 下一个可分配槽号；本代数组里的槽号都小于它。 */
  next: number
}

const TASK_INDEX = new WeakMap<readonly TaskDto[], TaskIndex>()

/** `removed` 累计超过该数就放弃增量维护，下次查找时整表重建（摊到每删 4096 个任务一次）。 */
const COMPACT_REMOVED = 4096

/** 删除命中不超过该数时复制后逐个 `splice`（memmove），否则按段单趟复制。 */
const SPLICE_LIMIT = 16

function buildTaskIndex(tasks: readonly TaskDto[]): TaskIndex {
  const slots = new Map<string, number>()
  for (let position = 0; position < tasks.length; position += 1) {
    slots.set((tasks[position] as TaskDto).taskId, position)
  }
  return { slots, removed: [], next: tasks.length }
}

function taskIndexOf(tasks: readonly TaskDto[]): TaskIndex {
  let index = TASK_INDEX.get(tasks)
  if (index === undefined) {
    index = buildTaskIndex(tasks)
    TASK_INDEX.set(tasks, index)
  }
  return index
}

/** 升序数组里小于 `value` 的个数。 */
function countBelow(sorted: readonly number[], value: number): number {
  let low = 0
  let high = sorted.length
  while (low < high) {
    const mid = (low + high) >>> 1
    if ((sorted[mid] as number) < value) low = mid + 1
    else high = mid
  }
  return low
}

/** 任务在数组中的下标；不存在返回 -1。 */
function findTaskIndex(tasks: readonly TaskDto[], taskId: string): number {
  const index = taskIndexOf(tasks)
  const slot = index.slots.get(taskId)
  if (slot === undefined || slot >= index.next) return -1
  const below = countBelow(index.removed, slot)
  // 同一个 id 在链上始终是同一个槽号：槽号已被删除就是这个任务不在了。
  if (index.removed[below] === slot) return -1
  const position = slot - below
  if (tasks[position]?.taskId === taskId) return position
  // 位置读回来 id 对不上（分叉的兄弟分支往共享表里写了别家的条目；正常链路不会发生）：
  // 重建自己的那一份后重查，宁可慢也不读错。
  const rebuilt = buildTaskIndex(tasks)
  TASK_INDEX.set(tasks, rebuilt)
  return rebuilt.slots.get(taskId) ?? -1
}

/** 把一个新任务的槽号记进索引（数组追加该任务之前调用）；返回该数组此后使用的索引。 */
function indexAppend(tasks: readonly TaskDto[], taskId: string): TaskIndex {
  let index = taskIndexOf(tasks)
  // 共享表里已有这个 id（已删除的旧槽 / 兄弟分支追加的）：不覆盖共享条目，重建自己的那一份。
  if (index.slots.has(taskId)) index = buildTaskIndex(tasks)
  index.slots.set(taskId, index.next)
  return { slots: index.slots, removed: index.removed, next: index.next + 1 }
}

/** 两个升序数组归并。 */
function mergeSorted(left: readonly number[], right: readonly number[]): number[] {
  if (left.length === 0) return right.slice()
  const merged: number[] = []
  let i = 0
  let j = 0
  while (i < left.length && j < right.length) {
    if ((left[i] as number) < (right[j] as number)) merged.push(left[i++] as number)
    else merged.push(right[j++] as number)
  }
  while (i < left.length) merged.push(left[i++] as number)
  while (j < right.length) merged.push(right[j++] as number)
  return merged
}

// ---- 任务数组变更日志 -----------------------------------------------------------------------

/**
 * 下游投影（下载页行引擎）要知道「这一帧哪些任务被触及」。靠比较新旧数组逐项找差异是 O(N) 且每项都要读对象头
 * （25 万任务约 2ms，且是纯 cache miss）。这里让草稿在交出新任务数组时记一条日志：新数组的版本号 →
 * 父版本号 + 被触及（原位替换 / 追加 / 删除）的 id。消费者沿版本链从新数组走回自己见过的旧数组，
 * 合并沿途的 id 集合；链路缺失（外部来源的数组、整表替换、日志已淘汰）返回 null，消费者退回逐项差分。
 * 日志只存 id 与版本号，不持有任何任务数组。
 */
interface TaskDelta {
  parent: number
  /** 被触及的 id；null = 整表替换（载入 / 重载 / 外部快照），无法增量描述。 */
  ids: readonly string[] | null
}

const TASK_VERSION = new WeakMap<readonly TaskDto[], number>()
const TASK_LOG = new Map<number, TaskDelta>()
let taskVersionCounter = 0
/** 日志保留的最近版本数；消费者落后更多帧就退回逐项差分。 */
const TASK_LOG_LIMIT = 256

function taskVersionOf(tasks: readonly TaskDto[]): number {
  let version = TASK_VERSION.get(tasks)
  if (version === undefined) {
    taskVersionCounter += 1
    version = taskVersionCounter
    TASK_VERSION.set(tasks, version)
  }
  return version
}

function recordTaskDelta(base: readonly TaskDto[], next: readonly TaskDto[], ids: readonly string[] | null): void {
  const parent = taskVersionOf(base)
  taskVersionCounter += 1
  TASK_VERSION.set(next, taskVersionCounter)
  TASK_LOG.set(taskVersionCounter, { parent, ids })
  if (TASK_LOG.size > TASK_LOG_LIMIT) {
    // Map 按插入序迭代：淘汰最旧的一半，摊到每 128 次记录一次。
    let drop = TASK_LOG.size - TASK_LOG_LIMIT / 2
    for (const version of TASK_LOG.keys()) {
      if (drop <= 0) break
      TASK_LOG.delete(version)
      drop -= 1
    }
  }
}

/**
 * `next` 相对 `prev` 被触及的任务 id（可能包含已被后续步骤撤销的变化，消费者按 id 对账即可）。
 * `next` 不是 `prev` 在草稿链上的后代、或沿途有整表替换 / 日志缺失时返回 null。
 */
export function touchedTaskIds(prev: readonly TaskDto[], next: readonly TaskDto[]): Set<string> | null {
  const target = TASK_VERSION.get(prev)
  let version = TASK_VERSION.get(next)
  if (target === undefined || version === undefined) return null
  const touched = new Set<string>()
  while (version !== target) {
    const delta = TASK_LOG.get(version)
    if (delta === undefined || delta.ids === null) return null
    for (const taskId of delta.ids) touched.add(taskId)
    version = delta.parent
  }
  return touched
}

/** 按 id 取任务（走下标索引，O(log R)）。 */
export function taskById(tasks: readonly TaskDto[], taskId: string): TaskDto | undefined {
  const position = findTaskIndex(tasks, taskId)
  return position < 0 ? undefined : tasks[position]
}

// ---- 队列顺序 -------------------------------------------------------------------------------

/**
 * 把队列顺序增量并入按 `order` 升序的全量位置表（对应 Rust `apply_queue_order`）：先移除
 * `removed`，再按键插入 / 更新 `added`（同一任务重复出现以最后一项为准），最后按名次重写
 * `position`（1 起）。无任何变化时返回原数组引用；位置未变的条目沿用原对象。
 */
export function applyQueueOrder(
  positions: readonly QueuePositionDto[],
  removed: readonly string[],
  added: readonly QueueOrderEntryDto[],
): QueuePositionDto[] {
  const dropped = new Set(removed)
  for (const entry of added) dropped.add(entry.taskId)
  const seen = new Set<string>()
  const incoming: QueueOrderEntryDto[] = []
  for (let at = added.length - 1; at >= 0; at -= 1) {
    const entry = added[at] as QueueOrderEntryDto
    if (seen.has(entry.taskId)) continue
    seen.add(entry.taskId)
    incoming.push(entry)
  }
  incoming.sort((left, right) => left.order - right.order)

  const merged: QueuePositionDto[] = []
  let changed = incoming.length > 0
  let next = 0
  for (const entry of positions) {
    if (dropped.has(entry.taskId)) {
      changed = true
      continue
    }
    while (next < incoming.length && (incoming[next] as QueueOrderEntryDto).order < entry.order) {
      const fresh = incoming[next] as QueueOrderEntryDto
      merged.push({ taskId: fresh.taskId, position: merged.length + 1, order: fresh.order })
      next += 1
    }
    const position = merged.length + 1
    if (entry.position === position) {
      merged.push(entry)
    } else {
      changed = true
      merged.push({ ...entry, position })
    }
  }
  for (; next < incoming.length; next += 1) {
    const fresh = incoming[next] as QueueOrderEntryDto
    merged.push({ taskId: fresh.taskId, position: merged.length + 1, order: fresh.order })
  }
  return changed ? merged : (positions as QueuePositionDto[])
}

/**
 * 同一批里多个 `queueOrderChanged` 的合成：每条都重写全表名次是 O(N·B)，这里只累计
 * 「被动过的任务」与「最终键」，批末只并入一次。
 *
 * 合成等价于逐条应用（键唯一时）：`removed` 累计所有被移除或被重写的 id（基表里的这些条目都要丢），
 * `added` 只保留最后一次写入且之后没被移除的条目。
 */
interface QueueDelta {
  removed: Set<string>
  added: Map<string, QueueOrderEntryDto>
}

// ---- 批量草稿 -------------------------------------------------------------------------------

/**
 * 一批事件的写时复制草稿。
 *
 * 单条事件原先每次都 `slice` 任务数组、`{...taskRuntime}`，一批 B 帧就是 O(N·B)。草稿对
 * 「任务数组」「taskRuntime」「快照对象本身」各只复制一次，之后在副本上原位修改；批末
 * `finish()` 交出的仍是不可变快照（未变化的分支保持原引用，选择器按引用比较照常生效）。
 * 草稿不跨批复用：下一批从交出的快照重新开始，再次首写时复制。
 */
class DaemonDraft {
  snapshot: DaemonSnapshot
  private snapshotOwned = false
  private tasksOwned = false
  private runtimeOwned = false
  private queueDelta: QueueDelta | null = null
  /** 批开始时的任务数组：批末与它不同就记一条变更日志。 */
  private readonly baseTasks: readonly TaskDto[]
  /** 本批被触及的任务 id（原位替换 / 追加 / 删除）。 */
  private readonly touched = new Set<string>()
  /** 本批整表替换过任务数组：无法用 id 集合描述。 */
  private tasksReset = false

  constructor(base: DaemonSnapshot) {
    this.snapshot = base
    this.baseTasks = base.tasks
  }

  /** 批末交出快照（先并入累计的队列顺序增量）。 */
  finish(): DaemonSnapshot {
    if (this.snapshot.tasks !== this.baseTasks) {
      recordTaskDelta(this.baseTasks, this.snapshot.tasks, this.tasksReset ? null : [...this.touched])
    }
    const delta = this.queueDelta
    if (delta !== null) {
      this.queueDelta = null
      const base = this.snapshot.queuePositions
      const next = applyQueueOrder(base, [...delta.removed], [...delta.added.values()])
      if (next !== base) this.own().queuePositions = next
    }
    return this.snapshot
  }

  /** 整份替换（`snapshotReplaced`）：新对象归事件所有，之后任何写入都要重新复制。 */
  replace(next: DaemonSnapshot): void {
    this.snapshot = next
    this.snapshotOwned = false
    this.tasksOwned = false
    this.runtimeOwned = false
    this.queueDelta = null
    this.tasksReset = true
  }

  private own(): DaemonSnapshot {
    if (!this.snapshotOwned) {
      this.snapshot = { ...this.snapshot }
      this.snapshotOwned = true
    }
    return this.snapshot
  }

  set<K extends keyof DaemonSnapshot>(key: K, value: DaemonSnapshot[K]): void {
    this.own()[key] = value
  }

  // 任务

  taskAt(taskId: string): TaskDto | undefined {
    const position = findTaskIndex(this.snapshot.tasks, taskId)
    return position < 0 ? undefined : this.snapshot.tasks[position]
  }

  private writableTasks(): TaskDto[] {
    if (!this.tasksOwned) {
      const current = this.snapshot.tasks
      const index = taskIndexOf(current)
      const copy = current.slice()
      TASK_INDEX.set(copy, index)
      this.own().tasks = copy
      this.tasksOwned = true
    }
    return this.snapshot.tasks
  }

  /**
   * 整表替换（`tasksSnapshot` / 分页载入装入）：数组归事件所有，下标索引按需重建；清除 `taskTotal`
   * （表已完整）。`taskRuntime` 只保留仍存在的任务，非活跃任务的传输读数归零
   * （对应 Rust `DaemonSnapshot::install_tasks` / `settle_task_table`）。
   * 单遍 O(N)：新 taskRuntime 只分配一次，不活跃任务就地写入清零值。
   */
  installTasks(tasks: TaskDto[]): void {
    const statuses = new Map<string, number>()
    for (const task of tasks) statuses.set(task.taskId, task.status)
    const taskRuntime: Record<string, TaskRuntimeDto> = {}
    for (const [id, runtime] of Object.entries(this.snapshot.taskRuntime)) {
      const status = statuses.get(id)
      if (status === undefined || !runtimeRetained(status)) continue
      taskRuntime[id] = isActiveStatus(status) ? runtime : clearedRuntime(runtime)
    }
    TASK_INDEX.set(tasks, buildTaskIndex(tasks))
    this.own().tasks = tasks
    this.tasksOwned = false
    this.tasksReset = true
    this.replaceRuntime(taskRuntime)
    this.clearTotal('taskTotal')
  }

  /** 清除「待载入」标记（表已完整）。 */
  private clearTotal(key: 'taskTotal' | 'queueTotal'): void {
    if (this.snapshot[key] === undefined) return
    delete this.own()[key]
  }

  /** 服务端宣告表需重新载入（`tablesReloading`）：清空对应表并标记待载入（对应 Rust `begin_reload`）。 */
  beginReload(tasks: number | undefined, queue: number | undefined): void {
    if (tasks !== undefined) {
      this.own().tasks = []
      this.tasksOwned = false
      this.tasksReset = true
      this.set('taskTotal', tasks)
    }
    if (queue !== undefined) {
      this.setQueuePositions([])
      this.set('queueTotal', queue)
    }
  }

  /** 按 id 更新（对应 Rust `task_mut` 后原位改写）；任务不存在时什么也不做。 */
  patchTask(taskId: string, update: (task: TaskDto) => TaskDto): void {
    const position = findTaskIndex(this.snapshot.tasks, taskId)
    if (position < 0) return
    const current = this.snapshot.tasks[position] as TaskDto
    this.writableTasks()[position] = update(current)
    this.touched.add(taskId)
  }

  /** 已存在则整条替换，否则追加（对应 Rust `apply_task_changed`）。 */
  upsertTask(task: TaskDto): void {
    const position = findTaskIndex(this.snapshot.tasks, task.taskId)
    const tasks = this.writableTasks()
    this.touched.add(task.taskId)
    if (position >= 0) {
      tasks[position] = task
      return
    }
    TASK_INDEX.set(tasks, indexAppend(tasks, task.taskId))
    tasks.push(task)
  }

  /**
   * 批量删除；没有命中保持原数组。命中项的位置由索引直接给出，新数组按段复制（不逐项过滤），
   * 新数组的索引只并入被删槽号，不整表重建。
   */
  removeTasks(taskIds: ReadonlySet<string>): void {
    const before = this.snapshot.tasks
    const index = taskIndexOf(before)
    const positions: number[] = []
    for (const taskId of taskIds) {
      const position = findTaskIndex(before, taskId)
      if (position >= 0) positions.push(position)
    }
    if (positions.length === 0) return
    positions.sort((left, right) => left - right)
    for (const position of positions) this.touched.add((before[position] as TaskDto).taskId)

    // 位置升序 ⇒ 槽号升序（数组按槽号排列），可直接归并进 `removed`。
    const slots = positions.map((position) => index.slots.get((before[position] as TaskDto).taskId) as number)
    let kept: TaskDto[]
    if (positions.length <= SPLICE_LIMIT) {
      kept = before.slice()
      for (let at = positions.length - 1; at >= 0; at -= 1) kept.splice(positions[at] as number, 1)
    } else {
      kept = new Array<TaskDto>(before.length - positions.length)
      let write = 0
      let from = 0
      for (let at = 0; at <= positions.length; at += 1) {
        const until = at < positions.length ? (positions[at] as number) : before.length
        for (let position = from; position < until; position += 1) kept[write++] = before[position] as TaskDto
        from = until + 1
      }
    }
    const removed = mergeSorted(index.removed, slots)
    if (removed.length <= COMPACT_REMOVED) TASK_INDEX.set(kept, { slots: index.slots, removed, next: index.next })
    this.own().tasks = kept
    this.tasksOwned = true
  }

  // 传输采样

  runtimeOf(taskId: string): TaskRuntimeDto | undefined {
    return this.snapshot.taskRuntime[taskId]
  }

  private writableRuntime(): Record<string, TaskRuntimeDto> {
    if (!this.runtimeOwned) {
      this.own().taskRuntime = { ...this.snapshot.taskRuntime }
      this.runtimeOwned = true
    }
    return this.snapshot.taskRuntime
  }

  setRuntime(taskId: string, runtime: TaskRuntimeDto): void {
    this.writableRuntime()[taskId] = runtime
  }

  /** 整表替换：调用方保证 `record` 是刚建的新对象。 */
  replaceRuntime(record: Record<string, TaskRuntimeDto>): void {
    this.own().taskRuntime = record
    this.runtimeOwned = true
  }

  deleteRuntime(taskId: string): void {
    if (!Object.hasOwn(this.snapshot.taskRuntime, taskId)) return
    delete this.writableRuntime()[taskId]
  }

  /** 清零传输活跃读数（对应 Rust `clear_active_runtime`）。 */
  clearActiveRuntime(taskId: string): void {
    const runtime = this.snapshot.taskRuntime[taskId]
    if (!runtime) return
    const cleared = clearedRuntime(runtime)
    if (cleared !== runtime) this.setRuntime(taskId, cleared)
  }

  /** 任务状态变化后的运行时收尾（对应 Rust `settle_runtime`）：完成即删除条目，其余非活跃状态清零读数。 */
  settleRuntime(taskId: string, status: number): void {
    if (!runtimeRetained(status)) this.deleteRuntime(taskId)
    else if (!isActiveStatus(status)) this.clearActiveRuntime(taskId)
  }

  // 队列顺序

  /** 全量位置表替换（含载入完成装入）：之前累计的增量作废，清除 `queueTotal`。 */
  setQueuePositions(positions: QueuePositionDto[]): void {
    this.queueDelta = null
    this.set('queuePositions', positions)
    this.clearTotal('queueTotal')
  }

  queueOrderChanged(removed: readonly string[], added: readonly QueueOrderEntryDto[]): void {
    const delta = (this.queueDelta ??= { removed: new Set(), added: new Map() })
    for (const taskId of removed) {
      delta.removed.add(taskId)
      delta.added.delete(taskId)
    }
    for (const entry of added) {
      delta.removed.add(entry.taskId)
      // 先删再写：让 Map 的迭代顺序等于「最后一次出现」的顺序。
      delta.added.delete(entry.taskId)
      delta.added.set(entry.taskId, entry)
    }
  }
}

/** 采样序号不落后、任务存在且未完成时返回任务状态（对应 Rust `accepted_runtime_status`）。 */
export function acceptedRuntimeStatus(snapshot: DaemonSnapshot, runtime: TaskRuntimeDto): number | null {
  const previous = snapshot.taskRuntime[runtime.taskId]
  if (previous && previous.sampleSequence !== 0 && runtime.sampleSequence <= previous.sampleSequence) {
    return null
  }
  const position = findTaskIndex(snapshot.tasks, runtime.taskId)
  if (position < 0) return null
  const status = (snapshot.tasks[position] as TaskDto).status
  return runtimeRetained(status) ? status : null
}

function applyEngineMessage(draft: DaemonDraft, message: WsServerMsg): void {
  switch (message.type) {
    case 'tasksSnapshot':
      draft.installTasks(message.tasks)
      return
    case 'taskProgress': {
      if (message.status === 4 && message.errorMessage === 'deleted') {
        draft.deleteRuntime(message.taskId)
        draft.removeTasks(new Set([message.taskId]))
        return
      }
      draft.settleRuntime(message.taskId, message.status)
      draft.patchTask(message.taskId, (task) => ({
        ...task,
        status: message.status,
        downloadedBytes: message.downloadedBytes,
        totalBytes: message.totalBytes,
        fileName: message.fileName !== '' ? message.fileName : task.fileName,
        saveDir: message.saveDir !== '' ? message.saveDir : task.saveDir,
        url: message.url !== '' ? message.url : task.url,
        errorMessage: message.errorMessage,
        uploadedBytes: message.uploadedBytes,
        seedingStatus: message.seedingStatus,
        seedingMessage: message.seedingMessage,
        seedingTimeSecs: message.seedingTimeSecs,
      }))
      return
    }
    case 'segmentProgress': {
      const task = draft.taskAt(message.taskId)
      if (!task || !runtimeRetained(task.status)) return
      const inactive = !isActiveStatus(task.status)
      const existing = draft.runtimeOf(message.taskId)
      // 真实传输采样存在时不得被持久几何覆盖（对应 Rust 注释）。
      if (existing && existing.sampleSequence !== 0) return
      const base: TaskRuntimeDto = existing ?? {
        taskId: message.taskId,
        sampledAtMs: 0,
        sampleSequence: 0,
        activeTransfers: inactive ? 0 : null,
        connectedPeers: null,
        parallelismLimit: null,
        totalBytes: 0,
        segments: [],
      }
      draft.setRuntime(message.taskId, {
        ...base,
        totalBytes: message.totalBytes,
        segments: message.segments.map((segment) => ({
          index: segment.index,
          startByte: segment.startByte,
          endByte: segment.endByte,
          downloadedBytes: segment.downloadedBytes,
          active: inactive ? false : null,
        })),
      })
      return
    }
    case 'taskMetaProbed':
      draft.patchTask(message.taskId, (task) => ({
        ...task,
        fileName: message.fileName !== '' ? message.fileName : task.fileName,
        totalBytes: message.totalBytes,
      }))
      return
    case 'taskQueueChanged':
      draft.patchTask(message.taskId, (task) => ({ ...task, queueId: message.queueId }))
      return
    case 'taskRouteChanged':
      draft.patchTask(message.taskId, (task) => ({ ...task, autoRoute: message.route }))
      return
    case 'queuesChanged':
      draft.set('queues', message.queues)
      return
    case 'queuePositionsChanged':
      draft.setQueuePositions(message.positions)
      return
    case 'queueOrderChanged':
      draft.queueOrderChanged(message.removed, message.added)
      return
    case 'groupsChanged':
      draft.set('groups', message.groups)
      return
    case 'rssSourcesChanged':
      draft.set('rssSources', message.sources)
      return
    case 'rssItemsChanged': {
      const revisions = draft.snapshot.rssItemRevisions
      draft.set('rssItemRevisions', { ...revisions, [message.sourceId]: (revisions[message.sourceId] ?? 0) + 1 })
      return
    }
    case 'webhookDeliveriesChanged':
      draft.set('webhookDeliveries', mergeWebhookDeliveries(draft.snapshot.webhookDeliveries, message.deliveries))
      return
    case 'fileMissingChanged':
      for (const update of message.updates) {
        draft.patchTask(update.taskId, (task) => ({ ...task, fileMissing: update.missing }))
      }
      return
    case 'priorityTaskChanged':
      draft.set('priority', message.priorityTaskId !== '' ? [message.priorityTaskId] : [])
      return
    case 'pluginAutoDisabled': {
      const plugins = draft.snapshot.plugins
      const position = plugins.findIndex((plugin) => plugin.identity === message.identity)
      if (position < 0) return
      const next = plugins.slice()
      next[position] = { ...(plugins[position] as (typeof plugins)[number]), enabled: false, disabledReason: message.reason }
      draft.set('plugins', next)
      return
    }
    default:
      return
  }
}

function applyDaemonEventTo(draft: DaemonDraft, event: DaemonEvent): void {
  switch (event.type) {
    case 'taskRuntimeChanged': {
      const status = acceptedRuntimeStatus(draft.snapshot, event.data)
      if (status === null) return
      const inactive = !isActiveStatus(status)
      const previous = draft.runtimeOf(event.data.taskId)
      let runtime: TaskRuntimeDto = event.data
      if (runtime.segments.length === 0 && previous) runtime = { ...runtime, segments: previous.segments }
      if (inactive) {
        runtime = {
          ...runtime,
          activeTransfers: 0,
          segments: runtime.segments.map((segment) => ({ ...segment, active: false })),
        }
      }
      draft.setRuntime(runtime.taskId, runtime)
      return
    }
    case 'taskActivityAdded':
    case 'taskNotice':
    case 'pluginViewFrame':
    case 'pluginViewClosed':
    case 'pluginToast':
    case 'btGuardChanged':
    case 'btGuardCloudRefreshWanted':
    case 'btGuardEvidenceReady':
      return
    case 'snapshotReplaced':
      // 载荷是头：整体替换投影（含 `taskTotal` / `queueTotal`），任务表随后分页载入。
      draft.replace(event.data)
      return
    case 'tablesReloading':
      draft.beginReload(event.data.tasks, event.data.queue)
      return
    case 'engine':
      applyEngineMessage(draft, event.data)
      return
    case 'taskChanged':
      applyTaskChanged(draft, event.data)
      return
    case 'tasksChanged':
      // 同一事件内整批只复制一次任务数组（建任务块通常上百条）；逐条语义不变。
      for (const task of event.data) applyTaskChanged(draft, task)
      return
    case 'tasksDeleted': {
      if (event.data.taskIds.length === 0) return
      draft.removeTasks(new Set(event.data.taskIds))
      for (const taskId of event.data.taskIds) draft.deleteRuntime(taskId)
      return
    }
    case 'taskDeleted':
      draft.removeTasks(new Set([event.data.taskId]))
      draft.deleteRuntime(event.data.taskId)
      return
    case 'queuesChanged':
      draft.set('queues', event.data)
      return
    case 'groupsChanged':
      draft.set('groups', event.data)
      return
    case 'configChanged':
      draft.set('config', event.data)
      return
    case 'rssChanged':
      draft.set('rssItemRevisions', { ...draft.snapshot.rssItemRevisions, [event.data.sourceId]: event.data.itemRevision })
      return
    case 'pluginsChanged':
      draft.set('plugins', event.data)
      return
    case 'componentsChanged':
      draft.set('components', event.data)
      return
    case 'webhooksChanged':
      draft.set('webhookDeliveries', mergeWebhookDeliveries(draft.snapshot.webhookDeliveries, event.data))
      return
    case 'webhooksCleared':
      if (draft.snapshot.webhookDeliveries.length !== 0) draft.set('webhookDeliveries', [])
      return
    case 'runtimeStatsChanged':
      draft.set('runtimeStats', event.data)
      return
    case 'selectionPending':
      draft.set('pendingSelections', [
        ...draft.snapshot.pendingSelections.filter((item) => item.requestId !== event.data.requestId),
        event.data,
      ])
      return
    case 'selectionResolved':
      draft.set(
        'pendingSelections',
        draft.snapshot.pendingSelections.filter((item) => item.requestId !== event.data.requestId),
      )
      return
    default:
      event satisfies never
  }
}

/** 对应 Rust `apply_task_changed`：整条替换或追加，完成删除运行时，其余非活跃状态清零传输读数。 */
function applyTaskChanged(draft: DaemonDraft, task: TaskDto): void {
  draft.upsertTask(task)
  draft.settleRuntime(task.taskId, task.status)
}

/**
 * 按序应用一批 daemon 事件：语义等同逐条 `applyDaemonEvent`，但任务数组、`taskRuntime`
 * 与快照对象整批各最多复制一次。没有任何事件产生变化时返回原快照引用。
 */
export function applyDaemonEvents(snapshot: DaemonSnapshot, events: readonly DaemonEvent[]): DaemonSnapshot {
  if (events.length === 0) return snapshot
  const draft = new DaemonDraft(snapshot)
  for (const event of events) applyDaemonEventTo(draft, event)
  return draft.finish()
}

export function applyDaemonEvent(snapshot: DaemonSnapshot, event: DaemonEvent): DaemonSnapshot {
  return applyDaemonEvents(snapshot, [event])
}

/** 任务表或队列位置表仍待分页载入：此时 `tasks` / `queuePositions` 不是完整表（对应 Rust `tables_loading`）。 */
export function tablesLoading(snapshot: DaemonSnapshot): boolean {
  return snapshot.taskTotal !== undefined || snapshot.queueTotal !== undefined
}

/** 以分页载入的完整任务表装入投影（对应 Rust `install_tasks`）：清除 `taskTotal`。 */
export function installTasks(snapshot: DaemonSnapshot, tasks: TaskDto[]): DaemonSnapshot {
  const draft = new DaemonDraft(snapshot)
  draft.installTasks(tasks)
  return draft.finish()
}

/**
 * 以分页载入的队列位置装入投影（对应 Rust `install_queue`）：按 `(order, taskId)` 排序，名次 = 下标 + 1，
 * 清除 `queueTotal`。
 */
export function installQueue(snapshot: DaemonSnapshot, entries: readonly QueueOrderEntryDto[]): DaemonSnapshot {
  const sorted = entries
    .slice()
    .sort((left, right) => left.order - right.order || (left.taskId < right.taskId ? -1 : left.taskId > right.taskId ? 1 : 0))
  const draft = new DaemonDraft(snapshot)
  draft.setQueuePositions(sorted.map((entry, index) => ({ taskId: entry.taskId, position: index + 1, order: entry.order })))
  return draft.finish()
}

/**
 * 本事件是否要求接收方分页重载表（对应 Rust `reloads_tables`）：替换事件的头带待载入标记，
 * 或 `tablesReloading`。
 */
export function daemonEventReloadsTables(event: DaemonEvent): boolean {
  switch (event.type) {
    case 'snapshotReplaced':
      return tablesLoading(event.data)
    case 'tablesReloading':
      return event.data.tasks !== undefined || event.data.queue !== undefined
    default:
      return false
  }
}

export function agentEventReloadsTables(event: AgentEvent): boolean {
  switch (event.type) {
    case 'daemon':
      return daemonEventReloadsTables(event.data)
    case 'daemonSnapshotReplaced':
      return tablesLoading(event.data)
    default:
      return false
  }
}

export function serviceEventReloadsTables(event: ServiceEvent): boolean {
  return event.service === 'daemon' ? daemonEventReloadsTables(event.event) : agentEventReloadsTables(event.event)
}

function settleDraft(agent: AgentSnapshot, draft: DaemonDraft | null): AgentSnapshot {
  if (draft === null) return agent
  const daemon = draft.finish()
  return daemon === agent.daemon ? agent : { ...agent, daemon }
}

/**
 * 按序应用一批 agent 事件：连续的 daemon 事件共用一份草稿（整批只复制一次大集合），
 * 遇到其它 agent 事件先结算草稿再应用，保持逐条应用的先后语义。
 */
export function applyAgentEvents(snapshot: AgentSnapshot, events: readonly AgentEvent[]): AgentSnapshot {
  let agent = snapshot
  let draft: DaemonDraft | null = null
  for (const event of events) {
    if (event.type === 'daemon') {
      draft ??= new DaemonDraft(agent.daemon)
      applyDaemonEventTo(draft, event.data)
      continue
    }
    agent = settleDraft(agent, draft)
    draft = null
    agent = applyNonDaemonAgentEvent(agent, event)
  }
  return settleDraft(agent, draft)
}

export function applyAgentEvent(snapshot: AgentSnapshot, event: AgentEvent): AgentSnapshot {
  return applyAgentEvents(snapshot, [event])
}

function applyNonDaemonAgentEvent(snapshot: AgentSnapshot, event: Exclude<AgentEvent, { type: 'daemon' }>): AgentSnapshot {
  switch (event.type) {
    case 'daemonSnapshotReplaced':
      return { ...snapshot, daemon: event.data }
    case 'daemonConnectionChanged':
      return {
        ...snapshot,
        daemonConnected: event.data,
        daemon: event.data ? snapshot.daemon : { ...snapshot.daemon, taskRuntime: {} },
      }
    case 'sessionChanged':
      // 会话结束后账号维度投影随之失效（对应 Rust apply_agent_event）。
      return event.data === null
        ? {
            ...snapshot,
            session: null,
            cloudDevices: [],
            remoteTasks: [],
            cloudConnection: { state: 'disconnected' },
            cloudNotify: snapshot.cloudNotify
              ? {
                  ...snapshot.cloudNotify,
                  overview: null,
                  loading: false,
                  lastErrorReason: null,
                  updatedAtUnixMs: null,
                  recentDeliveries: [],
                  recentNextCursor: null,
                }
              : snapshot.cloudNotify,
          }
        : { ...snapshot, session: event.data }
    case 'syncChanged':
      return { ...snapshot, sync: event.data }
    case 'updateChanged':
      return { ...snapshot, update: event.data }
    case 'cloudConnectionChanged':
      return {
        ...snapshot,
        cloudConnection: event.data,
        cloudDevices: event.data.state === 'connected'
          ? snapshot.cloudDevices
          : snapshot.cloudDevices.map((device) => device.isCurrent ? { ...device, isOnline: false } : device),
      }
    case 'preferencesChanged':
      return { ...snapshot, preferences: event.data }
    case 'gatewayChanged':
      return { ...snapshot, gateway: event.data }
    case 'cloudDevicesChanged':
      return { ...snapshot, cloudDevices: event.data }
    case 'linkedDevicesChanged':
      return { ...snapshot, linkedDevices: event.data }
    case 'linkPairingRequestsChanged':
      return { ...snapshot, linkPairingRequests: event.data }
    case 'linkDiscoveredChanged':
      return { ...snapshot, linkDiscovered: event.data }
    case 'remoteTasksChanged':
      return { ...snapshot, remoteTasks: event.data }
    case 'pendingCapturesChanged':
      return { ...snapshot, pendingCaptures: event.data }
    case 'shellChanged':
      return { ...snapshot, shell: event.data }
    case 'powerChanged':
      return { ...snapshot, power: event.data }
    case 'cloudNotifyChanged':
      return { ...snapshot, cloudNotify: event.data }
    case 'captureTasksStarted':
    case 'captureTasksResumed':
    case 'sessionRevoked':
      return snapshot
  }
}
