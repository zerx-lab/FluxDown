import { describe, expect, test } from 'bun:test'
import type { CreateTaskItem, CreateTaskRequest, DaemonCreateTasksParams, DaemonCreateTasksResult, RpcErrorData } from '../../../lib/rpc'
import { RpcError } from '../../../lib/rpc/error'
import { IMPORT_CHUNK_SIZE, importNotice, runImport } from './importRunner'
import type { ImportProgress } from './importRunner'
import { buildBatch } from './model'
import type { DraftOptions } from './model'

const common = { url: '', fileName: '', checksum: '' } as CreateTaskRequest

function items(count: number): CreateTaskItem[] {
  return Array.from({ length: count }, (_, index) => ({ url: `https://example.com/${index}`, fileName: '', checksum: '' }))
}

function transport(code: 'unavailable' | 'timeout'): RpcError {
  return new RpcError({ code: -32001, message: code, data: { code, retryable: true } })
}

function createdAll(params: DaemonCreateTasksParams): DaemonCreateTasksResult {
  return { results: params.items.map((item) => ({ taskId: `t:${item.url}` })) }
}

const never = <T>() => new Promise<T>(() => undefined)
const instant = () => Promise.resolve()

describe('runImport', () => {
  test('按 256 分块，每块唯一 batchId、共享 common', async () => {
    for (const [count, expected] of [[1, [1]], [255, [255]], [256, [256]], [257, [256, 1]], [512, [256, 256]], [600, [256, 256, 88]]] as const) {
      const seen: DaemonCreateTasksParams[] = []
      const report = await runImport({
        common,
        items: items(count),
        call: async (params) => {
          seen.push(params)
          return createdAll(params)
        },
        sleep: instant,
      })
      expect(seen.map((params) => params.items.length)).toEqual([...expected])
      expect(new Set(seen.map((params) => params.batchId)).size).toBe(seen.length)
      expect(seen.every((params) => params.batchId !== '' && params.common === common)).toBe(true)
      expect(report.succeeded).toBe(count)
      expect(report.cancelled).toBe(false)
      expect(importNotice(report).kind).toBe('created')
    }
    expect(IMPORT_CHUNK_SIZE).toBe(256)
  })

  test('逐条结果：失败计数、首个错误与同序结果', async () => {
    const denied: RpcErrorData = { code: 'invalidArgument', retryable: false }
    const report = await runImport({
      common,
      items: items(4),
      call: async (params) => ({
        results: params.items.map((_, index) => (index === 1 || index === 3 ? { error: { ...denied, field: `f${index}` } } : { taskId: `id${index}` })),
      }),
      sleep: instant,
    })
    expect(report.succeeded).toBe(2)
    expect(report.failed).toBe(2)
    expect(report.firstError?.field).toBe('f1')
    expect(report.outcomes.map((outcome) => outcome.status)).toEqual(['created', 'failed', 'created', 'failed'])
    const notice = importNotice(report)
    expect(notice).toMatchObject({ kind: 'partial', added: 2, total: 4, failed: 2 })
  })

  test('缺失结果按内部错误计失败；全失败为 failed', async () => {
    const report = await runImport({ common, items: items(2), call: async () => ({ results: [] }), sleep: instant })
    expect(report.failed).toBe(2)
    expect(report.firstError?.code).toBe('internal')
    expect(importNotice(report).kind).toBe('failed')
  })

  test('不确定结果用同一 batchId 退避 2/4/8s 重发，用尽后放弃后续分块', async () => {
    const ids: string[] = []
    const delays: number[] = []
    const report = await runImport({
      common,
      items: items(600),
      call: async (params) => {
        ids.push(params.batchId)
        throw transport('unavailable')
      },
      sleep: async (ms) => {
        delays.push(ms)
      },
    })
    expect(ids).toHaveLength(4)
    expect(ids.every((id) => id === ids[0])).toBe(true)
    expect(delays).toEqual([2000, 4000, 8000])
    expect(report.failed).toBe(256)
    expect(report.notSubmitted).toBe(600 - 256)
    expect(report.cancelled).toBe(false)
  })

  test('Timeout 同样视为不确定并重发', async () => {
    let calls = 0
    const report = await runImport({
      common,
      items: items(2),
      call: async (params) => {
        calls += 1
        if (calls === 1) throw transport('timeout')
        return createdAll(params)
      },
      sleep: instant,
    })
    expect(calls).toBe(2)
    expect(report.succeeded).toBe(2)
  })

  test('重发成功只计一次，进度含重试状态且末次为完成', async () => {
    const ids: string[] = []
    const progress: ImportProgress[] = []
    const report = await runImport({
      common,
      items: items(3),
      call: async (params) => {
        ids.push(params.batchId)
        if (ids.length < 3) throw transport('unavailable')
        return createdAll(params)
      },
      sleep: instant,
      onProgress: (update) => progress.push(update),
    })
    expect(ids).toHaveLength(3)
    expect(ids.every((id) => id === ids[0])).toBe(true)
    expect(report.succeeded).toBe(3)
    expect(progress.some((update) => update.retry !== null)).toBe(true)
    expect(progress.find((update) => update.retry)?.retry).toEqual({ attempt: 1, max: 3, delaySeconds: 2 })
    expect(progress.at(-1)).toEqual({ total: 3, done: 3, failed: 0, retry: null })
  })

  test('非不确定的整块错误：该块全部失败，继续后续分块', async () => {
    let calls = 0
    const report = await runImport({
      common,
      items: items(300),
      call: async (params) => {
        calls += 1
        if (calls === 1) throw new RpcError({ code: -32000, message: 'bad', data: { code: 'invalidArgument', retryable: false } })
        return createdAll(params)
      },
      sleep: instant,
    })
    expect(calls).toBe(2)
    expect(report.failed).toBe(256)
    expect(report.succeeded).toBe(44)
  })

  test('开始前已取消：不发送，全部未提交', async () => {
    const controller = new AbortController()
    controller.abort()
    let calls = 0
    const report = await runImport({
      common,
      items: items(300),
      call: async (params) => {
        calls += 1
        return createdAll(params)
      },
      sleep: instant,
      signal: controller.signal,
    })
    expect(calls).toBe(0)
    expect(report.cancelled).toBe(true)
    expect(report.notSubmitted).toBe(300)
    expect(report.succeeded).toBe(0)
    expect(importNotice(report)).toMatchObject({ kind: 'cancelled', added: 0, total: 300 })
  })

  test('块之间取消：保留已确认任务，不再发后续分块', async () => {
    const controller = new AbortController()
    let calls = 0
    const report = await runImport({
      common,
      items: items(700),
      call: async (params) => {
        calls += 1
        return createdAll(params)
      },
      sleep: instant,
      signal: controller.signal,
      onProgress: (update) => {
        if (update.done >= 256) controller.abort()
      },
    })
    expect(calls).toBe(1)
    expect(report.cancelled).toBe(true)
    expect(report.succeeded).toBe(256)
    expect(report.notSubmitted).toBe(700 - 256)
  })

  test('块内取消：在 3s 收尾内等到在途结果，计数准确', async () => {
    const controller = new AbortController()
    const graces: number[] = []
    const report = await runImport({
      common,
      items: items(600),
      call: async (params) => {
        controller.abort()
        await Promise.resolve()
        await Promise.resolve()
        return createdAll(params)
      },
      sleep: (ms) => {
        graces.push(ms)
        return never<void>()
      },
      signal: controller.signal,
    })
    expect(graces).toEqual([3000])
    expect(report.cancelled).toBe(true)
    expect(report.succeeded).toBe(256)
    expect(report.notSubmitted).toBe(600 - 256)
  })

  test('块内取消：收尾到期仍无结果则整块计未提交', async () => {
    const controller = new AbortController()
    let calls = 0
    const report = await runImport({
      common,
      items: items(600),
      call: () => {
        calls += 1
        controller.abort()
        return never<DaemonCreateTasksResult>()
      },
      sleep: instant,
      signal: controller.signal,
    })
    expect(calls).toBe(1)
    expect(report.cancelled).toBe(true)
    expect(report.succeeded).toBe(0)
    expect(report.failed).toBe(0)
    expect(report.notSubmitted).toBe(600)
  })

  test('重试等待期间取消：不再重发', async () => {
    const controller = new AbortController()
    let calls = 0
    const report = await runImport({
      common,
      items: items(10),
      call: async () => {
        calls += 1
        throw transport('unavailable')
      },
      sleep: () => never<void>(),
      signal: controller.signal,
      onProgress: (update) => {
        if (update.retry) controller.abort()
      },
    })
    expect(calls).toBe(1)
    expect(report.cancelled).toBe(true)
    expect(report.notSubmitted).toBe(10)
    expect(report.failed).toBe(0)
  })

  test('非安全上下文（明文 HTTP 访问，无 crypto.randomUUID）仍能生成 batchId', async () => {
    // 浏览器只在安全上下文暴露 randomUUID；getRandomValues 不受限制。
    const own = Object.getOwnPropertyDescriptor(crypto, 'randomUUID')
    Object.defineProperty(crypto, 'randomUUID', { value: undefined, configurable: true, writable: true })
    try {
      const seen: string[] = []
      const report = await runImport({
        common,
        items: items(300),
        call: async (params) => {
          seen.push(params.batchId)
          return createdAll(params)
        },
        sleep: instant,
      })
      expect(report.succeeded).toBe(300)
      expect(seen).toHaveLength(2)
      expect(new Set(seen).size).toBe(2)
      expect(seen.every((batchId) => batchId.length > 0 && batchId.length <= 128)).toBe(true)
    } finally {
      if (own) Object.defineProperty(crypto, 'randomUUID', own)
      else Reflect.deleteProperty(crypto, 'randomUUID')
    }
  })
})

describe('buildBatch', () => {
  const options: DraftOptions = {
    saveDir: '/dl',
    segments: 4,
    cookies: '',
    referrer: 'https://site.example/video/1',
    proxyUrl: '',
    userAgent: '',
    queueId: 'main',
    checksum: 'sha256=abc',
    ignoreTlsErrors: false,
    headers: {},
    startPaused: false,
    rename: 'renamed.bin',
    httpUser: 'u',
    httpPassword: 'p',
    saveSiteAuth: true,
  }

  test('单条：重命名 / 校验值进条目，认证进 common', () => {
    const { common: shared, items: built } = buildBatch([{ url: 'https://a/x', fileName: 'out.bin', checksum: 'md5=1' }], options)
    expect(built).toEqual([{ url: 'https://a/x', fileName: 'renamed.bin', checksum: 'sha256=abc' }])
    expect(shared).toMatchObject({ url: '', fileName: '', checksum: '', saveDir: '/dl', httpUser: 'u', httpPassword: 'p', saveSiteAuth: true, referrer: 'https://site.example/video/1' })
  })

  test('多条：只用条目自带选项，common 不带认证', () => {
    const { common: shared, items: built } = buildBatch(
      [
        { url: 'https://a/1', fileName: 'one', checksum: 'md5=1' },
        { url: 'https://a/2', fileName: '', checksum: '' },
      ],
      options,
    )
    expect(built).toEqual([
      { url: 'https://a/1', fileName: 'one', checksum: 'md5=1' },
      { url: 'https://a/2', fileName: '', checksum: '' },
    ])
    expect(shared).toMatchObject({ httpUser: '', httpPassword: '', saveSiteAuth: false, queueId: 'main', segments: 4 })
  })
})
