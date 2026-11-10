// 任务表可见行管线（纯函数）：在行引擎「排序 + 行顺序保持 + 侧栏筛选 + 搜索」之后，把任务组并成「文件夹行」、
// 按分组方式分桶、展开成员 / 目录行，并给出选择用的可见任务序列。与 GPUI 的同名管线逐条对齐
// （规格：任务组文件夹行 §1 / §2）。

import type { GroupDto } from '../../../lib/rpc'
import { aggregateOf, displayName, flatten, groupSortView, memberDir } from './groupTree'
import type { TreeMember } from './groupTree'
import type { TaskRow } from './rowEngine'
import type { DownloadTaskView, RowKey } from './task'

/** 分桶标题的归属信息。 */
export interface BucketInfo {
  key: string
  label: string
  order: number
}

/** 分组方式的桶标题行。 */
export interface BucketRow {
  type: 'bucket'
  key: string
  label: string
  /** 桶内任务数（组计其通过成员数）。 */
  count: number
  collapsed: boolean
}

/** 任务组「文件夹行」。成员行 / 目录行（展开时）紧随其后。 */
export interface GroupFolderRow {
  type: 'group'
  /** `g:<groupId>`。 */
  key: string
  groupId: string
  group: GroupDto
  /** 显示名。 */
  name: string
  /** 通过侧栏筛选 + 搜索的成员（引擎顺序；行对象稳定，`view` 随进度原位更新）。 */
  members: readonly TaskRow[]
  expanded: boolean
  /** 通过成员行，按 flatten 顺序（所有目录视为展开；进度柱状图的成员序）；惰性计算并缓存。 */
  orderedRows: () => readonly TaskRow[]
  /** 通过成员键，同上顺序。最后一个即「组的最后一个成员」。 */
  orderedKeys: () => readonly RowKey[]
}

/** 展开组内的成员行：与普通任务行同构，`depth` = 缩进级数（组根成员 1，目录下成员 2）。 */
export interface MemberRow {
  type: 'member'
  key: RowKey
  row: TaskRow
  depth: 1 | 2
}

/** 展开组内的目录分段行（平铺，显示完整相对路径）。 */
export interface DirRow {
  type: 'dir'
  /** `d:<groupId>\0<path>`。 */
  key: string
  groupId: string
  path: string
  count: number
  sizeBytes: number
  collapsed: boolean
}

/** 表格可见行：顶层任务行（引擎持有的稳定行对象）、桶标题、组行及其展开内容。 */
export type VisibleRow = TaskRow | BucketRow | GroupFolderRow | MemberRow | DirRow

/**
 * 划选覆盖的任务键：闭区间 `[from, to]` 内的任务行 / 成员行，以及落在区间内的组行的全部成员
 * （折叠组没有单独的成员行）。桶标题与目录行不贡献任务。顺序为行序，重复键只保留第一次。
 */
/** 锚点任务所在的可见行：优先任务行 / 成员行，否则落在包含该成员的组行。 */
export function rowIndexOfKey(rows: readonly VisibleRow[], key: RowKey): number {
  let groupHit = -1
  for (let index = 0; index < rows.length; index += 1) {
    const row = rows[index]
    if (!row) continue
    if ((row.type === 'task' || row.type === 'member') && row.key === key) return index
    if (row.type === 'group' && groupHit < 0 && row.orderedKeys().includes(key)) groupHit = index
  }
  return groupHit
}

export function keysInVisualSpan(rows: readonly VisibleRow[], from: number, to: number): RowKey[] {
  if (rows.length === 0) return []
  const start = Math.max(0, Math.min(from, to))
  const end = Math.min(rows.length - 1, Math.max(from, to))
  const keys: RowKey[] = []
  const seen = new Set<RowKey>()
  const add = (key: RowKey) => {
    if (seen.has(key)) return
    seen.add(key)
    keys.push(key)
  }
  for (let index = start; index <= end; index += 1) {
    const row = rows[index]
    if (!row) continue
    if (row.type === 'task' || row.type === 'member') add(row.key)
    else if (row.type === 'group') for (const key of row.orderedKeys()) add(key)
  }
  return keys
}

export const groupRowKey = (groupId: string): string => `g:${groupId}`

/** 目录折叠状态的集合键。 */
export const dirCollapseKey = (groupId: string, dir: string): string => `${groupId}\u0000${dir}`

export interface VisibleRowsInput {
  /** 引擎行（已通过筛选 + 搜索，保持排序与保持期顺序）。 */
  rows: readonly TaskRow[]
  groups: readonly GroupDto[]
  /** 当前排序比较器（与引擎同一个）。 */
  compare: (left: DownloadTaskView, right: DownloadTaskView) => number
  /** null = 不分组。组行传入其通过成员的视图，供「类型」这种不能直接用代理视图的分桶使用。 */
  bucketOf: ((view: DownloadTaskView, groupMembers: readonly DownloadTaskView[] | null) => BucketInfo) | null
  /** 已折叠的桶键。 */
  collapsedBuckets: ReadonlySet<string>
  /** 运行期展开的组。 */
  expanded: ReadonlySet<string>
  /** 运行期折叠的目录（`dirCollapseKey`）。 */
  dirCollapsed: ReadonlySet<string>
  /** 搜索非空：所有组强制展开。 */
  searching: boolean
}

export interface VisibleRows {
  rows: readonly VisibleRow[]
  /** 全部有通过成员的组行（含折叠桶内的），按组 id 查。 */
  folders: ReadonlyMap<string, GroupFolderRow>
  /** 可见任务数：顶层任务 + 折叠组的通过成员 + 展开组显示出来的成员（不含折叠桶内、折叠目录下的）。 */
  visibleCount: number
  /** 可见任务序列（全选 / Shift 范围 / 表头计数）：折叠的组在组行位置展开为其通过成员（flatten 顺序）。惰性计算并缓存。 */
  sequence: () => readonly RowKey[]
}

const EMPTY_FOLDERS: ReadonlyMap<string, GroupFolderRow> = new Map()

interface Entity {
  /** 排序 / 分桶用视图（任务视图或组代理）。 */
  view: DownloadTaskView
  task: TaskRow | null
  group: GroupFolderRow | null
}

/** 二分：`[from, rows.length)` 内第一个 `compare(task, proxy) >= 0` 的下标（Rust `partition_point`）。 */
function partitionPoint(
  tops: readonly TaskRow[],
  from: number,
  proxy: DownloadTaskView,
  compare: VisibleRowsInput['compare'],
): number {
  let lo = from
  let hi = tops.length
  while (lo < hi) {
    const mid = (lo + hi) >>> 1
    if (compare((tops[mid] as TaskRow).view, proxy) < 0) lo = mid + 1
    else hi = mid
  }
  return lo
}

function treeMembers(group: GroupDto, members: readonly TaskRow[]): TreeMember[] {
  return members.map((row) => ({
    dir: memberDir(group.saveDir, row.view.saveDir),
    nameFold: row.view.nameFold,
    sizeBytes: row.view.sizeBytes,
  }))
}

export function buildVisibleRows(input: VisibleRowsInput): VisibleRows {
  const { rows, compare } = input
  const known = new Map(input.groups.map((group) => [group.groupId, group]))

  // 2. 归组：已知组的本地成员进组，其余（远程 / 孤儿 groupId）是顶层任务。
  const tops: TaskRow[] = []
  const byGroup = new Map<string, TaskRow[]>()
  for (const row of rows) {
    const view = row.view
    if (view.source === 'local' && view.groupId !== '' && known.has(view.groupId)) {
      const list = byGroup.get(view.groupId)
      if (list === undefined) byGroup.set(view.groupId, [row])
      else list.push(row)
    } else {
      tops.push(row)
    }
  }
  if (byGroup.size === 0 && input.bucketOf === null) {
    // 没有任何组参与、也不分桶：引擎的行数组即最终结果（不再分配）。
    let keys: readonly RowKey[] | null = null
    return { rows, folders: EMPTY_FOLDERS, visibleCount: rows.length, sequence: () => (keys ??= rows.map((row) => row.key)) }
  }

  // 3. 每个有通过成员的组一个组行；聚合与代理视图只基于通过成员。
  const folderMap = new Map<string, GroupFolderRow>()
  const folders: { entity: Entity; members: readonly DownloadTaskView[] }[] = []
  for (const [groupId, members] of byGroup) {
    const group = known.get(groupId) as GroupDto
    const name = displayName(group.name, group.saveDir, groupId)
    const views = members.map((row) => row.view)
    const proxy = groupSortView(
      { groupId, name, sourceUrl: group.sourceUrl, createdAtSecs: Number(group.createdAt) || 0 },
      aggregateOf(views),
      views,
    )
    let ordered: TaskRow[] | null = null
    let keys: RowKey[] | null = null
    const folder: GroupFolderRow = {
      type: 'group',
      key: groupRowKey(groupId),
      groupId,
      group,
      name,
      members,
      expanded: input.searching || input.expanded.has(groupId),
      orderedRows: () => {
        if (ordered === null) {
          ordered = []
          for (const item of flatten(treeMembers(group, members), () => false)) {
            if (item.type === 'member') ordered.push(members[item.index] as TaskRow)
          }
        }
        return ordered
      },
      orderedKeys: () => {
        keys ??= folder.orderedRows().map((row) => row.key)
        return keys
      },
    }
    folderMap.set(groupId, folder)
    folders.push({ entity: { view: proxy, task: null, group: folder }, members: views })
  }

  // 4. 顶层排序：任务保持既有顺序；组按代理排序后并入（位置 = partition point，单调不回退）。
  folders.sort((left, right) => compare(left.entity.view, right.entity.view))
  const entities: Entity[] = []
  const membersOf = new Map<Entity, readonly DownloadTaskView[]>()
  let cursor = 0
  for (const { entity, members } of folders) {
    const at = partitionPoint(tops, cursor, entity.view, compare)
    for (; cursor < at; cursor += 1) {
      const task = tops[cursor] as TaskRow
      entities.push({ view: task.view, task, group: null })
    }
    entities.push(entity)
    membersOf.set(entity, members)
  }
  for (; cursor < tops.length; cursor += 1) {
    const task = tops[cursor] as TaskRow
    entities.push({ view: task.view, task, group: null })
  }

  // 5. 分桶（不分组时只有一个无标题桶）。
  interface Bucket {
    info: BucketInfo | null
    entities: Entity[]
    count: number
  }
  const buckets: Bucket[] = []
  if (input.bucketOf === null) {
    buckets.push({ info: null, entities, count: 0 })
  } else {
    const byKey = new Map<string, Bucket>()
    for (const entity of entities) {
      const info = input.bucketOf(entity.view, membersOf.get(entity) ?? null)
      let bucket = byKey.get(info.key)
      if (bucket === undefined) {
        bucket = { info, entities: [], count: 0 }
        byKey.set(info.key, bucket)
        buckets.push(bucket)
      }
      bucket.entities.push(entity)
      bucket.count += entity.group === null ? 1 : entity.group.members.length
    }
    buckets.sort((a, b) => {
      const left = a.info as BucketInfo
      const right = b.info as BucketInfo
      return left.order - right.order || (left.label < right.label ? -1 : left.label > right.label ? 1 : 0)
    })
  }

  // 6. 展开：组行后紧跟 flatten 出的成员行 / 目录行。
  const out: VisibleRow[] = []
  let visibleCount = 0
  for (const bucket of buckets) {
    let collapsedBucket = false
    if (bucket.info !== null) {
      collapsedBucket = input.collapsedBuckets.has(bucket.info.key)
      out.push({ type: 'bucket', key: bucket.info.key, label: bucket.info.label, count: bucket.count, collapsed: collapsedBucket })
    }
    if (collapsedBucket) continue
    for (const entity of bucket.entities) {
      if (entity.task !== null) {
        out.push(entity.task)
        visibleCount += 1
        continue
      }
      const folder = entity.group as GroupFolderRow
      out.push(folder)
      if (!folder.expanded) {
        visibleCount += folder.members.length
        continue
      }
      const flat = flatten(treeMembers(folder.group, folder.members), (dir) =>
        input.dirCollapsed.has(dirCollapseKey(folder.groupId, dir)),
      )
      for (const item of flat) {
        if (item.type === 'dir') {
          out.push({
            type: 'dir',
            key: `d:${dirCollapseKey(folder.groupId, item.path)}`,
            groupId: folder.groupId,
            path: item.path,
            count: item.count,
            sizeBytes: item.sizeBytes,
            collapsed: item.collapsed,
          })
        } else {
          const row = folder.members[item.index] as TaskRow
          out.push({ type: 'member', key: row.key, row, depth: item.nested ? 2 : 1 })
          visibleCount += 1
        }
      }
    }
  }

  let sequence: readonly RowKey[] | null = null
  return {
    rows: out,
    folders: folderMap,
    visibleCount,
    sequence: () => {
      if (sequence !== null) return sequence
      const keys: RowKey[] = []
      for (const row of out) {
        if (row.type === 'task' || row.type === 'member') keys.push(row.key)
        else if (row.type === 'group' && !row.expanded) for (const key of row.orderedKeys()) keys.push(key)
      }
      sequence = keys
      return keys
    },
  }
}

/**
 * 「类型」分组时组行所在的分类：通过成员中数量最多的分类；并列取字节合计大者；再并列取分类规则顺序靠前者
 * （`order` 越小越靠前，未知分类排最后）。成员为空返回 null。
 */
export function dominantCategory(
  members: readonly DownloadTaskView[],
  categoryOf: (view: DownloadTaskView) => string,
  order: (categoryId: string) => number,
): string | null {
  const stats = new Map<string, { count: number; bytes: number }>()
  for (const view of members) {
    const id = categoryOf(view)
    const stat = stats.get(id)
    if (stat === undefined) stats.set(id, { count: 1, bytes: view.sizeBytes })
    else {
      stat.count += 1
      stat.bytes += view.sizeBytes
    }
  }
  let best: string | null = null
  let bestStat: { count: number; bytes: number } | null = null
  for (const [id, stat] of stats) {
    if (
      bestStat === null ||
      stat.count > bestStat.count ||
      (stat.count === bestStat.count &&
        (stat.bytes > bestStat.bytes || (stat.bytes === bestStat.bytes && order(id) < order(best as string))))
    ) {
      best = id
      bestStat = stat
    }
  }
  return best
}

/** 搜索词命中显示名（小写包含）的组 id；词为空返回空集。 */
export function groupsMatchingQuery(groups: readonly GroupDto[], query: string): Set<string> {
  const matched = new Set<string>()
  if (query === '') return matched
  for (const group of groups) {
    if (displayName(group.name, group.saveDir, group.groupId).toLowerCase().includes(query)) matched.add(group.groupId)
  }
  return matched
}

/** 选区变换的结果（`focusedGroup` = 普通单击后聚焦的组，其余为 null）。 */
export interface GroupSelectionResult {
  selected: ReadonlySet<RowKey>
  multiSelect: boolean
  anchor: RowKey | null
  focusedGroup: string | null
}

export interface GroupSelectionState {
  selected: ReadonlySet<RowKey>
  multiSelect: boolean
  anchor: RowKey | null
}

/**
 * 组行点击的选区（规格 §2）：
 * - 普通单击：选区 = 通过成员，退出多选，锚点 = 组的最后一个成员，聚焦该组；
 * - Cmd/Ctrl 单击：成员已全选则全部移出，否则全部加入，进入多选，不聚焦；
 * - Shift 单击且有锚点：锚点到该组最后一个成员的范围，按可见任务序列取。
 */
export function selectGroupOnClick(
  state: GroupSelectionState,
  groupId: string,
  memberKeys: readonly RowKey[],
  sequence: readonly RowKey[],
  modifiers: { shift: boolean; secondary: boolean },
): GroupSelectionResult {
  const last = memberKeys[memberKeys.length - 1] ?? null
  if (modifiers.shift && state.anchor !== null && last !== null) {
    const from = sequence.indexOf(state.anchor)
    const to = sequence.indexOf(last)
    if (from >= 0 && to >= 0) {
      const [start, end] = from <= to ? [from, to] : [to, from]
      return { selected: new Set(sequence.slice(start, end + 1)), multiSelect: state.multiSelect, anchor: last, focusedGroup: null }
    }
  }
  if (modifiers.secondary) {
    const next = new Set(state.selected)
    if (groupFullySelected(state.selected, memberKeys)) for (const key of memberKeys) next.delete(key)
    else for (const key of memberKeys) next.add(key)
    return { selected: next, multiSelect: true, anchor: last, focusedGroup: null }
  }
  return { selected: new Set(memberKeys), multiSelect: false, anchor: last, focusedGroup: groupId }
}

/** 组行「选中」外观 ⇔ 通过成员非空且全部在选区。 */
export function groupFullySelected(selected: ReadonlySet<RowKey>, memberKeys: readonly RowKey[]): boolean {
  return memberKeys.length > 0 && memberKeys.every((key) => selected.has(key))
}

/** 组行复选框状态（按通过成员行）：全选 / 部分 / 无。 */
export function groupCheckState(selected: ReadonlySet<RowKey>, members: readonly TaskRow[]): boolean | 'indeterminate' {
  let hit = 0
  for (const row of members) if (selected.has(row.key)) hit += 1
  if (hit === 0) return false
  return hit === members.length ? true : 'indeterminate'
}

/** 选区恰好等于该组通过成员：停靠面板显示「组概览」的条件之一。 */
export function selectionIsGroup(selected: ReadonlySet<RowKey>, members: readonly TaskRow[]): boolean {
  return members.length > 0 && selected.size === members.length && members.every((row) => selected.has(row.key))
}
