// 任务组纯函数 —— 火花条抽样 / 路径链压缩 / 成员目录归属 / 组聚合统计。
// 移植自桌面端 lib/src/models/task_group.dart（sampleSparkline/compressPathChain）+
// lib/src/models/list_entity.dart（GroupEntity 派生规则/GroupMemberCounts）+
// lib/src/models/download_controller.dart（groupMemberDirPath/flattenGroupMembers）。
// 纯函数抽出，供 GroupRow/TaskList 渲染复用，也便于单测。

import type { GroupDto, TaskStatus } from './types'

/** 组成员聚合所需的最小任务形状（`ViewTask` 结构上满足，避免反向依赖 useViewTasks）。 */
export interface GroupMemberLike {
  taskId: string
  status: TaskStatus
  downloadedBytes: number
  totalBytes: number
  speed: number
  saveDir: string
  fileName: string
}

/** 活跃态：排队中(0)/下载中(1)/正在准备(5)（对齐桌面 isActiveOrQueued）。 */
export function isActiveStatus(status: TaskStatus): boolean {
  return status === 0 || status === 1 || status === 5
}

/** 状态 → 视觉分类（火花条/成员点用），对齐桌面 _sparkBarColor/_sparklineColor。 */
export function memberStatusClass(status: TaskStatus): 'done' | 'err' | 'pause' | 'active' {
  if (status === 3) return 'done'
  if (status === 4) return 'err'
  if (status === 2) return 'pause'
  return 'active'
}

// ---- 火花条抽样（design-proto-spec §8 sparkHtml） ----

/** 等距抽样 maxBars 根（`step = len / maxBars`，取 `items[floor(i*step)]`）；
 *  成员数 <= maxBars 时逐项返回。 */
export function sampleSparkline<T>(items: T[], maxBars = 24): T[] {
  if (items.length <= maxBars) return items
  const step = items.length / maxBars
  const out: T[] = []
  for (let i = 0; i < maxBars; i++) out.push(items[Math.floor(i * step)])
  return out
}

// ---- 路径链压缩（design-proto-spec §8 dirRowHtml 路径压缩） ----

/** 目录路径压缩：按 `/`（含反斜杠归一化）分段，`>3` 段压缩为 "首段 / … / 末段 /"，
 *  `<=3` 段整链 "seg / seg / … /"。空路径（组根目录）返回空串。 */
export function compressPathChain(path: string): string {
  const segments = path.replace(/\\/g, '/').split('/').filter(Boolean)
  if (segments.length === 0) return ''
  if (segments.length <= 3) return `${segments.join(' / ')} /`
  return `${segments[0]} / … / ${segments[segments.length - 1]} /`
}

/** 成员在组内的相对目录路径（''=组根目录）：`task.saveDir` 相对 `group.saveDir` 的前缀差；
 *  两者相同或无法识别前缀关系时视为组根目录。 */
export function groupMemberDirPath(taskSaveDir: string, groupSaveDir: string): string {
  const root = groupSaveDir.replace(/\\/g, '/')
  const dir = taskSaveDir.replace(/\\/g, '/')
  if (dir === root) return ''
  const rootWithSlash = root.endsWith('/') ? root : `${root}/`
  if (!dir.startsWith(rootWithSlash)) return ''
  return dir.slice(rootWithSlash.length)
}

/** 展示用名称：组名为空（用户建组未命名）时回退保存目录末段，保证组行永远有非空可读名称。 */
export function groupDisplayName(group: GroupDto): string {
  if (group.name) return group.name
  const normalized = group.saveDir.replace(/\\/g, '/')
  const trimmed = normalized.endsWith('/') ? normalized.slice(0, -1) : normalized
  const idx = trimmed.lastIndexOf('/')
  return idx >= 0 ? trimmed.slice(idx + 1) : trimmed
}

// ---- 组成员计数（design-proto-spec §8 groupCountsHtml） ----

export interface GroupMemberCounts {
  total: number
  done: number
  downloading: number
  pending: number
  paused: number
  failed: number
}

export function computeGroupCounts(members: { status: TaskStatus }[]): GroupMemberCounts {
  const counts: GroupMemberCounts = { total: members.length, done: 0, downloading: 0, pending: 0, paused: 0, failed: 0 }
  for (const m of members) {
    switch (m.status) {
      case 3:
        counts.done++
        break
      case 1:
      case 5:
        counts.downloading++
        break
      case 0:
        counts.pending++
        break
      case 2:
        counts.paused++
        break
      case 4:
        counts.failed++
        break
    }
  }
  return counts
}

// ---- 组聚合（design-proto-spec §3.2 GroupEntity 派生规则） ----

export interface GroupAggregate {
  downloadedBytes: number
  totalBytes: number
  /** [0,1] */
  progress: number
  speedBytesPerSec: number
  statusBucket: TaskStatus
  counts: GroupMemberCounts
}

/** 组按成员状态推导的聚合字段：SUM 进度/速度/计数 + 组级 statusBucket
 *  （空组=completed；有活跃成员=downloading；无活跃但有失败=error；全部完成=completed；否则=paused）。 */
export function computeGroupAggregate<T extends GroupMemberLike>(members: T[]): GroupAggregate {
  let downloadedBytes = 0
  let totalBytes = 0
  let speedBytesPerSec = 0
  for (const m of members) {
    downloadedBytes += m.downloadedBytes
    totalBytes += m.totalBytes
    if (isActiveStatus(m.status)) speedBytesPerSec += m.speed
  }
  const progress = totalBytes > 0 ? Math.min(1, Math.max(0, downloadedBytes / totalBytes)) : 0
  const counts = computeGroupCounts(members)

  let statusBucket: TaskStatus
  if (members.length === 0) statusBucket = 3
  else if (members.some((m) => isActiveStatus(m.status))) statusBucket = 1
  else if (members.some((m) => m.status === 4)) statusBucket = 4
  else if (members.every((m) => m.status === 3)) statusBucket = 3
  else statusBucket = 2

  return { downloadedBytes, totalBytes, progress, speedBytesPerSec, statusBucket, counts }
}

// ---- 组内成员排序 + 目录分段行（design-proto-spec §8 membersHtml） ----

export type GroupFlatEntry<T> =
  | { kind: 'dir'; path: string; fileCount: number; totalBytes: number }
  | { kind: 'member'; task: T; dirPath: string }

/** 组内目录折叠状态的复合键（`<groupId>:<path>`），与 context.tsx collapsedDirs 配套使用。 */
export function dirKey(groupId: string, path: string): string {
  return `${groupId}:${path}`
}

/** 组内成员按（目录+文件名）排序，非空目录变化处插入目录分段行；`isDirCollapsed` 为真
 *  时该目录下成员行整体跳过（目录头仍保留，供重新展开）。返回值不含组头行本身
 *  （渲染方在其后紧跟插入）；根目录（空 dir）成员直贴、不产出目录头。 */
export function flattenGroupMembers<T extends GroupMemberLike>(
  members: T[],
  groupSaveDir: string,
  isDirCollapsed?: (path: string) => boolean,
): GroupFlatEntry<T>[] {
  const withDir = members.map((task) => ({ task, dir: groupMemberDirPath(task.saveDir, groupSaveDir) }))
  withDir.sort((a, b) => {
    const fa = a.dir ? `${a.dir}/${a.task.fileName}` : a.task.fileName
    const fb = b.dir ? `${b.dir}/${b.task.fileName}` : b.task.fileName
    return fa.localeCompare(fb)
  })

  const result: GroupFlatEntry<T>[] = []
  let currentDir: string | null = null
  for (const e of withDir) {
    if (e.dir !== currentDir) {
      currentDir = e.dir
      if (e.dir) {
        const inDir = withDir.filter((x) => x.dir === e.dir)
        result.push({
          kind: 'dir',
          path: e.dir,
          fileCount: inDir.length,
          totalBytes: inDir.reduce((s, x) => s + x.task.totalBytes, 0),
        })
      }
    }
    if (e.dir && isDirCollapsed?.(e.dir)) continue
    result.push({ kind: 'member', task: e.task, dirPath: e.dir })
  }
  return result
}
