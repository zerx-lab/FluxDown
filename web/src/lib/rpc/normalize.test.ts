// 协议 v17：`TaskDto` 默认值字段不再上线，所有入口必须补齐。

import { describe, expect, test } from 'bun:test'
import {
  normalizeAgentEvent,
  normalizeAgentSnapshot,
  normalizeDaemonEvent,
  normalizeFrame,
  normalizeTablePage,
  normalizeTask,
} from './normalize'
import type { AgentSnapshot, DaemonEvent, EventFrame, TaskDtoWire } from './protocol'

/** 线上永远带的字段。 */
function wire(taskId: string, patch: Partial<TaskDtoWire> = {}): TaskDtoWire {
  return {
    taskId,
    url: 'http://x/a',
    fileName: 'a.bin',
    saveDir: '/d',
    status: 1,
    downloadedBytes: 5,
    totalBytes: 10,
    createdAt: '1700000000',
    ...patch,
  }
}

const DEFAULTS = {
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
  seedRatioLimitMilli: -2,
  seedPostRatioLimitMilli: -2,
  seedTimeLimitMinutes: -2,
  seedInactiveTimeLimitMinutes: -2,
  seedUploadLimitBps: 0,
  sourceBytes: { cdnBytes: 0, proxyBytes: 0, nicBytes: 0 },
}

describe('normalizeTask', () => {
  test('缺省字段回落到 Rust serde 默认值：空串 / 0 / false / 全 0 sourceBytes / seed*Limit* = -2', () => {
    expect(normalizeTask(wire('t'))).toEqual({ ...wire('t'), ...DEFAULTS })
  })

  test('已上线的字段原样保留（包括与默认值不同的 0 / -1 哨兵）', () => {
    const task = normalizeTask(
      wire('t', {
        errorMessage: 'boom',
        queueId: 'later',
        ignoreTlsErrors: true,
        seedRatioLimitMilli: -1,
        seedTimeLimitMinutes: 0,
        seedUploadLimitBps: 1024,
        sourceBytes: { cdnBytes: 1, proxyBytes: 2, nicBytes: 3 },
        queueOrder: 7,
      }),
    )
    expect(task.errorMessage).toBe('boom')
    expect(task.queueId).toBe('later')
    expect(task.ignoreTlsErrors).toBe(true)
    expect(task.seedRatioLimitMilli).toBe(-1)
    expect(task.seedTimeLimitMinutes).toBe(0)
    expect(task.seedPostRatioLimitMilli).toBe(-2)
    expect(task.seedUploadLimitBps).toBe(1024)
    expect(task.sourceBytes).toEqual({ cdnBytes: 1, proxyBytes: 2, nicBytes: 3 })
    expect(task.queueOrder).toBe(7)
  })

  test('幂等：已补齐的任务再归一化内容不变', () => {
    const once = normalizeTask(wire('t', { groupId: 'g' }))
    expect(normalizeTask(once)).toEqual(once)
  })

  test('不就地修改线上对象；各任务的 sourceBytes 默认值不会互相污染', () => {
    const raw = wire('t')
    const task = normalizeTask(raw)
    expect(raw).toEqual(wire('t'))
    expect(Object.isFrozen(task)).toBe(false)
    task.groupId = 'changed'
    expect(normalizeTask(wire('u')).groupId).toBe('')
  })
})

describe('入口归一化', () => {
  test('快照：任务表非空（旧内联形态）也补齐；头（空表）原样返回', () => {
    const inline = { daemon: { tasks: [wire('a')], queuePositions: [], taskRuntime: {} } } as unknown as AgentSnapshot
    const normalized = normalizeAgentSnapshot(inline)
    expect(normalized.daemon.tasks[0]).toEqual({ ...wire('a'), ...DEFAULTS })
    const header = { daemon: { tasks: [], taskTotal: 9, queuePositions: [], taskRuntime: {} } } as unknown as AgentSnapshot
    expect(normalizeAgentSnapshot(header)).toBe(header)
  })

  test('daemon 事件：taskChanged / tasksChanged / engine(tasksSnapshot) / snapshotReplaced', () => {
    const changed = normalizeDaemonEvent({ type: 'taskChanged', data: wire('a') as never })
    expect(changed).toEqual({ type: 'taskChanged', data: { ...wire('a'), ...DEFAULTS } })
    const batch = normalizeDaemonEvent({ type: 'tasksChanged', data: [wire('a'), wire('b')] as never })
    expect(batch.type === 'tasksChanged' && batch.data.map((item) => item.seedRatioLimitMilli)).toEqual([-2, -2])
    const engine = normalizeDaemonEvent({ type: 'engine', data: { type: 'tasksSnapshot', tasks: [wire('a')] as never } })
    expect(engine.type === 'engine' && engine.data.type === 'tasksSnapshot' && engine.data.tasks[0]?.sourceBytes).toEqual(
      DEFAULTS.sourceBytes,
    )
    const replaced = normalizeDaemonEvent({
      type: 'snapshotReplaced',
      data: { tasks: [wire('a')], taskTotal: undefined } as never,
    })
    expect(replaced.type === 'snapshotReplaced' && replaced.data.tasks[0]?.errorMessage).toBe('')
  })

  test('与任务无关的事件保持原引用', () => {
    const event: DaemonEvent = { type: 'taskDeleted', data: { taskId: 'a' } }
    expect(normalizeDaemonEvent(event)).toBe(event)
    const engine: DaemonEvent = { type: 'engine', data: { type: 'queuesChanged', queues: [] } }
    expect(normalizeDaemonEvent(engine)).toBe(engine)
    expect(normalizeAgentEvent({ type: 'syncChanged', data: {} as never })).toMatchObject({ type: 'syncChanged' })
  })

  test('agent 转发（嵌套两层）与帧：帧头原样保留', () => {
    const frame: EventFrame = {
      epoch: 'e',
      sequence: 4,
      previous: 1,
      event: { service: 'agent', event: { type: 'daemon', data: { type: 'taskChanged', data: wire('a') as never } } },
    }
    const out = normalizeFrame(frame)
    expect(out).not.toBe(frame)
    expect(out.epoch).toBe('e')
    expect(out.sequence).toBe(4)
    expect(out.previous).toBe(1)
    const inner = out.event.service === 'agent' && out.event.event.type === 'daemon' ? out.event.event.data : null
    expect(inner).toEqual({ type: 'taskChanged', data: { ...wire('a'), ...DEFAULTS } })
    const plain: EventFrame = { epoch: 'e', sequence: 5, event: { service: 'agent', event: { type: 'daemonConnectionChanged', data: true } } }
    expect(normalizeFrame(plain)).toBe(plain)
    const replaced: EventFrame = {
      epoch: 'e',
      sequence: 6,
      event: { service: 'agent', event: { type: 'daemonSnapshotReplaced', data: { tasks: [wire('z')] } as never } },
    }
    const replacedOut = normalizeFrame(replaced)
    expect(replacedOut.event.service === 'agent' && replacedOut.event.event.type === 'daemonSnapshotReplaced').toBe(true)
  })
})

describe('normalizeTablePage', () => {
  test('任务页补齐默认值；缺省的行集合 / nextCursor 补空', () => {
    const page = normalizeTablePage({ epoch: 'e', sequence: 3, section: 'tasks', total: 2, tasks: [wire('a'), wire('b')], nextCursor: 'b' })
    expect(page.tasks.map((item) => item.seedRatioLimitMilli)).toEqual([-2, -2])
    expect(page.queue).toEqual([])
    expect(page.nextCursor).toBe('b')
    const last = normalizeTablePage({ epoch: 'e', sequence: 3, section: 'queue', total: 1, queue: [{ taskId: 'a', order: 1 }] })
    expect(last.tasks).toEqual([])
    expect(last.queue).toEqual([{ taskId: 'a', order: 1 }])
    expect('nextCursor' in last).toBe(false)
    const empty = normalizeTablePage({ epoch: 'e', sequence: 3, section: 'tasks', total: 0 })
    expect(empty.tasks).toEqual([])
    expect(empty.queue).toEqual([])
  })
})
