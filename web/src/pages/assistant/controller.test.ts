import { describe, expect, test } from 'bun:test'
import { RpcError } from '../../lib/rpc/error'
import type {
  AssistantAttachmentParams,
  AssistantAttachmentResult,
  AssistantExportParams,
  AssistantExportResult,
  AssistantRenameParams,
  AssistantAskParams,
  AssistantConfigDto,
  AssistantIssueDto,
  AssistantIssueParams,
  AssistantMessageDto,
  AssistantRateParams,
  AssistantRunDto,
  AssistantSessionDetail,
  AssistantSessionDto,
  AssistantSessionPage,
  AssistantStateDto,
  ApplicationErrorCode,
  ErrorReason,
} from '../../lib/rpc/protocol'
import { AssistantController, INITIAL_ENV } from './controller'
import type { AssistantEnv, AssistantPort } from './controller'
import { SESSION_PAGE_SIZE, gate } from './model'

// ───────────────────────── 替身 ─────────────────────────

interface Deferred<T> {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (error: unknown) => void
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

/** 排空已排队的微任务（promise 回调链）；不依赖真实时间。 */
async function flush(): Promise<void> {
  for (let index = 0; index < 20; index += 1) await Promise.resolve()
}

const config = (extra: Partial<AssistantConfigDto> = {}): AssistantConfigDto => ({
  available: true,
  maxMessageChars: 4000,
  imagesSupported: false,
  maxImages: 0,
  maxImageBytes: 0,
  dailyLimit: 20,
  usedToday: 3,
  toolsEnabled: true,
  ...extra,
})

const state = (runs: AssistantRunDto[] = [], extra: Partial<AssistantStateDto> = {}): AssistantStateDto => ({
  config: config(),
  loading: false,
  lastErrorReason: null,
  lastErrorMessage: null,
  visible: true,
  runs,
  ...extra,
})

const run = (sessionId: string, status: AssistantRunDto['status'], extra: Partial<AssistantRunDto> = {}): AssistantRunDto => ({
  sessionId,
  runId: `run-${sessionId}`,
  status,
  question: 'q',
  startedAtUnixMs: 1_000,
  items: [],
  ...extra,
})

const session = (id: string, extra: Partial<AssistantSessionDto> = {}): AssistantSessionDto => ({
  id,
  channel: 'web',
  kind: 'ask',
  title: `title ${id}`,
  messageCount: 2,
  createdAt: '2026-11-27T01:00:00Z',
  updatedAt: '2026-11-27T01:00:00Z',
  ...extra,
})

const message = (id: string, seq: number, role: string): AssistantMessageDto => ({
  id,
  runId: 'r1',
  seq,
  role,
  content: id,
  createdAt: '2026-11-27T01:00:00Z',
})

const issue = (id: string, status = 'draft'): AssistantIssueDto => ({
  id,
  kind: 'bug',
  title: 'title',
  body: 'body',
  status,
  createdAt: '',
  updatedAt: '',
})

const rpcError = (code: ApplicationErrorCode, reason?: ErrorReason) =>
  new RpcError({ code: -32000, message: 'failed', data: { code, retryable: false, ...(reason ? { reason } : {}) } })

/** 每个方法一个待决请求队列，测试里按需兑现 / 拒绝。 */
class FakePort implements AssistantPort {
  readonly calls: { method: string; params: unknown }[] = []
  readonly pending = {
    get: [] as Deferred<AssistantStateDto>[],
    refresh: [] as Deferred<AssistantStateDto>[],
    sessions: [] as Deferred<AssistantSessionPage>[],
    session: [] as Deferred<AssistantSessionDetail>[],
    deleteSession: [] as Deferred<{ ok: true }>[],
    clearSessions: [] as Deferred<{ cleared: number }>[],
    ask: [] as Deferred<{ sessionId: string }>[],
    cancel: [] as Deferred<{ ok: true }>[],
    confirm: [] as Deferred<{ ok: true }>[],
    rate: [] as Deferred<{ ok: true }>[],
    confirmIssue: [] as Deferred<AssistantIssueDto>[],
    discardIssue: [] as Deferred<{ ok: true }>[],
    renameSession: [] as Deferred<AssistantSessionDto>[],
    exportSession: [] as Deferred<AssistantExportResult>[],
    attachment: [] as Deferred<AssistantAttachmentResult>[],
  }

  private enqueue<K extends keyof FakePort['pending'], T>(method: K, params: unknown): Promise<T> {
    this.calls.push({ method, params })
    const entry = deferred<T>()
    ;(this.pending[method] as unknown as Deferred<T>[]).push(entry)
    return entry.promise
  }

  count(method: keyof FakePort['pending']): number {
    return this.calls.filter((call) => call.method === method).length
  }

  last(method: keyof FakePort['pending']): unknown {
    return this.calls.filter((call) => call.method === method).at(-1)?.params
  }

  get = () => this.enqueue<'get', AssistantStateDto>('get', undefined)
  refresh = () => this.enqueue<'refresh', AssistantStateDto>('refresh', undefined)
  sessions = (params?: unknown) => this.enqueue<'sessions', AssistantSessionPage>('sessions', params)
  session = (params: unknown) => this.enqueue<'session', AssistantSessionDetail>('session', params)
  deleteSession = (params: unknown) => this.enqueue<'deleteSession', { ok: true }>('deleteSession', params)
  clearSessions = () => this.enqueue<'clearSessions', { cleared: number }>('clearSessions', undefined)
  ask = (params: AssistantAskParams) => this.enqueue<'ask', { sessionId: string }>('ask', params)
  cancel = (params: unknown) => this.enqueue<'cancel', { ok: true }>('cancel', params)
  confirm = (params: unknown) => this.enqueue<'confirm', { ok: true }>('confirm', params)
  rate = (params: AssistantRateParams) => this.enqueue<'rate', { ok: true }>('rate', params)
  confirmIssue = (params: AssistantIssueParams) => this.enqueue<'confirmIssue', AssistantIssueDto>('confirmIssue', params)
  discardIssue = (params: AssistantIssueParams) => this.enqueue<'discardIssue', { ok: true }>('discardIssue', params)
  renameSession = (params: AssistantRenameParams) => this.enqueue<'renameSession', AssistantSessionDto>('renameSession', params)
  exportSession = (params: AssistantExportParams) => this.enqueue<'exportSession', AssistantExportResult>('exportSession', params)
  attachment = (params: AssistantAttachmentParams) => this.enqueue<'attachment', AssistantAttachmentResult>('attachment', params)
}

const env = (extra: Partial<AssistantEnv> = {}): AssistantEnv => ({ state: state(), signedIn: true, stale: false, locale: 'zh-CN', ...extra })

function setup(extra: Partial<AssistantEnv> = {}) {
  const port = new FakePort()
  const controller = new AssistantController(port, INITIAL_ENV)
  controller.sync(env(extra))
  return { port, controller, view: () => controller.getSnapshot() }
}

/** 兑现首个待决请求。 */
function settle<T>(queue: Deferred<T>[], value: NoInfer<T>): void {
  queue.shift()?.resolve(value)
}

const page = (items: AssistantSessionDto[], total = items.length): AssistantSessionPage => ({ items, total })

// ───────────────────────── 用例 ─────────────────────────

describe('loading', () => {
  test('ensureLoaded fetches the config once, then the sessions once the gate is open', async () => {
    const { port, controller, view } = setup({ state: state([], { config: null }) })
    controller.ensureLoaded()
    controller.ensureLoaded()
    expect(port.count('get')).toBe(1)
    expect(view().configLoading).toBe(true)
    // 配置未到：门禁还没开，不拉会话列表。
    expect(port.count('sessions')).toBe(0)

    settle(port.pending.get, state())
    await flush()
    expect(view().configLoading).toBe(false)
    expect(view().configFailed).toBe(false)

    // 配置经推送进入快照 → 门禁开放 → 再次 ensureLoaded 拉会话。
    controller.sync(env({ state: state() }))
    controller.ensureLoaded()
    controller.ensureLoaded()
    expect(port.count('get')).toBe(1)
    expect(port.count('sessions')).toBe(1)
    expect(port.last('sessions')).toEqual({ limit: SESSION_PAGE_SIZE, offset: 0, query: null })
    expect(view().sessions.load).toBe('loading')

    settle(port.pending.sessions, page([session('a'), session('b')], 5))
    await flush()
    expect(view().sessions).toEqual({ items: [session('a'), session('b')], total: 5, load: 'ready', moreLoading: false, query: '' })
  })

  test('nothing loads while signed out or disconnected', () => {
    const signedOut = setup({ signedIn: false })
    signedOut.controller.ensureLoaded()
    expect(signedOut.port.calls).toEqual([])
    const stale = setup({ stale: true })
    stale.controller.ensureLoaded()
    expect(stale.port.calls).toEqual([])
  })

  test('a failed config fetch turns the gate into a service error until a push brings the config', async () => {
    const { port, controller, view } = setup({ state: state([], { config: null }) })
    controller.ensureLoaded()
    port.pending.get[0]?.reject(rpcError('unavailable'))
    await flush()
    expect(view().configFailed).toBe(true)
    expect(gate(true, view().env.state, view().configFailed).kind).toBe('serviceError')

    // 重试：成功后失败标记清掉。
    controller.fetchConfig(true)
    expect(port.count('refresh')).toBe(1)
    settle(port.pending.refresh, state())
    await flush()
    expect(view().configFailed).toBe(false)
  })

  test('a config push clears an earlier failure', async () => {
    const { port, controller, view } = setup({ state: state([], { config: null }) })
    controller.ensureLoaded()
    port.pending.get[0]?.reject(rpcError('unavailable'))
    await flush()
    controller.sync(env({ state: state() }))
    expect(view().configFailed).toBe(false)
  })

  test('the session list reload keeps already loaded pages', async () => {
    const { port, controller } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page(Array.from({ length: 45 }, (_, index) => session(`s${index}`)), 60))
    await flush()
    controller.reloadSessions()
    expect(port.last('sessions')).toEqual({ limit: 45, offset: 0, query: null })
  })

  test('only the newest session list response counts', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('old')]))
    await flush()
    expect(view().sessions.load).toBe('loading')
    settle(port.pending.sessions, page([session('new')]))
    await flush()
    expect(view().sessions.items.map((item) => item.id)).toEqual(['new'])
  })

  test('load more appends without duplicates and ignores a page from before a reload', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a'), session('b')], 4))
    await flush()

    controller.loadMoreSessions()
    controller.loadMoreSessions()
    expect(port.count('sessions')).toBe(2)
    expect(port.last('sessions')).toEqual({ limit: SESSION_PAGE_SIZE, offset: 2, query: null })
    settle(port.pending.sessions, page([session('b'), session('c')], 4))
    await flush()
    expect(view().sessions.items.map((item) => item.id)).toEqual(['a', 'b', 'c'])
    expect(view().sessions.moreLoading).toBe(false)

    controller.loadMoreSessions()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('stale-page')], 4))
    await flush()
    expect(view().sessions.items.map((item) => item.id)).toEqual(['a', 'b', 'c'])
    expect(view().sessions.moreLoading).toBe(false)
  })

  test('no more button once everything is loaded', async () => {
    const { port, controller } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a')], 1))
    await flush()
    controller.loadMoreSessions()
    expect(port.count('sessions')).toBe(1)
  })
})

describe('account changes', () => {
  test('signing out clears every cache and drops late responses', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a')]))
    await flush()
    controller.selectSession('a')
    controller.toggleArguments('c1')
    controller.requestDelete('a')
    controller.requestClear()

    controller.reloadSessions()
    controller.sync(env({ signedIn: false, state: state() }))
    expect(view().sessions.items).toEqual([])
    expect(view().sessions.load).toBe('idle')
    expect(view().current).toBeNull()
    expect(view().detail.load).toBe('idle')
    expect(view().argsOpen.size).toBe(0)
    expect(view().deletePending).toBeNull()
    expect(view().clearPending).toBe(false)
    expect(view().clearing).toBe(false)

    // 登出前发出的响应不再落地。
    settle(port.pending.sessions, page([session('late')]))
    settle(port.pending.session, { session: session('a'), messages: [message('m', 1, 'user')] })
    await flush()
    expect(view().sessions.items).toEqual([])
    expect(view().detail.messages).toEqual([])
  })

  test('signing in again starts from scratch and refetches', async () => {
    const { port, controller } = setup({ signedIn: false })
    controller.ensureLoaded()
    expect(port.calls).toEqual([])
    controller.sync(env({ signedIn: true, state: state([], { config: null }) }))
    controller.ensureLoaded()
    expect(port.count('get')).toBe(1)
  })

  test('a config response from the previous account does not flag the new one', async () => {
    const { port, controller, view } = setup({ state: state([], { config: null }) })
    controller.ensureLoaded()
    controller.sync(env({ signedIn: false, state: state([], { config: null }) }))
    controller.sync(env({ signedIn: true, state: state([], { config: null }) }))
    port.pending.get[0]?.reject(rpcError('unavailable'))
    await flush()
    expect(view().configFailed).toBe(false)
  })
})

describe('current session', () => {
  test('selecting a session loads its history and forces a scroll', async () => {
    const { port, controller, view } = setup()
    const before = view().scrollToken
    controller.selectSession('a')
    expect(view().current).toBe('a')
    expect(view().detail.load).toBe('loading')
    expect(view().scrollToken).toBe(before + 1)
    expect(port.last('session')).toEqual({ sessionId: 'a' })

    settle(port.pending.session, { session: session('a'), messages: [message('m1', 1, 'user')], issues: [issue('i1')] })
    await flush()
    expect(view().detail.load).toBe('ready')
    expect(view().detail.messages).toHaveLength(1)
    expect(view().detail.issues).toHaveLength(1)
    // 首次载入历史也滚到底。
    expect(view().scrollToken).toBe(before + 2)

    // 再次选中同一会话：不重复请求。
    controller.selectSession('a')
    expect(port.count('session')).toBe(1)
  })

  test('a late history of a previous selection is ignored', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    controller.selectSession('b')
    settle(port.pending.session, { session: session('a'), messages: [message('from-a', 1, 'user')] })
    await flush()
    expect(view().detail.load).toBe('loading')
    expect(view().detail.messages).toEqual([])
    settle(port.pending.session, { session: session('b'), messages: [message('from-b', 1, 'user')] })
    await flush()
    expect(view().detail.messages.map((item) => item.id)).toEqual(['from-b'])
  })

  test('a failed history can be retried', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    port.pending.session[0]?.reject(rpcError('unavailable'))
    await flush()
    expect(view().detail.load).toBe('failed')
    controller.loadDetail('a')
    expect(view().detail.load).toBe('loading')
  })

  test('new chat leaves the session and resets the transient state', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: [message('m', 1, 'user')] })
    await flush()
    controller.requestDelete('a')
    controller.newChat()
    expect(view().current).toBeNull()
    expect(view().detail.messages).toEqual([])
    expect(view().deletePending).toBeNull()
    // 旧会话的迟到响应不会把它带回来。
    controller.selectSession('b')
    controller.newChat()
    settle(port.pending.session, { session: session('b'), messages: [message('late', 1, 'user')] })
    await flush()
    expect(view().detail.messages).toEqual([])
  })

  test('selecting a session drops the local issue overrides', () => {
    const { controller, view } = setup()
    controller.selectSession('a')
    expect(view().issueOverrides.size).toBe(0)
  })

  test('the kind can only change on a new chat', () => {
    const { controller, view } = setup()
    controller.setKind('feedback')
    expect(view().kind).toBe('feedback')
    controller.selectSession('a')
    controller.setKind('ask')
    expect(view().kind).toBe('feedback')
  })
})

describe('finished runs', () => {
  test('a run that ends reloads the sessions and the open conversation', async () => {
    const running = state([run('a', 'running')])
    const { port, controller } = setup({ state: running })
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: [] })
    await flush()
    const sessionsBefore = port.count('sessions')
    const detailBefore = port.count('session')

    controller.sync(env({ state: state([run('a', 'completed')]) }))
    expect(port.count('sessions')).toBe(sessionsBefore + 1)
    expect(port.count('session')).toBe(detailBefore + 1)
  })

  test('a run that ends in another conversation only reloads the list', async () => {
    const { port, controller } = setup({ state: state([run('b', 'running')]) })
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: [] })
    await flush()
    const detailBefore = port.count('session')
    controller.sync(env({ state: state([run('b', 'failed')]) }))
    expect(port.count('sessions')).toBe(1)
    expect(port.count('session')).toBe(detailBefore)
  })

  test('a run trimmed out of the state counts as finished', () => {
    const { port, controller } = setup({ state: state([run('a', 'running')]) })
    controller.sync(env({ state: state([]) }))
    expect(port.count('sessions')).toBe(1)
  })

  test('streaming pushes that keep the run active reload nothing', () => {
    const { port, controller } = setup({ state: state([run('a', 'running')]) })
    controller.sync(env({ state: state([run('a', 'running', { items: [{ type: 'text', text: 'hi' }] })]) }))
    expect(port.calls).toEqual([])
  })
})

describe('send', () => {
  test('a new chat creates the session, shows it and reloads the list', async () => {
    const { port, controller, view } = setup()
    controller.setKind('feedback')
    const before = view().scrollToken
    const result = controller.send('hello', ['data:image/png;base64,AA=='])
    expect(view().sending).toBe(true)
    expect(port.last('ask')).toEqual({
      sessionId: null,
      kind: 'feedback',
      channel: 'web',
      text: 'hello',
      images: ['data:image/png;base64,AA=='],
      locale: 'zh-CN',
    })
    settle(port.pending.ask, { sessionId: 'new' })
    expect(await result).toBe(true)
    expect(view().sending).toBe(false)
    expect(view().current).toBe('new')
    // 新会话没有历史：直接以空历史起步，不去云端读。
    expect(view().detail.load).toBe('ready')
    expect(port.count('session')).toBe(0)
    expect(view().scrollToken).toBe(before + 1)
    expect(port.count('sessions')).toBe(1)
  })

  test('an existing conversation keeps its kind and id', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a', { kind: 'feedback' }), messages: [message('m', 1, 'user')] })
    await flush()
    const result = controller.send('more', [])
    expect(port.last('ask')).toMatchObject({ sessionId: 'a', kind: 'feedback', channel: 'web', text: 'more' })
    settle(port.pending.ask, { sessionId: 'a' })
    expect(await result).toBe(true)
    expect(view().current).toBe('a')
    // 已有会话的历史不被清空。
    expect(view().detail.messages).toHaveLength(1)
  })

  test('send is refused while disconnected, already sending or running', async () => {
    const stale = setup({ stale: true })
    expect(await stale.controller.send('x', [])).toBe(false)
    expect(stale.port.count('ask')).toBe(0)

    const { port, controller } = setup()
    const first = controller.send('x', [])
    expect(await controller.send('y', [])).toBe(false)
    expect(port.count('ask')).toBe(1)
    settle(port.pending.ask, { sessionId: 's' })
    await first

    controller.sync(env({ state: state([run('s', 'running')]) }))
    expect(await controller.send('z', [])).toBe(false)
    expect(port.count('ask')).toBe(1)
  })

  test('failures are classified and the input is kept', async () => {
    const cases: [Error, string][] = [
      [rpcError('unauthorized'), 'signedOut'],
      [rpcError('conflict'), 'busy'],
      [rpcError('invalidArgument'), 'rejected'],
      [rpcError('unavailable'), 'unavailable'],
      [rpcError('internal', 'assistantQuotaExceeded'), 'quotaExceeded'],
      [rpcError('internal', 'rateLimited'), 'rateLimited'],
      [new RpcError({ code: -32001, message: 'closed' }), 'failed'],
    ]
    for (const [error, expected] of cases) {
      const { port, controller, view } = setup()
      const result = controller.send('x', [])
      port.pending.ask[0]?.reject(error)
      expect(await result).toBe(false)
      expect(view().sending).toBe(false)
      expect(view().sendFailure).toBe(expected as never)
    }
  })

  test('a quota or availability failure refreshes the configuration', async () => {
    for (const [error, expectedFailure] of [
      [rpcError('internal', 'assistantQuotaExceeded'), 'quotaExceeded'],
      [rpcError('unsupported'), 'notOpen'],
    ] as const) {
      const { port, controller, view } = setup()
      const result = controller.send('x', [])
      port.pending.ask[0]?.reject(error)
      await result
      expect(view().sendFailure).toBe(expectedFailure)
      expect(port.count('refresh')).toBe(1)
    }
    const { port, controller } = setup()
    const result = controller.send('x', [])
    port.pending.ask[0]?.reject(rpcError('conflict'))
    await result
    expect(port.count('refresh')).toBe(0)

    // 限频只是稍后再试：不重新取配置。
    const limited = setup()
    const limitedResult = limited.controller.send('x', [])
    limited.port.pending.ask[0]?.reject(rpcError('internal', 'rateLimited'))
    await limitedResult
    expect(limited.view().sendFailure).toBe('rateLimited')
    expect(limited.port.count('refresh')).toBe(0)
  })

  test('a session limit rejection marks the open session as reached and reloads its detail', async () => {
    const { port, controller, view } = setup({ state: state([], { config: config({ sessionLimit: 10 }) }) })
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: [message('m', 1, 'user')], questionsUsed: 7 })
    await flush()
    expect(view().detail.questionsUsed).toBe(7)

    const result = controller.send('x', [])
    port.pending.ask[0]?.reject(rpcError('conflict', 'assistantSessionLimit'))
    expect(await result).toBe(false)
    expect(view().sendFailure).toBe('sessionLimit')
    expect(view().detail.questionsUsed).toBe(10)
    // 以服务端口径重新读取详情与配置。
    expect(port.count('session')).toBe(2)
    expect(port.count('refresh')).toBe(1)
    settle(port.pending.session, { session: session('a'), messages: [message('m', 1, 'user')], questionsUsed: 10 })
    await flush()
    expect(view().detail.questionsUsed).toBe(10)
  })

  test('the next attempt clears the previous failure', async () => {
    const { port, controller, view } = setup()
    const failed = controller.send('x', [])
    port.pending.ask.shift()?.reject(rpcError('unavailable'))
    await failed
    expect(view().sendFailure).toBe('unavailable')
    const retry = controller.send('x', [])
    expect(view().sendFailure).toBeNull()
    settle(port.pending.ask, { sessionId: 's' })
    await retry
  })

  test('an answer arriving after sign-out is dropped', async () => {
    const { port, controller, view } = setup()
    const result = controller.send('x', [])
    controller.sync(env({ signedIn: false, state: state() }))
    settle(port.pending.ask, { sessionId: 'ghost' })
    expect(await result).toBe(false)
    expect(view().current).toBeNull()
  })
})

describe('stop', () => {
  test('cancels the running session once', async () => {
    const { port, controller, view } = setup({ state: state([run('a', 'running')]) })
    controller.selectSession('a')
    controller.stop()
    controller.stop()
    expect(port.count('cancel')).toBe(1)
    expect(port.last('cancel')).toEqual({ sessionId: 'a' })
    expect(view().cancelling).toBe(true)
    settle(port.pending.cancel, { ok: true })
    await flush()
    expect(view().cancelling).toBe(false)
    expect(view().actionFailure).toBeNull()
  })

  test('does nothing without a running answer in the open conversation', () => {
    const { port, controller } = setup({ state: state([run('other', 'running')]) })
    controller.selectSession('a')
    controller.stop()
    expect(port.count('cancel')).toBe(0)
  })

  test('a failed cancel shows the action failure', async () => {
    const { port, controller, view } = setup({ state: state([run('a', 'running')]) })
    controller.selectSession('a')
    controller.stop()
    port.pending.cancel[0]?.reject(rpcError('unavailable'))
    await flush()
    expect(view().cancelling).toBe(false)
    expect(view().actionFailure).toBe('failed')
  })
})

describe('tool confirmation', () => {
  const pendingCall = (requestId: string) => ({
    callId: `call-${requestId}`,
    name: 'settings_patch',
    kind: 'client',
    status: 'running' as const,
    requestId,
    effect: 'write' as const,
    confirmation: 'pending' as const,
  })
  const withPending = (...ids: string[]) =>
    state([run('a', 'running', { items: ids.map((id) => ({ type: 'tool' as const, call: pendingCall(id) })) })])

  test('a request is sent once and stays busy until the push settles it', async () => {
    const { port, controller, view } = setup({ state: withPending('r1') })
    controller.confirmTool('r1', true)
    controller.confirmTool('r1', false)
    expect(port.count('confirm')).toBe(1)
    expect(port.last('confirm')).toEqual({ requestId: 'r1', approved: true })
    expect(view().confirming.has('r1')).toBe(true)

    settle(port.pending.confirm, { ok: true })
    await flush()
    // 成功后按钮保持忙碌，直到推送把请求改成已处理。
    expect(view().confirming.has('r1')).toBe(true)
    controller.sync(env({ state: state([run('a', 'running')]) }))
    expect(view().confirming.has('r1')).toBe(false)
  })

  test('only settled requests leave the busy set', () => {
    const { controller, view } = setup({ state: withPending('r1', 'r2') })
    controller.confirmTool('r1', true)
    controller.confirmTool('r2', false)
    controller.sync(env({ state: withPending('r2') }))
    expect([...view().confirming]).toEqual(['r2'])
  })

  test('a failure releases the buttons and reports', async () => {
    const { port, controller, view } = setup({ state: withPending('r1') })
    controller.confirmTool('r1', true)
    port.pending.confirm[0]?.reject(rpcError('unavailable'))
    await flush()
    expect(view().confirming.has('r1')).toBe(false)
    expect(view().actionFailure).toBe('failed')
    // 可以重试。
    controller.confirmTool('r1', true)
    expect(port.count('confirm')).toBe(2)
    expect(view().actionFailure).toBeNull()
  })

  test('an expired approval window is reported on its own', async () => {
    const { port, controller, view } = setup({ state: withPending('r1') })
    controller.confirmTool('r1', true)
    port.pending.confirm[0]?.reject(rpcError('conflict', 'assistantConfirmExpired'))
    await flush()
    expect(view().confirming.has('r1')).toBe(false)
    expect(view().actionFailure).toBe('confirmExpired')
  })

  test('refused while disconnected', () => {
    const { port, controller } = setup({ state: withPending('r1'), stale: true })
    controller.confirmTool('r1', true)
    expect(port.count('confirm')).toBe(0)
  })
})

describe('rating', () => {
  test('rating a message sends the message id and remembers the result', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    controller.rate('m1', null, 'up')
    expect(port.last('rate')).toEqual({ sessionId: 'a', messageId: 'm1', rating: 'up', comment: null })
    expect(view().ratingBusy.has('m1')).toBe(true)
    settle(port.pending.rate, { ok: true })
    await flush()
    expect(view().ratingBusy.has('m1')).toBe(false)
    expect(view().ratingOverrides.get('m1')).toBe('up')
  })

  test('choosing the current rating again clears it; the other one replaces it', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    controller.rate('m1', 'up', 'up')
    expect(port.last('rate')).toMatchObject({ rating: null })
    settle(port.pending.rate, { ok: true })
    await flush()
    expect(view().ratingOverrides.get('m1')).toBeNull()
    expect(view().ratingOverrides.has('m1')).toBe(true)

    controller.rate('m1', 'up', 'down')
    expect(port.last('rate')).toMatchObject({ rating: 'down' })
  })

  test('ratings of different messages are independent; the same message is not sent twice', () => {
    const { port, controller } = setup()
    controller.selectSession('a')
    controller.rate('m1', null, 'up')
    controller.rate('m1', null, 'down')
    controller.rate('m2', null, 'down')
    expect(port.count('rate')).toBe(2)
  })

  test('a failure keeps the previous rating and reports', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    controller.rate('m1', 'up', 'down')
    port.pending.rate[0]?.reject(rpcError('unavailable'))
    await flush()
    expect(view().ratingOverrides.has('m1')).toBe(false)
    expect(view().ratingBusy.has('m1')).toBe(false)
    expect(view().actionFailure).toBe('failed')
  })

  test('the new rating shows at once and a failure rolls back to the earlier local rating', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    controller.rate('m1', null, 'up')
    expect(view().ratingOverrides.get('m1')).toBe('up')
    settle(port.pending.rate, { ok: true })
    await flush()

    controller.rate('m1', 'up', 'down')
    expect(view().ratingOverrides.get('m1')).toBe('down')
    port.pending.rate[0]?.reject(rpcError('unavailable'))
    await flush()
    expect(view().ratingOverrides.get('m1')).toBe('up')
    expect(view().actionFailure).toBe('failed')
  })

  test('needs a saved conversation and a connection', () => {
    const fresh = setup()
    fresh.controller.rate('m1', null, 'up')
    expect(fresh.port.count('rate')).toBe(0)
    const stale = setup({ stale: true })
    stale.controller.selectSession('a')
    stale.controller.rate('m1', null, 'up')
    expect(stale.port.count('rate')).toBe(0)
  })

  test('ratings survive a history reload', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    controller.rate('m1', null, 'up')
    settle(port.pending.rate, { ok: true })
    await flush()
    controller.loadDetail('a')
    settle(port.pending.session, { session: session('a'), messages: [] })
    await flush()
    expect(view().ratingOverrides.get('m1')).toBe('up')
  })
})

describe('issues', () => {
  test('confirming stores the server copy and closes the editor', async () => {
    const { port, controller, view } = setup({
      state: state([run('a', 'completed', { items: [{ type: 'issueDraft', issue: issue('i1') }] })]),
    })
    controller.editIssue('i1')
    expect(view().issueEditing).toBe('i1')
    controller.confirmIssue('i1', { title: 'new title', body: 'new body' })
    controller.confirmIssue('i1')
    expect(port.count('confirmIssue')).toBe(1)
    expect(port.last('confirmIssue')).toEqual({ issueId: 'i1', title: 'new title', body: 'new body' })
    expect(view().issueBusy.has('i1')).toBe(true)

    settle(port.pending.confirmIssue, issue('i1', 'pending_review'))
    await flush()
    expect(view().issueBusy.has('i1')).toBe(false)
    expect(view().issueEditing).toBeNull()
    expect(view().issueOverrides.get('i1')?.status).toBe('pending_review')
  })

  test('confirming without edits sends no title or body', () => {
    const { port, controller } = setup()
    controller.confirmIssue('i1')
    expect(port.last('confirmIssue')).toEqual({ issueId: 'i1', title: null, body: null })
  })

  test('discarding marks the local copy as discarded', async () => {
    const { port, controller, view } = setup({
      state: state([run('a', 'completed', { items: [{ type: 'issueDraft', issue: issue('i1') }] })]),
    })
    controller.discardIssue('i1')
    expect(port.last('discardIssue')).toEqual({ issueId: 'i1' })
    settle(port.pending.discardIssue, { ok: true })
    await flush()
    expect(view().issueOverrides.get('i1')?.status).toBe('discarded')
    expect(controller.issue('i1')?.status).toBe('discarded')
  })

  test('failures release the card and report', async () => {
    const { port, controller, view } = setup()
    controller.confirmIssue('i1')
    port.pending.confirmIssue[0]?.reject(rpcError('unavailable'))
    await flush()
    expect(view().issueBusy.has('i1')).toBe(false)
    expect(view().actionFailure).toBe('failed')
    expect(view().issueOverrides.size).toBe(0)
  })

  test('an unknown issue cannot be edited; editing can be cancelled', () => {
    const { controller, view } = setup()
    controller.editIssue('missing')
    expect(view().issueEditing).toBeNull()
    controller.confirmIssue('i1')
    controller.cancelIssueEdit()
    expect(view().issueEditing).toBeNull()
  })

  test('issues are found in the live run first, then the history', async () => {
    const { port, controller } = setup({ state: state([run('a', 'completed', { items: [{ type: 'issueDraft', issue: issue('live') }] })]) })
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: [], issues: [issue('history')] })
    await flush()
    expect(controller.issue('live')?.id).toBe('live')
    expect(controller.issue('history')?.id).toBe('history')
    expect(controller.issue('nope')).toBeUndefined()
  })
})

describe('delete', () => {
  test('requires a second step and removes the row', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a'), session('b')], 7))
    await flush()
    controller.requestDelete('a')
    expect(view().deletePending).toBe('a')
    controller.cancelDelete('b')
    expect(view().deletePending).toBe('a')
    controller.cancelDelete('a')
    expect(view().deletePending).toBeNull()

    controller.requestDelete('a')
    controller.deleteSession('a')
    expect(view().deletePending).toBeNull()
    expect(port.last('deleteSession')).toEqual({ sessionId: 'a' })
    settle(port.pending.deleteSession, { ok: true })
    await flush()
    expect(view().sessions.items.map((item) => item.id)).toEqual(['b'])
    expect(view().sessions.total).toBe(6)
  })

  test('deleting the open conversation returns to a new chat', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a')], 1))
    await flush()
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: [message('m', 1, 'user')] })
    await flush()
    controller.deleteSession('a')
    settle(port.pending.deleteSession, { ok: true })
    await flush()
    expect(view().current).toBeNull()
    expect(view().detail.messages).toEqual([])
    expect(view().sessions.total).toBe(0)
  })

  test('deleting another conversation keeps the open one', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a'), session('b')]))
    await flush()
    controller.selectSession('a')
    controller.deleteSession('b')
    settle(port.pending.deleteSession, { ok: true })
    await flush()
    expect(view().current).toBe('a')
  })

  test('a failure keeps the row and reports', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a')]))
    await flush()
    controller.deleteSession('a')
    port.pending.deleteSession[0]?.reject(rpcError('unavailable'))
    await flush()
    expect(view().sessions.items).toHaveLength(1)
    expect(view().actionFailure).toBe('failed')
  })

  test('refused while disconnected', () => {
    const { port, controller } = setup({ stale: true })
    controller.deleteSession('a')
    expect(port.count('deleteSession')).toBe(0)
  })

  test('requesting a delete drops a pending clear and the other way round', () => {
    const { controller, view } = setup()
    controller.requestClear()
    controller.requestDelete('a')
    expect(view().clearPending).toBe(false)
    expect(view().deletePending).toBe('a')
    controller.requestClear()
    expect(view().deletePending).toBeNull()
    expect(view().clearPending).toBe(true)
  })
})

describe('clear all', () => {
  test('requires a second step and empties the list', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a'), session('b')], 7))
    await flush()
    controller.requestClear()
    expect(view().clearPending).toBe(true)
    controller.cancelClear()
    expect(view().clearPending).toBe(false)
    expect(port.count('clearSessions')).toBe(0)

    controller.requestClear()
    controller.clearSessions()
    expect(view().clearPending).toBe(false)
    expect(view().clearing).toBe(true)
    expect(port.count('clearSessions')).toBe(1)
    // 清空期间不重复发出。
    controller.clearSessions()
    expect(port.count('clearSessions')).toBe(1)

    settle(port.pending.clearSessions, { cleared: 7 })
    await flush()
    expect(view().clearing).toBe(false)
    expect(view().sessions.items).toEqual([])
    expect(view().sessions.total).toBe(0)
    expect(view().sessions.load).toBe('ready')
    expect(view().actionFailure).toBeNull()
  })

  test('returns the open conversation to a new chat and drops in-flight responses', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a'), session('b')]))
    await flush()
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: [message('m', 1, 'user')] })
    await flush()
    controller.reloadSessions()
    controller.selectSession('b')
    controller.clearSessions()
    settle(port.pending.clearSessions, { cleared: 2 })
    await flush()
    expect(view().current).toBeNull()
    expect(view().detail.messages).toEqual([])
    expect(view().sessions.items).toEqual([])

    // 清空前发出的列表 / 历史响应不再落地。
    settle(port.pending.sessions, page([session('late')]))
    settle(port.pending.session, { session: session('b'), messages: [message('late', 1, 'user')] })
    await flush()
    expect(view().sessions.items).toEqual([])
    expect(view().sessions.total).toBe(0)
    expect(view().current).toBeNull()
    expect(view().detail.messages).toEqual([])
  })

  test('a failure keeps the list and reports', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a')]))
    await flush()
    controller.selectSession('a')
    controller.clearSessions()
    port.pending.clearSessions[0]?.reject(rpcError('unavailable'))
    await flush()
    expect(view().clearing).toBe(false)
    expect(view().sessions.items).toHaveLength(1)
    expect(view().current).toBe('a')
    expect(view().actionFailure).toBe('failed')
  })

  test('refused while disconnected', () => {
    const { port, controller } = setup({ stale: true })
    controller.clearSessions()
    expect(port.count('clearSessions')).toBe(0)
  })

  test('a response after signing out is ignored', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a')]))
    await flush()
    controller.clearSessions()
    controller.sync(env({ signedIn: false, state: state() }))
    expect(view().clearing).toBe(false)
    settle(port.pending.clearSessions, { cleared: 1 })
    await flush()
    expect(view().sessions.load).toBe('idle')
  })
})

describe('view toggles', () => {
  test('the steps choice and the arguments are remembered per key', () => {
    const { controller, view } = setup()
    controller.toggleSteps('run:r1', false)
    expect(view().stepsChoice.get('run:r1')).toBe(true)
    controller.toggleSteps('run:r1', true)
    expect(view().stepsChoice.get('run:r1')).toBe(false)

    controller.toggleArguments('c1')
    expect(view().argsOpen.has('c1')).toBe(true)
    controller.toggleArguments('c1')
    expect(view().argsOpen.has('c1')).toBe(false)
  })

  test('subscribers are notified only on real changes', () => {
    const { controller } = setup()
    let notified = 0
    const off = controller.subscribe(() => {
      notified += 1
    })
    const current = controller.getSnapshot().env
    controller.sync({ ...current })
    expect(notified).toBe(0)
    controller.toggleArguments('c1')
    expect(notified).toBe(1)
    off()
    controller.toggleArguments('c1')
    expect(notified).toBe(1)
  })
})

describe('regenerate / edit and resend', () => {
  const history = () => [message('m1', 1, 'user'), message('m2', 2, 'assistant'), message('m3', 3, 'user'), message('m4', 4, 'assistant')]

  test('replaceFrom goes on the wire and the replaced history is cut once the answer is accepted', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: history() })
    await flush()
    const result = controller.regenerate('m3', 'again', true)
    expect(port.last('ask')).toMatchObject({ sessionId: 'a', text: 'again', images: [], replaceFrom: 'm3', keepImages: true })
    // 回包前历史原样保留。
    expect(view().detail.messages).toHaveLength(4)
    settle(port.pending.ask, { sessionId: 'a' })
    expect(await result).toBe(true)
    expect(view().detail.messages.map((item) => item.id)).toEqual(['m1', 'm2'])
  })

  test('a plain question carries no replaceFrom / keepImages', () => {
    const { port, controller } = setup()
    void controller.send('x', [])
    expect('replaceFrom' in (port.last('ask') as object)).toBe(false)
    expect('keepImages' in (port.last('ask') as object)).toBe(false)
  })

  test('a failed resend keeps the history', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: history() })
    await flush()
    const result = controller.send('edited', [], { replaceFrom: 'm1' })
    port.pending.ask[0]?.reject(rpcError('conflict'))
    expect(await result).toBe(false)
    expect(view().detail.messages).toHaveLength(4)
    expect(view().sendFailure).toBe('busy')
  })
})

describe('send switching race', () => {
  test('selecting another conversation while the question is in flight keeps that selection', async () => {
    const { port, controller, view } = setup()
    const result = controller.send('hello', [])
    controller.selectSession('other')
    settle(port.pending.ask, { sessionId: 'new' })
    expect(await result).toBe(true)
    expect(view().current).toBe('other')
    expect(view().sending).toBe(false)
    // 列表仍会刷新，新会话出现在列表里。
    expect(port.count('sessions')).toBe(1)
  })

  test('a new chat started while the question is in flight is not overwritten', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: [message('m1', 1, 'user')] })
    await flush()
    const result = controller.send('hello', [])
    controller.newChat()
    settle(port.pending.ask, { sessionId: 'a' })
    expect(await result).toBe(true)
    expect(view().current).toBeNull()
    expect(view().sent).toBeNull()
  })

  test('a resend whose position was left keeps the shown history', async () => {
    const { port, controller, view } = setup()
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: [message('m1', 1, 'user'), message('m2', 2, 'assistant')] })
    await flush()
    const result = controller.send('edited', [], { replaceFrom: 'm1' })
    controller.selectSession('b')
    settle(port.pending.ask, { sessionId: 'a' })
    expect(await result).toBe(true)
    expect(view().current).toBe('b')
    expect(view().detail.messages).toEqual([])
  })
})

describe('rename', () => {
  test('the trimmed title goes out and both the list and the open conversation follow', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a'), session('b')]))
    await flush()
    controller.selectSession('a')
    settle(port.pending.session, { session: session('a'), messages: [] })
    await flush()

    const result = controller.renameSession('a', '  new title  ')
    expect(port.last('renameSession')).toEqual({ sessionId: 'a', title: 'new title' })
    settle(port.pending.renameSession, session('a', { title: 'new title' }))
    expect(await result).toBe(true)
    expect(view().sessions.items.map((item) => item.title)).toEqual(['new title', 'title b'])
    expect(view().detail.session?.title).toBe('new title')
  })

  test('blank, over-long and offline renames never reach the port', async () => {
    const { port, controller } = setup()
    expect(await controller.renameSession('a', '   ')).toBe(false)
    expect(await controller.renameSession('a', 'x'.repeat(81))).toBe(false)
    expect(port.count('renameSession')).toBe(0)
    const stale = setup({ stale: true })
    expect(await stale.controller.renameSession('a', 'ok')).toBe(false)
    expect(stale.port.count('renameSession')).toBe(0)
  })

  test('a failure keeps the old title and reports', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a')]))
    await flush()
    const result = controller.renameSession('a', 'new')
    port.pending.renameSession[0]?.reject(rpcError('unavailable'))
    expect(await result).toBe(false)
    expect(view().sessions.items[0]?.title).toBe('title a')
    expect(view().actionFailure).toBe('failed')
  })
})

describe('export', () => {
  const file: AssistantExportResult = { fileName: 'chat.md', mime: 'text/markdown', content: '# x' }

  test('returns the cloud file and refuses a second export while one is running', async () => {
    const { port, controller, view } = setup()
    const result = controller.exportSession('a', 'markdown')
    expect(view().exporting).toBe('a')
    expect(port.last('exportSession')).toEqual({ sessionId: 'a', format: 'markdown' })
    expect(await controller.exportSession('a', 'json')).toBeNull()
    expect(port.count('exportSession')).toBe(1)
    settle(port.pending.exportSession, file)
    expect(await result).toEqual(file)
    expect(view().exporting).toBeNull()
    expect(view().actionFailure).toBeNull()
  })

  test('a failure reports and releases the button', async () => {
    const { port, controller, view } = setup()
    const result = controller.exportSession('a', 'json')
    port.pending.exportSession[0]?.reject(rpcError('unavailable'))
    expect(await result).toBeNull()
    expect(view().exporting).toBeNull()
    expect(view().actionFailure).toBe('failed')
  })

  test('refused while disconnected', async () => {
    const { port, controller } = setup({ stale: true })
    expect(await controller.exportSession('a', 'json')).toBeNull()
    expect(port.count('exportSession')).toBe(0)
  })
})

describe('history images', () => {
  const dataUrl = 'data:image/png;base64,AA=='

  test('an attachment is fetched once per id', async () => {
    const { port, controller } = setup()
    const first = controller.attachment('att1')
    const again = controller.attachment('att1')
    expect(port.count('attachment')).toBe(1)
    expect(port.last('attachment')).toEqual({ attachmentId: 'att1' })
    settle(port.pending.attachment, { dataUrl })
    expect(await first).toBe(dataUrl)
    expect(await again).toBe(dataUrl)
    expect(await controller.attachment('att1')).toBe(dataUrl)
    expect(port.count('attachment')).toBe(1)
  })

  test('a failed fetch is not cached', async () => {
    const { port, controller } = setup()
    const failing = controller.attachment('att1')
    port.pending.attachment[0]?.reject(rpcError('unavailable'))
    expect(await failing.then(() => 'ok', () => 'rejected')).toBe('rejected')
    void controller.attachment('att1').catch(() => undefined)
    expect(port.count('attachment')).toBe(2)
  })

  test('signing out drops the cache', async () => {
    const { port, controller } = setup()
    const first = controller.attachment('att1')
    settle(port.pending.attachment, { dataUrl })
    await first
    controller.sync(env({ signedIn: false, state: state() }))
    controller.sync(env({ signedIn: true, state: state() }))
    void controller.attachment('att1')
    expect(port.count('attachment')).toBe(2)
  })
})

describe('search', () => {
  test('a query restarts the list from the first page; the same or a blank query behaves', async () => {
    const { port, controller, view } = setup()
    controller.reloadSessions()
    settle(port.pending.sessions, page([session('a')], 1))
    await flush()

    controller.setQuery('  proxy ')
    expect(view().sessions).toMatchObject({ items: [], load: 'loading', query: 'proxy' })
    expect(port.last('sessions')).toEqual({ limit: SESSION_PAGE_SIZE, offset: 0, query: 'proxy' })
    settle(port.pending.sessions, page([session('hit')], 1))
    await flush()
    expect(view().sessions.items.map((item) => item.id)).toEqual(['hit'])
    expect(view().sessions.load).toBe('ready')

    const requests = port.count('sessions')
    controller.setQuery('proxy')
    expect(port.count('sessions')).toBe(requests)

    controller.setQuery('   ')
    expect(view().sessions.query).toBe('')
    expect(port.last('sessions')).toEqual({ limit: SESSION_PAGE_SIZE, offset: 0, query: null })
  })

  test('load more keeps the query and an older response of the previous query is dropped', async () => {
    const { port, controller, view } = setup()
    controller.setQuery('a')
    settle(port.pending.sessions, page([session('a1'), session('a2')], 4))
    await flush()
    controller.loadMoreSessions()
    expect(port.last('sessions')).toEqual({ limit: SESSION_PAGE_SIZE, offset: 2, query: 'a' })
    settle(port.pending.sessions, page([session('a3')], 4))
    await flush()
    expect(view().sessions.items.map((item) => item.id)).toEqual(['a1', 'a2', 'a3'])

    controller.setQuery('b')
    controller.setQuery('c')
    settle(port.pending.sessions, page([session('stale-b')]))
    await flush()
    expect(view().sessions.load).toBe('loading')
    settle(port.pending.sessions, page([session('c1')]))
    await flush()
    expect(view().sessions.items.map((item) => item.id)).toEqual(['c1'])
  })
})
