import { describe, expect, test } from 'bun:test'
import { canChangeSegments, canChangeUrl, canRename, canSetPiecePicking, canVerifyIntegrity, isBtUrl, isReplaceableUrl, parseTaskSegments, pieceEndsParams, pieceOptionAllOn, pieceOrder, pieceOrderTargets, pieceSequentialParams } from './taskEdit'
import type { TaskState, TaskSource } from './task'

function facts(state: TaskState, over: { source?: TaskSource; url?: string } = {}) {
  return { source: over.source ?? 'local', state, url: over.url ?? 'https://example.com/a.bin' } as const
}

describe('重命名可用性', () => {
  const named = (state: TaskState, over: { source?: TaskSource; url?: string; preparing?: boolean } = {}) => ({
    ...facts(state, over),
    preparing: over.preparing ?? false,
  })

  test('非活跃的本地 HTTP / eD2K 任务可用（含已完成）', () => {
    for (const state of ['pending', 'paused', 'failed', 'completed'] as const) {
      expect(canRename(named(state))).toBe(true)
      expect(canRename(named(state, { url: 'ed2k://|file|a.iso|1|h|/' }))).toBe(true)
    }
  })

  test('下载中 / 准备中 / 远程 / BT 不可（引擎 task-active / bt-unsupported）', () => {
    expect(canRename(named('downloading'))).toBe(false)
    expect(canRename(named('pending', { preparing: true }))).toBe(false)
    expect(canRename(named('paused', { source: 'remote' }))).toBe(false)
    for (const url of ['magnet:?xt=urn:btih:abc', 'MAGNET:?xt=urn:btih:abc', 'torrent-file://local']) {
      for (const state of ['paused', 'completed'] as const) expect(canRename(named(state, { url }))).toBe(false)
    }
  })
})

describe('更换链接可用性', () => {
  test('仅暂停 / 失败 / 等待', () => {
    for (const state of ['paused', 'failed', 'pending'] as const) expect(canChangeUrl(facts(state))).toBe(true)
    for (const state of ['downloading', 'completed'] as const) expect(canChangeUrl(facts(state))).toBe(false)
  })

  test('远程任务与非 http(s)/ftp 源不可用', () => {
    expect(canChangeUrl(facts('paused', { source: 'remote' }))).toBe(false)
    for (const url of ['magnet:?xt=urn:btih:abc', 'torrent-file://local', 'ed2k://|file|a|1|h|/', 'sftp://a/b', '']) {
      expect(canChangeUrl(facts('paused', { url }))).toBe(false)
    }
    for (const url of ['http://a/b', 'HTTPS://a/b', 'ftp://a/b']) {
      expect(canChangeUrl(facts('failed', { url }))).toBe(true)
    }
    // 引擎 `task_url_protocol` 不接受 ftps://。
    expect(canChangeUrl(facts('failed', { url: 'ftps://a/b' }))).toBe(false)
  })
})

describe('修改线程数可用性', () => {
  test('下载中也可，已完成不可', () => {
    for (const state of ['downloading', 'paused', 'failed', 'pending'] as const) {
      expect(canChangeSegments(facts(state))).toBe(true)
    }
    expect(canChangeSegments(facts('completed'))).toBe(false)
  })

  test('远程与 BT 任务不可', () => {
    expect(canChangeSegments(facts('paused', { source: 'remote' }))).toBe(false)
    expect(canChangeSegments(facts('paused', { url: 'magnet:?xt=urn:btih:abc' }))).toBe(false)
    expect(canChangeSegments(facts('paused', { url: 'torrent-file://local' }))).toBe(false)
  })
})

describe('完整性校验可用性', () => {
  const bt = 'magnet:?xt=urn:btih:abc'
  const ok = { ...facts('completed', { url: bt }), fileMissing: false }
  test('已完成 BT 可用（torrent-file 与大小写同样）', () => {
    expect(canVerifyIntegrity(ok)).toBe(true)
    expect(canVerifyIntegrity({ ...ok, url: 'TORRENT-FILE://x' })).toBe(true)
  })
  test('未完成 / 非 BT / 远程 / 文件缺失不可', () => {
    expect(canVerifyIntegrity({ ...ok, state: 'paused' })).toBe(false)
    expect(canVerifyIntegrity({ ...ok, url: 'https://a/b' })).toBe(false)
    expect(canVerifyIntegrity({ ...ok, source: 'remote' })).toBe(false)
    expect(canVerifyIntegrity({ ...ok, fileMissing: true })).toBe(false)
  })
})

describe('URL 分类', () => {
  test('BT 与可换源互斥，ed2k 两者皆非', () => {
    expect(isBtUrl('MAGNET:?x')).toBe(true)
    expect(isReplaceableUrl('MAGNET:?x')).toBe(false)
    expect(isBtUrl('ed2k://x')).toBe(false)
    expect(isReplaceableUrl('ed2k://x')).toBe(false)
  })
})

describe('线程数输入解析', () => {
  test('0..=512 的整数，空串不等同自动', () => {
    expect(parseTaskSegments('0')).toBe(0)
    expect(parseTaskSegments(' 16 ')).toBe(16)
    expect(parseTaskSegments('512')).toBe(512)
    for (const input of ['', '  ', 'abc', '-1', '513', '1.5', '99999999999999999999']) {
      expect(parseTaskSegments(input)).toBeNull()
    }
  })
})

describe('BT 选块开关', () => {
  const magnet = 'magnet:?xt=urn:btih:abc'
  const torrent = 'torrent-file://local'
  const row = (state: TaskState, over: { source?: TaskSource; url?: string; btSequential?: boolean; btFirstLastPiecesFirst?: boolean } = {}) => ({
    ...facts(state, over),
    btSequential: over.btSequential ?? false,
    btFirstLastPiecesFirst: over.btFirstLastPiecesFirst ?? false,
  })

  test('仅全部为本地未完成 BT 时显示', () => {
    expect(canSetPiecePicking([])).toBe(false)
    expect(canSetPiecePicking([row('downloading', { url: magnet })])).toBe(true)
    expect(canSetPiecePicking([row('paused', { url: torrent }), row('failed', { url: 'MAGNET:?xt=urn:btih:def' })])).toBe(true)
    expect(canSetPiecePicking([row('completed', { url: magnet })])).toBe(false)
    expect(canSetPiecePicking([row('downloading', { url: 'https://example.com/a.bin' })])).toBe(false)
    expect(canSetPiecePicking([row('downloading', { source: 'remote', url: magnet })])).toBe(false)
    expect(canSetPiecePicking([row('paused', { url: magnet }), row('downloading')])).toBe(false)
  })

  test('默认全关选最稀有，全顺序选顺序，混选与空选都不点亮', () => {
    const off = row('paused', { url: torrent })
    const on = row('downloading', { url: magnet, btSequential: true })
    expect(pieceOrder([off, off])).toBe('rarest')
    expect(pieceOrder([on, on])).toBe('sequential')
    expect(pieceOrder([on, off])).toBe('mixed')
    expect(pieceOrder([])).toBe('mixed')
  })

  test('设顺序只返回需要改的任务，参数只带 sequential', () => {
    const off = { ...row('paused', { url: torrent }), taskId: 'off' }
    const on = { ...row('downloading', { url: magnet, btSequential: true }), taskId: 'on' }
    expect(pieceOrderTargets([off, on], true).map((view) => view.taskId)).toEqual(['off'])
    expect(pieceOrderTargets([on, on], true)).toEqual([])
    expect(pieceOrderTargets([off, on], false).map((view) => view.taskId)).toEqual(['on'])
    expect(pieceSequentialParams('t1', false)).toEqual({ taskId: 't1', sequential: false })
    expect(pieceSequentialParams('t1', true)).toEqual({ taskId: 't1', sequential: true })
    expect('firstLastPiecesFirst' in pieceSequentialParams('t1', true)).toBe(false)
  })

  test('首尾勾选要求全部打开；切换只带该字段', () => {
    const on = row('downloading', { url: magnet, btFirstLastPiecesFirst: true })
    const off = row('paused', { url: torrent })
    expect(pieceOptionAllOn([on, on])).toBe(true)
    expect(pieceOptionAllOn([on, off])).toBe(false)
    expect(pieceOptionAllOn([])).toBe(false)
    expect(pieceEndsParams('t2', false)).toEqual({ taskId: 't2', firstLastPiecesFirst: true })
    expect(pieceEndsParams('t2', true)).toEqual({ taskId: 't2', firstLastPiecesFirst: false })
    expect('sequential' in pieceEndsParams('t2', true)).toBe(false)
  })
})
