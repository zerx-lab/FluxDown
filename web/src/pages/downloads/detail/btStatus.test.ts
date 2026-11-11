import { describe, expect, test } from 'bun:test'
import type { BtPeerDto, BtTrackerDto } from '../../../lib/rpc'
import { GUARD_COLUMNS_MIN_WIDTH } from '../../../lib/btGuard'
import {
  btMetadataReady,
  btTaskRunning,
  countText,
  hasGuardData,
  infoHashV2,
  mapPeerRows,
  mapTrackerRows,
  nextTrackerSort,
  peerColumns,
  peerLayout,
  TRACKER_STATS_MIN_WIDTH,
  trackerCounts,
  trackerLayout,
  trackerMessageColumn,
  trackerStatsItems,
  trackerView,
  webSeedStateKey,
} from './btStatus'
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

// 镜像 GPUI `task_detail_bt::tracker_tier_stats_web_seed_and_v2_hash`。
describe('tracker 层级 / 统计 / HTTP 源 / v2 hash', () => {
  const base: BtTrackerDto = { url: 'udp://t/announce', source: 'torrent', status: 'working' }
  const labels = ['T', 'S', 'L', 'D'] as const

  test('旧 daemon：tier 缺省为第一层，统计未知显示 —', () => {
    const [old] = mapTrackerRows([base])
    if (old === undefined) throw new Error('missing row')
    expect(old.tier).toBe(1)
    expect([old.seeders, old.leechers, old.downloaded]).toEqual([null, null, null])
    expect(trackerStatsItems(labels, old)).toEqual(['T: 1', 'S: —', 'L: —', 'D: —'])
  })

  test('0 是事实而非未知；大数原样', () => {
    const [row] = mapTrackerRows([{ ...base, tier: 2, seeders: 0, leechers: 4_000_000_000, downloaded: 7 }])
    if (row === undefined) throw new Error('missing row')
    expect(row.tier).toBe(3)
    expect(trackerStatsItems(labels, row)).toEqual(['T: 3', 'S: 0', 'L: 4000000000', 'D: 7'])
    expect(countText(null)).toBe('—')
  })

  test('HTTP 源状态键：未知取值返回 null', () => {
    expect(webSeedStateKey('backingOff')).toBe('btWebSeedStateBackingOff')
    expect(webSeedStateKey('idle')).toBe('btWebSeedStateIdle')
    expect(webSeedStateKey('downloading')).toBe('btWebSeedStateDownloading')
    expect(webSeedStateKey('disabled')).toBe('btWebSeedStateDisabled')
    expect(webSeedStateKey('somethingNew')).toBeNull()
  })

  test('v2 info hash：缺省 / 空白不显示', () => {
    expect(infoHashV2(null)).toBeNull()
    expect(infoHashV2({})).toBeNull()
    expect(infoHashV2({ infoHashV2: '  ' })).toBeNull()
    expect(infoHashV2({ infoHashV2: ' ab12 ' })).toBe('ab12')
  })
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

describe('tracker 版式', () => {
  test('统计四列只在 GPUI 同款阈值以上并排；其下仍在 URL 下方', () => {
    expect(TRACKER_STATS_MIN_WIDTH).toBe(1100)
    expect([0, 559, 560, 899, 900, 1099, 1100, 1600].map(trackerLayout)).toEqual(['card', 'card', 'table', 'table', 'wide', 'wide', 'wideStats', 'wideStats'])
    expect((['card', 'table', 'wide', 'wideStats'] as const).map(trackerMessageColumn)).toEqual([false, false, true, true])
  })
})

describe('强制汇报 / 元数据', () => {
  test('运行 = 下载中或正在做种（排队等名额的 8 不算，镜像 GPUI bt_task_running）', () => {
    expect(btTaskRunning({ state: 'downloading', seedingStatus: 0 })).toBe(true)
    expect(btTaskRunning({ state: 'completed', seedingStatus: 1 })).toBe(true)
    expect(btTaskRunning({ state: 'completed', seedingStatus: 8 })).toBe(false)
    expect(btTaskRunning({ state: 'completed', seedingStatus: 0 })).toBe(false)
    expect(btTaskRunning({ state: 'paused', seedingStatus: 1 })).toBe(false)
  })

  test('磁力带 dn= 时名称先到，总大小为 0 仍视为元数据未到', () => {
    expect(btMetadataReady({ metadataPending: true, sizeBytes: 0 })).toBe(false)
    expect(btMetadataReady({ metadataPending: false, sizeBytes: 0 })).toBe(false)
    expect(btMetadataReady({ metadataPending: false, sizeBytes: 5_000_000_000 })).toBe(true)
  })
})
