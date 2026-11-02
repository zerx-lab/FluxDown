// 桌面任务表的任务组「文件夹行」与组内目录行（规格 §3）。与任务行等高；组行聚合只基于通过成员，
// 悬停操作用全组成员判定。聚合在渲染时读成员行最新视图（行对象原位更新），不随进度帧重建行数组。

import { ChevronDown, ChevronRight, FolderOpen, Folder, Pause, PanelRight, Play } from 'lucide-react'
import type { MouseEvent } from 'react'
import { cn } from '../../../lib/cn'
import { Checkbox, ContextMenuArea, Icon, Tooltip } from '../../../ui'
import type { MenuEntry } from '../../../ui'
import { canOpenGroupFolder, pauseGroup, resumeGroup } from '../model/actions'
import { groupCheckState } from '../model/groupRows'
import type { DirRow, GroupFolderRow } from '../model/groupRows'
import { aggregateOf, anyActive, anyResumable, compressPath } from '../model/groupTree'
import type { GroupAggregate } from '../model/groupTree'
import type { TaskRow } from '../model/rowEngine'
import { formatBytes, formatDateTime, MAX_ETA_SECS, urlHost } from '../model/task'
import type { DownloadTaskView, RowKey } from '../model/task'
import type { ViewDensity } from '../model/viewPrefs'
import { stateLabel } from '../state'
import { GroupSparkline } from './GroupSparkline'
import { groupCountSegments, uniformProtocolLabel, uniformValue } from './groupFacts'
import { CountsLine, GroupGlyph } from './groupText'
import {
  CELL_PADDING_X,
  cellStyle,
  PROGRESS_GAP,
  PROGRESS_LABEL_WIDTH,
  rowHeightClass,
  selectionColumnWidth,
  TRAILING_GUTTER,
  TREE_CHEVRON,
  DIR_ICON_WIDTH,
  TREE_INDENT,
} from './rowMetrics'
import type { LayoutColumn } from './rowMetrics'
import { Meta, RowActionsBar } from './rowParts'
import { formatEta, STATUS_TEXT } from './text'
import type { Translate } from './text'

function* viewsOf(rows: readonly TaskRow[]): Generator<DownloadTaskView> {
  for (const row of rows) yield row.view
}

/** 组创建时间：组自己的；缺失时取通过成员里最早的正值。 */
function createdAtOf(row: GroupFolderRow): number {
  const own = Number(row.group.createdAt)
  if (Number.isFinite(own) && own > 0) return own
  let earliest = 0
  for (const member of row.members) {
    const at = member.view.createdAtSecs
    if (at > 0 && (earliest === 0 || at < earliest)) earliest = at
  }
  return earliest
}

const DASH = '—'

function TertiaryDash() {
  return (
    <Meta numeric tertiary>
      {DASH}
    </Meta>
  )
}

/** 展开 / 折叠 chevron：点击只切换展开，不改选区也不触发行单击 / 双击。 */
function Chevron({ expanded, label, onToggle }: { expanded: boolean; label: string; onToggle: () => void }) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-expanded={expanded}
      onClick={(event) => {
        event.stopPropagation()
        onToggle()
      }}
      onDoubleClick={(event) => event.stopPropagation()}
      className="flex shrink-0 items-center justify-center text-text-tertiary hover:text-foreground"
      style={{ width: TREE_CHEVRON, height: TREE_CHEVRON }}
    >
      <Icon icon={expanded ? ChevronDown : ChevronRight} size="sm" />
    </button>
  )
}

interface GroupRowProps {
  t: Translate
  row: GroupFolderRow
  columns: readonly LayoutColumn[]
  density: ViewDensity
  selected: ReadonlySet<RowKey>
  /** 显式多选模式：选择列常驻复选框。 */
  multiSelect: boolean
  /** 停靠详情面板是否已打开（未打开时悬停操作多一个「详情」）。 */
  detailOpen: boolean
  /** 全组成员（不受侧栏筛选 / 搜索影响）的聚合；悬停操作据此判定。 */
  totals: GroupAggregate | undefined
  queueName: (queueId: string) => string
  labels: { pauseAll: string; resumeAll: string; openFolder: string; detail: string; select: string }
  onClick: (event: MouseEvent, row: GroupFolderRow) => void
  onToggleSelected: (row: GroupFolderRow) => void
  onToggleExpanded: (groupId: string) => void
  getMenu: (row: GroupFolderRow) => MenuEntry[]
  onContext: (row: GroupFolderRow) => void
  onDetail: (row: GroupFolderRow) => void
  onOpenFolder: (row: GroupFolderRow) => void
}

function GroupNameCell({
  t,
  row,
  aggregate,
  density,
  onToggleExpanded,
}: {
  t: Translate
  row: GroupFolderRow
  aggregate: GroupAggregate
  density: ViewDensity
  onToggleExpanded: (groupId: string) => void
}) {
  const segments = groupCountSegments(t, aggregate)
  const chevron = <Chevron expanded={row.expanded} label={row.name} onToggle={() => onToggleExpanded(row.groupId)} />
  const name = (
    <span className="min-w-0 truncate text-sm font-medium text-foreground" title={row.name}>
      {row.name}
    </span>
  )
  if (density === 'compact') {
    return (
      <div className="flex min-w-0 items-center gap-1">
        {chevron}
        {name}
        <CountsLine segments={segments} className="ml-1 min-w-0 truncate text-xs text-text-tertiary" />
      </div>
    )
  }
  const counts = (
    <CountsLine segments={segments} className="block truncate text-xs text-text-tertiary" />
  )
  return (
    <div className="flex min-w-0 flex-col justify-center">
      <div className="flex min-w-0 items-center gap-1">
        {chevron}
        {name}
      </div>
      <div className="min-w-0" style={{ paddingLeft: TREE_INDENT }}>
        {counts}
      </div>
    </div>
  )
}

function renderGroupCell(props: GroupRowProps, column: LayoutColumn, aggregate: GroupAggregate) {
  const { t, row, density, queueName } = props
  const downloading = aggregate.state === 'downloading'
  switch (column.kind) {
    case 'file_name':
      return <GroupNameCell t={t} row={row} aggregate={aggregate} density={density} onToggleExpanded={props.onToggleExpanded} />
    case 'progress': {
      // 进度列 = 柱状图（所有密度，已完成也显示）+ 百分比。
      const availableWidth = Math.max(0, column.width - 2 * CELL_PADDING_X - PROGRESS_LABEL_WIDTH - PROGRESS_GAP)
      return (
        <GroupSparkline
          row={row}
          availableWidth={availableWidth}
          compact={density === 'compact'}
          completed={aggregate.state === 'completed'}
          progress={aggregate.progress}
        />
      )
    }
    case 'size':
      return aggregate.sizeBytes > 0 ? <Meta numeric>{formatBytes(aggregate.sizeBytes)}</Meta> : <TertiaryDash />
    case 'status':
      return (
        <div className="tabular flex min-w-0 flex-col justify-center text-xs">
          <div className={cn('truncate', STATUS_TEXT[aggregate.state])}>{stateLabel(t, aggregate.state)}</div>
          {density !== 'compact' ? (
            <div className="truncate text-xs text-text-tertiary">
              {t('groupDoneOfTotal', { done: aggregate.counts.done, total: aggregate.counts.total })}
            </div>
          ) : null}
        </div>
      )
    case 'speed':
      return downloading && aggregate.speed > 0 ? <Meta numeric>{`${formatBytes(aggregate.speed)}/s`}</Meta> : <TertiaryDash />
    case 'eta':
      return downloading && aggregate.etaSeconds !== null && aggregate.etaSeconds <= MAX_ETA_SECS ? (
        <Meta numeric>{formatEta(t, aggregate.etaSeconds)}</Meta>
      ) : (
        <TertiaryDash />
      )
    case 'created': {
      const created = createdAtOf(row)
      return created > 0 ? <Meta numeric>{formatDateTime(created)}</Meta> : <TertiaryDash />
    }
    case 'protocol':
      return <Meta numeric={false}>{uniformProtocolLabel(row.members)}</Meta>
    case 'source':
      return <Meta numeric={false}>{urlHost(row.group.sourceUrl)}</Meta>
    case 'queue': {
      const queueId = uniformValue(row.members, (view) => view.queueId)
      return <Meta numeric={false}>{queueId === null ? DASH : queueName(queueId)}</Meta>
    }
  }
}

/** 悬停操作：anyActive → 全部暂停；否则 anyResumable → 全部恢复；+ 打开组文件夹；详情面板未打开时 + 详情。 */
function GroupActions({ props, selected }: { props: GroupRowProps; selected: boolean }) {
  const { row, totals, labels } = props
  const buttons: { key: string; label: string; icon: typeof Pause; run: () => void }[] = []
  if (totals !== undefined && anyActive(totals.counts)) {
    buttons.push({ key: 'pause', label: labels.pauseAll, icon: Pause, run: () => void pauseGroup(row.groupId) })
  } else if (totals !== undefined && anyResumable(totals.counts)) {
    buttons.push({ key: 'resume', label: labels.resumeAll, icon: Play, run: () => void resumeGroup(row.groupId) })
  }
  if (canOpenGroupFolder()) buttons.push({ key: 'folder', label: labels.openFolder, icon: FolderOpen, run: () => props.onOpenFolder(row) })
  if (!props.detailOpen) buttons.push({ key: 'detail', label: labels.detail, icon: PanelRight, run: () => props.onDetail(row) })
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

/** 任务组「文件夹行」。不做 memo：聚合随成员行内容每帧重读。 */
export function GroupRowView(props: GroupRowProps) {
  const { row, columns, density, selected, multiSelect, onClick, onToggleSelected, onToggleExpanded, getMenu, onContext, labels } = props
  const twoLine = density !== 'compact'
  const aggregate = aggregateOf(viewsOf(row.members))
  const check = groupCheckState(selected, row.members)
  const isSelected = check === true
  return (
    <ContextMenuArea entries={() => getMenu(row)} title={row.name}>
      <div
        role="row"
        aria-selected={isSelected}
        aria-expanded={row.expanded}
        onClick={(event) => onClick(event, row)}
        onDoubleClick={() => onToggleExpanded(row.groupId)}
        onContextMenu={() => onContext(row)}
        className={cn('group/row relative flex cursor-default select-none items-center', rowHeightClass(density))}
      >
        <div
          className={cn(
            'pointer-events-none absolute inset-y-0 left-1 right-1 rounded-[var(--fx-components-task-row-radius)]',
            isSelected ? 'bg-accent' : 'group-hover/row:bg-row-hover',
          )}
        />
        <div className="relative flex shrink-0 items-center justify-center self-stretch" style={{ width: selectionColumnWidth(density) }}>
          {multiSelect ? (
            <div
              data-no-sweep
              className="flex p-1"
              onPointerDown={(event) => event.stopPropagation()}
              onClick={(event) => event.stopPropagation()}
              onDoubleClick={(event) => event.stopPropagation()}
            >
              <Checkbox checked={check} onCheckedChange={() => onToggleSelected(row)} aria-label={row.name} />
            </div>
          ) : (
            <Tooltip content={labels.select}>
              <div
                role="button"
                aria-label={labels.select}
                data-no-sweep
                className="group/sel flex size-full cursor-pointer items-center justify-center"
                onPointerDown={(event) => event.stopPropagation()}
                onClick={(event) => {
                  event.stopPropagation()
                  onToggleSelected(row)
                }}
                onDoubleClick={(event) => event.stopPropagation()}
              >
                <span className="flex group-hover/sel:hidden">
                  <GroupGlyph count={row.members.length} />
                </span>
                <span aria-hidden className="hidden p-1 group-hover/sel:flex">
                  <Checkbox checked={check} className="pointer-events-none" />
                </span>
              </div>
            </Tooltip>
          )}
        </div>
        {columns.map((column) => (
          <div
            key={column.kind}
            className={cn('relative min-w-0 shrink-0', twoLine ? 'py-1' : '')}
            style={{ ...cellStyle(column), paddingInline: CELL_PADDING_X }}
          >
            {renderGroupCell(props, column, aggregate)}
          </div>
        ))}
        <div style={{ width: TRAILING_GUTTER }} className="shrink-0" />
        <GroupActions props={props} selected={isSelected} />
      </div>
    </ContextMenuArea>
  )
}

interface DirRowProps {
  t: Translate
  row: DirRow
  columns: readonly LayoutColumn[]
  density: ViewDensity
  onToggle: (groupId: string, dir: string) => void
}

/** 组内目录分段行：单击切换该目录折叠；不可选、无右键菜单、不可拖。 */
export function DirRowView({ t, row, columns, density, onToggle }: DirRowProps) {
  const twoLine = density !== 'compact'
  const label = compressPath(row.path)
  return (
    <div
      role="row"
      aria-expanded={!row.collapsed}
      onClick={() => onToggle(row.groupId, row.path)}
      className={cn('group/row relative flex cursor-pointer select-none items-center', rowHeightClass(density))}
    >
      <div className="pointer-events-none absolute inset-y-0 left-1 right-1 rounded-[var(--fx-components-task-row-radius)] group-hover/row:bg-row-hover" />
      <div className="relative flex shrink-0 items-center justify-center self-stretch" style={{ width: selectionColumnWidth(density) }}>
        {/* 目录行不可选：选择列只有树轨（与成员行同一条竖线）。 */}
        <span aria-hidden className="pointer-events-none absolute inset-y-0 left-1/2 w-px bg-hairline" />
      </div>
      {columns.map((column) => {
        const isName = column.kind === 'file_name'
        return (
          <div
            key={column.kind}
            className={cn('relative min-w-0 shrink-0', isName ? 'flex items-center self-stretch' : twoLine ? 'py-1' : '')}
            style={{ ...cellStyle(column), paddingInline: CELL_PADDING_X }}
          >
            {isName ? (
              <div className="flex w-full min-w-0 items-center">
                <span
                  aria-hidden
                  className="flex shrink-0 items-center justify-center text-text-tertiary"
                  style={{ width: TREE_CHEVRON, height: TREE_CHEVRON }}
                >
                  <Icon icon={row.collapsed ? ChevronRight : ChevronDown} size="sm" />
                </span>
                <span aria-hidden className="mr-2 flex shrink-0 items-center text-muted-foreground" style={{ width: DIR_ICON_WIDTH }}>
                  <Icon icon={row.collapsed ? Folder : FolderOpen} size="md" />
                </span>
                <span className="min-w-[48px] flex-1 truncate text-sm font-medium text-muted-foreground" title={row.path}>
                  {label}
                </span>
                <span className="tabular ml-2 shrink-0 text-xs text-text-tertiary">{t('groupDirCount', { count: row.count })}</span>
              </div>
            ) : column.kind === 'size' ? (
              row.sizeBytes > 0 ? <Meta numeric>{formatBytes(row.sizeBytes)}</Meta> : null
            ) : null}
          </div>
        )
      })}
      <div style={{ width: TRAILING_GUTTER }} className="shrink-0" />
    </div>
  )
}
