import { describe, expect, test } from 'bun:test'
import type { PreviewItemDto } from '../../../lib/rpc'
import { manifestRowsAt } from './manifest'

const item = (id: string, name: string, path = ''): PreviewItemDto => ({ id, name, path, size: 1, variants: [] })

const labels = (items: PreviewItemDto[]): string[] =>
  manifestRowsAt({
    items,
    cwd: '',
    selectedItemIds: new Set(),
    extFilter: new Set(),
    search: '',
    sortKey: 'name',
  }).rows.map((r) => (r.kind === 'dir' ? `dir:${r.row.labels.join('/')}` : r.row.item.name))

describe('清单按名称自然序（与 GPUI manifest_tests 同组用例）', () => {
  test('数字文件名与目录名按数值排序', () => {
    const items = [
      item('a', '10'),
      item('b', '100'),
      item('c', '2'),
      item('d', '1'),
      item('e', '20'),
      item('f', '101'),
      item('g', 'x', '10'),
      item('h', 'x', '2'),
      item('i', 'x', '1'),
    ]
    expect(labels(items)).toEqual(['dir:1', 'dir:2', 'dir:10', '1', '2', '10', '20', '100', '101'])
  })

  test('文本中的数字段按数值，同名按 id 定序', () => {
    const items = [item('z', 'a10.bin'), item('y', 'a2.bin'), item('x', 'dup'), item('w', 'dup')]
    const rows = manifestRowsAt({
      items,
      cwd: '',
      selectedItemIds: new Set(),
      extFilter: new Set(),
      search: '',
      sortKey: 'name',
    }).rows
    expect(rows.map((r) => (r.kind === 'file' ? `${r.row.item.name}:${r.row.item.id}` : ''))).toEqual([
      'a2.bin:y',
      'a10.bin:z',
      'dup:w',
      'dup:x',
    ])
  })
})
