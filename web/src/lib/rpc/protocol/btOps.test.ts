// BT 任务操作 / 连接设置镜像：方法名、配置键与 Rust 源码逐字一致；JSON 形状与
// native/protocol/tests/protocol/bt_ops_wire.rs 同组用例。
import { describe, expect, test } from 'bun:test'
import { normalizeTask } from '../normalize'
import { daemonConfigField } from './config'
import { METHOD } from './method'
import {
  BT_FILE_PRIORITY_HIGH,
  BT_FILE_PRIORITY_MAXIMUM,
  BT_FILE_PRIORITY_NORMAL,
  BT_FILE_PRIORITY_SKIP,
} from './params'
import type { DaemonSeedLocalParams, MoveTaskStorageParams, RenameBtFileParams, SetTaskBtLimitsParams, SetTaskFilePrioritiesParams, SetTaskSuperSeedingParams } from './params'
import type { BtFilesDto, BtPortStatusDto, BtStatusDto, BtTrackerDto, BtWebSeedDto, SeedLocalResult, TorrentShareDto, TaskDtoWire } from './task'

// tsconfig 未引入 Bun 类型，仅声明测试用到的最小接口。
declare const Bun: { file(path: URL): { text(): Promise<string> } }

const read = (relative: string) => Bun.file(new URL(`../../../../../native/protocol/src/${relative}`, import.meta.url)).text()

/** JSON 往返：确认对象可序列化且形状不变（TS 类型检查负责字段名）。 */
const roundTrip = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T

const NEW_METHODS = [
  'DAEMON_TASK_BT_FILES',
  'DAEMON_TASK_SET_FILE_PRIORITIES',
  'DAEMON_TASK_RENAME_BT_FILE',
  'DAEMON_TASK_RECHECK',
  'DAEMON_TASK_MOVE_STORAGE',
  'DAEMON_TASK_SET_BT_LIMITS',
  'DAEMON_TASK_SET_SUPER_SEEDING',
  'DAEMON_TASK_REANNOUNCE',
  'DAEMON_BT_PORT_STATUS',
] as const

const NEW_CONFIG_KEYS = [
  'bt_enable_lsd',
  'bt_utp_congestion',
  'bt_max_connections',
  'bt_max_connections_per_torrent',
  'bt_enable_webseed',
  'bt_announce_all_tiers',
  'bt_announce_all_trackers',
  'bt_proxy_enabled',
  'bt_proxy_source',
  'bt_proxy_peers',
  'bt_i2p_enabled',
  'bt_i2p_sam_host',
  'bt_i2p_sam_port',
  'bt_i2p_mixed',
] as const

describe('BT 任务操作镜像与 Rust 对齐', () => {
  test('方法常量与 method.rs 逐字一致', async () => {
    const source = await read('method.rs')
    for (const name of NEW_METHODS) {
      const wire = new RegExp(`pub const ${name}: &str = "([^"]+)"`).exec(source)?.[1]
      expect(wire === undefined).toBe(false)
      expect(METHOD[name]).toBe(wire as string)
    }
  })

  test('新增 BT 配置键的类型 / 默认值 / 范围与 daemon_config.rs 一致', async () => {
    const source = await read('daemon_config.rs')
    for (const key of NEW_CONFIG_KEYS) {
      const match = new RegExp(`field\\(\\s*"${key}",\\s*([\\s\\S]*?),\\s*"([^"]*)",?\\s*\\)`).exec(source)
      expect(match === null).toBe(false)
      const kind = match?.[1] ?? ''
      const field = daemonConfigField(key)
      expect(field?.default).toBe(match?.[2] as string)
      if (kind.includes('Bool')) expect(field?.kind).toBe('bool')
      else if (kind.includes('Text')) expect(field?.kind).toBe('text')
      else if (kind.includes('Enum')) {
        expect(field?.kind).toBe('enum')
        const constName = /Enum\((\w+)\)/.exec(kind)?.[1] ?? ''
        const list = new RegExp(`pub const ${constName}: &\\[&str\\] = &\\[([^\\]]*)\\]`).exec(source)?.[1] ?? ''
        expect(field?.options).toEqual([...list.matchAll(/"([^"]+)"/g)].map((m) => m[1] as string))
      } else {
        expect(field?.kind).toBe('integer')
        expect(field?.min).toBe(Number((/min:\s*([\d_]+)/.exec(kind)?.[1] ?? '').replaceAll('_', '')))
        expect(field?.max).toBe(Number((/max:\s*([\d_]+)/.exec(kind)?.[1] ?? '').replaceAll('_', '')))
      }
    }
  })

  test('文件优先级档位对齐 qBittorrent', () => {
    expect([BT_FILE_PRIORITY_SKIP, BT_FILE_PRIORITY_NORMAL, BT_FILE_PRIORITY_HIGH, BT_FILE_PRIORITY_MAXIMUM]).toEqual([0, 1, 6, 7])
  })

  test('参数 camelCase、未设置字段不上线', () => {
    const priorities: SetTaskFilePrioritiesParams = { taskId: 't1', priorities: [{ index: 2, priority: BT_FILE_PRIORITY_HIGH }] }
    expect(roundTrip(priorities)).toEqual({ taskId: 't1', priorities: [{ index: 2, priority: 6 }] })
    const rename: RenameBtFileParams = { taskId: 't1', index: 0, path: '字幕/第 1 集.srt' }
    expect(roundTrip(rename)).toEqual({ taskId: 't1', index: 0, path: '字幕/第 1 集.srt' })
    const moved: MoveTaskStorageParams = { taskId: 't1', saveDir: 'D:\\下载 2024\\  深层' }
    expect(roundTrip(moved)).toEqual({ taskId: 't1', saveDir: 'D:\\下载 2024\\  深层' })
    const limits: SetTaskBtLimitsParams = { taskId: 't1', downloadLimitBps: 0 }
    expect(roundTrip(limits)).toEqual({ taskId: 't1', downloadLimitBps: 0 })
    const superSeeding: SetTaskSuperSeedingParams = { taskId: 't1', enabled: true }
    expect(roundTrip(superSeeding)).toEqual({ taskId: 't1', enabled: true })
  })

  test('文件表与端口状态往返（> 2^32 的字节数保持精确）', () => {
    const files: BtFilesDto = {
      editable: true,
      files: [{ index: 3, path: 'Season 1/E01.mkv', size: 5_000_000_000, downloaded: 4_294_967_296, priority: BT_FILE_PRIORITY_MAXIMUM }],
    }
    expect(roundTrip(files)).toEqual(files)

    const idle: BtPortStatusDto = roundTrip({ running: false })
    expect(idle.listenPort ?? 0).toBe(0)
    expect(idle.mappings ?? []).toEqual([])
    const status: BtPortStatusDto = {
      running: true,
      listenPort: 6881,
      mappings: [
        { protocol: 'tcp', internalPort: 6881, externalPort: 50123, method: 'natPmp', externalIp: '203.0.113.7', state: 'mapped' },
        { protocol: 'udp', internalPort: 6881, state: 'failed', message: 'no gateway answered' },
      ],
    }
    const value = roundTrip(status)
    expect(value.mappings?.[0]?.externalPort).toBe(50123)
    expect(value.mappings?.[1]?.externalPort).toBeUndefined()
  })

  test('旧 daemon 回复缺新字段：可选字段落到「未设置」', () => {
    const tracker: BtTrackerDto = roundTrip({ url: 'udp://tracker.example:6969/announce', source: 'torrent', status: 'working' })
    expect(tracker.tier ?? 0).toBe(0)
    expect([tracker.seeders, tracker.leechers, tracker.downloaded]).toEqual([undefined, undefined, undefined])

    const status: BtStatusDto = roundTrip({
      dhtEnabled: true,
      peerCounts: { live: 0, connecting: 0, queued: 0, seen: 0, dead: 0 },
      trackers: [],
      peers: [],
    })
    expect(status.infoHash ?? '').toBe('')
    expect(status.infoHashV2 ?? '').toBe('')
    expect(status.webSeeds ?? []).toEqual([])

    const share: TorrentShareDto = roundTrip({ infoHash: 'aa', name: 'n', magnet: 'magnet:?' })
    expect(share.infoHashV2 ?? '').toBe('')

    const seeded: SeedLocalResult = roundTrip({ taskId: 't1' })
    expect(seeded.torrentPath ?? '').toBe('')
  })

  test('tracker 统计数（0 是事实）与 HTTP 源原样携带', () => {
    const tracker: BtTrackerDto = { url: 'https://tracker.example/announce', source: 'torrent', status: 'unknown', tier: 2, seeders: 0, leechers: 41, downloaded: 1_000_000 }
    expect(roundTrip(tracker)).toEqual(tracker)
    const seed: BtWebSeedDto = { url: 'https://mirror.example/pub/', state: 'backingOff', message: 'HTTP 503' }
    const value = roundTrip(seed)
    expect(value.state).toBe('backingOff')
    expect(value.downloaded).toBeUndefined()
  })

  test('本机做种的种子选项字段名', () => {
    const full: DaemonSeedLocalParams = {
      path: '/data/show',
      unlimited: false,
      version: 'hybrid',
      private: true,
      pieceLength: 4 * 1024 * 1024,
      webSeeds: ['https://mirror.example/show/'],
      comment: 'c',
      source: 'SITE',
      superSeeding: true,
      torrentOutput: '/data/show.torrent',
      createOnly: true,
    }
    const value = roundTrip(full)
    expect(value.pieceLength).toBe(4_194_304)
    expect(value.webSeeds?.[0]).toBe('https://mirror.example/show/')
    expect(value.torrentOutput).toBe('/data/show.torrent')
    expect(value.createOnly).toBe(true)
  })

  test('任务行：BT 限速 / 超级做种缺省补默认值，设置值原样保留', () => {
    const wire: TaskDtoWire = {
      taskId: 't1',
      url: 'magnet:?xt=urn:btih:aa',
      fileName: 'f',
      saveDir: '/d',
      status: 1,
      downloadedBytes: 0,
      totalBytes: 0,
      createdAt: '0',
    }
    const task = normalizeTask(wire)
    expect(task.btDownloadLimitBps).toBe(0)
    expect(task.btSuperSeeding).toBe(false)
    const set = normalizeTask({ ...wire, btDownloadLimitBps: 512_000, btSuperSeeding: true })
    expect(set.btDownloadLimitBps).toBe(512_000)
    expect(set.btSuperSeeding).toBe(true)
  })
})
