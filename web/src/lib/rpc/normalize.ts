// 线上帧的入口归一化（协议 v17）。
//
// `TaskDto` 的默认值字段（空串 / 0 / false / 全 0 `sourceBytes`、四个 `seed*Limit*` = -2）不再上线，
// 解码必须补齐，之后 store / reducer / UI 只面对完整的 `TaskDto`，不再散落 `?? 0`。
// 所有进入 store 的入口都经这里：`system.snapshot`、`system.taskPage`、`service.event`
// （`taskChanged` / `tasksChanged` / `engine(tasksSnapshot)` / 替换事件）、`daemon.task.get`。
// 归一化是幂等的：已补齐的 `TaskDto` 再过一遍内容不变。

import {
  SEED_LIMIT_INHERIT,
  type AgentEvent,
  type AgentSnapshot,
  type DaemonEvent,
  type DaemonSnapshot,
  type EventFrame,
  type TaskDto,
  type TaskDtoRequiredKey,
  type TaskDtoWire,
  type TablePage,
  type TablePageWire,
  type TaskSourceBytesDto,
} from './protocol'

const ZERO_SOURCE_BYTES: TaskSourceBytesDto = Object.freeze({ cdnBytes: 0, proxyBytes: 0, nicBytes: 0 })

/** 对应 Rust `TaskDto` 各字段的 serde 默认值。 */
const TASK_DEFAULTS: Omit<TaskDto, TaskDtoRequiredKey> = Object.freeze({
  errorMessage: '',
  proxyUrl: '',
  queueId: '',
  checksum: '',
  ignoreTlsErrors: false,
  fileMissing: false,
  completedAt: '',
  fileHash: '',
  referrer: '',
  groupId: '',
  rssSourceId: '',
  originUrl: '',
  autoRoute: '',
  queueOrder: 0,
  uploadedBytes: 0,
  uploadedAtCompletion: 0,
  seedingStatus: 0,
  seedingMessage: '',
  seedingTimeSecs: 0,
  seedRatioLimitMilli: SEED_LIMIT_INHERIT,
  seedPostRatioLimitMilli: SEED_LIMIT_INHERIT,
  seedTimeLimitMinutes: SEED_LIMIT_INHERIT,
  seedInactiveTimeLimitMinutes: SEED_LIMIT_INHERIT,
  seedUploadLimitBps: 0,
  btSequential: false,
  btFirstLastPiecesFirst: false,
  sourceBytes: ZERO_SOURCE_BYTES,
})

/** 补齐线上缺省的默认值字段。 */
export function normalizeTask(wire: TaskDtoWire): TaskDto {
  const task = { ...TASK_DEFAULTS, ...wire } as TaskDto
  // `sourceBytes` 若出现必为完整对象；这里只防御服务端将来改成部分字段。
  if (wire.sourceBytes !== undefined) task.sourceBytes = { ...ZERO_SOURCE_BYTES, ...wire.sourceBytes }
  return task
}

function normalizeTasks(tasks: readonly TaskDtoWire[]): TaskDto[] {
  const out = new Array<TaskDto>(tasks.length)
  for (let at = 0; at < tasks.length; at += 1) out[at] = normalizeTask(tasks[at] as TaskDtoWire)
  return out
}

/** 快照头 / 内联旧形态：任务表为空时原样返回。 */
export function normalizeDaemonSnapshot(snapshot: DaemonSnapshot): DaemonSnapshot {
  return snapshot.tasks.length === 0 ? snapshot : { ...snapshot, tasks: normalizeTasks(snapshot.tasks) }
}

export function normalizeAgentSnapshot(snapshot: AgentSnapshot): AgentSnapshot {
  const daemon = normalizeDaemonSnapshot(snapshot.daemon)
  return daemon === snapshot.daemon ? snapshot : { ...snapshot, daemon }
}

export function normalizeDaemonEvent(event: DaemonEvent): DaemonEvent {
  switch (event.type) {
    case 'taskChanged':
      return { type: 'taskChanged', data: normalizeTask(event.data) }
    case 'tasksChanged':
      return { type: 'tasksChanged', data: normalizeTasks(event.data) }
    case 'snapshotReplaced': {
      const data = normalizeDaemonSnapshot(event.data)
      return data === event.data ? event : { type: 'snapshotReplaced', data }
    }
    case 'engine':
      return event.data.type === 'tasksSnapshot'
        ? { type: 'engine', data: { ...event.data, tasks: normalizeTasks(event.data.tasks) } }
        : event
    default:
      return event
  }
}

export function normalizeAgentEvent(event: AgentEvent): AgentEvent {
  switch (event.type) {
    case 'daemon': {
      const data = normalizeDaemonEvent(event.data)
      return data === event.data ? event : { type: 'daemon', data }
    }
    case 'daemonSnapshotReplaced': {
      const data = normalizeDaemonSnapshot(event.data)
      return data === event.data ? event : { type: 'daemonSnapshotReplaced', data }
    }
    default:
      return event
  }
}

/** `service.event` 帧：事件体归一化，帧头原样保留；无变化时返回原帧。 */
export function normalizeFrame(frame: EventFrame): EventFrame {
  const wrapped = frame.event
  if (wrapped.service === 'daemon') {
    const event = normalizeDaemonEvent(wrapped.event)
    return event === wrapped.event ? frame : { ...frame, event: { service: 'daemon', event } }
  }
  const event = normalizeAgentEvent(wrapped.event)
  return event === wrapped.event ? frame : { ...frame, event: { service: 'agent', event } }
}

/** `system.taskPage` 应答：任务补齐默认值，缺省的行集合补空数组。 */
export function normalizeTablePage(wire: TablePageWire): TablePage {
  return {
    epoch: wire.epoch,
    sequence: wire.sequence,
    section: wire.section,
    total: wire.total,
    tasks: wire.tasks === undefined ? [] : normalizeTasks(wire.tasks),
    queue: wire.queue ?? [],
    ...(wire.nextCursor === undefined ? {} : { nextCursor: wire.nextCursor }),
  }
}
