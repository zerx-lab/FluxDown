// 中央任务列表：过滤 → 按时间分组（今天/昨天/本周/本月/更早）→ 组内按创建时间降序 →
// 扁平化 [组头, 行, 行, ...] 用 @tanstack/react-virtual 虚拟滚动。
// 对齐 design/web/app.js renderList()。

import { useRef } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useVirtualizer } from '@tanstack/react-virtual'
import { ChevronDown } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { groupLabel, GROUP_ORDER, timeGroup, type TimeGroup } from '../../lib/format'
import { useI18n } from '../../lib/i18n'
import { TaskRow } from './TaskRow'
import { filterTasks } from './filters'
import { useTasksUi } from './context'
import { useViewTasks, type ViewTask } from './useViewTasks'

type FlatItem = { kind: 'group'; group: TimeGroup; count: number } | { kind: 'row'; task: ViewTask }

// 行的估算尺寸取自 design.css：.task-row min-height 64 + margin-bottom 4（虚拟滚动下
// margin 不参与相邻元素排布，需并入 estimateSize 才能还原视觉间距）。
const GROUP_HEAD_SIZE = 32
const ROW_SIZE = 68

export function TaskList() {
  const { t } = useI18n()
  const { statusTab, typeFilter, queueFilter, search, folded, toggleFold, manageMode } = useTasksUi()
  const tasks = useViewTasks()
  const { data: queues = [] } = useQuery({ queryKey: ['queues'], queryFn: api.listQueues })
  const parentRef = useRef<HTMLDivElement>(null)

  const filtered = filterTasks(tasks, { statusTab, typeFilter, queueFilter, search })
  const grouped = new Map<TimeGroup, ViewTask[]>()
  for (const task of filtered) {
    const g = timeGroup(task.createdAt)
    const arr = grouped.get(g)
    if (arr) arr.push(task)
    else grouped.set(g, [task])
  }
  for (const arr of grouped.values()) arr.sort((a, b) => Number(b.createdAt) - Number(a.createdAt))

  const flat: FlatItem[] = []
  for (const g of GROUP_ORDER) {
    const items = grouped.get(g)
    if (!items || items.length === 0) continue
    flat.push({ kind: 'group', group: g, count: items.length })
    if (!folded.has(g)) for (const t of items) flat.push({ kind: 'row', task: t })
  }

  const virtualizer = useVirtualizer({
    count: flat.length,
    getScrollElement: () => parentRef.current,
    estimateSize: (i) => (flat[i].kind === 'group' ? GROUP_HEAD_SIZE : ROW_SIZE),
    overscan: 8,
  })

  return (
    <div className={cn('task-scroll', manageMode && 'manage')} ref={parentRef}>
      {flat.length === 0 ? (
        <p className="empty-tip">{t('list.empty')}</p>
      ) : (
        <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
          {virtualizer.getVirtualItems().map((vi) => {
            const item = flat[vi.index]
            return (
              <div key={vi.key} style={{ position: 'absolute', top: 0, left: 0, right: 0, transform: `translateY(${vi.start}px)` }}>
                {item.kind === 'group' ? (
                  <div className={cn('group-head', folded.has(item.group) && 'folded')} onClick={() => toggleFold(item.group)}>
                    <ChevronDown size={12} />
                    {groupLabel(item.group)} <em>· {item.count}</em>
                  </div>
                ) : (
                  <TaskRow task={item.task} queues={queues} />
                )}
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}
