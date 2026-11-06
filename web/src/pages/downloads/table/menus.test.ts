import { Circle, CircleDot, Square, SquareCheck } from 'lucide-react'
import { describe, expect, test } from 'bun:test'
import type { DownloadTaskView } from '../model/task'
import { buildTaskMenu } from './menus'

const view = { key: 'l:t1', source: 'local', taskId: 't1', state: 'completed', protocol: 'http', fileMissing: false, kind: 'other', url: 'https://e.com/a.zip' } as unknown as DownloadTaskView
const t = (key: string) => key

describe('buildTaskMenu plugin commands', () => {
  const base = { t, views: [view], queues: [], queueName: (id: string) => id, showDetail: () => undefined }
  const command = { identity: 'a@x', pluginName: 'Alpha', command: { id: 'send', title: 'Send', palette: false, taskMenu: true } }

  test('plugin group sits between the selection group and the delete group', () => {
    const keys = buildTaskMenu({
      ...base,
      pluginCommands: [command],
      selection: { multiSelect: false, enterMultiSelect: () => undefined, selectAll: () => undefined },
    }).map((entry) => entry.key)
    const select = keys.indexOf('select-all')
    const sep = keys.indexOf('sep-plugin')
    expect(select > -1).toBe(true)
    expect(sep).toBe(select + 1)
    expect(keys[sep + 1]).toBe('plugin:a@x:send')
    expect(keys[sep + 2]).toBe('sep-delete')
  })

  test('no plugin commands leaves the menu unchanged', () => {
    const keys = buildTaskMenu(base).map((entry) => entry.key)
    expect(keys.includes('sep-plugin')).toBe(false)
    expect(keys.includes('sep-delete')).toBe(true)
  })
})

describe('buildTaskMenu ignore plugin retry', () => {
  const failed = { ...view, state: 'failed', errorMessage: '[插件] yt: boom', groupId: '' } as unknown as DownloadTaskView
  const build = (v: DownloadTaskView) =>
    buildTaskMenu({ t, views: [v], queues: [], queueName: (id: string) => id, showDetail: () => undefined }).map((entry) => entry.key)

  test('standalone failed plugin task offers ignore-retry', () => {
    expect(build(failed).includes('ignore-plugin-retry')).toBe(true)
  })

  test('task group member with plugin error does not', () => {
    const member = { ...failed, groupId: 'g1' } as unknown as DownloadTaskView
    expect(build(member).includes('ignore-plugin-retry')).toBe(false)
  })
})

describe('buildTaskMenu piece picking', () => {
  const build = (views: DownloadTaskView[]) =>
    buildTaskMenu({ t, views, queues: [], queueName: (id: string) => id, showDetail: () => undefined })
  const bt = (taskId: string, patch: Partial<DownloadTaskView> = {}) =>
    ({ ...view, taskId, key: `l:${taskId}`, state: 'downloading', url: 'magnet:?xt=urn:btih:abc', btSequential: false, btFirstLastPiecesFirst: false, ...patch }) as unknown as DownloadTaskView

  test('incomplete local BT nests order radios and the ends checkbox', () => {
    const entries = build([bt('a', { btSequential: true, btFirstLastPiecesFirst: true }), bt('b', { btSequential: true })])
    const keys = entries.map((entry) => entry.key)
    const picking = keys.indexOf('btPiecePicking')
    expect(picking >= 0).toBe(true)
    expect(keys.indexOf('copy-url')).toBe(picking + 1)
    const sub = entries[picking]
    expect(sub?.type).toBe('sub')
    if (sub?.type !== 'sub') return
    expect(sub.label).toBe('btPiecePicking')
    expect(sub.entries.map((entry) => entry.key)).toEqual(['btRarestFirst', 'btSequential', 'btFirstLastPiecesFirst'])
    const [rarest, sequential, ends] = sub.entries
    expect(rarest?.type).toBe('item')
    expect(sequential?.type).toBe('item')
    expect(ends?.type).toBe('item')
    if (rarest?.type === 'item') expect(rarest.icon).toBe(Circle)
    if (sequential?.type === 'item') expect(sequential.icon).toBe(CircleDot)
    if (ends?.type === 'item') expect(ends.icon).toBe(Square)
  })

  test('default order selects rarest; mixed order selects neither radio', () => {
    const allOff = build([bt('a'), bt('b')])
    const offSub = allOff.find((entry) => entry.key === 'btPiecePicking')
    if (offSub?.type !== 'sub') throw new Error('missing piece picking')
    const [rarest, sequential] = offSub.entries
    if (rarest?.type === 'item') expect(rarest.icon).toBe(CircleDot)
    if (sequential?.type === 'item') expect(sequential.icon).toBe(Circle)
    const mixed = build([bt('a', { btSequential: true }), bt('b')])
    const mixedSub = mixed.find((entry) => entry.key === 'btPiecePicking')
    if (mixedSub?.type !== 'sub') throw new Error('missing piece picking')
    expect(mixedSub.entries.filter((entry) => entry.type === 'item').slice(0, 2).map((entry) => entry.icon)).toEqual([Circle, Circle])
    const checked = build([bt('a', { btFirstLastPiecesFirst: true }), bt('b', { btFirstLastPiecesFirst: true })])
    const checkedSub = checked.find((entry) => entry.key === 'btPiecePicking')
    if (checkedSub?.type !== 'sub') throw new Error('missing piece picking')
    const endsOn = checkedSub.entries[2]
    if (endsOn?.type === 'item') expect(endsOn.icon).toBe(SquareCheck)
  })

  test('completed, remote, or mixed selection hides piece picking', () => {
    expect(build([bt('a', { state: 'completed' })]).some((entry) => entry.key === 'btPiecePicking')).toBe(false)
    expect(build([bt('a', { source: 'remote' })]).some((entry) => entry.key === 'btPiecePicking')).toBe(false)
    expect(build([bt('a'), { ...view, state: 'paused' } as unknown as DownloadTaskView]).some((entry) => entry.key === 'btPiecePicking')).toBe(false)
  })
})
