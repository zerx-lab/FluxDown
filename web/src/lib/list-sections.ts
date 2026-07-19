// 任务列表视图系统 —— 7 维分桶 × 6 键排序纯函数。
// 移植自 lib/src/models/download_controller.dart 的 bucketEntities*/compareEntities*
// 系列纯函数（buildListSections 管线的分桶/排序阶段；成员分组/过滤/展开扁平化仍由
// TaskList.tsx 编排，对齐桌面 buildListSections 与本仓 task-group.ts 的既有分层）。

import { fileType, groupLabel, GROUP_ORDER, timeGroup, typeLabel, TYPE_ORDER, type FileType } from './format'
import { t } from './i18n'
import { extractSiteKey, extractSiteLabel } from './site'
import { isActiveStatus } from './task-group'
import type { GroupDto, QueueDto, TaskStatus } from './types'
import type { SortDir, ViewGroupBy, ViewSortKey } from './view-prefs'

/** 分桶/排序所需的最小任务形状（`ViewTask` 结构上满足，避免反向依赖 useViewTasks）。 */
export interface EntityMember {
  taskId: string
  url: string
  fileName: string
  status: TaskStatus
  downloadedBytes: number
  totalBytes: number
  speed: number
  createdAt: string
  queueId: string
}

export type SectionEntity<T> = { kind: 'task'; task: T } | { kind: 'group'; group: GroupDto; members: T[] }

export interface ListSection<T> {
  /** 稳定桶 id（如 `smart:live`、`date:today`、`status:1`、`site:baidu_com`），供分组头折叠状态键控。 */
  key: string
  /** null = 不渲染分组头（「不分组」维度）。 */
  title: string | null
  entities: SectionEntity<T>[]
}

// ---- 聚合字段访问器（对齐桌面 ListEntity 抽象 getter） ----

export function entityName<T extends EntityMember>(e: SectionEntity<T>, groupDisplayName: (g: GroupDto) => string): string {
  return e.kind === 'task' ? e.task.fileName || e.task.url : groupDisplayName(e.group)
}

export function entityProgress<T extends EntityMember>(e: SectionEntity<T>): number {
  if (e.kind === 'task') return e.task.totalBytes > 0 ? Math.min(1, e.task.downloadedBytes / e.task.totalBytes) : 0
  const total = entityTotalBytes(e)
  if (total <= 0) return 0
  const downloaded = e.members.reduce((s, m) => s + m.downloadedBytes, 0)
  return Math.min(1, downloaded / total)
}

export function entityTotalBytes<T extends EntityMember>(e: SectionEntity<T>): number {
  return e.kind === 'task' ? e.task.totalBytes : e.members.reduce((s, m) => s + m.totalBytes, 0)
}

export function entitySpeed<T extends EntityMember>(e: SectionEntity<T>): number {
  if (e.kind === 'task') return e.task.speed
  return e.members.reduce((s, m) => s + (isActiveStatus(m.status) ? m.speed : 0), 0)
}

export function entityCreatedAt<T extends EntityMember>(e: SectionEntity<T>): number {
  return Number(e.kind === 'task' ? e.task.createdAt : e.group.createdAt)
}

export function entityStatusBucket<T extends EntityMember>(e: SectionEntity<T>): TaskStatus {
  if (e.kind === 'task') return e.task.status
  if (e.members.length === 0) return 3
  if (e.members.some((m) => isActiveStatus(m.status))) return 1
  if (e.members.some((m) => m.status === 4)) return 4
  if (e.members.every((m) => m.status === 3)) return 3
  return 2
}

/** 组的「主导类型」：成员占比最高者，并列取字节数更大者。 */
export function entityCategoryKey<T extends EntityMember>(e: SectionEntity<T>): FileType {
  if (e.kind === 'task') return fileType(e.task.fileName, e.task.url)
  if (e.members.length === 0) return 'other'
  const counts = new Map<FileType, number>()
  const bytes = new Map<FileType, number>()
  for (const m of e.members) {
    const cat = fileType(m.fileName, m.url)
    counts.set(cat, (counts.get(cat) ?? 0) + 1)
    bytes.set(cat, (bytes.get(cat) ?? 0) + m.totalBytes)
  }
  let best: FileType | null = null
  for (const [cat, count] of counts) {
    if (best === null) {
      best = cat
      continue
    }
    const cmp = count - (counts.get(best) ?? 0)
    if (cmp > 0 || (cmp === 0 && (bytes.get(cat) ?? 0) > (bytes.get(best) ?? 0))) best = cat
  }
  return best ?? 'other'
}

export function entityQueueId<T extends EntityMember>(e: SectionEntity<T>): string {
  return e.kind === 'task' ? e.task.queueId : e.members[0]?.queueId ?? ''
}

function entitySourceUrl<T extends EntityMember>(e: SectionEntity<T>): string {
  return e.kind === 'task' ? e.task.url : e.group.sourceUrl
}

export function entitySiteKey<T extends EntityMember>(e: SectionEntity<T>): string {
  return extractSiteKey(entitySourceUrl(e))
}

export function entitySiteLabel<T extends EntityMember>(e: SectionEntity<T>): string {
  return extractSiteLabel(entitySourceUrl(e), t('view.siteBt'))
}

// ---- 分桶函数（7 维） ----

function bucketNone<T extends EntityMember>(entities: SectionEntity<T>[]): ListSection<T>[] {
  return entities.length ? [{ key: 'none:all', title: null, entities }] : []
}

function bucketByDate<T extends EntityMember>(entities: SectionEntity<T>[]): ListSection<T>[] {
  const buckets = new Map<string, SectionEntity<T>[]>()
  for (const e of entities) {
    const tg = timeGroup(entityCreatedAt(e))
    const arr = buckets.get(tg)
    if (arr) arr.push(e)
    else buckets.set(tg, [e])
  }
  const out: ListSection<T>[] = []
  for (const tg of GROUP_ORDER) {
    const arr = buckets.get(tg)
    if (arr && arr.length) out.push({ key: `date:${tg}`, title: groupLabel(tg), entities: arr })
  }
  return out
}

/** 「智能」：活跃（下载中/准备中/排队）置顶一桶，其余按时间分档。 */
function bucketSmart<T extends EntityMember>(entities: SectionEntity<T>[]): ListSection<T>[] {
  const active: SectionEntity<T>[] = []
  const historical: SectionEntity<T>[] = []
  for (const e of entities) (isActiveStatus(entityStatusBucket(e)) ? active : historical).push(e)
  const out: ListSection<T>[] = []
  if (active.length) out.push({ key: 'smart:live', title: t('view.activeGroup'), entities: active })
  out.push(...bucketByDate(historical))
  return out
}

const STATUS_BUCKET_ORDER: TaskStatus[] = [1, 0, 2, 4, 3]

function statusBucketLabel(s: TaskStatus): string {
  const KEYS = { 0: 'status.pending', 1: 'status.downloading', 2: 'status.paused', 3: 'status.completed', 4: 'status.error', 5: 'status.downloading' } as const
  return t(KEYS[s])
}

/** 「状态」：固定顺序 [下载中,排队,暂停,失败,完成]，preparing 视觉上并入下载中桶。 */
function bucketByStatus<T extends EntityMember>(entities: SectionEntity<T>[]): ListSection<T>[] {
  const buckets = new Map<TaskStatus, SectionEntity<T>[]>()
  for (const e of entities) {
    const raw = entityStatusBucket(e)
    const bucket: TaskStatus = isActiveStatus(raw) && raw !== 0 ? 1 : raw
    const arr = buckets.get(bucket)
    if (arr) arr.push(e)
    else buckets.set(bucket, [e])
  }
  const out: ListSection<T>[] = []
  for (const st of STATUS_BUCKET_ORDER) {
    const arr = buckets.get(st)
    if (arr && arr.length) out.push({ key: `status:${st}`, title: statusBucketLabel(st), entities: arr })
  }
  return out
}

/** 「类型」：固定顺序（`TYPE_ORDER` 去掉 `all`），仅保留有成员的桶。 */
function bucketByType<T extends EntityMember>(entities: SectionEntity<T>[]): ListSection<T>[] {
  const buckets = new Map<FileType, SectionEntity<T>[]>()
  for (const e of entities) {
    const cat = entityCategoryKey(e)
    const arr = buckets.get(cat)
    if (arr) arr.push(e)
    else buckets.set(cat, [e])
  }
  const out: ListSection<T>[] = []
  for (const cat of TYPE_ORDER) {
    if (cat === 'all') continue
    const arr = buckets.get(cat)
    if (arr && arr.length) out.push({ key: `type:${cat}`, title: typeLabel(cat), entities: arr })
  }
  return out
}

/** 「队列」：默认队列（queueId==''）固定排最前，其余按 queues 已排序顺序分桶。 */
function bucketByQueue<T extends EntityMember>(entities: SectionEntity<T>[], queues: QueueDto[]): ListSection<T>[] {
  const buckets = new Map<string, SectionEntity<T>[]>()
  for (const e of entities) {
    const qid = entityQueueId(e)
    const arr = buckets.get(qid)
    if (arr) arr.push(e)
    else buckets.set(qid, [e])
  }
  const out: ListSection<T>[] = []
  const def = buckets.get('')
  if (def && def.length) out.push({ key: 'queue:', title: t('view.ungroupedQueue'), entities: def })
  for (const q of queues) {
    const arr = buckets.get(q.queueId)
    if (arr && arr.length) out.push({ key: `queue:${q.queueId}`, title: q.name, entities: arr })
  }
  return out
}

/** 「站点」：按 siteKey 分桶，桶按成员数降序排列。 */
function bucketBySite<T extends EntityMember>(entities: SectionEntity<T>[]): ListSection<T>[] {
  const buckets = new Map<string, SectionEntity<T>[]>()
  for (const e of entities) {
    const key = entitySiteKey(e)
    const arr = buckets.get(key)
    if (arr) arr.push(e)
    else buckets.set(key, [e])
  }
  const keys = Array.from(buckets.keys()).sort((a, b) => (buckets.get(b)?.length ?? 0) - (buckets.get(a)?.length ?? 0))
  return keys.map((key) => {
    const arr = buckets.get(key)!
    return { key: `site:${key.replace(/\W/g, '_')}`, title: entitySiteLabel(arr[0]), entities: arr }
  })
}

/** 分桶函数表：ViewGroupBy → 分桶函数。 */
export function bucketEntities<T extends EntityMember>(entities: SectionEntity<T>[], groupBy: ViewGroupBy, queues: QueueDto[]): ListSection<T>[] {
  switch (groupBy) {
    case 'smart':
      return bucketSmart(entities)
    case 'date':
      return bucketByDate(entities)
    case 'status':
      return bucketByStatus(entities)
    case 'type':
      return bucketByType(entities)
    case 'queue':
      return bucketByQueue(entities, queues)
    case 'site':
      return bucketBySite(entities)
    case 'none':
      return bucketNone(entities)
  }
}

// ---- 排序（6 键） ----

/** tier: 下载中0 < 准备中1 < 排队2 < 暂停3 < 失败4 < 完成5，桶内按创建时间升序
 *  （REST 无队列内排队位置字段，回退创建时间——固定升序稳定，忽略 sortDir）。 */
const SMART_TIER: Record<TaskStatus, number> = { 1: 0, 5: 1, 0: 2, 2: 3, 4: 4, 3: 5 }

function compareSmart<T extends EntityMember>(a: SectionEntity<T>, b: SectionEntity<T>): number {
  const diff = SMART_TIER[entityStatusBucket(a)] - SMART_TIER[entityStatusBucket(b)]
  if (diff !== 0) return diff
  return entityCreatedAt(a) - entityCreatedAt(b)
}

/** 6 键排序比较器（`smart` 忽略 dir；其余按 dir 升/降序）。 */
export function compareSectionEntities<T extends EntityMember>(
  sortKey: ViewSortKey,
  sortDir: SortDir,
  a: SectionEntity<T>,
  b: SectionEntity<T>,
  groupDisplayName: (g: GroupDto) => string,
): number {
  if (sortKey === 'smart') return compareSmart(a, b)
  const mul = sortDir === 'asc' ? 1 : -1
  switch (sortKey) {
    case 'created':
      return (entityCreatedAt(a) - entityCreatedAt(b)) * mul
    case 'name':
      return entityName(a, groupDisplayName).localeCompare(entityName(b, groupDisplayName)) * mul
    case 'size':
      return (entityTotalBytes(a) - entityTotalBytes(b)) * mul
    case 'progress':
      return (entityProgress(a) - entityProgress(b)) * mul
    case 'speed':
      return (entitySpeed(a) - entitySpeed(b)) * mul
  }
}

/** 桶间排序（「排序控全局叙事」，对齐桌面 orderSections）：显式排序键下，
 *  分桶结果按各桶首行（桶内排序后的极值代表）用同一比较器重排，使全列表
 *  首行恒为当前排序键的全局极值；`smart` 保持各维度固定叙事顺序；
 *  `smart:live` 活跃桶恒置顶；比较相等时保持分桶产出顺序（显式索引平局裁决）。
 *  前置条件：各桶已按同一 sortKey/sortDir 完成桶内排序。 */
export function orderSections<T extends EntityMember>(
  sections: ListSection<T>[],
  sortKey: ViewSortKey,
  sortDir: SortDir,
  groupDisplayName: (g: GroupDto) => string,
): ListSection<T>[] {
  if (sortKey === 'smart' || sections.length < 2) return sections
  const pinned: ListSection<T>[] = []
  const movable: ListSection<T>[] = []
  for (const s of sections) (s.key === 'smart:live' ? pinned : movable).push(s)
  const order = movable.map((_, i) => i)
  order.sort((ia, ib) => {
    const c = compareSectionEntities(sortKey, sortDir, movable[ia].entities[0], movable[ib].entities[0], groupDisplayName)
    return c !== 0 ? c : ia - ib
  })
  return [...pinned, ...order.map((i) => movable[i])]
}
