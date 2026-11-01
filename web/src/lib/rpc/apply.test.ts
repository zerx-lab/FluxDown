import { describe, expect, test } from 'bun:test'
import {
  WEBHOOK_DELIVERY_LIMIT,
  applyAgentEvent,
  applyAgentEvents,
  applyDaemonEvent,
  applyDaemonEvents,
  applyQueueOrder,
} from './apply'
import type {
  AgentEvent,
  AgentSnapshot,
  DaemonEvent,
  DaemonSnapshot,
  QueuePositionDto,
  TaskDto,
  TaskRuntimeDto,
  WebhookDeliveryDto,
} from './protocol'

test('cloud connection is independent of sync, clears stale self presence and resets on logout', () => {
  const initial = {
    sync: { connected: true },
    cloudDevices: [{ deviceId: 'self', isCurrent: true, isOnline: true }],
    remoteTasks: [],
  } as unknown as AgentSnapshot
  const failed = applyAgentEvent(initial, {
    type: 'cloudConnectionChanged',
    data: { state: 'reconnecting', lastError: 'heartbeat rejected', lastErrorReason: 'cloudUnreachable' },
  })
  expect(failed.sync).toBe(initial.sync)
  expect(failed.cloudDevices[0]?.isOnline).toBe(false)
  expect(failed.cloudConnection?.lastErrorReason).toBe('cloudUnreachable')
  const restored = applyAgentEvent(failed, { type: 'cloudConnectionChanged', data: { state: 'connected' } })
  expect(restored.cloudConnection?.lastError).toBeUndefined()
  expect(restored.cloudDevices[0]?.isOnline).toBe(false)
  const loggedOut = applyAgentEvent(restored, { type: 'sessionChanged', data: null })
  expect(loggedOut.cloudConnection).toEqual({ state: 'disconnected' })
})

function task(taskId: string, status: number): TaskDto {
  return { taskId, status, fileName: 'a.bin', saveDir: '/d', url: 'http://x/a', downloadedBytes: 0, totalBytes: 10, errorMessage: '' } as unknown as TaskDto
}

function runtime(taskId: string, sampleSequence: number, active = 2): TaskRuntimeDto {
  return {
    taskId,
    sampleSequence,
    totalBytes: 10,
    activeTransfers: active,
    segments: [{ index: 0, startByte: 0, endByte: 9, downloadedBytes: 1, active: true }],
  } as unknown as TaskRuntimeDto
}

function daemon(tasks: TaskDto[], taskRuntime: Record<string, TaskRuntimeDto> = {}): DaemonSnapshot {
  return { tasks, taskRuntime, pendingSelections: [], rssItemRevisions: {}, priority: [], plugins: [] } as unknown as DaemonSnapshot
}

describe('apply daemon events', () => {
  test('过期采样被丢弃；分段沿用上次；非活跃任务清零活跃读数', () => {
    let snap = daemon([task('t', 1)], { t: runtime('t', 5) })
    const stale = applyDaemonEvent(snap, { type: 'taskRuntimeChanged', data: runtime('t', 5, 9) })
    expect(stale).toBe(snap)

    const empty = { ...runtime('t', 6, 3), segments: [] }
    snap = applyDaemonEvent(snap, { type: 'taskRuntimeChanged', data: empty })
    expect(snap.taskRuntime.t?.segments.length).toBe(1)
    expect(snap.taskRuntime.t?.activeTransfers).toBe(3)

    snap = applyDaemonEvent(snap, { type: 'taskChanged', data: task('t', 2) })
    expect(snap.taskRuntime.t?.activeTransfers).toBe(0)
    expect(snap.taskRuntime.t?.segments[0]?.active).toBe(false)
  })

  test('tasksChanged：同一事件内插入 + 更新，等价逐条 taskChanged；整批只复制一次数组并清零非活跃运行时', () => {
    const base = daemon([task('a', 1), task('b', 1)], { a: runtime('a', 5), b: runtime('b', 5) })
    const batch = [task('a', 2), task('c', 0), task('d', 1), task('c', 1)]
    const batched = applyDaemonEvent(base, { type: 'tasksChanged', data: batch })

    let sequential = base
    for (const item of batch) sequential = applyDaemonEvent(sequential, { type: 'taskChanged', data: item })
    expect(batched).toEqual(sequential)

    expect(batched.tasks.map((item) => [item.taskId, item.status])).toEqual([
      ['a', 2],
      ['b', 1],
      ['c', 1],
      ['d', 1],
    ])
    // a 变为暂停：活跃读数清零；b 不在批内：运行时保持原引用。
    expect(batched.taskRuntime.a?.activeTransfers).toBe(0)
    expect(batched.taskRuntime.b).toBe(base.taskRuntime.b)
    // 原快照不被就地修改。
    expect(base.tasks.length).toBe(2)
  })

  test('tasksChanged：空批保持引用；只含活跃任务时不复制 taskRuntime', () => {
    const base = daemon([task('a', 1)], { a: runtime('a', 5) })
    expect(applyDaemonEvent(base, { type: 'tasksChanged', data: [] })).toBe(base)
    const next = applyDaemonEvent(base, { type: 'tasksChanged', data: [task('n', 1)] })
    expect(next.tasks.length).toBe(2)
    expect(next.taskRuntime).toBe(base.taskRuntime)
  })

  test('tasksDeleted：整批等价于逐条 taskDeleted；空批或无命中保持引用', () => {
    const base = daemon([task('a', 1), task('b', 1), task('c', 3)], { a: runtime('a', 5), c: runtime('c', 5) })
    const ids = ['a', 'c', 'missing']
    const batched = applyDaemonEvent(base, { type: 'tasksDeleted', data: { taskIds: ids } })

    let sequential = base
    for (const taskId of ids) sequential = applyDaemonEvent(sequential, { type: 'taskDeleted', data: { taskId } })
    expect(batched.tasks).toEqual(sequential.tasks)
    expect(batched.taskRuntime).toEqual(sequential.taskRuntime)
    expect(batched.tasks.map((item) => item.taskId)).toEqual(['b'])
    expect(base.tasks.length).toBe(3)
    expect(base.taskRuntime.a).not.toBeUndefined()

    expect(applyDaemonEvent(base, { type: 'tasksDeleted', data: { taskIds: [] } })).toBe(base)
    expect(applyDaemonEvent(base, { type: 'tasksDeleted', data: { taskIds: ['zzz'] } })).toBe(base)
  })

  test('未知任务的采样被忽略', () => {
    const snap = daemon([])
    expect(applyDaemonEvent(snap, { type: 'taskRuntimeChanged', data: runtime('x', 1) })).toBe(snap)
  })

  test('完成任务不留运行时：三条完成路径删除条目，迟到采样 / 持久几何不会把条目加回来', () => {
    const progress = {
      type: 'taskProgress', taskId: '', status: 3, downloadedBytes: 10, totalBytes: 10, speed: 0, fileName: '',
      saveDir: '', uploadSpeed: 0, url: '', errorMessage: '', uploadedBytes: 0, seedingStatus: 0, seedingMessage: '', seedingTimeSecs: 0,
    } as const
    const geometry = (taskId: string): DaemonEvent => ({
      type: 'engine',
      data: {
        type: 'segmentProgress', taskId, totalBytes: 10, segmentCount: 1,
        segments: [{ index: 0, startByte: 0, endByte: 9, downloadedBytes: 10 }],
      },
    })
    let snap = daemon([])
    for (let index = 0; index < 600; index += 1) {
      const id = `t${index}`
      snap = applyDaemonEvent(snap, { type: 'taskChanged', data: task(id, 1) })
      snap = applyDaemonEvent(snap, { type: 'taskRuntimeChanged', data: runtime(id, index + 1) })
      expect(Object.keys(snap.taskRuntime)).toEqual([id])
      const done: DaemonEvent[] = [
        { type: 'taskChanged', data: task(id, 3) },
        { type: 'tasksChanged', data: [task(id, 3)] },
        { type: 'engine', data: { ...progress, taskId: id } },
      ]
      snap = applyDaemonEvent(snap, done[index % 3] as DaemonEvent)
      expect(snap.taskRuntime).toEqual({})
      const late = applyDaemonEvent(snap, { type: 'taskRuntimeChanged', data: runtime(id, index + 10_000) })
      expect(late).toBe(snap)
      expect(applyDaemonEvent(snap, geometry(id))).toBe(snap)
    }
    expect(snap.tasks.length).toBe(600)
    expect(Object.keys(snap.taskRuntime).length).toBe(0)
  })

  test('完成不影响暂停 / 失败：分段保留且读数清零；同批里先完成再迟到的采样被拒', () => {
    let snap = daemon([task('p', 1), task('f', 1), task('c', 1)], { p: runtime('p', 1), f: runtime('f', 1), c: runtime('c', 1) })
    snap = applyDaemonEvents(snap, [
      { type: 'taskChanged', data: task('p', 2) },
      { type: 'taskChanged', data: task('f', 4) },
      { type: 'taskChanged', data: task('c', 3) },
      { type: 'taskRuntimeChanged', data: runtime('c', 2) },
    ])
    expect(Object.keys(snap.taskRuntime).sort()).toEqual(['f', 'p'])
    expect(snap.taskRuntime.p?.activeTransfers).toBe(0)
    expect(snap.taskRuntime.f?.segments[0]?.active).toBe(false)
  })

  test('engine taskProgress：status 4 + deleted 删除任务；否则只覆盖非空字符串字段', () => {
    const base = daemon([task('t', 1)], { t: runtime('t', 1) })
    const msg = {
      type: 'taskProgress', taskId: 't', status: 1, downloadedBytes: 5, totalBytes: 10, speed: 0, fileName: '',
      saveDir: '', uploadSpeed: 0, url: '', errorMessage: '', uploadedBytes: 0, seedingStatus: 0, seedingMessage: '', seedingTimeSecs: 0,
    } as const
    const progressed = applyDaemonEvent(base, { type: 'engine', data: msg })
    expect(progressed.tasks[0]?.fileName).toBe('a.bin')
    expect(progressed.tasks[0]?.downloadedBytes).toBe(5)

    const deleted = applyDaemonEvent(base, { type: 'engine', data: { ...msg, status: 4, errorMessage: 'deleted' } })
    expect(deleted.tasks.length).toBe(0)
    expect(deleted.taskRuntime.t).toBeUndefined()
  })

  test('engine tasksSnapshot：剪除已消失任务；非活跃清零，活跃与已清零条目保持引用', () => {
    const active = runtime('a', 1)
    const cleared = { ...runtime('c', 1, 0), connectedPeers: 0, segments: [] } as unknown as TaskRuntimeDto
    const base = daemon([], { a: active, b: runtime('b', 1), c: cleared, gone: runtime('gone', 1) })
    const next = applyDaemonEvent(base, {
      type: 'engine',
      data: { type: 'tasksSnapshot', tasks: [task('a', 1), task('b', 2), task('c', 2)] },
    } as never)
    expect(next.taskRuntime.a).toBe(active)
    expect(next.taskRuntime.c).toBe(cleared)
    expect(next.taskRuntime.gone).toBeUndefined()
    expect(next.taskRuntime.b?.activeTransfers).toBe(0)
    expect(next.taskRuntime.b?.segments[0]?.active).toBe(false)
  })

  test('selectionPending 按 requestId 去重，selectionResolved 移除', () => {
    const req = (requestId: string) => ({ requestId, taskId: 't' }) as never
    let snap = daemon([])
    snap = applyDaemonEvent(snap, { type: 'selectionPending', data: req('r1') })
    snap = applyDaemonEvent(snap, { type: 'selectionPending', data: req('r1') })
    expect(snap.pendingSelections.length).toBe(1)
    snap = applyDaemonEvent(snap, { type: 'selectionResolved', data: { requestId: 'r1' } })
    expect(snap.pendingSelections.length).toBe(0)
  })

  test('daemonConnectionChanged(false) 清空 taskRuntime', () => {
    const snap = { daemon: daemon([task('t', 1)], { t: runtime('t', 1) }), daemonConnected: true } as unknown as AgentSnapshot
    const next = applyAgentEvent(snap, { type: 'daemonConnectionChanged', data: false })
    expect(next.daemonConnected).toBe(false)
    expect(Object.keys(next.daemon.taskRuntime).length).toBe(0)
  })

  test('webhooksChanged 按 deliveryId 合并、按时间降序、封顶；空增量保留历史，webhooksCleared 才清空', () => {
    const delivery = (deliveryId: string, timestampMs: number, success = true) =>
      ({ deliveryId, timestampMs, success }) as unknown as WebhookDeliveryDto
    let snap = { ...daemon([]), webhookDeliveries: [] } as DaemonSnapshot
    snap = applyDaemonEvent(snap, { type: 'webhooksChanged', data: [delivery('b', 20), delivery('a', 10)] })
    snap = applyDaemonEvent(snap, { type: 'webhooksChanged', data: [delivery('a', 30, false), delivery('c', 15)] })
    expect(snap.webhookDeliveries.map((item) => item.deliveryId)).toEqual(['a', 'b', 'c'])
    expect(snap.webhookDeliveries[0]?.success).toBe(false)

    const kept = applyDaemonEvent(snap, { type: 'webhooksChanged', data: [] })
    expect(kept.webhookDeliveries).toBe(snap.webhookDeliveries)
    const keptEngine = applyDaemonEvent(snap, { type: 'engine', data: { type: 'webhookDeliveriesChanged', deliveries: [] } })
    expect(keptEngine.webhookDeliveries).toBe(snap.webhookDeliveries)
    expect(applyDaemonEvent(snap, { type: 'webhooksCleared' }).webhookDeliveries).toEqual([])

    const flood = Array.from({ length: WEBHOOK_DELIVERY_LIMIT + 5 }, (_, n) => delivery(`d${n}`, n)).reverse()
    const capped = applyDaemonEvent(snap, { type: 'webhooksChanged', data: flood })
    expect(capped.webhookDeliveries.length).toBe(WEBHOOK_DELIVERY_LIMIT)
    expect(capped.webhookDeliveries[0]?.timestampMs).toBe(WEBHOOK_DELIVERY_LIMIT + 4)
  })
})

describe('apply cloud notify', () => {
  const state = {
    reporting: true,
    includeUrl: true,
    includeSaveDir: false,
    catalog: [{ kind: 'email', available: true }],
    overview: { enabled: true, channels: [], maxChannels: 2, accountEmail: 'a@b.c', usage: {} },
    loading: true,
    lastErrorReason: 'cloudUnreachable',
    updatedAtUnixMs: 1,
  } as unknown as NonNullable<AgentSnapshot['cloudNotify']>

  test('cloudNotifyChanged replaces the state; taskNotice never changes the snapshot', () => {
    const initial = { cloudNotify: undefined, daemon: daemon([]) } as unknown as AgentSnapshot
    const next = applyAgentEvent(initial, { type: 'cloudNotifyChanged', data: state })
    expect(next.cloudNotify).toBe(state)
    const notice = applyAgentEvent(next, {
      type: 'daemon',
      data: { type: 'taskNotice', data: { deliveryId: 'd', event: 'task.completed', timestampMs: 1, queueId: 'q', queueName: 'Q' } },
    })
    expect(notice).toBe(next)
  })

  test('plugin view events never change the snapshot', () => {
    const snapshot = { cloudNotify: undefined, daemon: daemon([]) } as unknown as AgentSnapshot
    const events: DaemonEvent[] = [
      { type: 'pluginViewFrame', data: { session: 's', seq: 1, ops: [{ op: 'error', message: 'x' }] } },
      { type: 'pluginViewClosed', data: { session: 's', reason: 'closed' } },
      { type: 'pluginToast', data: { identity: 'a@b', level: 'info', text: 'hi' } },
    ]
    for (const data of events) {
      expect(applyAgentEvent(snapshot, { type: 'daemon', data })).toBe(snapshot)
    }
  })

  test('bt guard events never change the snapshot', () => {
    const snapshot = { cloudNotify: undefined, daemon: daemon([]) } as unknown as AgentSnapshot
    const events: DaemonEvent[] = [
      { type: 'btGuardChanged' },
      { type: 'btGuardCloudRefreshWanted', data: { haveSequence: 7 } },
      { type: 'btGuardEvidenceReady', data: { count: 3 } },
    ]
    for (const data of events) {
      expect(applyAgentEvent(snapshot, { type: 'daemon', data })).toBe(snapshot)
    }
  })

  test('session end clears overview, loading and error but keeps local preferences and the public catalog', () => {
    const snapshot = { cloudNotify: state, cloudDevices: [], remoteTasks: [] } as unknown as AgentSnapshot
    const next = applyAgentEvent(snapshot, { type: 'sessionChanged', data: null })
    expect(next.cloudNotify).toEqual({
      reporting: true,
      catalog: [{ kind: 'email', available: true }],
      includeUrl: true,
      includeSaveDir: false,
      overview: null,
      loading: false,
      lastErrorReason: null,
      updatedAtUnixMs: null,
      recentDeliveries: [],
      recentNextCursor: null,
    })
  })
})

describe('批量应用与队列顺序', () => {
  const entry = (taskId: string, order: number) => ({ taskId, order })
  const pos = (taskId: string, position: number, order: number) => ({ taskId, position, order })

  // Rust `apply_queue_order` 的逐条移植：每个事件整表重写名次（键唯一）。
  function reference(positions: QueuePositionDto[], removed: string[], added: { taskId: string; order: number }[]) {
    const last = new Map(added.map((item) => [item.taskId, item]))
    const dropped = new Set([...removed, ...last.keys()])
    const kept = positions.filter((item) => !dropped.has(item.taskId))
    const merged = [...kept.map((item) => ({ taskId: item.taskId, order: item.order })), ...last.values()]
    merged.sort((a, b) => a.order - b.order)
    return merged.map((item, index) => pos(item.taskId, index + 1, item.order))
  }

  test('queueOrderChanged：先移除再按键插入/更新，名次重写；同 id 以最后一项为准', () => {
    const base = [pos('a', 1, 10), pos('b', 2, 20), pos('c', 3, 30)]
    const next = applyQueueOrder(base, ['a'], [entry('d', 25), entry('x', 5), entry('x', 40)])
    expect(next).toEqual([pos('b', 1, 20), pos('d', 2, 25), pos('c', 3, 30), pos('x', 4, 40)])
    // 队首出队：其余条目键不变，只有名次前移。
    expect(applyQueueOrder(base, ['a'], []).map((item) => item.order)).toEqual([20, 30])
  })

  test('queueOrderChanged：无变化保持原数组；名次未变的条目沿用原对象', () => {
    const base = [pos('a', 1, 10), pos('b', 2, 20), pos('c', 3, 30)]
    expect(applyQueueOrder(base, [], [])).toBe(base)
    expect(applyQueueOrder(base, ['zzz'], [])).toBe(base)
    const tailRemoved = applyQueueOrder(base, ['c'], [])
    expect(tailRemoved[0]).toBe(base[0])
    expect(tailRemoved[1]).toBe(base[1])
  })

  test('同一批的多个 queueOrderChanged 合成，等价逐条应用', () => {
    const base = {
      ...daemon([]),
      queuePositions: [pos('a', 1, 10), pos('b', 2, 20), pos('c', 3, 30), pos('d', 4, 40)],
    } as DaemonSnapshot
    const engine = (removed: string[], added: { taskId: string; order: number }[]): DaemonEvent => ({
      type: 'engine',
      data: { type: 'queueOrderChanged', removed, added },
    })
    const events = [
      engine(['a'], [entry('e', 50)]),
      engine(['e'], [entry('f', 15), entry('b', 60)]),
      engine([], [entry('f', 35)]),
      engine(['d', 'ghost'], []),
    ]
    const batched = applyDaemonEvents(base, events)
    let expected = base.queuePositions
    expected = reference(expected, ['a'], [entry('e', 50)])
    expected = reference(expected, ['e'], [entry('f', 15), entry('b', 60)])
    expected = reference(expected, [], [entry('f', 35)])
    expected = reference(expected, ['d', 'ghost'], [])
    expect(batched.queuePositions).toEqual(expected)
    expect(batched.queuePositions.map((item) => item.taskId)).toEqual(['c', 'f', 'b'])
  })

  test('queuePositionsChanged 全量替换使之前累计的增量作废', () => {
    const base = { ...daemon([]), queuePositions: [pos('a', 1, 10)] } as DaemonSnapshot
    const full = [pos('z', 1, 1), pos('y', 2, 2)]
    const next = applyDaemonEvents(base, [
      { type: 'engine', data: { type: 'queueOrderChanged', removed: ['a'], added: [entry('q', 3)] } },
      { type: 'engine', data: { type: 'queuePositionsChanged', positions: full } },
      { type: 'engine', data: { type: 'queueOrderChanged', removed: ['z'], added: [] } },
    ])
    expect(next.queuePositions).toEqual([pos('y', 1, 2)])
  })

  // 确定性伪随机序列：一批事件的结果必须等于逐条应用，且不改动入参快照。
  function sequence(seed: number): () => number {
    let state = seed
    return () => {
      state = (state * 1664525 + 1013904223) % 4294967296
      return state / 4294967296
    }
  }

  function randomEvents(random: () => number, count: number): DaemonEvent[] {
    const events: DaemonEvent[] = []
    let orderSeed = 1000
    let sampleSeed = 100
    const pick = () => `t${Math.floor(random() * 12)}`
    const nextOrder = () => (orderSeed += 1 + Math.floor(random() * 5))
    for (let n = 0; n < count; n += 1) {
      const roll = Math.floor(random() * 11)
      const id = pick()
      switch (roll) {
        case 0:
          events.push({ type: 'taskChanged', data: task(id, Math.floor(random() * 6)) })
          break
        case 1:
          events.push({ type: 'tasksChanged', data: [task(id, 1), task(pick(), 3), task(id, 2)] })
          break
        case 2:
          events.push({ type: 'tasksDeleted', data: { taskIds: [id, pick()] } })
          break
        case 3:
          events.push({ type: 'taskDeleted', data: { taskId: id } })
          break
        case 4:
          events.push({
            type: 'engine',
            data: {
              type: 'taskProgress', taskId: id, status: random() < 0.2 ? 4 : 1, downloadedBytes: n, totalBytes: 9,
              fileName: '', saveDir: '', url: '', errorMessage: random() < 0.5 ? 'deleted' : '', uploadedBytes: 0,
              seedingStatus: 0, seedingMessage: '', seedingTimeSecs: 0,
            },
          } as DaemonEvent)
          break
        case 5:
          events.push({ type: 'taskRuntimeChanged', data: runtime(id, (sampleSeed += 1), n % 4) })
          break
        case 6:
          events.push({
            type: 'engine',
            data: { type: 'segmentProgress', taskId: id, totalBytes: 10, segments: [{ index: 0, startByte: 0, endByte: 9, downloadedBytes: n }] },
          } as DaemonEvent)
          break
        case 7:
          events.push({ type: 'engine', data: { type: 'taskMetaProbed', taskId: id, fileName: `m${n}`, totalBytes: n } })
          break
        case 8:
          events.push({
            type: 'engine',
            data: { type: 'queueOrderChanged', removed: [pick()], added: [entry(pick(), nextOrder()), entry(pick(), nextOrder())] },
          })
          break
        case 9:
          events.push({
            type: 'engine',
            data: { type: 'queuePositionsChanged', positions: [pos(id, 1, nextOrder()), pos(pick(), 2, nextOrder() + 100_000)] },
          })
          break
        default:
          events.push({ type: 'engine', data: { type: 'fileMissingChanged', updates: [{ taskId: id, missing: random() < 0.5 }] } })
      }
    }
    return events
  }

  test('随机事件序列：批量应用等价逐条应用，入参快照不被改动', () => {
    for (let seed = 1; seed <= 40; seed += 1) {
      const random = sequence(seed)
      const base = {
        ...daemon(
          [task('t0', 1), task('t1', 0), task('t2', 3), task('t5', 1)],
          { t0: runtime('t0', 3), t2: runtime('t2', 2), t5: runtime('t5', 1) },
        ),
        queuePositions: [pos('t1', 1, 1), pos('t3', 2, 2)],
      } as DaemonSnapshot
      const frozen = structuredClone(base)
      const events = randomEvents(random, 60)
      let sequential = base
      for (const event of events) sequential = applyDaemonEvent(sequential, event)
      expect(applyDaemonEvents(base, events)).toEqual(sequential)
      expect(base).toEqual(frozen)
      // 分批（含空批）与整批等价：批边界不影响结果。
      let chunked = base
      for (let at = 0; at < events.length; at += 7) chunked = applyDaemonEvents(chunked, events.slice(at, at + 7))
      expect(chunked).toEqual(sequential)
    }
  })

  test('批内只触及 taskRuntime 时 tasks 数组保持引用；只触及 tasks 时 taskRuntime 保持引用', () => {
    const base = daemon([task('a', 1), task('b', 1)], { a: runtime('a', 1) })
    const runtimeOnly = applyDaemonEvents(base, [
      { type: 'taskRuntimeChanged', data: runtime('a', 2) },
      { type: 'taskRuntimeChanged', data: runtime('b', 1) },
    ])
    expect(runtimeOnly.tasks).toBe(base.tasks)
    expect(runtimeOnly.taskRuntime).not.toBe(base.taskRuntime)
    const tasksOnly = applyDaemonEvents(base, [
      { type: 'taskChanged', data: task('a', 1) },
      { type: 'taskChanged', data: task('c', 1) },
      { type: 'taskChanged', data: task('b', 1) },
    ])
    expect(tasksOnly.taskRuntime).toBe(base.taskRuntime)
    expect(tasksOnly.tasks.map((item) => item.taskId)).toEqual(['a', 'b', 'c'])
  })

  test('快照链分叉：同一祖先上的两个分支各自追加，下标索引互不串位', () => {
    const base = daemon([task('a', 1), task('b', 1)])
    const left = applyDaemonEvent(base, { type: 'taskChanged', data: task('x', 1) })
    const right = applyDaemonEvent(base, { type: 'taskChanged', data: task('y', 1) })
    const leftMore = applyDaemonEvent(left, { type: 'taskChanged', data: task('y', 2) })
    const rightMore = applyDaemonEvent(right, { type: 'taskChanged', data: task('x', 2) })
    expect(leftMore.tasks.map((item) => [item.taskId, item.status])).toEqual([['a', 1], ['b', 1], ['x', 1], ['y', 2]])
    expect(rightMore.tasks.map((item) => [item.taskId, item.status])).toEqual([['a', 1], ['b', 1], ['y', 1], ['x', 2]])
    // 祖先快照仍可继续被应用。
    const again = applyDaemonEvent(base, { type: 'taskChanged', data: task('b', 2) })
    expect(again.tasks.map((item) => item.taskId)).toEqual(['a', 'b'])
    expect(left.tasks.map((item) => item.taskId)).toEqual(['a', 'b', 'x'])
  })

  test('agent 批量应用：夹在 daemon 事件之间的 agent 事件保持先后语义', () => {
    const snap = { daemon: daemon([task('a', 1)]), daemonConnected: true } as unknown as AgentSnapshot
    const events: AgentEvent[] = [
      { type: 'daemon', data: { type: 'taskChanged', data: task('b', 1) } },
      { type: 'daemonSnapshotReplaced', data: daemon([task('z', 1)]) },
      { type: 'daemon', data: { type: 'taskChanged', data: task('c', 1) } },
    ]
    const batched = applyAgentEvents(snap, events)
    let sequential = snap
    for (const event of events) sequential = applyAgentEvent(sequential, event)
    expect(batched).toEqual(sequential)
    expect(batched.daemon.tasks.map((item) => item.taskId)).toEqual(['z', 'c'])
  })
})

describe('任务下标索引（槽号 + 删除表）', () => {
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

  /** 与数组语义逐步对拍：随机在历史快照上追加 / 更新 / 批量删除 / 删后重建同 id / 进度补丁。 */
  const cases = [1, 2, 3, 4, 5, 6]
  for (const seed of cases) test(`随机事件序列与朴素数组模型一致（含分叉的历史快照）seed=${seed}`, () => {
    const random = mulberry(seed)
    const shape = (list: readonly TaskDto[]) => list.map((item) => [item.taskId, item.status, item.downloadedBytes])
    const initial = Array.from({ length: 40 }, (_, index) => task(`t${index}`, 1))
    const history: { snapshot: DaemonSnapshot; model: TaskDto[] }[] = [{ snapshot: daemon(initial), model: initial }]
    let counter = 40
    for (let step = 0; step < 400; step += 1) {
      const from = history[Math.floor(random() * history.length)] as { snapshot: DaemonSnapshot; model: TaskDto[] }
      const ids = from.model.map((item) => item.taskId)
      const pick = (): string => (ids[Math.floor(random() * ids.length)] as string | undefined) ?? 'none'
      const roll = random()
      let event: DaemonEvent
      let model = from.model
      if (roll < 0.25) {
        const data = task(`t${counter++}`, 1)
        event = { type: 'taskChanged', data }
        model = [...model, data]
      } else if (roll < 0.45) {
        const data = task(pick(), 2)
        event = { type: 'taskChanged', data }
        model = model.some((item) => item.taskId === data.taskId)
          ? model.map((item) => (item.taskId === data.taskId ? data : item))
          : [...model, data]
      } else if (roll < 0.6) {
        const taskId = pick()
        event = { type: 'taskDeleted', data: { taskId } }
        model = model.filter((item) => item.taskId !== taskId)
      } else if (roll < 0.8) {
        const taskIds = Array.from({ length: 1 + Math.floor(random() * 30) }, pick)
        event = { type: 'tasksDeleted', data: { taskIds } }
        const gone = new Set(taskIds)
        model = model.filter((item) => !gone.has(item.taskId))
      } else if (roll < 0.9) {
        // 删后用同一个 id 重建：不得读到旧槽。
        const taskId = pick()
        const recreated = task(taskId, 3)
        const deleted = applyDaemonEvent(from.snapshot, { type: 'taskDeleted', data: { taskId } })
        const next = applyDaemonEvent(deleted, { type: 'taskChanged', data: recreated })
        model = [...model.filter((item) => item.taskId !== taskId), recreated]
        history.push({ snapshot: next, model })
        expect(shape(next.tasks)).toEqual(shape(model))
        continue
      } else {
        const taskId = pick()
        event = {
          type: 'engine',
          data: { type: 'taskProgress', taskId, status: 1, downloadedBytes: step + 1, totalBytes: 10, fileName: '', saveDir: '', url: '', errorMessage: '', uploadedBytes: 0, seedingStatus: 0, seedingMessage: '', seedingTimeSecs: 0, speed: 0, uploadSpeed: 0 },
        } as unknown as DaemonEvent
        model = model.map((item) => (item.taskId === taskId ? ({ ...item, status: 1, downloadedBytes: step + 1 } as TaskDto) : item))
      }
      const next = applyDaemonEvent(from.snapshot, event)
      expect(shape(next.tasks)).toEqual(shape(model))
      history.push({ snapshot: next, model })
    }
  })

  test('累计删除超过压实阈值后仍然正确', () => {
    const tasks = Array.from({ length: 6000 }, (_, index) => task(`t${index}`, 1))
    let snapshot = daemon(tasks)
    // 先建立索引再删：每次删 1000 个，累计 5000 > 4096，触发整表重建。
    snapshot = applyDaemonEvent(snapshot, { type: 'taskChanged', data: { ...task('t0', 1), downloadedBytes: 1 } as TaskDto })
    for (let round = 0; round < 5; round += 1) {
      snapshot = applyDaemonEvent(snapshot, {
        type: 'tasksDeleted',
        data: { taskIds: Array.from({ length: 1000 }, (_, index) => `t${round * 1000 + index + 1}`) },
      })
      const survivor = snapshot.tasks[snapshot.tasks.length - 1] as TaskDto
      snapshot = applyDaemonEvent(snapshot, { type: 'taskChanged', data: { ...survivor, downloadedBytes: 7 } as TaskDto })
      expect(snapshot.tasks[snapshot.tasks.length - 1]?.downloadedBytes).toBe(7)
    }
    expect(snapshot.tasks.map((item) => item.taskId)).toEqual(['t0', 't5001'].concat(Array.from({ length: 998 }, (_, index) => `t${5002 + index}`)))
  })
})
