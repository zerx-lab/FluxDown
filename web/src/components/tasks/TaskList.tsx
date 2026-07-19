// 中央任务列表：过滤 → 显示已完成开关 → 7 维分桶（智能/日期/状态/类型/队列/站点/不分组）
// → 桶内 6 键排序 → （仅列表形态）在组行后插入目录分段行+成员行 → 用
// @tanstack/react-virtual 虚拟滚动。网格形态为 bento 行装箱卡片网格（GridCard.tsx，
// 组卡跨 2 列；对齐桌面 _buildGridBody：最小卡宽 210 / 间距 10 / 卡高 138），
// 不支持组内展开（对齐桌面「网格降级」）。分桶/排序纯函数见 lib/list-sections.ts，
// 视图偏好（形态/密度/分组/排序/显示开关/列）见 lib/view-prefs.ts，按状态页签独立记忆。
//
// 组聚合：groupId 非空但组列表查不到（孤儿成员）与无 groupId 的任务一律按普通任务平铺
// 兜底。状态/类型/队列筛选 + 显示已完成开关作用于成员；搜索词命中组名时整组（含全部
// 成员）可见，命中成员文件名时组行+命中成员可见（组行仅在有可见成员时出现，计数按可见
// 成员）。组行本身不参与既有多选批量。
// 对齐 design/web/app.js renderList() + lib/src/models/download_controller.dart buildListSections()。

import { useEffect, useRef, useState, type CSSProperties } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useVirtualizer } from '@tanstack/react-virtual'
import { ChevronDown, ChevronRight, Folder } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { fmtBytes } from '../../lib/format'
import { useI18n } from '../../lib/i18n'
import { bucketEntities, compareSectionEntities, orderSections, type SectionEntity } from '../../lib/list-sections'
import { compressPathChain, dirKey, flattenGroupMembers, groupDisplayName } from '../../lib/task-group'
import { useViewPrefs } from '../../lib/view-prefs'
import type { GroupDto } from '../../lib/types'
import { GroupRow } from './GroupRow'
import { GroupGridCard, TaskGridCard } from './GridCard'
import { TaskRow } from './TaskRow'
import { filterTasks } from './filters'
import { useTasksUi } from './context'
import { useViewTasks, type ViewTask } from './useViewTasks'

type FlatItem =
  | { kind: 'sectionhead'; key: string; title: string; count: number }
  | { kind: 'row'; task: ViewTask }
  | { kind: 'grouprow'; group: GroupDto; members: ViewTask[] }
  | { kind: 'groupdir'; groupId: string; path: string; fileCount: number; totalBytes: number }
  | { kind: 'groupmember'; task: ViewTask }
  | { kind: 'gridrow'; entities: SectionEntity<ViewTask>[] }

// 行的估算尺寸取自 design.css：.task-row/.grow min-height 64 + margin-bottom 4（虚拟
// 滚动下 margin 不参与相邻元素排布，需并入 estimateSize 才能还原视觉间距）。
// 组行与任务行等高（§4.6 组不豁免行高节奏）：舒适 64+4、紧凑 44+4 单行化
// （meta/计数并入行内、进度条移行底或隐藏）。
const SECTION_HEAD_SIZE = 32
const ROW_SIZE = 68
const ROW_COMPACT_SIZE = 48
const GROW_SIZE = 68
const GROW_COMPACT_SIZE = 48
const GDIR_SIZE = 28
const GDIR_COMPACT_SIZE = 24
const GRID_CARD_HEIGHT = 138
const GRID_GAP = 10
const GRID_ROW_SIZE = GRID_CARD_HEIGHT + GRID_GAP
const GRID_CARD_MIN_WIDTH = 210

export function TaskList() {
  const { t } = useI18n()
  const {
    statusTab,
    typeFilter,
    queueFilter,
    search,
    foldedSections,
    toggleSectionFold,
    manageMode,
    expandedGroups,
    scrollTarget,
    clearScrollTarget,
    collapsedDirs,
    toggleDirCollapsed,
  } = useTasksUi()
  const prefs = useViewPrefs(statusTab)
  const tasks = useViewTasks()
  const { data: queues = [] } = useQuery({ queryKey: ['queues'], queryFn: api.listQueues })
  const { data: groups = [] } = useQuery({ queryKey: ['groups'], queryFn: api.listGroups })
  const parentRef = useRef<HTMLDivElement>(null)
  const [containerWidth, setContainerWidth] = useState(960)

  useEffect(() => {
    const el = parentRef.current
    if (!el) return
    // RO 的初始回调依赖渲染帧（后台标签页/无帧环境可能不派发）——先同步量一次
    // 内容盒宽度，保证首帧列数就正确；RO 只负责后续尺寸变化。
    const cs = getComputedStyle(el)
    const initial = el.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight)
    if (initial > 0) setContainerWidth(initial)
    const ro = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width
      if (w) setContainerWidth(w)
    })
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  const groupsById = new Map(groups.map((g) => [g.groupId, g]))
  const groupNameByGroupId = new Map(groups.map((g) => [g.groupId, groupDisplayName(g).toLowerCase()]))
  const filteredByDims = filterTasks(tasks, { statusTab, typeFilter, queueFilter, search, groupNameByGroupId })
  const filtered = prefs.showCompleted ? filteredByDims : filteredByDims.filter((task) => task.status !== 3)

  // 按 groupId 聚合：仅存在于 groups 列表中的组才聚合为活卡片。
  const membersByGroup = new Map<string, ViewTask[]>()
  const flatTasks: ViewTask[] = []
  for (const task of filtered) {
    const g = task.groupId ? groupsById.get(task.groupId) : undefined
    if (!g) {
      flatTasks.push(task)
      continue
    }
    const arr = membersByGroup.get(g.groupId)
    if (arr) arr.push(task)
    else membersByGroup.set(g.groupId, [task])
  }
  const entities: SectionEntity<ViewTask>[] = flatTasks.map((task) => ({ kind: 'task', task }))
  for (const [groupId, members] of membersByGroup) entities.push({ kind: 'group', group: groupsById.get(groupId)!, members })

  const bucketed = bucketEntities(entities, prefs.groupBy, queues)
  for (const section of bucketed) {
    section.entities.sort((a, b) => compareSectionEntities(prefs.sortKey, prefs.sortDir, a, b, groupDisplayName))
  }
  const sections = orderSections(bucketed, prefs.sortKey, prefs.sortDir, groupDisplayName)

  const isGrid = prefs.form === 'grid'
  const cardsPerRow = Math.max(1, Math.floor((containerWidth + GRID_GAP) / (GRID_CARD_MIN_WIDTH + GRID_GAP)))

  const flat: FlatItem[] = []
  for (const section of sections) {
    if (section.title !== null) flat.push({ kind: 'sectionhead', key: section.key, title: section.title, count: section.entities.length })
    if (foldedSections.has(section.key)) continue

    if (isGrid) {
      // 行装箱：task 占 1 槽，group 占 2 槽，贪心凑满 cardsPerRow 即换行（网格无组内展开机制）。
      let row: SectionEntity<ViewTask>[] = []
      let used = 0
      for (const e of section.entities) {
        const cost = e.kind === 'group' ? Math.min(2, cardsPerRow) : 1
        if (used + cost > cardsPerRow && row.length > 0) {
          flat.push({ kind: 'gridrow', entities: row })
          row = []
          used = 0
        }
        row.push(e)
        used += cost
      }
      if (row.length > 0) flat.push({ kind: 'gridrow', entities: row })
      continue
    }

    for (const e of section.entities) {
      if (e.kind === 'task') {
        flat.push({ kind: 'row', task: e.task })
        continue
      }
      flat.push({ kind: 'grouprow', group: e.group, members: e.members })
      if (!expandedGroups.has(e.group.groupId)) continue
      const groupId = e.group.groupId
      const isDirCollapsed = (path: string) => collapsedDirs.has(dirKey(groupId, path))
      for (const m of flattenGroupMembers(e.members, e.group.saveDir, isDirCollapsed)) {
        if (m.kind === 'dir') flat.push({ kind: 'groupdir', groupId, path: m.path, fileCount: m.fileCount, totalBytes: m.totalBytes })
        else flat.push({ kind: 'groupmember', task: m.task })
      }
    }
  }

  // 紧凑档行高（design §4.4：任务/组行 44+4、目录行 24）；网格形态密度不适用。
  const isCompact = prefs.density === 'compact'
  const virtualizer = useVirtualizer({
    count: flat.length,
    getScrollElement: () => parentRef.current,
    // estimateSize 不在 react-virtual 的 measurements memo deps 里——把形态/密度
    // 编进 item key，切换时强制整表重算，否则沿用旧行高导致行错位。
    getItemKey: (i) => `${prefs.form}:${prefs.density}:${i}`,
    estimateSize: (i) => {
      const item = flat[i]
      if (item.kind === 'sectionhead') return SECTION_HEAD_SIZE
      if (item.kind === 'gridrow') return GRID_ROW_SIZE
      if (item.kind === 'grouprow') return isCompact ? GROW_COMPACT_SIZE : GROW_SIZE
      if (item.kind === 'groupdir') return isCompact ? GDIR_COMPACT_SIZE : GDIR_SIZE
      return isCompact ? ROW_COMPACT_SIZE : ROW_SIZE
    },
    overscan: 8,
  })

  // 失败直达（组计数行点击）：目标组/目录已由 jumpToGroupMember 展开，这里只负责滚动；
  // 找不到（已被筛选隐藏，或当前为网格形态无展开机制）时静默清空，避免悬挂状态。
  useEffect(() => {
    if (!scrollTarget) return
    const index = flat.findIndex((item) => item.kind === 'groupmember' && item.task.taskId === scrollTarget)
    if (index >= 0) virtualizer.scrollToIndex(index, { align: 'center' })
    clearScrollTarget()
  }, [scrollTarget])

  return (
    <div className={cn('task-scroll', manageMode && 'manage', isGrid && 'grid-form')} ref={parentRef}>
      {flat.length === 0 ? (
        <p className="empty-tip">{t('list.empty')}</p>
      ) : (
        <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
          {virtualizer.getVirtualItems().map((vi) => {
            const item = flat[vi.index]
            return (
              <div key={vi.key} style={{ position: 'absolute', top: 0, left: 0, right: 0, transform: `translateY(${vi.start}px)` }}>
                {item.kind === 'sectionhead' && (
                  <div className={cn('group-head', foldedSections.has(item.key) && 'folded')} onClick={() => toggleSectionFold(item.key)}>
                    <ChevronDown size={12} />
                    {item.title} <em>· {item.count}</em>
                  </div>
                )}
                {item.kind === 'row' && (
                  <TaskRow task={item.task} queues={queues} density={prefs.density} protocolBadges={prefs.protocolBadges} columns={prefs.columns} />
                )}
                {item.kind === 'grouprow' && <GroupRow group={item.group} members={item.members} density={prefs.density} />}
                {item.kind === 'groupdir' && (
                  <div className={cn('gdir-row', isCompact && 'compact')} onClick={() => toggleDirCollapsed(item.groupId, item.path)}>
                    {collapsedDirs.has(dirKey(item.groupId, item.path)) ? <ChevronRight size={11} /> : <ChevronDown size={11} />}
                    <Folder size={12} />
                    <span className="ellip">{compressPathChain(item.path)}</span>
                    <span className="gdir-meta">{t('group.dirMeta', { count: item.fileCount, size: fmtBytes(item.totalBytes) })}</span>
                  </div>
                )}
                {item.kind === 'groupmember' && (
                  <div className="grow-member">
                    <TaskRow task={item.task} queues={queues} density={prefs.density} protocolBadges={prefs.protocolBadges} columns={prefs.columns} />
                  </div>
                )}
                {item.kind === 'gridrow' && (
                  <div className="grid-row" style={{ '--grid-cols': cardsPerRow } as CSSProperties}>
                    {item.entities.map((e) =>
                      e.kind === 'task' ? (
                        <div className="grid-cell" key={e.task.taskId}>
                          <TaskGridCard task={e.task} queues={queues} protocolBadges={prefs.protocolBadges} />
                        </div>
                      ) : (
                        <div className="grid-cell grid-cell-wide" style={{ gridColumn: `span ${Math.min(2, cardsPerRow)}` }} key={e.group.groupId}>
                          <GroupGridCard group={e.group} members={e.members} />
                        </div>
                      ),
                    )}
                  </div>
                )}
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}
