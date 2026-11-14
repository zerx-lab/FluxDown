import { describe, expect, test } from 'bun:test'
import type { CustomCategoryDto } from '../../../lib/rpc'
import { CategoryIndex, builtinDefaults } from './categories'
import { SidebarCounts } from './counts'
import { LOCAL_DEVICE, STATUS_FILTERS, filterMatches } from './filters'
import type { DownloadFilter } from './filters'
import type { DownloadTaskView, TaskState } from './task'

const STATES: TaskState[] = ['pending', 'downloading', 'paused', 'completed', 'failed']

const custom = (
  id: string,
  patch: { matchMode?: string; extensions?: string[]; regexPattern?: string },
): CustomCategoryDto => ({
  id,
  name: id,
  saveDir: '',
  icon: 'file',
  matchMode: 'extension',
  extensions: [],
  regexPattern: '',
  urlPattern: '',
  position: 100,
  visible: true,
  isBuiltin: false,
  builtinType: null,
  ...patch,
})

const view = (
  n: number,
  name: string,
  state: TaskState,
  source: 'local' | 'remote',
  queueId: string,
  toDevice: string,
  seedingStatus = 0,
) => ({ key: `${source}:${n}`, name, state, source, queueId, toDevice, seedingStatus, fileMissing: false }) as unknown as DownloadTaskView

describe('SidebarCounts', () => {
  // 重复 id、扩展名重叠、正则、other：一次扫描必须与逐项 filterMatches 计数相同。
  const dtos = [
    ...builtinDefaults(),
    custom('zips', { extensions: ['zip', '.7z'] }),
    custom('zips', { extensions: ['rar'] }),
    custom('logs', { matchMode: 'regex', regexPattern: '^log-\\d+' }),
    custom('overlap', { extensions: ['zip', 'mp4'] }),
  ]
  const categories = new CategoryIndex(dtos)

  test('单次扫描与逐项 filterMatches 计数逐一相同', () => {
    const names = ['a.zip', 'b.7z', 'c.rar', 'd.mp4', 'log-12.txt', 'readme', 'e.iso', 'f.pdf', 'g.exe', 'h.mp3', 'i.torrent']
    const views: DownloadTaskView[] = []
    for (let n = 0; n < 220; n += 1) {
      views.push(
        view(
          n,
          names[n % names.length] as string,
          STATES[(n * 7) % STATES.length] as TaskState,
          n % 5 === 0 ? 'remote' : 'local',
          n % 3 === 0 ? 'main' : 'later',
          `dev${n % 4}`,
          (n * 3) % 9,
        ),
      )
    }
    const counts = new SidebarCounts(views, categories)
    const filters: DownloadFilter[] = []
    for (const status of STATUS_FILTERS) {
      filters.push({ status, category: null })
      for (const rule of categories.rules) filters.push({ status, category: rule.dto.id })
      filters.push({ status, category: 'unknown-category' })
    }
    for (const filter of filters) {
      const expected = views.filter((item) => filterMatches(filter, item, categories)).length
      expect(counts.filter(filter)).toBe(expected)
    }
    for (const queueId of ['main', 'later', 'missing']) {
      expect(counts.queue(queueId)).toBe(views.filter((item) => item.source === 'local' && item.queueId === queueId).length)
    }
    expect(counts.device(LOCAL_DEVICE)).toBe(views.filter((item) => item.source === 'local').length)
    for (const device of ['dev0', 'dev1', 'dev2', 'dev3', 'none']) {
      expect(counts.device(device)).toBe(views.filter((item) => item.source === 'remote' && item.toDevice === device).length)
    }
  })

  test('explicit category override replaces rule classification in filters and counts', () => {
    const overridden = { ...view(999, 'movie.zip', 'paused', 'local', '', ''), dto: { properties: { category: 'builtin_video' } } } as unknown as DownloadTaskView
    expect(filterMatches({ status: 'all', category: 'builtin_video' }, overridden, categories)).toBe(true)
    expect(filterMatches({ status: 'all', category: 'builtin_archive' }, overridden, categories)).toBe(false)
    const counts = new SidebarCounts([overridden], categories)
    expect(counts.filter({ status: 'all', category: 'builtin_video' })).toBe(1)
    expect(counts.filter({ status: 'all', category: 'builtin_archive' })).toBe(0)
  })

  test('forEachMatch 与逐 id matches 等价：other 仅在没有具体分类命中时命中', () => {
    for (const name of ['a.zip', 'readme', 'log-1.txt', 'x.mp4', 'noext.']) {
      const item = view(1, name, 'completed', 'local', 'main', '')
      const emitted: string[] = []
      categories.forEachMatch(item, (id) => emitted.push(id))
      const ids = [...new Set(categories.rules.map((rule) => rule.dto.id))]
      expect(emitted.slice().sort()).toEqual(ids.filter((id) => categories.matches(id, item)).sort())
    }
  })
})
