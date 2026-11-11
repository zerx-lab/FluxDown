import { describe, expect, test } from 'bun:test'
import { ed2kIdCheckOutcome } from '../../../lib/ed2kNetwork'
import type { Ed2kNetworkDto, Ed2kPeerDto, Ed2kStatusDto } from '../../../lib/rpc'
import { MAX_PEER_ROWS } from './btStatus'
import {
  ed2kLayout,
  ed2kPeerColumns,
  isFuture,
  kadView,
  mapEd2kPeers,
  normalizeEd2kStatus,
  normalizeMaxSources,
  partsText,
  peerStateLabel,
} from './ed2kStatus'

const NOW = 1_000_000

const peer = (over: Partial<Ed2kPeerDto>): Ed2kPeerDto => ({ origin: 'server', state: 'known', ...over })

describe('normalizeMaxSources', () => {
  test('0 / 负数 / 缺省 / 非法 = 自动', () => {
    expect(normalizeMaxSources(0)).toBe(0)
    expect(normalizeMaxSources(-3)).toBe(0)
    expect(normalizeMaxSources(undefined)).toBe(0)
    expect(normalizeMaxSources(Number.NaN)).toBe(0)
  })
  test('>8 视为 8，范围内原样', () => {
    expect(normalizeMaxSources(9)).toBe(8)
    expect(normalizeMaxSources(512)).toBe(8)
    expect(normalizeMaxSources(1)).toBe(1)
    expect(normalizeMaxSources(8)).toBe(8)
  })
})

describe('isFuture', () => {
  test('未知、已过期、相等都不算未来', () => {
    expect(isFuture(undefined, NOW)).toBe(false)
    expect(isFuture(0, NOW)).toBe(false)
    expect(isFuture(NOW - 5, NOW)).toBe(false)
    expect(isFuture(NOW, NOW)).toBe(false)
    expect(isFuture(NOW + 1, NOW)).toBe(true)
  })
})

describe('peerStateLabel', () => {
  test('排队：有名次带名次，缺省 / 0 用无名次文案', () => {
    expect(peerStateLabel(peer({ state: 'queued', queueRank: 12 }), NOW)).toEqual({ key: 'ed2kPeerStateQueuedRank', params: { rank: 12 } })
    expect(peerStateLabel(peer({ state: 'queued' }), NOW)).toEqual({ key: 'ed2kPeerStateQueued' })
    expect(peerStateLabel(peer({ state: 'queued', queueRank: 0 }), NOW)).toEqual({ key: 'ed2kPeerStateQueued' })
  })
  test('传输中：块序号从 0 起，展示 +1', () => {
    expect(peerStateLabel(peer({ state: 'transferring', part: 0 }), NOW)).toEqual({ key: 'ed2kPeerStateTransferringPart', params: { part: 1 } })
    expect(peerStateLabel(peer({ state: 'transferring' }), NOW)).toEqual({ key: 'ed2kPeerStateTransferring' })
  })
  test('冷却：retryAt 在未来才带时间，已过期回落', () => {
    expect(peerStateLabel(peer({ state: 'cooldown', retryAt: NOW + 60 }), NOW)).toEqual({ key: 'ed2kPeerStateCooldownUntil', retryAt: NOW + 60 })
    expect(peerStateLabel(peer({ state: 'cooldown', retryAt: NOW - 1 }), NOW)).toEqual({ key: 'ed2kPeerStateCooldown' })
    expect(peerStateLabel(peer({ state: 'cooldown' }), NOW)).toEqual({ key: 'ed2kPeerStateCooldown' })
  })
  test('其余状态一一对应', () => {
    expect(peerStateLabel(peer({ state: 'known' }), NOW).key).toBe('ed2kPeerStateKnown')
    expect(peerStateLabel(peer({ state: 'connecting' }), NOW).key).toBe('ed2kPeerStateConnecting')
    expect(peerStateLabel(peer({ state: 'connected' }), NOW).key).toBe('ed2kPeerStateConnected')
    expect(peerStateLabel(peer({ state: 'failed', message: 'connection refused' }), NOW).key).toBe('ed2kPeerStateFailed')
    expect(peerStateLabel(peer({ state: 'banned' }), NOW).key).toBe('ed2kPeerStateBanned')
  })
})

describe('mapEd2kPeers', () => {
  test('LowID 源无 addr：保留 lowId，key 唯一', () => {
    const rows = mapEd2kPeers([peer({ lowId: 12345 }), peer({ lowId: 12345 }), peer({ addr: '1.2.3.4:4662' }), peer({ addr: '1.2.3.4:4662' })])
    expect(rows.map((r) => r.key)).toEqual(['low:12345', 'low:12345#1', '1.2.3.4:4662', '1.2.3.4:4662#1'])
    expect(rows[0]).toMatchObject({ addr: '', lowId: 12345 })
    expect(rows[2]).toMatchObject({ addr: '1.2.3.4:4662', lowId: null })
  })
  test('缺省字段归一：未知块数为 null，数值为 0，文本为空', () => {
    const [row] = mapEd2kPeers([peer({ client: '  eMule 0.50a  ' })])
    expect(row).toMatchObject({ parts: null, downloaded: 0, downBps: 0, queueRank: 0, retryAt: 0, client: 'eMule 0.50a', nickname: '', message: '' })
    expect(partsText(row!.parts, 10)).toBeNull()
    expect(partsText(0, 10)).toBe('0 / 10')
  })
  test('渲染封顶', () => {
    const many = Array.from({ length: MAX_PEER_ROWS + 5 }, (_, i) => peer({ addr: `10.0.0.${i}:1` }))
    expect(mapEd2kPeers(many)).toHaveLength(MAX_PEER_ROWS)
  })
})

describe('normalizeEd2kStatus', () => {
  test('最小 DTO（默认值字段全部不上线）补齐', () => {
    const dto = normalizeEd2kStatus({ phase: 'idle', network: { id: 'none', upnp: 'disabled' } as Ed2kNetworkDto } as Partial<Ed2kStatusDto>)
    expect(dto.peers).toEqual([])
    expect(dto.servers).toEqual([])
    expect(dto.kad).toMatchObject({ phase: 'idle', nodes: [], bootstrapNodes: 0 })
    expect(dto.peerCounts.total).toBe(0)
    expect(dto.network).toMatchObject({ kadEnabled: false, kadNodes: 0, serverCount: 0, listenPort: 0, server: '' })
    expect(dto.segments).toBe(0)
    expect(dto.fileHash).toBe('')
  })
  test('完全缺失 network / kad 也不崩', () => {
    const dto = normalizeEd2kStatus({})
    expect(dto.phase).toBe('idle')
    expect(dto.network.id).toBe('none')
  })
})

describe('kadView / 布局', () => {
  test('Kad 段三态', () => {
    expect(kadView({ kadEnabled: false, kadNodes: 50 })).toBe('off')
    expect(kadView({ kadEnabled: true, kadNodes: 0 })).toBe('noBootstrap')
    expect(kadView({ kadEnabled: true, kadNodes: 3 })).toBe('table')
  })
  test('宽度档位', () => {
    expect(ed2kLayout(0)).toBe('card')
    expect(ed2kLayout(479)).toBe('card')
    expect(ed2kLayout(480)).toBe('compact')
    expect(ed2kLayout(780)).toBe('full')
    expect(ed2kPeerColumns('full')).toEqual(['addr', 'origin', 'client', 'state', 'parts', 'downloaded', 'speed'])
  })
})

describe('ed2kIdCheckOutcome', () => {
  const t = (key: string, params?: Record<string, string | number>) => (params ? `${key}${JSON.stringify(params)}` : key)
  const net = (over: Partial<Ed2kNetworkDto>): Ed2kNetworkDto => ({ id: 'none', upnp: 'disabled', upnpEnabled: true, kadEnabled: true, kadNodes: 0, serverCount: 0, ...over })
  test('未登录 → 失败并附 error', () => {
    expect(ed2kIdCheckOutcome(t, net({ error: 'all servers timed out' }))).toEqual({ ok: false, message: 'ed2kIdCheckFailed: all servers timed out' })
    expect(ed2kIdCheckOutcome(t, net({}))).toEqual({ ok: false, message: 'ed2kIdCheckFailed' })
  })
  test('LowID 带提示，缺省端口 / 服务器显示 —', () => {
    const out = ed2kIdCheckOutcome(t, net({ id: 'low', upnp: 'failed' }))
    expect(out).toEqual({ ok: true, summary: 'ed2kIdCheckResult{"id":"ed2kIdLow","server":"—","port":"—","upnp":"ed2kUpnpFailed"}', hint: 'ed2kIdLowHint' })
  })
})
