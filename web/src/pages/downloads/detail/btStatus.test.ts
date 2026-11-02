import { describe, expect, test } from 'bun:test'
import type { BtPeerDto, BtTrackerDto } from '../../../lib/rpc'
import { GUARD_COLUMNS_MIN_WIDTH } from '../../../lib/btGuard'
import { hasGuardData, mapPeerRows, mapTrackerRows, nextTrackerSort, peerColumns, peerLayout, trackerCounts, trackerView } from './btStatus'
import type { TrackerFilter, TrackerSort } from './btStatus'

interface Fixture {
  trackers: BtTrackerDto[]
  counts: Record<TrackerFilter, number>
  cases: { name: string; filter: TrackerFilter; sort: TrackerSort; expect: number[] }[]
}

declare const Bun: { file(path: URL): { json(): Promise<unknown> } }
const fixture = (await Bun.file(new URL('../../../../../crates/downloads/tests/fixtures/bt_tracker_view_cases.json', import.meta.url)).json()) as Fixture
const rows = mapTrackerRows(fixture.trackers)

describe('tracker view (shared fixtures)', () => {
  test('counts', () => {
    expect(trackerCounts(rows)).toEqual(fixture.counts)
  })
  for (const item of fixture.cases) {
    test(item.name, () => {
      expect(trackerView(rows, item.filter, item.sort)).toEqual(item.expect)
    })
  }
})

describe('nextTrackerSort', () => {
  test('三态：升序 → 降序 → 无', () => {
    const asc = nextTrackerSort(null, 'peers')
    expect(asc).toEqual({ key: 'peers', desc: false })
    const desc = nextTrackerSort(asc, 'peers')
    expect(desc).toEqual({ key: 'peers', desc: true })
    expect(nextTrackerSort(desc, 'peers')).toBeNull()
  })
  test('换列从升序开始', () => {
    expect(nextTrackerSort({ key: 'peers', desc: true }, 'url')).toEqual({ key: 'url', desc: false })
  })
})

describe('peer rows with anti-leech data', () => {
  const base: BtPeerDto = { addr: '1.2.3.4:6881', transport: 'tcp', downloaded: 1, uploaded: 2, downBps: 0, upBps: 0 }

  test('old daemon peers map to harmless guard defaults and hide guard columns', () => {
    const { rows } = mapPeerRows([base])
    expect(rows[0]?.guard).toEqual({ ipGroup: '', groupDeliveredBytes: 0, suspicion: 'none', rules: [], action: 'allow', observeOnly: false })
    expect(hasGuardData(rows)).toBe(false)
    expect(peerColumns('full', hasGuardData(rows))).toEqual(['addr', 'client', 'transport', 'downloaded', 'uploaded', 'downBps', 'upBps'])
  })

  test('guard fields flow through and add columns per layout', () => {
    const { rows } = mapPeerRows([{ ...base, ipGroup: '1.2.3.4', groupDeliveredBytes: 9, suspicion: 'strong', rules: ['rewind'], action: 'banned', observeOnly: true }])
    expect(rows[0]?.guard).toMatchObject({ ipGroup: '1.2.3.4', groupDeliveredBytes: 9, suspicion: 'strong', rules: ['rewind'], action: 'banned', observeOnly: true })
    expect(hasGuardData(rows)).toBe(true)
    expect(peerColumns('full', true).slice(-3)).toEqual(['groupDelivered', 'suspicion', 'action'])
    expect(peerColumns('compact', true).at(-1)).toBe('action')
    expect(peerColumns('card', true)).toEqual(peerColumns('card'))
  })
})

describe('peerLayout with guard columns', () => {
  test('full layout needs the guard columns minimum width; plain layout is unchanged', () => {
    expect(peerLayout(900)).toBe('full')
    expect(peerLayout(900, true)).toBe('compact')
    expect(peerLayout(GUARD_COLUMNS_MIN_WIDTH - 1, true)).toBe('compact')
    expect(peerLayout(GUARD_COLUMNS_MIN_WIDTH, true)).toBe('full')
    expect(peerLayout(400, true)).toBe('card')
  })
})
