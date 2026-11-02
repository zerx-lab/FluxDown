// 移动端任务列表：卡片行（名称 / 进度 / 状态行）。
// 点击 = 有选中时切换选中，否则打开详情；左侧图标区点按 = 切换选中；⋯ / 长按 = 动作菜单（底部 Sheet）。

import { useVirtualizer } from '@tanstack/react-virtual'
import { ChevronDown, ChevronRight, Ellipsis, Folder } from 'lucide-react'
import { memo, useCallback, useRef } from 'react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { ActionMenu, Checkbox, ContextMenuArea, Icon } from '../../../ui'
import type { MenuEntry } from '../../../ui'
import { notePointerActivity } from '../model/rowOrder'
import { formatBytes, percentLabel, sourceSite } from '../model/task'
import type { DownloadTaskView } from '../model/task'
import { useDownloads } from '../state'
import { useConflictTaskIds } from '../dialogs/fileConflict'
import { ConflictBadge, KindGlyph } from './cells'
import { kindLabel, STATUS_TEXT, statusDetail, statusLabel, taskStatusClass } from './text'
import type { Translate } from './text'
import { usePluginTaskCommands } from '../../plugins/usePluginContributions'
import { buildGroupMenu, buildTaskMenu } from './menus'
import { SegmentProgress } from './SegmentProgress'
import { TaskEmpty } from './TaskEmpty'
import { TaskLoading } from './TaskLoading'
import { useRowAccessors } from './rowLayout'
import { groupCheckState } from '../model/groupRows'
import type { DirRow, GroupFolderRow, MemberRow } from '../model/groupRows'
import { aggregateOf, compressPath } from '../model/groupTree'
import type { TaskRow as EngineTaskRow } from '../model/rowEngine'
import { groupCountSegments } from './groupFacts'
import { CountsLine, GroupGlyph } from './groupText'
import { TREE_INDENT } from './rowMetrics'
import { TreeRails } from './rowParts'
import { stateLabel } from '../state'

interface CardProps {
  t: Translate
  view: DownloadTaskView
  /** 树缩进级数（组内成员行 1 / 2，顶层任务 0）。 */
  depth: number
  conflict: boolean
  selected: boolean
  anySelected: boolean
  onTap: (view: DownloadTaskView) => void
  onToggle: (view: DownloadTaskView) => void
  getMenu: (view: DownloadTaskView) => MenuEntry[]
  onContext: (view: DownloadTaskView) => void
}

const TaskCard = memo(function TaskCard({ t, view, depth, conflict, selected, anySelected, onTap, onToggle, getMenu, onContext }: CardProps) {
  const detail = statusDetail(t, view)
  const site = sourceSite(view)
  const meta = [kindLabel(t, view.kind), site, view.sizeBytes > 0 ? formatBytes(view.sizeBytes) : '']
    .filter((part) => part !== '')
    .join(' · ')
  return (
    <ContextMenuArea entries={() => getMenu(view)} title={view.name} className="h-full">
      <div
        role="row"
        aria-selected={selected}
        onContextMenu={() => onContext(view)}
        className={cn('relative flex min-h-touch select-none items-stretch active:bg-row-hover', selected && 'bg-accent')}
        style={{ paddingLeft: depth * TREE_INDENT }}
      >
        {depth > 0 ? <TreeRails depth={depth} /> : null}
        <button
          type="button"
          aria-label={view.name}
          onClick={(event) => {
            event.stopPropagation()
            onToggle(view)
          }}
          className="flex w-11 shrink-0 items-center justify-center"
        >
          {anySelected || selected ? (
            <Checkbox checked={selected} aria-label={view.name} className="pointer-events-none" />
          ) : (
            <KindGlyph view={view} />
          )}
        </button>
        <div className="min-w-0 flex-1 cursor-pointer py-2" onClick={() => onTap(view)}>
          <div className={cn('truncate text-sm', view.metadataPending ? 'text-muted-foreground' : 'text-foreground')}>
            {view.metadataPending ? t('statusPreparing') : view.name}
          </div>
          {view.state !== 'completed' ? (
            <div className="mt-1.5 flex items-center gap-2">
              <SegmentProgress
                runtime={view.runtime}
                progress={view.progress}
                state={view.state}
                className="min-w-0 flex-1"
              />
              <span className="tabular w-8 shrink-0 text-right text-xs text-muted-foreground">
                {percentLabel(view.progress)}
              </span>
            </div>
          ) : null}
          <div className="tabular mt-1 flex min-w-0 items-center gap-1.5 text-xs">
            <span className={cn('shrink-0', taskStatusClass(view))}>{statusLabel(t, view)}</span>
            {conflict ? <ConflictBadge t={t} /> : null}
            {view.state === 'completed' ? (
              <span className="truncate text-text-tertiary">{meta}</span>
            ) : detail ? (
              <span className="truncate text-text-tertiary">{detail}</span>
            ) : null}
          </div>
        </div>
        <ActionMenu
          title={view.name}
          entries={getMenu(view)}
          trigger={
            <button
              type="button"
              aria-label={view.name}
              onClick={() => onContext(view)}
              className="flex w-11 shrink-0 items-center justify-center text-muted-foreground active:text-foreground"
            >
              <Icon icon={Ellipsis} />
            </button>
          }
        />
      </div>
    </ContextMenuArea>
  )
})

function* viewsOf(rows: readonly EngineTaskRow[]): Generator<DownloadTaskView> {
  for (const row of rows) yield row.view
}

interface GroupCardProps {
  t: Translate
  row: GroupFolderRow
  selected: ReadonlySet<string>
  anySelected: boolean
  onTap: (row: GroupFolderRow) => void
  onToggle: (row: GroupFolderRow) => void
  getMenu: (row: GroupFolderRow) => MenuEntry[]
  onContext: (row: GroupFolderRow) => void
}

/** 任务组卡片：左侧图标区 = 切换全部成员选中；点击 = 有选中时切换选中，否则展开 / 折叠；⋯ / 长按 = 组菜单。 */
function GroupCard({ t, row, selected, anySelected, onTap, onToggle, getMenu, onContext }: GroupCardProps) {
  const aggregate = aggregateOf(viewsOf(row.members))
  const check = groupCheckState(selected, row.members)
  const isSelected = check === true
  return (
    <ContextMenuArea entries={() => getMenu(row)} title={row.name} className="h-full">
      <div
        role="row"
        aria-selected={isSelected}
        aria-expanded={row.expanded}
        onContextMenu={() => onContext(row)}
        className={cn('relative flex min-h-touch select-none items-stretch active:bg-row-hover', isSelected && 'bg-accent')}
      >
        <button
          type="button"
          aria-label={row.name}
          onClick={(event) => {
            event.stopPropagation()
            onToggle(row)
          }}
          className="flex w-11 shrink-0 items-center justify-center"
        >
          {anySelected || check !== false ? (
            <Checkbox checked={check} aria-label={row.name} className="pointer-events-none" />
          ) : (
            <GroupGlyph count={row.members.length} />
          )}
        </button>
        <div className="min-w-0 flex-1 cursor-pointer py-2" onClick={() => onTap(row)}>
          <div className="flex min-w-0 items-center gap-1 text-sm font-medium text-foreground">
            <Icon icon={row.expanded ? ChevronDown : ChevronRight} size="sm" className="shrink-0 text-text-tertiary" />
            <span className="truncate">{row.name}</span>
          </div>
          {aggregate.state !== 'completed' ? (
            <div className="mt-1.5 flex items-center gap-2">
              <SegmentProgress progress={aggregate.progress} state={aggregate.state} className="min-w-0 flex-1" />
              <span className="tabular w-8 shrink-0 text-right text-xs text-muted-foreground">{percentLabel(aggregate.progress)}</span>
            </div>
          ) : null}
          <div className="tabular mt-1 flex min-w-0 items-center gap-1.5 text-xs">
            <span className={cn('shrink-0', STATUS_TEXT[aggregate.state])}>{stateLabel(t, aggregate.state)}</span>
            <CountsLine segments={groupCountSegments(t, aggregate)} className="min-w-0 truncate text-text-tertiary" />
          </div>
        </div>
        <ActionMenu
          title={row.name}
          entries={getMenu(row)}
          trigger={
            <button
              type="button"
              aria-label={row.name}
              onClick={() => onContext(row)}
              className="flex w-11 shrink-0 items-center justify-center text-muted-foreground active:text-foreground"
            >
              <Icon icon={Ellipsis} />
            </button>
          }
        />
      </div>
    </ContextMenuArea>
  )
}

/** 组内目录分段卡片：点击切换该目录折叠；不可选、无菜单。 */
function DirCard({ t, row, onToggle }: { t: Translate; row: DirRow; onToggle: (groupId: string, dir: string) => void }) {
  return (
    <div
      role="row"
      aria-expanded={!row.collapsed}
      onClick={() => onToggle(row.groupId, row.path)}
      className="relative flex min-h-touch cursor-pointer select-none items-center gap-2 active:bg-row-hover"
      style={{ paddingLeft: TREE_INDENT }}
    >
      <TreeRails depth={1} />
      <div className="flex w-11 shrink-0 items-center justify-center">
        <Icon icon={Folder} className="text-muted-foreground" />
      </div>
      <div className="min-w-0 flex-1 py-2 text-sm">
        <div className="flex min-w-0 items-center gap-1">
          <Icon icon={row.collapsed ? ChevronRight : ChevronDown} size="sm" className="shrink-0 text-text-tertiary" />
          <span className="truncate font-medium text-muted-foreground" title={row.path}>
            {compressPath(row.path)}
          </span>
        </div>
        <div className="tabular truncate text-xs text-text-tertiary">
          {t('groupDirCount', { count: row.count })}
        </div>
      </div>
    </div>
  )
}

export function TaskCards() {
  const t = useT()
  const ctx = useDownloads()
  const conflictTaskIds = useConflictTaskIds()
  const pluginCommands = usePluginTaskCommands()
  const { rows, selected, selectedViews, queues } = ctx
  const scrollRef = useRef<HTMLDivElement>(null)
  // 行数据随进度帧每帧换新，键序列不变时回调保持引用稳定，虚拟化器不重算全部行位置。
  const { getItemKey, estimateSize } = useRowAccessors(rows, 36, 84)
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize,
    overscan: 8,
    getItemKey,
  })

  const onTap = useCallback(
    (view: DownloadTaskView) => {
      if (selected.size > 0 || view.source !== 'local') ctx.toggleSelected(view.key)
      else ctx.showDetail(view.taskId)
    },
    [ctx, selected.size],
  )
  const getMenu = useCallback(
    (view: DownloadTaskView) =>
      buildTaskMenu({
        t,
        views: selected.has(view.key) ? selectedViews : [view],
        queues,
        queueName: ctx.queueName,
        showDetail: ctx.showDetail,
        pluginCommands,
      }),
    [t, selected, selectedViews, queues, ctx.queueName, ctx.showDetail, pluginCommands],
  )
  const getGroupMenu = (row: GroupFolderRow) =>
    buildGroupMenu(t, row.group, row.name, ctx.groupTotals(row.groupId)?.counts, ctx.allViews)

  if (ctx.tasksLoading) return <TaskLoading />
  if (rows.length === 0) return <TaskEmpty />

  const renderTask = (row: EngineTaskRow | MemberRow) => {
    const view = row.type === 'member' ? row.row.view : row.view
    return (
      <TaskCard
        t={t}
        view={view}
        depth={row.type === 'member' ? row.depth : 0}
        conflict={view.source === 'local' && conflictTaskIds.has(view.taskId)}
        selected={selected.has(row.key)}
        anySelected={selected.size > 0}
        onTap={onTap}
        onToggle={(target) => ctx.toggleSelected(target.key)}
        getMenu={getMenu}
        onContext={(target) => ctx.contextSelect(target.key)}
      />
    )
  }

  return (
    <div
      ref={scrollRef}
      onTouchStart={() => notePointerActivity()}
      onTouchMove={() => notePointerActivity()}
      onWheel={() => notePointerActivity()}
      onContextMenu={() => notePointerActivity()}
      className="h-full min-h-0 overflow-y-auto overflow-x-hidden bg-surface pb-24"
    >
      <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((item) => {
          const row = rows[item.index]
          if (!row) return null
          return (
            <div
              key={item.key}
              data-index={item.index}
              ref={virtualizer.measureElement}
              className="absolute left-0 top-0 w-full"
              style={{ transform: `translateY(${item.start}px)` }}
            >
              {row.type === 'bucket' ? (
                <button
                  type="button"
                  onClick={() => ctx.toggleBucketCollapsed(row.key)}
                  className="flex min-h-touch w-full items-end gap-1 px-3 pb-1 text-left text-xs"
                >
                  <Icon icon={row.collapsed ? ChevronRight : ChevronDown} size="sm" className="mb-0.5 text-text-tertiary" />
                  <span className="min-w-0 truncate font-medium text-foreground">{row.label}</span>
                  <span className="tabular shrink-0 text-text-tertiary">{row.count}</span>
                </button>
              ) : row.type === 'group' ? (
                <GroupCard
                  t={t}
                  row={row}
                  selected={selected}
                  anySelected={selected.size > 0}
                  onTap={(target) => (selected.size > 0 ? ctx.toggleGroupSelected(target) : ctx.toggleGroupExpanded(target.groupId))}
                  onToggle={ctx.toggleGroupSelected}
                  getMenu={getGroupMenu}
                  onContext={ctx.contextSelectGroup}
                />
              ) : row.type === 'dir' ? (
                <DirCard t={t} row={row} onToggle={ctx.toggleDirCollapsed} />
              ) : (
                renderTask(row)
              )}
            </div>
          )
        })}
      </div>
    </div>
  )
}
