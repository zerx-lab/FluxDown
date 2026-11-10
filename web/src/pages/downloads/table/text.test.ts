import { describe, expect, test } from 'bun:test'
import type { DownloadTaskView } from '../model/task'
import { cardMeta, fileCellMeta, relaxedMeta } from './text'

const t = (key: string) => key
const view = (patch: Partial<DownloadTaskView>) =>
  ({ state: 'completed', kind: 'video', sizeBytes: 0, downloadedBytes: 0, speed: null, url: '', referrer: '', site: '', ...patch }) as DownloadTaskView

describe('relaxedMeta（宽松密度副标题）', () => {
  test('已完成：大小 · 类别 · 站点，未知字段省略', () => {
    expect(relaxedMeta(t, view({ sizeBytes: 2048, site: 'example.com' }))).toBe('2.0 KB · categoryVideo · example.com')
    expect(relaxedMeta(t, view({}))).toBe('categoryVideo')
  })

  test('未完成：已下 / 总量，仅下载中且速度 > 0 才带速度，不含百分比与剩余时间', () => {
    const base = { state: 'downloading' as const, sizeBytes: 2048, downloadedBytes: 1024, site: 'example.com' }
    expect(relaxedMeta(t, view({ ...base, speed: 512 }))).toBe('1.0 KB / 2.0 KB · 512 B/s · example.com')
    expect(relaxedMeta(t, view({ ...base, speed: 0 }))).toBe('1.0 KB / 2.0 KB · example.com')
    expect(relaxedMeta(t, view({ ...base, state: 'paused', speed: 512 }))).toBe('1.0 KB / 2.0 KB · example.com')
  })

  test('总量未知只给已下', () => {
    expect(relaxedMeta(t, view({ state: 'paused', downloadedBytes: 1024 }))).toBe('1.0 KB')
  })
})

describe('隐藏任务来源（hideSource）', () => {
  test('relaxedMeta 已完成：去掉站点，大小与类别保留', () => {
    const done = view({ sizeBytes: 2048, site: 'example.com' })
    expect(relaxedMeta(t, done, false)).toBe('2.0 KB · categoryVideo · example.com')
    expect(relaxedMeta(t, done, true)).toBe('2.0 KB · categoryVideo')
  })

  test('relaxedMeta 未完成：去掉站点，已下 / 总量与速度保留', () => {
    const running = view({ state: 'downloading', sizeBytes: 2048, downloadedBytes: 1024, speed: 512, site: 'example.com' })
    expect(relaxedMeta(t, running, false)).toBe('1.0 KB / 2.0 KB · 512 B/s · example.com')
    expect(relaxedMeta(t, running, true)).toBe('1.0 KB / 2.0 KB · 512 B/s')
  })

  test('来源取 referrer host 优先：隐藏时同样不出现', () => {
    const referred = view({ sizeBytes: 2048, site: 'cdn.example.net', referrer: 'https://github.com/a/b' })
    expect(relaxedMeta(t, referred, false)).toBe('2.0 KB · categoryVideo · github.com')
    expect(relaxedMeta(t, referred, true)).toBe('2.0 KB · categoryVideo')
  })

  test('fileCellMeta：类别 · 站点，隐藏时只留类别；站点为空时两种都只有类别', () => {
    expect(fileCellMeta(t, view({ site: 'example.com' }), false)).toBe('categoryVideo · example.com')
    expect(fileCellMeta(t, view({ site: 'example.com' }), true)).toBe('categoryVideo')
    expect(fileCellMeta(t, view({}), false)).toBe('categoryVideo')
  })

  test('cardMeta：类别 · 站点 · 大小，隐藏时去掉站点', () => {
    const card = view({ sizeBytes: 2048, site: 'example.com' })
    expect(cardMeta(t, card, false)).toBe('categoryVideo · example.com · 2.0 KB')
    expect(cardMeta(t, card, true)).toBe('categoryVideo · 2.0 KB')
  })
})
