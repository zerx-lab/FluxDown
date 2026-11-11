import { describe, expect, test } from 'bun:test'
import { buildSeedLocalParams, pieceLengthLabel, SEED_LOCAL_FORM_DEFAULTS, SEED_PIECE_LENGTHS, seedLocalFeedback } from './seedLocal'

const base = SEED_LOCAL_FORM_DEFAULTS

describe('buildSeedLocalParams', () => {
  test('空路径（含纯空白）报 pathRequired', () => {
    expect(buildSeedLocalParams(base)).toEqual({ ok: false, error: 'pathRequired' })
    expect(buildSeedLocalParams({ ...base, path: '  \t' })).toEqual({ ok: false, error: 'pathRequired' })
  })

  test('路径去空白，省略空的可选字段', () => {
    expect(buildSeedLocalParams({ ...base, path: ' /data/a ' })).toEqual({ ok: true, params: { path: '/data/a', unlimited: false } })
  })

  test('tracker 按行拆分、去空白；空行分隔层级（首尾 / 连续空行只算一个）', () => {
    const result = buildSeedLocalParams({ ...base, path: '/d', trackers: '\n udp://a:1/announce \r\nudp://c/announce\n\n  \nhttp://b/announce\n\n', unlimited: true })
    expect(result).toEqual({ ok: true, params: { path: '/d', unlimited: true, trackers: ['udp://a:1/announce', 'udp://c/announce', '', 'http://b/announce'] } })
    expect(buildSeedLocalParams({ ...base, path: '/d', trackers: ' \n\n ' })).toEqual({ ok: true, params: { path: '/d', unlimited: false } })
  })

  test('指定 torrentPath 时忽略 trackers', () => {
    const result = buildSeedLocalParams({ ...base, path: '/d', torrentPath: ' /t.torrent ', trackers: 'udp://a' })
    expect(result).toEqual({ ok: true, params: { path: '/d', torrentPath: '/t.torrent', unlimited: false } })
  })

  // 镜像 native/protocol/tests/protocol/bt_ops_wire.rs::seed_local_torrent_options_default_to_todays_behaviour
  test('默认种子选项不上线：旧形状原样', () => {
    expect(buildSeedLocalParams({ ...base, path: '/data/movie.mkv' })).toEqual({ ok: true, params: { path: '/data/movie.mkv', unlimited: false } })
  })

  test('全部种子选项按 wire 名上线', () => {
    const result = buildSeedLocalParams({
      ...base,
      path: '/data/show',
      version: 'hybrid',
      private: true,
      pieceLength: 4 * 1024 * 1024,
      webSeeds: ' https://mirror.example/show/ \n\n',
      comment: ' c ',
      source: 'SITE',
      superSeeding: true,
      torrentOutput: ' /data/show.torrent ',
      startSeeding: false,
    })
    expect(result).toEqual({
      ok: true,
      params: {
        path: '/data/show',
        unlimited: false,
        version: 'hybrid',
        private: true,
        pieceLength: 4_194_304,
        webSeeds: ['https://mirror.example/show/'],
        comment: 'c',
        source: 'SITE',
        // 超级做种只对会建任务的提交有意义：只生成种子文件时不上线（界面同时隐藏该开关）。
        torrentOutput: '/data/show.torrent',
        createOnly: true,
      },
    })
  })

  test('立即做种时超级做种与种子文件路径一起上线', () => {
    const result = buildSeedLocalParams({ ...base, path: '/d', superSeeding: true, torrentOutput: '/d.torrent' })
    expect(result).toEqual({ ok: true, params: { path: '/d', unlimited: false, superSeeding: true, torrentOutput: '/d.torrent' } })
  })

  test('不立即做种又没给种子文件路径 → outputRequired', () => {
    expect(buildSeedLocalParams({ ...base, path: '/d', startSeeding: false, torrentOutput: '   ' })).toEqual({ ok: false, error: 'outputRequired' })
  })

  test('现成 .torrent：种子选项全部忽略，只带超级做种与 skipCheck', () => {
    const form = { ...base, path: '/d', torrentPath: '/t.torrent', version: 'v2' as const, private: true, startSeeding: false, skipCheck: true }
    expect(buildSeedLocalParams(form)).toEqual({ ok: true, params: { path: '/d', torrentPath: '/t.torrent', unlimited: false, skipCheck: true } })
    expect(buildSeedLocalParams({ ...form, superSeeding: true })).toEqual({
      ok: true,
      params: { path: '/d', torrentPath: '/t.torrent', unlimited: false, superSeeding: true, skipCheck: true },
    })
  })

  test('现场生成时 skipCheck 不上线', () => {
    expect(buildSeedLocalParams({ ...base, path: '/d', skipCheck: true })).toEqual({ ok: true, params: { path: '/d', unlimited: false } })
  })
})

describe('分片大小选项', () => {
  test('自动 + 16 KiB … 16 MiB 的 2 的幂', () => {
    expect(SEED_PIECE_LENGTHS[0]).toBe(0)
    expect(SEED_PIECE_LENGTHS.slice(1).map(pieceLengthLabel)).toEqual([
      '16 KiB', '32 KiB', '64 KiB', '128 KiB', '256 KiB', '512 KiB', '1 MiB', '2 MiB', '4 MiB', '8 MiB', '16 MiB',
    ])
    for (const bytes of SEED_PIECE_LENGTHS.slice(1)) expect(bytes >= 16_384 && (bytes & (bytes - 1)) === 0).toBe(true)
  })
})

describe('seedLocalFeedback', () => {
  test('非重复 → 成功', () => {
    expect(seedLocalFeedback({})).toEqual({ key: 'seedLocalStarted', level: 'success' })
  })
  test('重复带名称 → 警告带 name', () => {
    expect(seedLocalFeedback({ duplicate: true, existingName: 'a' })).toEqual({ key: 'duplicateTorrentToast', level: 'warning', params: { name: 'a' } })
  })
  test('重复无名称 → Unnamed', () => {
    expect(seedLocalFeedback({ duplicate: true })).toEqual({ key: 'duplicateTorrentToastUnnamed', level: 'warning' })
    expect(seedLocalFeedback({ duplicate: true, existingName: '' })).toEqual({ key: 'duplicateTorrentToastUnnamed', level: 'warning' })
  })
  test('只生成种子文件 → 已保存到（结果路径优先，旧 daemon 缺省回退请求值）', () => {
    const request = { createOnly: true, torrentOutput: '/req.torrent' }
    expect(seedLocalFeedback({ torrentPath: '/out.torrent' }, request)).toEqual({ key: 'btCreateSaved', level: 'success', params: { path: '/out.torrent' } })
    expect(seedLocalFeedback({}, request)).toEqual({ key: 'btCreateSaved', level: 'success', params: { path: '/req.torrent' } })
  })
})
