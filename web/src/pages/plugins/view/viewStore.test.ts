import { afterEach, describe, expect, test } from 'bun:test'
import { rpc } from '../../../lib/rpc'
import type { EventFrame, PluginViewOpenedDto, PluginViewOpenParams, PluginViewSetLocaleParams, ViewNode } from '../../../lib/rpc'
import { ViewSession, onPluginViewEventForTest, reopenOnRevisionChange, setPluginNavigator } from './viewStore'

const root = (text: string): ViewNode => ({ id: 'root', k: 'text', p: { text }, c: [] })

/** 构造一个已登记的 ready 会话（绕过 RPC）。 */
function readySession(id: string, seq = 3): ViewSession {
  const session = new ViewSession({ identity: 'a@x', view: 'v', locale: 'zh-CN' })
  const internals = session as unknown as { seq: number; register(id: string): void; set(next: object): void }
  internals.seq = seq
  internals.register(id)
  internals.set({ phase: 'ready', tree: root('hello') })
  return session
}

const daemonFrame = (event: object): EventFrame => ({ event: { service: 'daemon', event } }) as unknown as EventFrame

afterEach(() => setPluginNavigator(null))

describe('plugin view effects', () => {
  test('action frame runs navigate without touching tree or seq', () => {
    const targets: string[] = []
    setPluginNavigator((target) => targets.push(target))
    const session = readySession('s1')
    const before = session.getState().tree
    onPluginViewEventForTest(
      daemonFrame({
        type: 'pluginViewFrame',
        data: { session: 's1', seq: 3, ops: [], effects: [{ kind: 'navigate', target: 'rss' }, { kind: 'future' }] },
      }),
    )
    expect(targets).toEqual(['rss'])
    expect(session.getState().tree).toBe(before)
    // seq 未被改动：下一帧 seq 4 仍按 last+1 应用。
    onPluginViewEventForTest(
      daemonFrame({
        type: 'pluginViewFrame',
        data: { session: 's1', seq: 4, ops: [{ op: 'props', id: 'root', set: { text: 'next' }, unset: [] }] },
      }),
    )
    expect(session.getState().tree?.p.text).toBe('next')
    session.dispose()
  })

  test('unknown session or missing navigator is ignored', () => {
    onPluginViewEventForTest(
      daemonFrame({ type: 'pluginViewFrame', data: { session: 'nope', seq: 1, ops: [], effects: [{ kind: 'navigate', target: 'rss' }] } }),
    )
    const session = readySession('s2')
    onPluginViewEventForTest(
      daemonFrame({ type: 'pluginViewFrame', data: { session: 's2', seq: 3, ops: [], effects: [{ kind: 'navigate', target: 'rss' }] } }),
    )
    expect(session.getState().phase).toBe('ready')
    session.dispose()
  })
})

describe('plugin view requested close', () => {
  test('requested close hands off to onRequestClose instead of closed state', () => {
    const session = readySession('s3')
    let closed = 0
    session.onRequestClose = () => {
      closed += 1
    }
    onPluginViewEventForTest(daemonFrame({ type: 'pluginViewClosed', data: { session: 's3', reason: 'requested' } }))
    expect(closed).toBe(1)
    expect(session.getState().phase).toBe('ready')
    expect(session.sessionId).toBeNull()
    session.dispose()
  })

  test('without handler requested close enters closed state', () => {
    const session = readySession('s4')
    onPluginViewEventForTest(daemonFrame({ type: 'pluginViewClosed', data: { session: 's4', reason: 'requested' } }))
    expect(session.getState().phase).toBe('closed')
    expect(session.getState().reason).toBe('requested')
    session.dispose()
  })

  test('other reasons still enter closed state even with a handler', () => {
    const session = readySession('s5')
    let closed = 0
    session.onRequestClose = () => {
      closed += 1
    }
    onPluginViewEventForTest(daemonFrame({ type: 'pluginViewClosed', data: { session: 's5', reason: 'expired' } }))
    expect(closed).toBe(0)
    expect(session.getState().phase).toBe('closed')
    session.dispose()
  })
})

describe('plugin view actions before the open reply', () => {
  const plugin = rpc.daemon.plugin
  const original = { viewOpen: plugin.viewOpen, viewClose: plugin.viewClose }
  afterEach(() => {
    Object.assign(plugin, original)
  })

  /** 让 `viewOpen` 挂起，返回放行函数。 */
  function holdOpen(session: string): () => void {
    let release: () => void = () => undefined
    const reply = new Promise<PluginViewOpenedDto>((resolve) => {
      release = () => resolve({ session, seq: 0, tree: root('init') })
    })
    Object.assign(plugin, { viewOpen: () => reply, viewClose: () => Promise.resolve({}) })
    return () => release()
  }

  test('navigate and close from init run once the session registers', async () => {
    const targets: string[] = []
    setPluginNavigator((target) => targets.push(target))
    const release = holdOpen('e1')
    const session = new ViewSession({ identity: 'a@x', view: 'v', locale: 'en' })
    let closed = 0
    session.onRequestClose = () => {
      closed += 1
    }
    const opening = session.open()
    onPluginViewEventForTest(
      daemonFrame({ type: 'pluginViewFrame', data: { session: 'e1', seq: 0, ops: [], effects: [{ kind: 'navigate', target: 'downloads' }] } }),
    )
    onPluginViewEventForTest(daemonFrame({ type: 'pluginViewClosed', data: { session: 'e1', reason: 'requested' } }))
    expect(targets).toEqual([])
    expect(closed).toBe(0)
    release()
    await opening
    expect(targets).toEqual(['downloads'])
    expect(closed).toBe(1)
    expect(session.sessionId).toBeNull()
    session.dispose()
  })

  test('actions for unknown sessions are dropped when no open is in flight', async () => {
    const session = new ViewSession({ identity: 'a@x', view: 'v', locale: 'en' })
    let closed = 0
    session.onRequestClose = () => {
      closed += 1
    }
    onPluginViewEventForTest(daemonFrame({ type: 'pluginViewClosed', data: { session: 'e2', reason: 'requested' } }))
    const release = holdOpen('e2')
    const opening = session.open()
    release()
    await opening
    expect(closed).toBe(0)
    expect(session.getState().phase).toBe('ready')
    session.dispose()
  })
})

describe('plugin view locale updates', () => {
  const plugin = rpc.daemon.plugin
  const original = { viewOpen: plugin.viewOpen, viewSetLocale: plugin.viewSetLocale, viewClose: plugin.viewClose }
  afterEach(() => {
    Object.assign(plugin, original)
  })

  test('applies the latest locale after an in-flight open without reopening', async () => {
    let release: () => void = () => undefined
    const reply = new Promise<PluginViewOpenedDto>((resolve) => {
      release = () => resolve({ session: 'locale-session', seq: 0, tree: root('initial') })
    })
    let openCount = 0
    const openedLocales: string[] = []
    const requests: PluginViewSetLocaleParams[] = []
    Object.assign(plugin, {
      viewOpen: (params: PluginViewOpenParams) => {
        openCount += 1
        openedLocales.push(params.locale ?? '')
        return reply
      },
      viewSetLocale: (params: PluginViewSetLocaleParams) => {
        requests.push(params)
        return Promise.resolve({})
      },
      viewClose: () => Promise.resolve({}),
    })
    const session = new ViewSession({ identity: 'a@x', view: 'v', locale: 'en' })
    const opening = session.open()
    session.setLocale('fr-CA')
    release()
    await opening
    expect(openedLocales).toEqual(['en'])
    expect(requests).toEqual([{ session: 'locale-session', locale: 'fr-CA' }])
    expect(openCount).toBe(1)
    expect(session.getState().phase).toBe('ready')
    session.dispose()
  })

  test('updates only the targeted existing session', () => {
    const requests: PluginViewSetLocaleParams[] = []
    Object.assign(plugin, {
      viewSetLocale: (params: PluginViewSetLocaleParams) => {
        requests.push(params)
        return Promise.resolve({})
      },
      viewClose: () => Promise.resolve({}),
    })
    const first = readySession('locale-first')
    const second = readySession('locale-second')
    first.setLocale('fr')
    expect(requests).toEqual([{ session: 'locale-first', locale: 'fr' }])
    expect(first.getState().phase).toBe('ready')
    expect(second.getState().phase).toBe('ready')
    first.dispose()
    second.dispose()
  })
})

describe('plugin package revision reopen', () => {
  const plugin = rpc.daemon.plugin
  const original = { viewOpen: plugin.viewOpen, viewClose: plugin.viewClose }
  afterEach(() => {
    Object.assign(plugin, original)
  })

  /** 每次 open 返回递增的会话 id；记录 open 次数与被关闭的会话。 */
  function countingDaemon() {
    const log = { opened: 0, closed: [] as string[] }
    Object.assign(plugin, {
      viewOpen: () => {
        log.opened += 1
        return Promise.resolve({ session: `r${log.opened}`, seq: 0, tree: root('init') })
      },
      viewClose: ({ session }: { session: string }) => {
        log.closed.push(session)
        return Promise.resolve({})
      },
    })
    return log
  }

  test('revision change reopens live sessions of that plugin only and closes the old one', async () => {
    const log = countingDaemon()
    const mine = new ViewSession({ identity: 'rev-a@x', view: 'v', locale: 'en' })
    const other = new ViewSession({ identity: 'rev-b@x', view: 'v', locale: 'en' })
    await mine.open()
    await other.open()
    expect(log.opened).toBe(2)
    const plugins = (a: number) => [{ identity: 'rev-a@x', packageRevision: a }, { identity: 'rev-b@x', packageRevision: 1 }]
    await reopenOnRevisionChange(plugins(1))
    await reopenOnRevisionChange(plugins(1))
    expect(log.opened).toBe(2)
    await reopenOnRevisionChange(plugins(2))
    expect(log.opened).toBe(3)
    expect(log.closed).toEqual(['r1'])
    expect(mine.sessionId).toBe('r3')
    expect(mine.getState().phase).toBe('ready')
    expect(other.sessionId).toBe('r2')
    mine.dispose()
    other.dispose()
  })

  test('sessions ended by pluginUnloaded reopen on the next revision', async () => {
    const log = countingDaemon()
    const session = new ViewSession({ identity: 'rev-c@x', view: 'v', locale: 'en' })
    await session.open()
    await reopenOnRevisionChange([{ identity: 'rev-c@x', packageRevision: 5 }])
    onPluginViewEventForTest(daemonFrame({ type: 'pluginViewClosed', data: { session: 'r1', reason: 'pluginUnloaded' } }))
    expect(session.getState().phase).toBe('closed')
    await reopenOnRevisionChange([{ identity: 'rev-c@x', packageRevision: 6 }])
    expect(log.opened).toBe(2)
    expect(log.closed).toEqual([])
    expect(session.getState().phase).toBe('ready')
    session.dispose()
  })

  test('missing revision keeps the baseline and the current view', async () => {
    const log = countingDaemon()
    const session = new ViewSession({ identity: 'rev-d@x', view: 'v', locale: 'en' })
    await session.open()
    await reopenOnRevisionChange([{ identity: 'rev-d@x', packageRevision: 3 }])
    await reopenOnRevisionChange([{ identity: 'rev-d@x' }])
    await reopenOnRevisionChange([{ identity: 'rev-d@x', packageRevision: 3 }])
    expect(log.opened).toBe(1)
    session.dispose()
  })

  test('disposed sessions are not reopened', async () => {
    const log = countingDaemon()
    const session = new ViewSession({ identity: 'rev-e@x', view: 'v', locale: 'en' })
    await session.open()
    await reopenOnRevisionChange([{ identity: 'rev-e@x', packageRevision: 1 }])
    session.dispose()
    await reopenOnRevisionChange([{ identity: 'rev-e@x', packageRevision: 2 }])
    expect(log.opened).toBe(1)
  })
})
