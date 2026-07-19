// 任务筛选/计数的纯函数 —— Sidebar/StatusTabs/TaskList/ManageBar 共用同一套语义。
// 状态 Tab 计数对齐 design/web/app.js：统计对象始终是「全量任务」，不叠加其余筛选维度；
// 仅 filterTasks（渲染任务列表用）会同时叠加 tab + 类型 + 队列 + 搜索四个维度。

import { fileType, type FileType } from '../../lib/format'
import type { TaskStatus } from '../../lib/types'
import type { ViewTask } from './useViewTasks'

export type StatusTab = 'all' | 'downloading' | 'completed' | 'paused' | 'error'

/** 下载中 Tab 归并 pending(0) / downloading(1) / preparing(5)；其余 Tab 各对应单一状态码。 */
const TAB_STATUSES: Record<Exclude<StatusTab, 'all'>, readonly TaskStatus[]> = {
  downloading: [0, 1, 5],
  completed: [3],
  paused: [2],
  error: [4],
}

export function matchesStatusTab(tab: StatusTab, status: TaskStatus): boolean {
  return tab === 'all' || TAB_STATUSES[tab].includes(status)
}

export function countByStatusTab(tasks: ViewTask[], tab: StatusTab): number {
  return tab === 'all' ? tasks.length : tasks.filter((t) => matchesStatusTab(tab, t.status)).length
}

export interface TaskFilters {
  statusTab: StatusTab
  typeFilter: 'all' | FileType
  queueFilter: string
  search: string
  /** groupId → 展示名小写形式；搜索词命中组名时该组全部成员视为命中，即使各自
   *  文件名不匹配（TopBar 搜索匹配组名）。未传等价于仅按文件名过滤。 */
  groupNameByGroupId?: Map<string, string>
}

/** 任务列表实际渲染用的组合过滤（tab + 类型 + 队列 + 搜索，搜索额外匹配所属组名）。 */
export function filterTasks(tasks: ViewTask[], f: TaskFilters): ViewTask[] {
  const q = f.search.trim().toLowerCase()
  return tasks.filter((t) => {
    if (!matchesStatusTab(f.statusTab, t.status)) return false
    if (f.typeFilter !== 'all' && fileType(t.fileName, t.url) !== f.typeFilter) return false
    if (f.queueFilter !== 'all' && t.queueId !== f.queueFilter) return false
    if (q) {
      const groupName = t.groupId ? f.groupNameByGroupId?.get(t.groupId) : undefined
      const matchesGroup = groupName !== undefined && groupName.includes(q)
      if (!matchesGroup && !t.fileName.toLowerCase().includes(q)) return false
    }
    return true
  })
}
