import { describe, expect, test } from 'bun:test'
import type { GroupDto, TaskDto } from '../../../lib/rpc'
import { CategoryIndex, builtinDefaults } from './categories'
import { SELECTION_ALL } from './filters'
import { buildVisibleRows, dirCollapseKey, dominantCategory, groupRowsByDirectory, groupsMatchingQuery, keysInVisualSpan, selectGroupOnClick } from './groupRows'
import type { BucketInfo, GroupFolderRow, VisibleRow, VisibleRowsInput } from './groupRows'
import { RowEngine } from './rowEngine'
import type { TaskRow } from './rowEngine'
import { buildLocalView, buildRemoteView } from './task'
import type { DownloadTaskView } from './task'
import { compareViews } from './viewPrefs'

const NONE: ReadonlySet<string> = new Set()

function dto(id: string, patch: Partial<TaskDto> = {}): TaskDto {
  return {
    taskId: id,
    url: `https://example.com/${id}`,
    fileName: `${id}.bin`,
    saveDir: '/d',
    status: 3,
    downloadedBytes: 0,
    totalBytes: 0,
    errorMessage: '',
    createdAt: '100',
    completedAt: '',
    proxyUrl: '',
    queueId: 'main',
    checksum: '',
    referrer: '',
    groupId: '',
    originUrl: '',
    fileMissing: false,
    seedingStatus: 0,
    uploadedBytes: 0,
    queueOrder: 0,
    ...patch,
  } as unknown as TaskDto
}

const row = (id: string, patch: Partial<TaskDto> = {}): TaskRow => ({
  type: 'task',
  key: `l:${id}`,
  view: buildLocalView(dto(id, patch), null, false, null, undefined, true),
})

const group = (id: string, patch: Partial<GroupDto> = {}): GroupDto => ({
  groupId: id,
  name: id,
  sourceUrl: '',
  saveDir: '/d/pack',
  createdAt: '100',
  ...patch,
})

const nameAsc = (left: DownloadTaskView, right: DownloadTaskView) => compareViews({ sort_key: 'name', sort_dir: 'asc' }, left, right)

function build(rows: TaskRow[], groups: GroupDto[], patch: Partial<VisibleRowsInput> = {}) {
  return buildVisibleRows({
    rows,
    groups,
    compare: nameAsc,
    bucketOf: null,
    collapsedBuckets: NONE,
    expanded: NONE,
    dirCollapsed: NONE,
    searching: false,
    ...patch,
  })
}

const describeRow = (item: VisibleRow): string => {
  switch (item.type) {
    case 'task':
      return item.key
    case 'member':
      return `${item.depth}:${item.key}`
    case 'group':
      return item.key
    case 'dir':
      return `dir:${item.path}`
    case 'bucket':
      return `# ${item.label}`
    case 'directory':
      return `directory:${item.key}`
  }
}

const keys = (rows: readonly VisibleRow[]) => rows.map(describeRow)

describe('directory row integration', () => {
  test('directory headings wrap top-level tasks and collapse descendants without changing item identity', () => {
    const visible = build([row('b', { saveDir: '/d/B' }), row('a', { saveDir: '/d/A' })], []).rows
    const items = [
      { path: '/d/B', root: '/d', scope: '', scope_label: '', count: 1 },
      { path: '/d/A', root: '/d', scope: '', scope_label: '', count: 1 },
    ]
    const expanded = groupRowsByDirectory(visible, items, NONE)
    const headers = expanded.filter((item) => item.type === 'directory')
    expect(headers.map((item) => item.type === 'directory' ? item.path : '')).toEqual(['/d', '/d/A', '/d/B'])
    expect(expanded.filter((item) => item.type === 'task').map((item) => item.key)).toEqual(['l:a', 'l:b'])
    const source = visible.find((item) => item.type === 'task' && item.key === 'l:a')
    if (!source || source.type !== 'task') throw new Error('missing source task')
    source.view = { ...source.view, downloadedBytes: 42 }
    const displayed = expanded.find((item) => item.type === 'task' && item.key === source.key)
    if (!displayed || displayed.type !== 'task') throw new Error('missing displayed task')
    expect(displayed.view.downloadedBytes).toBe(42)
    const header = headers[0]
    if (!header || header.type !== 'directory') throw new Error('missing directory header')
    const collapsed = groupRowsByDirectory(visible, items, new Set([header.key]))
    expect(collapsed).toEqual([{ ...header, collapsed: true }])
  })
})

describe('merge position', () => {
  test('group proxy lands at the partition point of the top-level order; orphan / remote stay top-level', () => {
    const remote = buildRemoteView({ id: 'r1', fileName: 'b.bin', url: 'https://e.com/b', status: 'completed', toDevice: 'x', downloadedBytes: 0, speed: 0, createdAt: '1' } as never)
    // 引擎顺序（名称升序）：a.bin b.bin(远程) c.bin m1.bin(组成员) orphan.bin z.bin。
    const rows: TaskRow[] = [
      row('a'),
      { type: 'task', key: remote.key, view: remote },
      row('c'),
      row('m1', { groupId: 'pack', fileName: 'm1.bin' }),
      row('orphan', { groupId: 'missing' }),
      row('z'),
    ]
    const built = build(rows, [group('pack', { name: 'm' })])
    // 代理名 "m" 排在 c.bin 之后、orphan.bin 之前；孤儿 groupId 与远程任务留在顶层。
    expect(keys(built.rows)).toEqual(['l:a', 'r:r1', 'l:c', 'g:pack', 'l:orphan', 'l:z'])
    expect(built.visibleCount).toBe(6)
  })

  test('several groups merge in proxy order with a monotone cursor', () => {
    const rows = [row('b'), row('x'), row('g1m', { groupId: 'g1' }), row('g2m', { groupId: 'g2' })]
    const built = build(rows, [group('g1', { name: 'a' }), group('g2', { name: 'y' })])
    expect(keys(built.rows)).toEqual(['g:g1', 'l:b', 'l:x', 'g:g2'])
  })
})

describe('filtered aggregates and search', () => {
  test('group rows only carry the members that passed the filter', () => {
    const built = build([row('a1', { groupId: 'g', status: 1, totalBytes: 100, downloadedBytes: 50 })], [group('g')])
    const folder = built.rows.find((item): item is GroupFolderRow => item.type === 'group')
    expect(folder?.members.map((member) => member.key)).toEqual(['l:a1'])
    expect(built.visibleCount).toBe(1)
  })

  test('groups matching the query are collected by lower-cased display name', () => {
    const groups = [group('g1', { name: 'Season One' }), group('g2', { name: '', saveDir: '/d/Season Two' }), group('g3', { name: 'Other' })]
    expect([...groupsMatchingQuery(groups, 'season')]).toEqual(['g1', 'g2'])
    expect(groupsMatchingQuery(groups, '').size).toBe(0)
  })

  test('engine keeps members of a matching group even when the task itself misses the query', () => {
    const engine = new RowEngine()
    const input = {
      tasks: [dto('t1', { groupId: 'g1' }), dto('t2', { groupId: 'g2' })],
      speeds: {},
      uploads: {},
      boostedId: null,
      queuePositions: [],
      runtimes: {},
      connected: true,
      remote: [],
      selection: SELECTION_ALL,
      query: 'zzz',
      groupMatches: new Set(['g1']),
      categories: new CategoryIndex(builtinDefaults()),
      sortKey: 'name' as const,
      sortDir: 'asc' as const,
    }
    const snapshot = engine.update(input, { now: 0, lastPointerAt: Number.NEGATIVE_INFINITY, menuOpen: false, force: true })
    expect(snapshot.rows.map((item) => item.key)).toEqual(['l:t1'])
  })

  test('searching forces every group open', () => {
    const rows = [row('m1', { groupId: 'g' })]
    expect(keys(build(rows, [group('g')]).rows)).toEqual(['g:g'])
    expect(keys(build(rows, [group('g')], { searching: true }).rows)).toEqual(['g:g', '1:l:m1'])
  })
})

describe('expansion and selection sequence', () => {
  const rows = [
    row('root', { groupId: 'g', saveDir: '/d/pack', fileName: 'root.bin' }),
    row('deep', { groupId: 'g', saveDir: '/d/pack/sub', fileName: 'deep.bin' }),
    row('solo'),
  ]
  const groups = [group('g', { name: 'a' })]

  test('collapsed group expands to its members in flatten order inside the sequence', () => {
    const built = build(rows, groups)
    expect(keys(built.rows)).toEqual(['g:g', 'l:solo'])
    expect(built.sequence()).toEqual(['l:root', 'l:deep', 'l:solo'])
    expect(built.visibleCount).toBe(3)
  })

  test('expanded group lists root members then directory rows; collapsed directories hide their members', () => {
    const open = build(rows, groups, { expanded: new Set(['g']) })
    expect(keys(open.rows)).toEqual(['g:g', '1:l:root', 'dir:sub', '2:l:deep', 'l:solo'])
    expect(open.sequence()).toEqual(['l:root', 'l:deep', 'l:solo'])
    const folded = build(rows, groups, { expanded: new Set(['g']), dirCollapsed: new Set([dirCollapseKey('g', 'sub')]) })
    expect(keys(folded.rows)).toEqual(['g:g', '1:l:root', 'dir:sub', 'l:solo'])
    expect(folded.sequence()).toEqual(['l:root', 'l:solo'])
    expect(folded.visibleCount).toBe(2)
  })

  test('members of a collapsed bucket are not part of the sequence', () => {
    const bucketOf = (view: DownloadTaskView): BucketInfo => ({ key: `b:${view.groupId === '' ? 'single' : 'group'}`, label: view.groupId === '' ? 'single' : 'group', order: view.groupId === '' ? 1 : 0 })
    const built = build(rows, groups, { bucketOf, collapsedBuckets: new Set(['b:group']) })
    expect(keys(built.rows)).toEqual(['# group', '# single', 'l:solo'])
    expect(built.rows[0]).toMatchObject({ count: 2, collapsed: true })
    expect(built.sequence()).toEqual(['l:solo'])
  })

  test('type buckets can use the dominant member category for a group', () => {
    const bucketOf = (view: DownloadTaskView, members: readonly DownloadTaskView[] | null): BucketInfo => {
      const id = members === null ? view.kind : (dominantCategory(members, (member) => member.kind, () => 0) ?? view.kind)
      return { key: `type:${id}`, label: id, order: 0 }
    }
    const typed = [
      row('v1', { groupId: 'g', fileName: 'a.mp4' }),
      row('v2', { groupId: 'g', fileName: 'b.mkv' }),
      row('d1', { groupId: 'g', fileName: 'c.pdf' }),
    ]
    const built = build(typed, groups, { bucketOf })
    expect(keys(built.rows)).toEqual(['# video', 'g:g'])
    expect(built.rows[0]).toMatchObject({ count: 3 })
  })
})

describe('dominant category', () => {
  const view = (kind: string, sizeBytes: number) => ({ kind, sizeBytes }) as unknown as DownloadTaskView
  const order = (id: string) => ['video', 'audio', 'doc'].indexOf(id)

  test('most members wins', () => {
    expect(dominantCategory([view('audio', 1), view('video', 1), view('audio', 1)], (m) => m.kind, order)).toBe('audio')
  })
  test('a tie goes to the larger byte total, then to the earlier rule', () => {
    expect(dominantCategory([view('audio', 5), view('video', 9)], (m) => m.kind, order)).toBe('video')
    expect(dominantCategory([view('audio', 5), view('video', 5)], (m) => m.kind, order)).toBe('video')
    expect(dominantCategory([view('doc', 5), view('audio', 5)], (m) => m.kind, order)).toBe('audio')
  })
  test('no members', () => {
    expect(dominantCategory([], (m) => m.kind, order)).toBeNull()
  })
})

describe('group click selection', () => {
  const members = ['l:a', 'l:b']
  const sequence = ['l:x', 'l:a', 'l:b', 'l:y']
  const state = (selected: string[], anchor: string | null = null, multiSelect = false) => ({ selected: new Set(selected), multiSelect, anchor })

  test('plain click selects the members, leaves multi-select, anchors at the last member and focuses the group', () => {
    const result = selectGroupOnClick(state(['l:x'], 'l:x', true), 'g', members, sequence, { shift: false, secondary: false })
    expect([...result.selected]).toEqual(members)
    expect(result).toMatchObject({ multiSelect: false, anchor: 'l:b', focusedGroup: 'g' })
  })

  test('secondary click adds all members, or removes them when already fully selected', () => {
    const add = selectGroupOnClick(state(['l:x', 'l:a']), 'g', members, sequence, { shift: false, secondary: true })
    expect([...add.selected].sort()).toEqual(['l:a', 'l:b', 'l:x'])
    expect(add).toMatchObject({ multiSelect: true, focusedGroup: null })
    const remove = selectGroupOnClick(state(['l:x', 'l:a', 'l:b']), 'g', members, sequence, { shift: false, secondary: true })
    expect([...remove.selected]).toEqual(['l:x'])
  })

  test('drag span keeps tasks in row order and expands a group row inside the span', () => {
    const group = {
      type: 'group',
      orderedKeys: () => ['l:a', 'l:b'],
    } as unknown as GroupFolderRow
    const rows: VisibleRow[] = [
      { type: 'task', key: 'l:x', view: {} as DownloadTaskView },
      { type: 'bucket', key: 's', label: 'S', count: 1, collapsed: false },
      group,
      { type: 'member', key: 'l:b', row: { type: 'task', key: 'l:b', view: {} as DownloadTaskView }, depth: 1 },
      { type: 'task', key: 'l:y', view: {} as DownloadTaskView },
    ]
    expect(keysInVisualSpan(rows, 4, 0)).toEqual(['l:x', 'l:a', 'l:b', 'l:y'])
    expect(keysInVisualSpan(rows, 3, 3)).toEqual(['l:b'])
  })

  test('shift click ranges from the anchor to the last member over the visible sequence', () => {
    const result = selectGroupOnClick(state(['l:x'], 'l:x'), 'g', members, sequence, { shift: true, secondary: false })
    expect([...result.selected]).toEqual(['l:x', 'l:a', 'l:b'])
    expect(result.anchor).toBe('l:b')
    expect(result.focusedGroup).toBeNull()
  })
})
