// 任务组「文件夹行」纯模型（移植 crates/downloads/src/model/group_tree.rs）：成员计数 / 聚合状态、
// 组内目录树展开、排序代理视图。与 Rust 逐条对齐，测试共用
// `crates/downloads/tests/fixtures/group_tree_cases.json`（见 groupTree.test.ts）。

import { fileKindOf } from '../../../lib/icon-pack'
import { localKey, urlHost } from './task'
import type { DownloadTaskView, TaskState } from './task'
import { compareNatural } from './viewPrefs'

/** 组成员按状态计数。准备中（引擎 status 5，视图 `preparing`）计入下载中：它已占用并发名额。 */
export interface GroupCounts {
  total: number
  done: number
  downloading: number
  pending: number
  paused: number
  failed: number
}

export const emptyGroupCounts = (): GroupCounts => ({ total: 0, done: 0, downloading: 0, pending: 0, paused: 0, failed: 0 })

export function addToCounts(counts: GroupCounts, view: Pick<DownloadTaskView, 'state' | 'preparing'>): void {
  counts.total += 1
  switch (view.state) {
    case 'completed':
      counts.done += 1
      break
    case 'downloading':
      counts.downloading += 1
      break
    case 'pending':
      if (view.preparing) counts.downloading += 1
      else counts.pending += 1
      break
    case 'paused':
      counts.paused += 1
      break
    case 'failed':
      counts.failed += 1
      break
  }
}

/** 组的聚合状态：下载中 → 等待 → 已暂停 → 失败 → 已完成（个别失败成员不盖过暂停成员；失败数仍在计数行显示）。 */
export function countsState(counts: GroupCounts): TaskState {
  if (counts.downloading > 0) return 'downloading'
  if (counts.pending > 0) return 'pending'
  if (counts.paused > 0) return 'paused'
  if (counts.failed > 0) return 'failed'
  return 'completed'
}

/** 有成员可暂停（下载中 / 准备中 / 排队）。 */
export const anyActive = (counts: GroupCounts): boolean => counts.downloading + counts.pending > 0

/** 有成员可继续（暂停 / 失败）。 */
export const anyResumable = (counts: GroupCounts): boolean => counts.paused + counts.failed > 0

/** 组聚合值（行展示与排序共用）。 */
export interface GroupAggregate {
  counts: GroupCounts
  state: TaskState
  sizeBytes: number
  downloadedBytes: number
  /** 下载中成员的速度合计。 */
  speed: number
  /** 有被「优先下载」的未完成成员（智能排序置顶）。 */
  boosted: boolean
  /** 字节进度；总大小未知时退回完成数 / 成员数。 */
  progress: number
  /** 速度 > 0 且未下完时的剩余秒数。 */
  etaSeconds: number | null
}

/** 由已累计的计数 / 字节 / 速度得出聚合值（成员遍历与行引擎的增量汇总共用）。 */
export function makeAggregate(
  counts: GroupCounts,
  sizeBytes: number,
  downloadedBytes: number,
  speed: number,
  boosted: boolean,
): GroupAggregate {
  const progress =
    sizeBytes > 0 ? Math.min(1, Math.max(0, downloadedBytes / sizeBytes)) : counts.total > 0 ? counts.done / counts.total : 0
  return {
    counts,
    state: countsState(counts),
    sizeBytes,
    downloadedBytes,
    speed,
    boosted,
    progress,
    etaSeconds: speed > 0 && downloadedBytes < sizeBytes ? Math.floor((sizeBytes - downloadedBytes) / speed) : null,
  }
}

export function aggregateOf(members: Iterable<DownloadTaskView>): GroupAggregate {
  const counts = emptyGroupCounts()
  let sizeBytes = 0
  let downloadedBytes = 0
  let speed = 0
  let boosted = false
  for (const view of members) {
    addToCounts(counts, view)
    sizeBytes += view.sizeBytes
    downloadedBytes += view.downloadedBytes
    if (view.state === 'downloading') speed += view.speed ?? 0
    if (view.boosted && (view.state === 'downloading' || view.state === 'pending')) boosted = true
  }
  return makeAggregate(counts, sizeBytes, downloadedBytes, speed, boosted)
}

/** 统一为 `/` 分隔、去掉末尾分隔符（根 `/` 保留）。 */
function normalizeDir(dir: string): string {
  const unified = dir.trim().replace(/\\/g, '/')
  const trimmed = unified.replace(/\/+$/, '')
  return trimmed === '' && unified.startsWith('/') ? '/' : trimmed
}

const trimSlashes = (value: string): string => value.replace(/^\/+/, '').replace(/\/+$/, '')

/**
 * 成员在组内的相对目录（`/` 分隔，空 = 组根）：任务保存目录相对组根目录；不在组根之下
 * （用户改过保存位置等）一律视为组根。
 */
export function memberDir(groupDir: string, taskDir: string): string {
  const root = normalizeDir(groupDir)
  const dir = normalizeDir(taskDir)
  if (root === '' || dir === root) return ''
  const prefix = root.endsWith('/') ? root : `${root}/`
  return dir.startsWith(prefix) ? trimSlashes(dir.slice(prefix.length)) : ''
}

/** 组显示名：组名为空时取组根目录末段，再空取组 id。 */
export function displayName(name: string, saveDir: string, groupId: string): string {
  const trimmed = name.trim()
  if (trimmed !== '') return trimmed
  const segments = normalizeDir(saveDir).split('/')
  for (let index = segments.length - 1; index >= 0; index -= 1) {
    const segment = segments[index] as string
    if (segment !== '') return segment
  }
  return groupId
}

/** 目录行显示路径：非空段 ≤ 2 原样以 ` / ` 连接；更深只留最后两段并以 `… / ` 开头。 */
export function compressPath(path: string): string {
  const segments = path.split('/').filter((part) => part !== '')
  if (segments.length > 2) return `… / ${segments.slice(-2).join(' / ')}`
  return segments.join(' / ')
}

/** [`flatten`] 的输入：成员在组内的相对目录、折叠后的名称与大小。 */
export interface TreeMember {
  dir: string
  nameFold: string
  sizeBytes: number
}

/** 组展开后的一行。 */
export type TreeRow =
  | { type: 'member'; index: number; nested: boolean }
  | { type: 'dir'; path: string; count: number; sizeBytes: number; collapsed: boolean }

const rawOrder = (left: string, right: string): number => (left < right ? -1 : left > right ? 1 : 0)

/** 与 Rust `natural_cmp` 同义：自然序，各段等价时按原串定序。 */
const naturalWithTie = (left: string, right: string): number => compareNatural(left, right) || rawOrder(left, right)

/** 组根（空目录）最前，其余目录按大小写折叠后的自然序；折叠后相同再按原文定序。 */
function compareDirs(left: string, right: string): number {
  if (left === '') return right === '' ? 0 : -1
  if (right === '') return 1
  return naturalWithTie(left.toLowerCase(), right.toLowerCase()) || rawOrder(left, right)
}

/**
 * 按目录展开组成员：组根成员在前，其后每个目录各一行（目录按自然序），目录行后跟其成员
 * （折叠的目录只保留目录行）；同一目录内按名称自然序。目录行平铺（显示完整相对路径），
 * 不做多级嵌套，每个目录只出现一次。
 */
export function flatten(members: readonly TreeMember[], dirCollapsed: (dir: string) => boolean): TreeRow[] {
  const order = members.map((_, index) => index)
  order.sort((left, right) => {
    const a = members[left] as TreeMember
    const b = members[right] as TreeMember
    return compareDirs(a.dir, b.dir) || naturalWithTie(a.nameFold, b.nameFold)
  })
  const rows: TreeRow[] = []
  let ix = 0
  while (ix < order.length) {
    const dir = (members[order[ix] as number] as TreeMember).dir
    let end = ix + 1
    while (end < order.length && (members[order[end] as number] as TreeMember).dir === dir) end += 1
    if (dir === '') {
      for (let at = ix; at < end; at += 1) rows.push({ type: 'member', index: order[at] as number, nested: false })
    } else {
      const collapsed = dirCollapsed(dir)
      let sizeBytes = 0
      for (let at = ix; at < end; at += 1) sizeBytes += (members[order[at] as number] as TreeMember).sizeBytes
      rows.push({ type: 'dir', path: dir, count: end - ix, sizeBytes, collapsed })
      if (!collapsed) {
        for (let at = ix; at < end; at += 1) rows.push({ type: 'member', index: order[at] as number, nested: true })
      }
    }
    ix = end
  }
  return rows
}

/** 组行的排序 / 分桶代理的输入。 */
export interface GroupProxy {
  groupId: string
  /** 显示名（[`displayName`]）。 */
  name: string
  sourceUrl: string
  createdAtSecs: number
}

/**
 * 组行的排序 / 分桶代理：把组投影成一个「任务视图」，复用任务的排序比较器与分桶规则。
 * 名称 = 组显示名，大小 / 进度 / 速度 / 状态取聚合值，创建时间取组的创建时间（缺失时取成员最早的），
 * 排队键与插入序取成员最小值，来源取组的来源链接。`key` 只用于比较器的最终平局，
 * 不能拿去查任务存储。
 */
export function groupSortView(
  group: GroupProxy,
  aggregate: GroupAggregate,
  members: Iterable<DownloadTaskView>,
): DownloadTaskView {
  let createdAtSecs = group.createdAtSecs
  let queueOrder = Number.POSITIVE_INFINITY
  let queueKey: number | null = null
  let queueId: string | null = null
  for (const member of members) {
    if (group.createdAtSecs <= 0 && member.createdAtSecs > 0) {
      createdAtSecs = createdAtSecs <= 0 ? member.createdAtSecs : Math.min(createdAtSecs, member.createdAtSecs)
    }
    queueOrder = Math.min(queueOrder, member.queueOrder)
    if (member.queueKey !== null) queueKey = queueKey === null ? member.queueKey : Math.min(queueKey, member.queueKey)
    queueId ??= member.queueId
  }
  return {
    key: localKey(group.groupId),
    source: 'local',
    taskId: group.groupId,
    queueId: queueId ?? '',
    name: group.name,
    sizeBytes: aggregate.sizeBytes,
    downloadedBytes: aggregate.downloadedBytes,
    speed: aggregate.speed,
    uploadSpeed: null,
    runtime: undefined,
    runtimeConnected: false,
    etaSeconds: aggregate.etaSeconds,
    createdAtSecs,
    completedAtSecs: 0,
    kind: fileKindOf(group.name),
    protocol: 'http',
    progress: aggregate.progress,
    state: aggregate.state,
    metadataPending: false,
    url: group.sourceUrl,
    originUrl: group.sourceUrl,
    site: urlHost(group.sourceUrl),
    referrer: '',
    saveDir: '',
    groupId: group.groupId,
    errorMessage: '',
    fileMissing: false,
    btSequential: false,
    btFirstLastPiecesFirst: false,
    fileHash: '',
    seedingStatus: 0,
    uploadedBytes: 0,
    boosted: aggregate.boosted,
    preparing: false,
    queueOrder: Number.isFinite(queueOrder) ? queueOrder : 0,
    queueKey,
    nameFold: group.name.toLowerCase(),
    searchFold: '',
    toDevice: '',
    remoteStatus: null,
    dto: undefined,
  }
}

/**
 * 「打开组文件夹」要定位的成员：优先位于组根目录的成员，没有则第一个成员（`views` 为组的全部成员）。
 */
export function folderAnchorMember<T extends Pick<DownloadTaskView, 'saveDir'>>(views: readonly T[], groupDir: string): T | undefined {
  return views.find((view) => memberDir(groupDir, view.saveDir) === '') ?? views[0]
}

/** 组行进度列柱状图最多画的柱数。 */
export const SPARKLINE_MAX_BARS = 24

/** 从 `len` 个成员里等距抽样出至多 `max` 根柱对应的下标（`⌊i·len/count⌋`，count = min(len, max)）。 */
export function sparklineSample(len: number, max: number): number[] {
  const count = Math.min(len, max)
  return Array.from({ length: count }, (_, index) => Math.floor((index * len) / count))
}

/** 单根柱的高度比例：至少 14%（未开始的成员也看得见），失败成员至少 1/3（醒目），已完成恒为满柱。 */
export function sparklineHeight(progress: number, state: TaskState): number {
  const floor = state === 'failed' ? 1 / 3 : 0.14
  const value = state === 'completed' ? 1 : progress
  return Math.max(Math.min(1, Math.max(0, value)), floor)
}
