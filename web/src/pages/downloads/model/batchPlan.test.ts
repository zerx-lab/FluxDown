import { describe, expect, test } from 'bun:test'
import { RpcError } from '../../../lib/rpc'
import type { RemoteCommandManyParams, RemoteCommandParams, RpcErrorData } from '../../../lib/rpc'
import {
  chunkIds,
  planTaskCommand,
  remoteTaskIds,
  REMOTE_FALLBACK_CONCURRENCY,
  runRemoteCommands,
  summarizeRemote,
} from './batchPlan'
import type { DownloadTaskView } from './task'

function local(taskId: string): DownloadTaskView {
  return { source: 'local', taskId, remoteStatus: null } as unknown as DownloadTaskView
}

function remote(taskId: string, remoteStatus: string | null): DownloadTaskView {
  return { source: 'remote', taskId, remoteStatus } as unknown as DownloadTaskView
}

describe('planTaskCommand', () => {
  test('多个本地任务合并为一次批量调用，并按选择顺序去重', () => {
    const plan = planTaskCommand([local('a'), local('b'), local('a'), local('c')], 'delete')
    expect(plan.batch).toBe(true)
    expect(plan.localIds).toEqual(['a', 'b', 'c'])
    expect(plan.remote).toEqual([])
  })

  test('单个本地任务走单任务方法', () => {
    const plan = planTaskCommand([local('a'), local('a')], 'pause')
    expect(plan.batch).toBe(false)
    expect(plan.localIds).toEqual(['a'])
  })

  test('远程任务按状态过滤后逐个发送，不进入批量', () => {
    const paused = remote('r1', 'paused')
    const running = remote('r2', 'downloading')
    const unknown = remote('r3', 'unknown')
    const plan = planTaskCommand([local('a'), paused, running, unknown], 'resume')
    expect(plan.localIds).toEqual(['a'])
    expect(plan.batch).toBe(false)
    expect(plan.remote).toEqual([paused])
  })

  test('继续跳过已完成的本地任务，删除仍包含（#935）', () => {
    const view = (taskId: string, state: string) =>
      ({ source: 'local', taskId, remoteStatus: null, state }) as unknown as DownloadTaskView
    const views = [view('done', 'completed'), view('failed', 'failed'), view('paused', 'paused')]
    expect(planTaskCommand(views, 'resume').localIds).toEqual(['failed', 'paused'])
    expect(planTaskCommand(views, 'delete').localIds).toEqual(['done', 'failed', 'paused'])
  })

  test('继续只放行可恢复做种的完成态任务（2..=7 且文件在），做种中 / 无做种 / 文件缺失仍跳过', () => {
    const view = (taskId: string, seedingStatus: number, fileMissing = false) =>
      ({ source: 'local', taskId, remoteStatus: null, state: 'completed', seedingStatus, fileMissing }) as unknown as DownloadTaskView
    const views = [view('none', 0), view('seeding', 1), view('ratio', 2), view('stopped', 4), view('inactive', 7), view('queued', 8), view('missing', 4, true)]
    expect(planTaskCommand(views, 'resume').localIds).toEqual(['ratio', 'stopped', 'inactive'])
    expect(planTaskCommand(views, 'pause').localIds).toHaveLength(views.length)
  })

  test('空选择无任何调用', () => {
    const plan = planTaskCommand([], 'delete')
    expect(plan.localIds).toEqual([])
    expect(plan.batch).toBe(false)
    expect(plan.remote).toEqual([])
  })
})

const tr = (key: string, params?: Record<string, string | number>) =>
  `${key}${params ? JSON.stringify(params) : ''}`

const rpcError = (data: RpcErrorData) => new RpcError({ code: -32000, message: '', data })
const gone: RpcErrorData = { code: 'notFound', retryable: false, reason: 'remoteTaskGone' }

describe('远程批量', () => {
  test('id 去重保序，按 1000 分块', () => {
    expect(remoteTaskIds([remote('a', 'paused'), remote('b', 'paused'), remote('a', 'paused')])).toEqual(['a', 'b'])
    const ids = Array.from({ length: 2500 }, (_, index) => `t${index}`)
    expect(chunkIds(ids).map((chunk) => chunk.length)).toEqual([1000, 1000, 500])
    expect(chunkIds([])).toEqual([])
  })

  test('宿主支持批量：按块发 commandMany，delete 带 deleteFiles，失败项还原为 RpcError', async () => {
    const calls: RemoteCommandManyParams[] = []
    const ids = Array.from({ length: 1001 }, (_, index) => `t${index}`)
    const outcome = await runRemoteCommands(ids, 'delete', true, {
      batch: true,
      command: () => Promise.reject(new Error('不应逐条调用')),
      commandMany: async (params) => {
        calls.push(params)
        const [first, ...rest] = params.taskIds
        return first === 't0'
          ? { succeeded: rest, failed: [{ taskId: 't0', error: gone }] }
          : { succeeded: params.taskIds, failed: [] }
      },
    })
    expect(calls.map((call) => call.taskIds.length)).toEqual([1000, 1])
    expect(calls.every((call) => call.action === 'delete' && call.deleteFiles === true)).toBe(true)
    expect(outcome.done).toBe(1000)
    expect(outcome.failed).toHaveLength(1)
    expect(outcome.failed[0]?.taskId).toBe('t0')
    const error = outcome.failed[0]?.error
    expect(error instanceof RpcError ? error.reason : undefined).toBe('remoteTaskGone')
  })

  test('非 delete 不带 deleteFiles；整次调用失败则该块全部记为失败', async () => {
    const seen: RemoteCommandManyParams[] = []
    const outcome = await runRemoteCommands(['a', 'b'], 'pause', false, {
      batch: true,
      command: () => Promise.resolve(),
      commandMany: (params) => {
        seen.push(params)
        return Promise.reject(rpcError({ code: 'unavailable', retryable: true }))
      },
    })
    expect(seen[0]).toEqual({ taskIds: ['a', 'b'], action: 'pause' })
    expect(outcome.done).toBe(0)
    expect(outcome.failed.map((failure) => failure.taskId)).toEqual(['a', 'b'])
  })

  test('旧宿主逐条回退：并发不超过 4，失败不中断其余', async () => {
    let active = 0
    let peak = 0
    const calls: RemoteCommandParams[] = []
    const ids = Array.from({ length: 12 }, (_, index) => `t${index}`)
    const outcome = await runRemoteCommands(ids, 'resume', false, {
      batch: false,
      commandMany: () => Promise.reject(new Error('不应批量调用')),
      command: async (params) => {
        calls.push(params)
        active += 1
        peak = Math.max(peak, active)
        await new Promise((resolve) => setTimeout(resolve, 2))
        active -= 1
        if (params.taskId === 't3') throw rpcError({ code: 'notFound', retryable: false })
      },
    })
    expect(peak <= REMOTE_FALLBACK_CONCURRENCY).toBe(true)
    expect(peak > 1).toBe(true)
    expect(calls).toHaveLength(12)
    expect(outcome.done).toBe(11)
    expect(outcome.failed.map((failure) => failure.taskId)).toEqual(['t3'])
  })

  test('汇总：单条成功不提示，多条成功 remoteBatchDone', () => {
    expect(summarizeRemote({ done: 1, failed: [] }, tr)).toBeNull()
    expect(summarizeRemote({ done: 0, failed: [] }, tr)).toBeNull()
    expect(summarizeRemote({ done: 3, failed: [] }, tr)).toEqual({ level: 'success', text: 'remoteBatchDone{"n":3}' })
  })

  test('汇总：单条失败直接给原因，多条含失败给 remoteBatchPartial 与首个失败原因', () => {
    const failure = { taskId: 'a', error: rpcError(gone) }
    expect(summarizeRemote({ done: 0, failed: [failure] }, tr)).toEqual({
      level: 'error',
      text: 'errReasonRemoteTaskGone',
    })
    const offline = { taskId: 'b', error: rpcError({ code: 'unavailable', retryable: false, reason: 'targetDeviceOffline' }) }
    expect(summarizeRemote({ done: 2, failed: [offline, failure] }, tr)).toEqual({
      level: 'error',
      text: 'remoteBatchPartial{"done":2,"failed":2,"reason":"errReasonTargetDeviceOffline"}',
    })
  })
})
