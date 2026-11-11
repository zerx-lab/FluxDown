import { describe, expect, test } from 'bun:test'
import {
  ed2kIdCheckOutcome,
  ed2kIdCheckParams,
  ed2kKadBootstrapOutcome,
  ed2kKadBootstrapParams,
  kadNodeIdText,
  parseHostPort,
} from './ed2kNetwork'
import type { Ed2kKadBootstrapDto, Ed2kNetworkDto } from './rpc'

const t = (key: string, params?: Record<string, string | number>) => (params ? `${key}${JSON.stringify(params)}` : key)
const net = (over: Partial<Ed2kNetworkDto>): Ed2kNetworkDto => ({ id: 'none', upnp: 'disabled', upnpEnabled: true, kadEnabled: true, kadNodes: 0, serverCount: 0, ...over })

describe('parseHostPort', () => {
  test('主机 trim，端口 1–65535', () => {
    expect(parseHostPort('  example.org ', ' 4661 ')).toEqual({ host: 'example.org', port: 4661 })
    expect(parseHostPort('1.2.3.4', '1')).toEqual({ host: '1.2.3.4', port: 1 })
    expect(parseHostPort('1.2.3.4', '65535')).toEqual({ host: '1.2.3.4', port: 65_535 })
  })
  test('主机为空 / IPv6 字面量 / 含空白 → null', () => {
    expect(parseHostPort('   ', '4661')).toBeNull()
    expect(parseHostPort('::1', '4661')).toBeNull()
    expect(parseHostPort('[2001:db8::1]', '4661')).toBeNull()
    expect(parseHostPort('a b', '4661')).toBeNull()
  })
  test('端口 0 / 65536 / 非数字 / 空 → null', () => {
    for (const port of ['0', '65536', 'abc', '', '12.5', '-1', '1e3']) expect(parseHostPort('h', port)).toBeNull()
  })
})

describe('ed2kIdCheckParams', () => {
  test('list / session 不需要服务器', () => {
    expect(ed2kIdCheckParams('list', '', '')).toEqual({ target: 'list' })
    expect(ed2kIdCheckParams('session', 'x', 'y')).toEqual({ target: 'session' })
  })
  test('server 拼 host:port，非法 → null', () => {
    expect(ed2kIdCheckParams('server', ' srv.example ', '4242')).toEqual({ target: 'server', server: 'srv.example:4242' })
    expect(ed2kIdCheckParams('server', '', '4242')).toBeNull()
    expect(ed2kIdCheckParams('server', 'h', '0')).toBeNull()
    expect(ed2kIdCheckParams('server', 'fe80::1', '4242')).toBeNull()
  })
})

describe('ed2kIdCheckOutcome 按目标', () => {
  test('session 无会话且无 error → 未登录说明', () => {
    expect(ed2kIdCheckOutcome(t, net({}), 'session')).toEqual({ ok: false, message: 'ed2kIdCheckNoSession' })
  })
  test('session 带 error / 其它目标 → 失败', () => {
    expect(ed2kIdCheckOutcome(t, net({ error: 'boom' }), 'session')).toEqual({ ok: false, message: 'ed2kIdCheckFailed: boom' })
    expect(ed2kIdCheckOutcome(t, net({}), 'list')).toEqual({ ok: false, message: 'ed2kIdCheckFailed' })
  })
  test('HighID 无提示', () => {
    expect(ed2kIdCheckOutcome(t, net({ id: 'high', server: 's:1', listenPort: 4662 }), 'server')).toMatchObject({ ok: true, hint: null })
  })
})

describe('ed2kKadBootstrapParams', () => {
  test('known / url（空 url 不上线）', () => {
    expect(ed2kKadBootstrapParams('known', '', '', 'x')).toEqual({ source: 'known' })
    expect(ed2kKadBootstrapParams('url', '', '', '   ')).toEqual({ source: 'url' })
    expect(ed2kKadBootstrapParams('url', '', '', ' http://a/nodes.dat ')).toEqual({ source: 'url', url: 'http://a/nodes.dat' })
  })
  test('node 需要合法主机与端口', () => {
    expect(ed2kKadBootstrapParams('node', 'n.example', '4672', '')).toEqual({ source: 'node', host: 'n.example', port: 4672 })
    expect(ed2kKadBootstrapParams('node', '', '4672', '')).toBeNull()
    expect(ed2kKadBootstrapParams('node', 'h', '65536', '')).toBeNull()
    expect(ed2kKadBootstrapParams('node', '::1', '4672', '')).toBeNull()
  })
})

describe('ed2kKadBootstrapOutcome', () => {
  const dto = (over: Partial<Ed2kKadBootstrapDto>): Ed2kKadBootstrapDto => ({ contacted: 5, responded: 2, discovered: 7, nodeCount: 40, nodes: [], ...over })
  test('error 非空 → 失败', () => {
    expect(ed2kKadBootstrapOutcome(t, dto({ error: 'no known nodes' }))).toEqual({ ok: false, message: 'ed2kKadBootstrapFailed{"error":"no known nodes"}' })
  })
  test('有应答 → 摘要，无警示', () => {
    expect(ed2kKadBootstrapOutcome(t, dto({}))).toEqual({
      ok: true,
      summary: 'ed2kKadBootstrapResult{"contacted":5,"responded":2,"discovered":7,"total":40}',
      noReply: null,
    })
  })
  test('responded = 0 → 附警示', () => {
    expect(ed2kKadBootstrapOutcome(t, dto({ responded: 0, error: '' }))).toMatchObject({ ok: true, noReply: 'ed2kKadBootstrapNoReply' })
  })
})

describe('kadNodeIdText', () => {
  test('空或全 0 → null', () => {
    expect(kadNodeIdText('')).toBeNull()
    expect(kadNodeIdText('00000000000000000000000000000000')).toBeNull()
    expect(kadNodeIdText('0A1B')).toBe('0A1B')
  })
})
