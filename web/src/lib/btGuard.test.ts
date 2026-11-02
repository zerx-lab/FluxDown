import { describe, expect, test } from 'bun:test'
import {
  canRemoveEntry,
  canWhitelistEntry,
  chunkIds,
  clampPage,
  cloudStatusKind,
  DEFAULT_ENTRY_FILTER,
  durationSecs,
  entriesQuery,
  eventsQuery,
  evidenceFacts,
  formatPermille,
  formatRatioMilli,
  formatRemaining,
  pageCount,
  parseCidr,
  peerCommandParams,
  PEER_THROTTLE_SECS,
  peerFlagged,
  peerGuardView,
  reportActive,
  reportGate,
  remainingParts,
  removableIds,
  groupAddress,
  isGuardChangedFrame,
  BT_GUARD_RULE_FILTER,
  evidenceQueries,
  reportSwitchDisabled,
  RemainingClock,
  countdownIntervalMs,
  remainingNow,
} from './btGuard'
import type { BtGuardCloudStatusDto, BtGuardEntryDto } from './rpc'

const t = (key: string, params?: Record<string, string | number>) => (params ? `${key}:${JSON.stringify(params)}` : key)
const bytes = (n: number) => `${n}B`

describe('parseCidr', () => {
  test('accepts IPv4 / IPv6 addresses and ranges', () => {
    for (const ok of ['1.2.3.4', '1.2.3.0/24', '0.0.0.0/0', ' 10.0.0.1 ', '2001:db8::/48', '::1', '::', '1::', 'fe80::1/128', '::ffff:1.2.3.4', '2001:db8:0:0:0:0:0:1/64']) {
      expect(parseCidr(ok).ok).toBe(true)
    }
    const v4 = parseCidr(' 1.2.3.0/24 ')
    expect(v4).toEqual({ ok: true, family: 4, cidr: '1.2.3.0/24', prefix: 24 })
    expect(parseCidr('2001:db8::')).toEqual({ ok: true, family: 6, cidr: '2001:db8::', prefix: null })
  })

  test('rejects malformed text with the right issue', () => {
    expect(parseCidr('   ')).toEqual({ ok: false, issue: 'empty' })
    for (const bad of ['1.2.3', '1.2.3.256', '01.2.3.4', 'a.b.c.d', '1.2.3.4.5', ':::', '1:2:3:4:5:6:7:8:9', '1::2::3', '2001:db8::g', 'fe80::1%eth0', '[::1]', '1.2.3.4 /24', '中文']) {
      expect(parseCidr(bad)).toEqual({ ok: false, issue: 'address' })
    }
    for (const bad of ['1.2.3.4/33', '1.2.3.4/', '1.2.3.4/-1', '1.2.3.4/x', '::1/129', '::1/1000']) {
      expect(parseCidr(bad)).toEqual({ ok: false, issue: 'prefix' })
    }
  })
})

describe('groupAddress', () => {
  test('strips the prefix length of an IP group', () => {
    expect(groupAddress('1.2.3.4')).toBe('1.2.3.4')
    expect(groupAddress(' 2001:db8::/64 ')).toBe('2001:db8::')
    expect(groupAddress('')).toBe('')
  })
})

describe('isGuardChangedFrame', () => {
  const frame = (event: unknown) => ({ epoch: 'e', sequence: 1, event }) as never

  test('accepts both the direct daemon and agent-forwarded shapes', () => {
    expect(isGuardChangedFrame(frame({ service: 'daemon', event: { type: 'btGuardChanged' } }))).toBe(true)
    expect(isGuardChangedFrame(frame({ service: 'agent', event: { type: 'daemon', data: { type: 'btGuardChanged' } } }))).toBe(true)
  })

  test('ignores everything else', () => {
    expect(isGuardChangedFrame(frame({ service: 'daemon', event: { type: 'btGuardEvidenceReady', data: { count: 1 } } }))).toBe(false)
    expect(isGuardChangedFrame(frame({ service: 'agent', event: { type: 'daemon', data: { type: 'taskChanged' } } }))).toBe(false)
    expect(isGuardChangedFrame(frame({ service: 'agent', event: { type: 'syncChanged', data: {} } }))).toBe(false)
  })
})

describe('duration', () => {
  test('presets map to seconds, forever has none', () => {
    expect(durationSecs('1h')).toBe(3600)
    expect(durationSecs('1d')).toBe(86_400)
    expect(durationSecs('7d')).toBe(604_800)
    expect(durationSecs('forever')).toBeUndefined()
  })

  test('remaining parts pick the largest whole unit', () => {
    expect(remainingParts(undefined)).toBeNull()
    expect(remainingParts(-5)).toEqual({ unit: 's', n: 0 })
    expect(remainingParts(Number.NaN)).toEqual({ unit: 's', n: 0 })
    expect(remainingParts(59)).toEqual({ unit: 's', n: 59 })
    expect(remainingParts(60)).toEqual({ unit: 'm', n: 1 })
    expect(remainingParts(3599)).toEqual({ unit: 'm', n: 59 })
    expect(remainingParts(3600)).toEqual({ unit: 'h', n: 1 })
    expect(remainingParts(86_399)).toEqual({ unit: 'h', n: 23 })
    expect(remainingParts(86_400 * 30 + 5)).toEqual({ unit: 'd', n: 30 })
  })

  test('formatRemaining uses permanent / remaining keys', () => {
    expect(formatRemaining(t, undefined)).toBe('btGuardPermanent')
    expect(formatRemaining(t, 7200)).toBe('btGuardRemaining:{"time":"btGuardDurationH:{\\"n\\":2}"}')
    expect(formatRemaining(t, 0)).toBe('btGuardRemaining:{"time":"btGuardDurationS:{\\"n\\":0}"}')
  })
})

describe('remainingNow', () => {
  test('counts down from the fetch time and never goes negative', () => {
    expect(remainingNow(undefined, 0, 10_000)).toBeUndefined()
    expect(remainingNow(100, 1000, 1000)).toBe(100)
    expect(remainingNow(100, 1000, 31_500)).toBe(69)
    expect(remainingNow(5, 0, 60_000)).toBe(0)
    expect(remainingNow(5, 10_000, 0)).toBe(5)
  })
})

describe('evidenceFacts', () => {
  const evidence = { rule: 'excessive', detail: 'x3.7', torrentSize: 1000, deliveredBytes: 3700, dupBytes: 0, ratioMilli: 3700, rerequests: 0, rewindPermille: 75, strikes: 2 }

  test('formats in a stable order and drops zero / missing fields', () => {
    expect(evidenceFacts(evidence, bytes)).toEqual([
      { id: 'detail', value: 'x3.7' },
      { id: 'torrentSize', value: '1000B' },
      { id: 'deliveredBytes', value: '3700B' },
      { id: 'ratio', value: '3.70×' },
      { id: 'rewind', value: '7.5%' },
      { id: 'strikes', value: '2' },
    ])
  })

  test('accepts JSON text, cloud category, and ignores garbage', () => {
    expect(evidenceFacts(JSON.stringify({ category: 'community' }), bytes)).toEqual([{ id: 'category', value: 'community' }])
    expect(evidenceFacts('not json', bytes)).toEqual([])
    expect(evidenceFacts(undefined, bytes)).toEqual([])
    expect(evidenceFacts(null, bytes)).toEqual([])
    expect(evidenceFacts([1, 2], bytes)).toEqual([])
    expect(evidenceFacts({ deliveredBytes: -1, ratioMilli: 'x', detail: '  ' }, bytes)).toEqual([])
  })

  test('number formatting helpers', () => {
    expect(formatRatioMilli(2000)).toBe('2.00×')
    expect(formatPermille(1000)).toBe('100.0%')
  })
})

describe('list queries', () => {
  test('entriesQuery omits all / blank filters and pages by offset', () => {
    expect(entriesQuery(DEFAULT_ENTRY_FILTER, 0)).toEqual({ offset: 0, limit: 100 })
    expect(entriesQuery({ layer: 'auto', rule: 'lag', query: '  1.2 ' }, 2, 50)).toEqual({ layer: 'auto', rule: 'lag', query: '1.2', offset: 100, limit: 50 })
    expect(entriesQuery(DEFAULT_ENTRY_FILTER, -3).offset).toBe(0)
  })

  test('eventsQuery omits all filters', () => {
    expect(eventsQuery({ rule: 'all', kind: 'all' }, 1)).toEqual({ offset: 100, limit: 100 })
    expect(eventsQuery({ rule: 'rewind', kind: 'enforced' }, 0)).toEqual({ offset: 0, limit: 100, rule: 'rewind', kind: 'enforced' })
  })

  test('paging clamps after the last page empties', () => {
    expect(pageCount(0)).toBe(1)
    expect(pageCount(101)).toBe(2)
    expect(clampPage(3, 101)).toBe(1)
    expect(clampPage(-1, 101)).toBe(0)
    expect(clampPage(5, 0)).toBe(0)
  })

  test('chunkIds splits on the protocol limit', () => {
    expect(chunkIds([])).toEqual([])
    const ids = Array.from({ length: 2001 }, (_, i) => i)
    expect(chunkIds(ids).map((c) => c.length)).toEqual([1000, 1000, 1])
    expect(chunkIds([1, 2, 3], 2)).toEqual([[1, 2], [3]])
  })
})

describe('entry actions', () => {
  const entry = (id: number, layer: BtGuardEntryDto['layer']): BtGuardEntryDto => ({ id, layer, cidr: '1.2.3.4', action: 'ban', createdAt: 0 })

  test('cloud entries cannot be removed; allow entries need no allow-listing', () => {
    expect(canRemoveEntry(entry(1, 'cloud'))).toBe(false)
    expect(canRemoveEntry(entry(1, 'file'))).toBe(true)
    expect(canWhitelistEntry(entry(1, 'allow'))).toBe(false)
    expect(canWhitelistEntry(entry(1, 'cloud'))).toBe(true)
  })

  test('removableIds keeps only selected non-cloud entries', () => {
    const rows = [entry(1, 'manual'), entry(2, 'cloud'), entry(3, 'auto'), entry(4, 'allow')]
    expect(removableIds(rows, new Set([2, 3, 4, 99]))).toEqual([3, 4])
  })
})

describe('status gates', () => {
  const cloud = (patch: Partial<BtGuardCloudStatusDto> = {}): BtGuardCloudStatusDto => ({ sequence: 5, updatedAt: 1, expiresAt: 2, entries: 1, clientRules: 0, stale: false, ...patch })

  test('cloud status priority', () => {
    expect(cloudStatusKind(cloud(), false)).toBe('disabled')
    expect(cloudStatusKind(cloud({ sequence: 0 }), true)).toBe('none')
    expect(cloudStatusKind(cloud({ sequence: 0, rejectedReason: 'bad' }), true)).toBe('rejected')
    expect(cloudStatusKind(cloud({ sequence: 0, lastError: 'net' }), true)).toBe('error')
    expect(cloudStatusKind(cloud({ stale: true, rejectedReason: 'x', lastError: 'y' }), true)).toBe('stale')
    expect(cloudStatusKind(cloud({ rejectedReason: 'x', lastError: 'y' }), true)).toBe('rejected')
    expect(cloudStatusKind(cloud({ lastError: 'y' }), true)).toBe('error')
    expect(cloudStatusKind(cloud(), true)).toBe('ok')
  })

  test('reporting needs a signed-in account', () => {
    expect(reportGate(true)).toBe('ready')
    expect(reportGate(false)).toBe('needLogin')
    expect(reportActive(true, false)).toBe(false)
    expect(reportActive(true, true)).toBe(true)
    expect(reportActive(false, true)).toBe(false)
  })
})

describe('peer guard view', () => {
  test('old daemon fields fall back to harmless defaults', () => {
    const view = peerGuardView({})
    expect(view).toEqual({ ipGroup: '', groupDeliveredBytes: 0, suspicion: 'none', rules: [], action: 'allow', observeOnly: false })
    expect(peerFlagged(view)).toBe(false)
  })

  test('unknown values are normalised, flagged peers detected', () => {
    const odd = peerGuardView({ ipGroup: ' 1.2.3.4 ', groupDeliveredBytes: -1, suspicion: 'extreme' as never, rules: ['lag', ''], action: 'nuke' as never })
    expect(odd).toMatchObject({ ipGroup: '1.2.3.4', groupDeliveredBytes: 0, suspicion: 'none', rules: ['lag'], action: 'allow' })
    expect(peerFlagged(odd)).toBe(true)
    const banned = peerGuardView({ suspicion: 'strong', action: 'banned', rules: ['excessive'], observeOnly: true, groupDeliveredBytes: 5 })
    expect(banned).toMatchObject({ suspicion: 'strong', action: 'banned', observeOnly: true, groupDeliveredBytes: 5 })
    expect(peerFlagged(peerGuardView({ action: 'choke' }))).toBe(true)
  })
})

describe('peerCommandParams', () => {
  test('ban uses the preset duration, forever omits it', () => {
    expect(peerCommandParams('1.2.3.4', { kind: 'ban', duration: '1h' })).toEqual({ cidr: '1.2.3.4', action: 'ban', durationSecs: 3600, reason: '' })
    expect(peerCommandParams('2001:db8::/64', { kind: 'ban', duration: 'forever' })).toEqual({ cidr: '2001:db8::/64', action: 'ban', reason: '' })
  })

  test('throttle lasts one day, allow is permanent', () => {
    expect(PEER_THROTTLE_SECS).toBe(86_400)
    expect(peerCommandParams('1.2.3.4', { kind: 'throttle' })).toEqual({ cidr: '1.2.3.4', action: 'throttle', durationSecs: 86_400, reason: '' })
    expect(peerCommandParams('1.2.3.4', { kind: 'allow' })).toEqual({ cidr: '1.2.3.4', action: 'allow', reason: '' })
  })
})

describe('evidenceQueries', () => {
  test('uses server-side ipGroup / containsIp filters instead of fetching and filtering locally', () => {
    expect(evidenceQueries('1.2.3.0/24', 20)).toEqual({
      entries: { containsIp: '1.2.3.0', offset: 0, limit: 100 },
      events: { ipGroup: '1.2.3.0/24', offset: 0, limit: 20 },
    })
    expect(evidenceQueries('2001:db8::/64', 5).entries.containsIp).toBe('2001:db8::')
    expect(evidenceQueries('9.9.9.9', 5).entries.containsIp).toBe('9.9.9.9')
  })
})

describe('reportSwitchDisabled', () => {
  test('turning reporting off is always possible', () => {
    expect(reportSwitchDisabled({ enabled: true, loggedIn: false, guardDisabled: true })).toBe(false)
    expect(reportSwitchDisabled({ enabled: true, loggedIn: true, guardDisabled: true })).toBe(false)
  })
  test('turning it on needs the master switch and a login', () => {
    expect(reportSwitchDisabled({ enabled: false, loggedIn: true, guardDisabled: true })).toBe(true)
    expect(reportSwitchDisabled({ enabled: false, loggedIn: false, guardDisabled: false })).toBe(true)
    expect(reportSwitchDisabled({ enabled: false, loggedIn: true, guardDisabled: false })).toBe(false)
  })
})

describe('RemainingClock', () => {
  const entry = (id: number, remainingSecs?: number) => ({ id, remainingSecs })
  test('counts down locally from the moment a value was observed', () => {
    const clock = new RemainingClock()
    clock.observe([entry(1, 100), entry(2)], 1_000)
    expect(clock.secs(entry(1, 100), 1_000)).toBe(100)
    expect(clock.secs(entry(1, 100), 31_000)).toBe(70)
    expect(clock.secs(entry(1, 100), 500_000)).toBe(0)
    expect(clock.secs(entry(2), 31_000)).toBeUndefined()
  })
  test('re-syncs when a refetch reports a new value and drops absent entries', () => {
    const clock = new RemainingClock()
    clock.observe([entry(1, 100)], 0)
    clock.observe([entry(1, 100)], 20_000)
    expect(clock.secs(entry(1, 100), 30_000)).toBe(70)
    clock.observe([entry(1, 60)], 40_000)
    expect(clock.secs(entry(1, 60), 50_000)).toBe(50)
    clock.observe([], 60_000)
    expect(clock.secs(entry(1, 60), 90_000)).toBe(60)
  })
})

describe('countdownIntervalMs', () => {
  test('no timer without timed entries; per-second near expiry; slow otherwise', () => {
    expect(countdownIntervalMs([])).toBeNull()
    expect(countdownIntervalMs([undefined])).toBeNull()
    expect(countdownIntervalMs([undefined, 90, 4000])).toBe(1000)
    expect(countdownIntervalMs([300, 4000])).toBe(15_000)
  })
})

describe('rule filter', () => {
  test('includes the manual / file rules engine writes for hand-added and subscription entries', () => {
    expect(BT_GUARD_RULE_FILTER.includes('manual')).toBe(true)
    expect(BT_GUARD_RULE_FILTER.includes('file')).toBe(true)
    expect(entriesQuery({ ...DEFAULT_ENTRY_FILTER, rule: 'manual' }, 0).rule).toBe('manual')
  })
})
