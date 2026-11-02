// 桌面任务表（移植 crates/downloads/src/components/task_table.rs）：
// 列 / 宽度 / 最小宽度 / 排序与 GPUI 一致，分组头，悬停行操作，复选框选择，右键菜单，虚拟化。

import { useVirtualizer } from '@tanstack/react-virtual'
import { ArrowDown, ArrowUp, ChevronDown, ChevronRight, Download, Pause, Play, RotateCw } from 'lucide-react'
import { memo, useCallback, useLayoutEffect, useMemo, useRef, useState } from 'react'
import type { MouseEvent, PointerEvent as ReactPointerEvent } from 'react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { Checkbox, ContextMenuArea, Icon, Tooltip } from '../../../ui'
import type { MenuEntry } from '../../../ui'
import { downloadViewsFiles, isDownloadable, openGroupFolder, pauseViews, resumeViews } from '../model/actions'
import { remoteCan } from '../model/batchPlan'
import { notePointerActivity } from '../model/rowOrder'
import { canResumeSeeding, formatBytes, formatDateTime, isSeeding, MAX_ETA_SECS, PROTOCOL_LABEL, sourceSite } from '../model/task'
import type { DownloadTaskView } from '../model/task'
import {
  COLUMN_LABEL_KEY,
  COLUMN_SORT_KEY,
  FILE_NAME_MAX_WIDTH,
  MAX_COLUMN_WIDTH,
  MIN_WIDTH,
  nextHeaderSort,
  resolveColumns,
  toColumnPrefs,
} from '../model/viewPrefs'
import type { ColumnKind, ViewDensity } from '../model/viewPrefs'
import { useDownloads } from '../state'
import { useConflictTaskIds } from '../dialogs/fileConflict'
import { FileCell, KindGlyph, ProgressCell, StatusCell } from './cells'
import { formatEta, seedingUploadLabel } from './text'
import type { Translate } from './text'
import { usePluginTaskCommands } from '../../plugins/usePluginContributions'
import { buildGroupMenu, buildTaskMenu } from './menus'
import { SelectionHeaderBar } from '../selection'
import { TaskEmpty } from './TaskEmpty'
import { TaskLoading } from './TaskLoading'
import { useRowAccessors } from './rowLayout'
import type { BucketRow, GroupFolderRow, MemberRow } from '../model/groupRows'
import type { TaskRow as EngineTaskRow } from '../model/rowEngine'
import { DirRowView, GroupRowView } from './GroupRows'
import {
  BUCKET_SLOT_HEIGHT,
  CELL_PADDING_X,
  cellStyle,
  DIR_INDENT_SLOT,
  DIR_RAIL_OFFSET,
  HEADER_HEIGHT,
  PROGRESS_GAP,
  PROGRESS_LABEL_WIDTH,
  rowHeightClass,
  selectionColumnWidth,
  TRAILING_GUTTER,
} from './rowMetrics'
import type { LayoutColumn } from './rowMetrics'
import { Meta, RowActionsBar } from './rowParts'

interface RowProps {
  t: Translate
  view: DownloadTaskView
  /** 树缩进级数：组内成员行 1（组根）/ 2（目录下），顶层任务 0。 */
  depth: number
  columns: readonly LayoutColumn[]
  density: ViewDensity
  conflict: boolean
  selected: boolean
  anySelected: boolean
  queueName: (queueId: string) => string
  onClick: (event: MouseEvent, view: DownloadTaskView) => void
  onDoubleClick: (view: DownloadTaskView) => void
  onToggle: (view: DownloadTaskView) => void
  getMenu: (view: DownloadTaskView) => MenuEntry[]
  onContext: (view: DownloadTaskView) => void
  rowActionLabels: { pause: string; resume: string; download: string; select: string }
}

function renderCell(props: RowProps, column: LayoutColumn) {
  const { t, view, density, queueName } = props
  const downloading = view.state === 'downloading'
  switch (column.kind) {
    case 'file_name':
      return <FileCell t={t} view={view} density={density} conflict={props.conflict} />
    case 'progress': {
      const barWidth = Math.max(0, column.width - 2 * CELL_PADDING_X - PROGRESS_LABEL_WIDTH - PROGRESS_GAP)
      return <ProgressCell view={view} barWidth={barWidth} />
    }
    case 'size':
      return view.sizeBytes > 0 ? (
        <Meta numeric>{formatBytes(view.sizeBytes)}</Meta>
      ) : (
        <Meta numeric tertiary>
          —
        </Meta>
      )
    case 'status':
      return <StatusCell t={t} view={view} density={density} />
    case 'speed': {
      const upload = seedingUploadLabel(view)
      if (upload !== null) {
        return (
          <Meta numeric success>
            {upload}
          </Meta>
        )
      }
      return downloading && view.speed !== null && view.speed > 0 ? (
        <Meta numeric>{`${formatBytes(view.speed)}/s`}</Meta>
      ) : (
        <Meta numeric tertiary>
          —
        </Meta>
      )
    }
    case 'eta':
      return downloading && view.etaSeconds !== null && view.etaSeconds <= MAX_ETA_SECS ? (
        <Meta numeric>{formatEta(t, view.etaSeconds)}</Meta>
      ) : (
        <Meta numeric tertiary>
          —
        </Meta>
      )
    case 'created':
      return view.createdAtSecs > 0 ? (
        <Meta numeric>{formatDateTime(view.createdAtSecs)}</Meta>
      ) : (
        <Meta numeric tertiary>
          —
        </Meta>
      )
    case 'protocol':
      return <Meta numeric={false}>{PROTOCOL_LABEL[view.protocol]}</Meta>
    case 'source':
      return <Meta numeric={false}>{sourceSite(view)}</Meta>
    case 'queue':
      return <Meta numeric={false}>{view.source === 'local' ? queueName(view.queueId) : ''}</Meta>
  }
}

/** 行悬停操作：下载中 / 排队 → 暂停；暂停 → 继续；失败 → 重试；完成 → 下载文件（仅本地）。 */
function RowActions({ view, labels, selected }: { view: DownloadTaskView; labels: RowProps['rowActionLabels']; selected: boolean }) {
  const buttons: { key: string; label: string; icon: typeof Pause; run: () => void }[] = []
  switch (view.state) {
    case 'downloading':
    case 'pending':
      if (remoteCan(view, 'pause')) buttons.push({ key: 'pause', label: labels.pause, icon: Pause, run: () => void pauseViews([view]) })
      break
    case 'paused':
      if (remoteCan(view, 'resume')) buttons.push({ key: 'resume', label: labels.resume, icon: Play, run: () => void resumeViews([view]) })
      break
    case 'failed':
      if (remoteCan(view, 'resume')) buttons.push({ key: 'retry', label: labels.resume, icon: RotateCw, run: () => void resumeViews([view]) })
      break
    case 'completed':
      if (isSeeding(view)) {
        buttons.push({ key: 'pause', label: labels.pause, icon: Pause, run: () => void pauseViews([view]) })
      } else if (canResumeSeeding(view)) {
        buttons.push({ key: 'resume', label: labels.resume, icon: Play, run: () => void resumeViews([view]) })
      }
      if (isDownloadable(view)) {
        buttons.push({ key: 'download', label: labels.download, icon: Download, run: () => downloadViewsFiles([view]) })
      }
      break
  }
  if (buttons.length === 0) return null
  return (
    <RowActionsBar selected={selected}>
      {buttons.map((button) => (
        <Tooltip key={button.key} content={button.label}>
          <button
            type="button"
            aria-label={button.label}
            onClick={(event) => {
              event.stopPropagation()
              button.run()
            }}
            onDoubleClick={(event) => event.stopPropagation()}
            className="flex size-6 items-center justify-center rounded-sm text-muted-foreground hover:bg-nav-selected hover:text-foreground"
          >
            <Icon icon={button.icon} size="md" />
          </button>
        </Tooltip>
      ))}
    </RowActionsBar>
  )
}

const TaskRow = memo(function TaskRow(props: RowProps) {
  const { view, columns, selected, anySelected, onClick, onDoubleClick, onToggle, getMenu, onContext, density, depth } = props
  const twoLine = density !== 'compact'
  return (
    <ContextMenuArea entries={() => getMenu(view)} title={view.name}>
      <div
        role="row"
        aria-selected={selected}
        onClick={(event) => onClick(event, view)}
        onDoubleClick={() => onDoubleClick(view)}
        onContextMenu={() => onContext(view)}
        className={cn(
          'group/row relative flex cursor-default select-none items-center',
          rowHeightClass(density),
        )}
      >
        <div className={cn('pointer-events-none absolute inset-y-0 left-1 right-1 rounded-[var(--fx-components-task-row-radius)]', selected ? 'bg-accent' : 'group-hover/row:bg-row-hover')} />
        <div className="relative flex shrink-0 items-center justify-center self-stretch" style={{ width: selectionColumnWidth(density) }}>
          {depth > 0 ? <span aria-hidden className="pointer-events-none absolute inset-y-0 left-1/2 w-px bg-hairline" /> : null}
          {anySelected ? (
            <div
              data-no-sweep
              className="relative flex p-1"
              onPointerDown={(event) => event.stopPropagation()}
              onClick={(event) => event.stopPropagation()}
              onDoubleClick={(event) => event.stopPropagation()}
            >
              <Checkbox checked={selected} onCheckedChange={() => onToggle(view)} aria-label={view.name} />
            </div>
          ) : (
            // 与 GPUI `render_selection_cell` 对齐：只在悬停该格本身时把图标换成复选框，点击以该行进入多选。
            <Tooltip content={props.rowActionLabels.select}>
              <div
                role="button"
                aria-label={props.rowActionLabels.select}
                data-no-sweep
                className="group/sel flex size-full cursor-pointer items-center justify-center"
                onPointerDown={(event) => event.stopPropagation()}
                onClick={(event) => {
                  event.stopPropagation()
                  onToggle(view)
                }}
                onDoubleClick={(event) => event.stopPropagation()}
              >
                <span className="relative flex group-hover/sel:hidden">
                  {depth > 0 ? null : <KindGlyph view={view} tile={density === 'relaxed'} />}
                </span>
                <span aria-hidden className="relative hidden p-1 group-hover/sel:flex">
                  <Checkbox checked={false} className="pointer-events-none" />
                </span>
              </div>
            </Tooltip>
          )}
        </div>
        {columns.map((column) => {
          const member = depth > 0 && column.kind === 'file_name'
          return (
            <div
              key={column.kind}
              className={cn('relative min-w-0 shrink-0', member ? 'flex items-center self-stretch' : twoLine ? 'py-1' : '')}
              style={{ ...cellStyle(column), paddingInline: CELL_PADDING_X }}
            >
              {member ? (
                <>
                  {depth === 2 ? (
                    // 目录之下：缩进槽 = 展开箭头槽 + 目录图标 + 间距，槽内细竖线对准上方目录行文件夹图标的中线。
                    <span aria-hidden className="relative shrink-0 self-stretch" style={{ width: DIR_INDENT_SLOT }}>
                      <span className="absolute inset-y-0 w-px bg-hairline" style={{ left: DIR_RAIL_OFFSET }} />
                    </span>
                  ) : null}
                  <span className="mr-2 flex shrink-0 items-center">
                    <KindGlyph view={view} tile={density === 'relaxed'} />
                  </span>
                  <div className="min-w-0 flex-1">{renderCell(props, column)}</div>
                </>
              ) : (
                renderCell(props, column)
              )}
            </div>
          )
        })}
        <div style={{ width: TRAILING_GUTTER }} className="shrink-0" />
        <RowActions view={view} labels={props.rowActionLabels} selected={selected} />
      </div>
    </ContextMenuArea>
  )
})

/** 分组方式的桶标题行（纯分组方式标题：名称 + 桶内任务数）。 */
function BucketHeaderRow({ row, onToggle }: { row: BucketRow; onToggle: () => void }) {
  return (
    <div role="row" onClick={onToggle} className="flex h-full cursor-pointer select-none items-end" style={{ paddingLeft: 8 }}>
      <div className="flex items-center gap-1 text-xs" style={{ height: HEADER_HEIGHT }}>
        <Icon icon={row.collapsed ? ChevronRight : ChevronDown} size="sm" className="text-text-tertiary" />
        <span className="min-w-0 truncate font-medium text-foreground">{row.label}</span>
        <span className="tabular shrink-0 text-text-tertiary">{row.count}</span>
      </div>
    </div>
  )
}

export function TaskTable() {
  const t = useT()
  const ctx = useDownloads()
  const conflictTaskIds = useConflictTaskIds()
  const { rows, prefs, selected, visibleCount, visibleSelected } = ctx
  const scrollRef = useRef<HTMLDivElement>(null)
  // 拖拽调宽中的临时宽度（松手后写回偏好）。
  const [resizing, setResizing] = useState<{ kind: ColumnKind; width: number } | null>(null)
  const [dragKind, setDragKind] = useState<ColumnKind | null>(null)

  const resolved = useMemo(() => resolveColumns(prefs.columns), [prefs.columns])
  const columns = useMemo<LayoutColumn[]>(() => {
    const shown = resolved.filter((column) => column.visible)
    return shown.map((column) => {
      let width = column.width
      let fluid = false
      if (column.kind === 'file_name') {
        if (prefs.file_name_width !== null) {
          width = Math.min(FILE_NAME_MAX_WIDTH, Math.max(MIN_WIDTH.file_name, prefs.file_name_width))
        } else {
          fluid = true
        }
      }
      if (resizing && resizing.kind === column.kind) {
        width = resizing.width
        fluid = false
      }
      return { ...column, width, fluid }
    })
  }, [resolved, prefs.file_name_width, resizing])
  const totalMinWidth = useMemo(
    () =>
      selectionColumnWidth(prefs.density) +
      TRAILING_GUTTER +
      columns.reduce((sum, column) => sum + (column.fluid ? MIN_WIDTH.file_name : column.width), 0),
    [columns, prefs.density],
  )

  const rowActionLabels = useMemo(
    () => ({ pause: t('pause'), resume: t('resume'), download: t('webDownloadFile'), select: t('selectTaskTooltip') }),
    [t],
  )
  const groupLabels = useMemo(
    () => ({
      pauseAll: t('groupPauseAll'),
      resumeAll: t('groupResumeAll'),
      openFolder: t('groupOpenFolder'),
      detail: t('detail'),
      select: t('selectTaskTooltip'),
    }),
    [t],
  )

  // 行数据随进度帧每帧换新，键序列与密度不变时回调保持引用稳定，虚拟化器不重算全部行位置。
  const taskRowHeight = prefs.density === 'relaxed' ? 64 : prefs.density === 'comfortable' ? 44 : 30
  const { getItemKey, estimateSize } = useRowAccessors(rows, BUCKET_SLOT_HEIGHT, taskRowHeight)
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize,
    overscan: 10,
    scrollMargin: HEADER_HEIGHT,
    getItemKey,
  })

  // 密度切换会改变所有任务行高，清掉屏外行的旧测量，避免滚动时保留旧间距。
  useLayoutEffect(() => {
    virtualizer.measure()
  }, [prefs.density, virtualizer])

  // 回调经 ref 读取最新上下文，自身身份恒定，TaskRow 的 memo 才能在进度帧中命中。
  const ctxRef = useRef(ctx)
  ctxRef.current = ctx
  const tRef = useRef(t)
  tRef.current = t
  const pluginCommands = usePluginTaskCommands()
  const pluginCommandsRef = useRef(pluginCommands)
  pluginCommandsRef.current = pluginCommands
  // 划选超过这个位移才改选区，避免普通单击被当成拖拽。
  const blockSweepClick = useRef(false)
  const onClick = useCallback((event: MouseEvent, view: DownloadTaskView) => {
    ctxRef.current.clickSelect(view.key, { shift: event.shiftKey, secondary: event.ctrlKey || event.metaKey })
  }, [])
  const onDoubleClick = useCallback((view: DownloadTaskView) => {
    if (isDownloadable(view)) downloadViewsFiles([view])
    else if (view.source === 'local') ctxRef.current.showDetail(view.taskId)
  }, [])
  const onToggle = useCallback((view: DownloadTaskView) => ctxRef.current.toggleMultiSelect(view.key), [])
  const onContext = useCallback((view: DownloadTaskView) => ctxRef.current.contextSelect(view.key), [])
  // 菜单在打开时才构建，此时读到的选区即右键选择后的最新选区。
  const getMenu = useCallback((view: DownloadTaskView) => {
    const current = ctxRef.current
    return buildTaskMenu({
      t: tRef.current,
      views: current.selected.has(view.key) ? current.selectedViews : [view],
      queues: current.queues,
      queueName: current.queueName,
      showDetail: current.showDetail,
      selection: { multiSelect: current.multiSelect, enterMultiSelect: current.enterMultiSelect, selectAll: current.selectAll },
      pluginCommands: pluginCommandsRef.current,
    })
  }, [])
  const onGroupClick = useCallback((event: MouseEvent, row: GroupFolderRow) => {
    ctxRef.current.clickGroup(row, { shift: event.shiftKey, secondary: event.ctrlKey || event.metaKey })
  }, [])
  // 整行都是划选。浏览器拿不到 daemon 磁盘上的文件，不能把任务拖进访达 / 资源管理器。
  // PC 端文件名列拖出文件，其他列划选，见 `classify_row_press`。
  const beginSweep = useCallback((event: ReactPointerEvent, index: number) => {
    if (event.button !== 0) return
    if (event.target instanceof Element && event.target.closest('[data-no-sweep]')) return
    blockSweepClick.current = false
    const originX = event.clientX
    const originY = event.clientY
    const additive = event.ctrlKey || event.metaKey
    const fromAnchor = event.shiftKey
    const base = additive ? new Set(ctxRef.current.selected) : null
    let active = false
    let last = index
    const move = (moveEvent: PointerEvent) => {
      const dx = moveEvent.clientX - originX
      const dy = moveEvent.clientY - originY
      if (!active && dx * dx + dy * dy < 36) return
      active = true
      blockSweepClick.current = true
      const list = scrollRef.current
      if (list) {
        const rect = list.getBoundingClientRect()
        if (moveEvent.clientY < rect.top + 36) list.scrollTop -= 18
        else if (moveEvent.clientY > rect.bottom - 24) list.scrollTop += 18
      }
      const hit = document.elementFromPoint(moveEvent.clientX, moveEvent.clientY)?.closest('[data-sweep-index]')
      const next = hit instanceof HTMLElement ? Number(hit.dataset.sweepIndex) : last
      if (!Number.isInteger(next)) return
      last = next
      ctxRef.current.sweepSelect(index, next, { additive, fromAnchor, base })
    }
    const up = () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
      if (!active) blockSweepClick.current = false
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
  }, [])
  // 组菜单在打开时才构建；可用项按全组成员（`groupTotals`）判定。
  const getGroupMenu = useCallback((row: GroupFolderRow) => {
    const current = ctxRef.current
    return buildGroupMenu(tRef.current, row.group, row.name, current.groupTotals(row.groupId)?.counts, current.allViews)
  }, [])
  const onOpenGroupFolder = useCallback((row: GroupFolderRow) => {
    void openGroupFolder(ctxRef.current.allViews(), row.groupId, row.group.saveDir)
  }, [])

  const anySelected = selected.size > 0
  const allChecked = anySelected && visibleCount > 0 && visibleSelected === visibleCount
  const someChecked = anySelected && !allChecked && visibleSelected > 0

  const toggleSort = (kind: ColumnKind) => {
    const key = COLUMN_SORT_KEY[kind]
    if (!key) return
    ctx.updatePrefs((current) => nextHeaderSort(current, key))
  }

  const startResize = (event: React.PointerEvent, column: LayoutColumn) => {
    event.preventDefault()
    event.stopPropagation()
    const startX = event.clientX
    const startWidth = event.currentTarget.parentElement?.getBoundingClientRect().width ?? column.width
    const max = column.kind === 'file_name' ? FILE_NAME_MAX_WIDTH : MAX_COLUMN_WIDTH
    const clamp = (width: number) => Math.round(Math.min(max, Math.max(MIN_WIDTH[column.kind], width)))
    let latest = clamp(startWidth)
    const move = (moveEvent: PointerEvent) => {
      latest = clamp(startWidth + moveEvent.clientX - startX)
      setResizing({ kind: column.kind, width: latest })
    }
    const up = () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
      setResizing(null)
      ctx.updatePrefs((current) => {
        if (column.kind === 'file_name') return { ...current, file_name_width: latest }
        const next = resolveColumns(current.columns).map((item) =>
          item.kind === column.kind ? { ...item, width: latest } : item,
        )
        return { ...current, columns: toColumnPrefs(next) }
      })
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
  }

  const moveColumn = (from: ColumnKind, to: ColumnKind) => {
    if (from === to) return
    ctx.updatePrefs((current) => {
      const list = resolveColumns(current.columns)
      const fromIndex = list.findIndex((column) => column.kind === from)
      const toIndex = list.findIndex((column) => column.kind === to)
      if (fromIndex < 0 || toIndex < 0) return current
      const [moved] = list.splice(fromIndex, 1)
      if (moved) list.splice(toIndex, 0, moved)
      return { ...current, columns: toColumnPrefs(list) }
    })
  }

  if (ctx.tasksLoading) return <TaskLoading />
  if (rows.length === 0) return <TaskEmpty />

  const renderTaskRow = (row: EngineTaskRow | MemberRow) => {
    const view = row.type === 'member' ? row.row.view : row.view
    return (
      <TaskRow
        t={t}
        view={view}
        depth={row.type === 'member' ? row.depth : 0}
        columns={columns}
        density={prefs.density}
        conflict={view.source === 'local' && conflictTaskIds.has(view.taskId)}
        selected={selected.has(row.key)}
        anySelected={ctx.multiSelect}
        queueName={ctx.queueName}
        onClick={onClick}
        onDoubleClick={onDoubleClick}
        onToggle={onToggle}
        getMenu={getMenu}
        onContext={onContext}
        rowActionLabels={rowActionLabels}
      />
    )
  }

  return (
    <div
      ref={scrollRef}
      onMouseMove={() => notePointerActivity()}
      onMouseDown={() => notePointerActivity()}
      onWheel={() => notePointerActivity()}
      onContextMenu={() => notePointerActivity()}
      className="relative h-full min-h-0 overflow-auto bg-surface"
    >
      <div style={{ minWidth: totalMinWidth }}>
        <div
          role="row"
          className="sticky top-0 z-10 flex items-center bg-surface"
          style={{ height: HEADER_HEIGHT }}
        >
          <div
            className="group/all flex shrink-0 items-center justify-center"
            style={{ width: selectionColumnWidth(prefs.density) }}
          >
            {/* 非多选模式（普通单击选中）表头全选框按未选处理；表头选择条只在显式多选模式出现，单项操作走右键菜单。 */}
            <div className={cn(ctx.multiSelect ? 'flex' : 'invisible group-hover/all:visible')}>
              <Checkbox
                checked={!ctx.multiSelect ? false : allChecked ? true : someChecked ? 'indeterminate' : false}
                onCheckedChange={() => (ctx.multiSelect && allChecked ? ctx.clearSelection() : ctx.selectAll())}
                aria-label={t('selectedCount', { n: visibleCount })}
              />
            </div>
          </div>
          {ctx.multiSelect ? (
            <SelectionHeaderBar />
          ) : (
            <>
              {columns.map((column) => {
                const sortKey = COLUMN_SORT_KEY[column.kind]
                const active = sortKey !== undefined && sortKey === prefs.sort_key
                const alignEnd = prefs.header_align === 'right'
                const arrow = active ? (
                  <Icon icon={prefs.sort_dir === 'asc' ? ArrowUp : ArrowDown} size="sm" />
                ) : null
                return (
                  <div
                    key={column.kind}
                    draggable
                    onDragStart={(event) => {
                      setDragKind(column.kind)
                      event.dataTransfer.effectAllowed = 'move'
                    }}
                    onDragOver={(event) => dragKind && event.preventDefault()}
                    onDrop={() => {
                      if (dragKind) moveColumn(dragKind, column.kind)
                      setDragKind(null)
                    }}
                    onDragEnd={() => setDragKind(null)}
                    onClick={() => toggleSort(column.kind)}
                    className={cn(
                      'group/th relative flex min-w-0 shrink-0 items-center gap-0.5 text-xs font-medium text-text-tertiary',
                      prefs.header_align === 'center' && 'justify-center',
                      alignEnd && 'justify-end',
                      sortKey && 'cursor-pointer',
                    )}
                    style={{ ...cellStyle(column), paddingInline: CELL_PADDING_X }}
                  >
                    {alignEnd ? arrow : null}
                    <span className="min-w-0 truncate">{t(COLUMN_LABEL_KEY[column.kind])}</span>
                    {alignEnd ? null : arrow}
                    <div
                      role="separator"
                      onPointerDown={(event) => startResize(event, column)}
                      onClick={(event) => event.stopPropagation()}
                      className="absolute inset-y-0 right-0 flex w-2 cursor-col-resize items-center justify-center"
                    >
                      <div className="h-3.5 w-px bg-hairline group-hover/th:h-full group-hover/th:bg-border" />
                    </div>
                  </div>
                )
              })}
              <div style={{ width: TRAILING_GUTTER }} className="shrink-0" />
            </>
          )}
        </div>
        <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
          {virtualizer.getVirtualItems().map((item) => {
            const row = rows[item.index]
            if (!row) return null
            return (
              <div
                key={item.key}
                data-index={item.index}
                data-sweep-index={item.index}
                onPointerDown={(event) => beginSweep(event, item.index)}
                onClickCapture={(event) => {
                  if (!blockSweepClick.current) return
                  blockSweepClick.current = false
                  event.stopPropagation()
                  event.preventDefault()
                }}
                ref={virtualizer.measureElement}
                className="absolute left-0 top-0 w-full"
                style={{
                  transform: `translateY(${item.start - HEADER_HEIGHT}px)`,
                  height: row.type === 'bucket' ? BUCKET_SLOT_HEIGHT : undefined,
                }}
              >
                {row.type === 'bucket' ? (
                  <BucketHeaderRow row={row} onToggle={() => ctx.toggleBucketCollapsed(row.key)} />
                ) : row.type === 'group' ? (
                  <GroupRowView
                    t={t}
                    row={row}
                    columns={columns}
                    density={prefs.density}
                    selected={selected}
                    multiSelect={ctx.multiSelect}
                    detailOpen={ctx.detailOpen}
                    totals={ctx.groupTotals(row.groupId)}
                    queueName={ctx.queueName}
                    labels={groupLabels}
                    onClick={onGroupClick}
                    onToggleSelected={ctx.toggleGroupSelected}
                    onToggleExpanded={ctx.toggleGroupExpanded}
                    getMenu={getGroupMenu}
                    onContext={ctx.contextSelectGroup}
                    onDetail={ctx.showGroupDetail}
                    onOpenFolder={onOpenGroupFolder}
                  />
                ) : row.type === 'dir' ? (
                  <DirRowView t={t} row={row} columns={columns} density={prefs.density} onToggle={ctx.toggleDirCollapsed} />
                ) : (
                  renderTaskRow(row)
                )}
              </div>
            )
          })}
        </div>
      </div>
    </div>
  )
}
