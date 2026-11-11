// 「视图」菜单内容（GPUI title_bar.rs `view_menu_content`）：顶部分段标签切页（列 / 分组 / 排序 / 显示），
// 下方为可纵向滚动的选项区。所有改动经 `updatePrefs`（重算可见行 + 防抖持久化）。

import { useRef, useState } from 'react'
import type { KeyboardEvent, PointerEvent as ReactPointerEvent, ReactNode } from 'react'
import { Check, GripVertical } from 'lucide-react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { Checkbox, Icon, SegmentedTabs } from '../../../ui'
import {
  COLUMN_LABEL_KEY,
  GROUP_BY_OPTIONS,
  SORT_KEY_OPTIONS,
  defaultSortDir,
  resolveColumns,
  toColumnPrefs,
} from '../model/viewPrefs'
import type {
  ColumnKind,
  DetailPlacement,
  HeaderAlign,
  ResolvedColumn,
  SortDir,
  ViewDensity,
  ViewGroupBy,
  ViewSortKey,
} from '../model/viewPrefs'
import { useDownloads } from '../state'

/** 视图菜单的分页（分段标签）。 */
export type ViewMenuPage = 'columns' | 'group' | 'sort' | 'display'

const PAGES: readonly { value: ViewMenuPage; labelKey: string }[] = [
  { value: 'columns', labelKey: 'viewSectionColumns' },
  { value: 'group', labelKey: 'viewSectionGroupBy' },
  { value: 'sort', labelKey: 'viewSectionSort' },
  { value: 'display', labelKey: 'viewSectionDisplay' },
]

const GROUP_BY_KEY: Record<ViewGroupBy, string> = {
  none: 'viewGroupNone',
  status: 'viewGroupStatus',
  date: 'viewGroupDate',
  type: 'viewGroupType',
  queue: 'viewGroupQueue',
  site: 'viewGroupSite',
  directory: 'viewGroupDirectory',
}

const SORT_KEY_KEY: Record<ViewSortKey, string> = {
  smart: 'viewSortSmart',
  created: 'viewSortCreated',
  name: 'viewSortName',
  size: 'viewSortSize',
  progress: 'viewSortProgress',
  speed: 'viewSortSpeed',
  status: 'viewSortStatus',
}

const SORT_DIRS: readonly { value: SortDir; labelKey: string }[] = [
  { value: 'asc', labelKey: 'viewSortAscending' },
  { value: 'desc', labelKey: 'viewSortDescending' },
]

const DENSITIES: readonly { value: ViewDensity; labelKey: string }[] = [
  { value: 'compact', labelKey: 'viewDensityCompact' },
  { value: 'comfortable', labelKey: 'viewDensityComfortable' },
  { value: 'relaxed', labelKey: 'viewDensityRelaxed' },
]

const HEADER_ALIGNS: readonly { value: HeaderAlign; labelKey: string }[] = [
  { value: 'left', labelKey: 'viewHeaderAlignLeft' },
  { value: 'center', labelKey: 'viewHeaderAlignCenter' },
  { value: 'right', labelKey: 'viewHeaderAlignRight' },
]

const PLACEMENTS: readonly { value: DetailPlacement; labelKey: string }[] = [
  { value: 'bottom', labelKey: 'viewDetailBottom' },
  { value: 'right', labelKey: 'viewDetailRight' },
]

const ROW = 'mx-0.5 flex h-nav-row items-center gap-2 rounded-sm px-2.5 text-sm text-foreground coarse:min-h-touch'

function SectionTitle({ children }: { children: ReactNode }) {
  return <div className="px-3 pb-0.5 pt-2 text-caption font-medium text-text-tertiary">{children}</div>
}

function Separator() {
  return <div role="separator" className="my-0.5 h-px bg-hairline" />
}

/** 单选 / 开关行：左侧对勾标记当前值（GPUI `option_row`）。 */
function OptionRow({
  label,
  checked,
  onSelect,
  disabled,
  role = 'menuitemradio',
}: {
  label: string
  checked: boolean
  onSelect: () => void
  disabled?: boolean
  role?: 'menuitemradio' | 'menuitemcheckbox' | 'menuitem'
}) {
  return (
    <button
      type="button"
      role={role}
      aria-checked={role === 'menuitem' ? undefined : checked}
      disabled={disabled}
      onClick={onSelect}
      className={cn(ROW, 'w-[calc(100%-4px)] text-left hover:bg-row-hover active:bg-nav-hover disabled:pointer-events-none disabled:opacity-50')}
    >
      <span className="flex w-[var(--fx-icon-md)] flex-none justify-center">
        {checked ? <Icon icon={Check} size="md" /> : null}
      </span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
    </button>
  )
}

function moveColumn(columns: readonly ResolvedColumn[], from: ColumnKind, to: ColumnKind): ResolvedColumn[] | null {
  const fromIx = columns.findIndex((column) => column.kind === from)
  const toIx = columns.findIndex((column) => column.kind === to)
  if (fromIx < 0 || toIx < 0 || fromIx === toIx) return null
  const next = [...columns]
  const [moved] = next.splice(fromIx, 1)
  next.splice(toIx, 0, moved as ResolvedColumn)
  return next
}

/** 列页：勾选显隐 + 拖动排序（手柄在右；指针事件，鼠标 / 触屏通用，手柄聚焦时 ↑↓ 键移动）+ 重置。 */
function ColumnsPage() {
  const t = useT()
  const { prefs, updatePrefs } = useDownloads()
  const columns = resolveColumns(prefs.columns)
  const visibleCount = columns.filter((column) => column.visible).length

  const rowRefs = useRef(new Map<ColumnKind, HTMLDivElement>())
  const dragRef = useRef<{ kind: ColumnKind; over: ColumnKind } | null>(null)
  const [drag, setDrag] = useState<{ kind: ColumnKind; over: ColumnKind } | null>(null)

  const applyColumns = (mutate: (current: ResolvedColumn[]) => ResolvedColumn[] | null) =>
    updatePrefs((current) => {
      const next = mutate(resolveColumns(current.columns))
      return next ? { ...current, columns: toColumnPrefs(next) } : current
    })

  const setVisible = (kind: ColumnKind, visible: boolean) =>
    applyColumns((current) => {
      // 至少保留一列可见（GPUI `set_column_visible`）。
      if (!visible && current.filter((column) => column.visible).length <= 1) return null
      const target = current.find((column) => column.kind === kind)
      if (!target || target.visible === visible) return null
      return current.map((column) => (column.kind === kind ? { ...column, visible } : column))
    })

  const reset = () => updatePrefs((current) => ({ ...current, columns: [], file_name_width: null }))

  const rowUnder = (clientY: number): ColumnKind | null => {
    let hit: ColumnKind | null = null
    let index = 0
    for (const column of columns) {
      const rect = rowRefs.current.get(column.kind)?.getBoundingClientRect()
      if (!rect) continue
      if (index === 0 && clientY < rect.top) return column.kind
      if (clientY >= rect.top && clientY < rect.bottom) return column.kind
      hit = column.kind
      index += 1
    }
    // 落在列表下方 → 最后一行。
    return hit
  }

  const onHandleDown = (event: ReactPointerEvent<HTMLButtonElement>, kind: ColumnKind) => {
    if (event.pointerType === 'mouse' && event.button !== 0) return
    event.preventDefault()
    event.currentTarget.setPointerCapture(event.pointerId)
    dragRef.current = { kind, over: kind }
    setDrag(dragRef.current)
  }

  const onHandleMove = (event: ReactPointerEvent<HTMLButtonElement>) => {
    const current = dragRef.current
    if (!current) return
    const over = rowUnder(event.clientY)
    if (over && over !== current.over) {
      dragRef.current = { kind: current.kind, over }
      setDrag(dragRef.current)
    }
  }

  const onHandleUp = (event: ReactPointerEvent<HTMLButtonElement>) => {
    const current = dragRef.current
    dragRef.current = null
    setDrag(null)
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId)
    if (current && current.over !== current.kind) {
      applyColumns((cols) => moveColumn(cols, current.kind, current.over))
    }
  }

  const onHandleCancel = () => {
    dragRef.current = null
    setDrag(null)
  }

  const onHandleKey = (event: KeyboardEvent<HTMLButtonElement>, index: number, kind: ColumnKind) => {
    const step = event.key === 'ArrowUp' ? -1 : event.key === 'ArrowDown' ? 1 : 0
    if (step === 0) return
    const target = columns[index + step]
    if (!target) return
    event.preventDefault()
    applyColumns((cols) => moveColumn(cols, kind, target.kind))
  }

  return (
    <>
      {columns.map((column, index) => {
        const label = t(COLUMN_LABEL_KEY[column.kind])
        const dragging = drag?.kind === column.kind
        const over = drag !== null && drag.over === column.kind && drag.kind !== column.kind
        return (
          <div
            key={column.kind}
            ref={(element) => {
              if (element) rowRefs.current.set(column.kind, element)
              else rowRefs.current.delete(column.kind)
            }}
            className={cn(ROW, 'pr-1 hover:bg-row-hover', dragging && 'opacity-50', over && 'bg-nav-selected')}
          >
            <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 self-stretch">
              <Checkbox checked={column.visible} onCheckedChange={(checked) => setVisible(column.kind, checked)} />
              <span className="min-w-0 flex-1 truncate">{label}</span>
            </label>
            <button
              type="button"
              aria-label={`${label} · ${t('moveUpAction')} / ${t('moveDownAction')}`}
              onPointerDown={(event) => onHandleDown(event, column.kind)}
              onPointerMove={onHandleMove}
              onPointerUp={onHandleUp}
              onPointerCancel={onHandleCancel}
              onKeyDown={(event) => onHandleKey(event, index, column.kind)}
              className="inline-flex min-w-6 flex-none cursor-grab touch-none items-center justify-center self-stretch rounded-sm text-text-tertiary hover:text-foreground active:cursor-grabbing coarse:min-w-touch"
            >
              <Icon icon={GripVertical} size="sm" />
            </button>
          </div>
        )
      })}
      {visibleCount === 1 ? (
        <div className="px-3 py-0.5 text-caption text-text-tertiary">{t('viewColumnsAtLeastOne')}</div>
      ) : null}
      <Separator />
      <OptionRow role="menuitem" label={t('viewColumnsResetAction')} checked={false} onSelect={reset} />
    </>
  )
}

/** 分组页：单选分组方式。 */
function GroupPage() {
  const t = useT()
  const { prefs, updatePrefs } = useDownloads()
  return (
    <>
      {GROUP_BY_OPTIONS.map((groupBy) => (
        <OptionRow
          key={groupBy}
          label={t(GROUP_BY_KEY[groupBy])}
          checked={prefs.group_by === groupBy}
          onSelect={() => updatePrefs((current) => (current.group_by === groupBy ? current : { ...current, group_by: groupBy }))}
        />
      ))}
    </>
  )
}

/** 排序页：排序键 + 方向（智能排序无方向）。 */
function SortPage() {
  const t = useT()
  const { prefs, updatePrefs } = useDownloads()
  return (
    <>
      {SORT_KEY_OPTIONS.map((sortKey) => (
        <OptionRow
          key={sortKey}
          label={t(SORT_KEY_KEY[sortKey])}
          checked={prefs.sort_key === sortKey}
          onSelect={() =>
            updatePrefs((current) =>
              current.sort_key === sortKey ? current : { ...current, sort_key: sortKey, sort_dir: defaultSortDir(sortKey) },
            )
          }
        />
      ))}
      <Separator />
      {SORT_DIRS.map((dir) => (
        <OptionRow
          key={dir.value}
          label={t(dir.labelKey)}
          checked={prefs.sort_key !== 'smart' && prefs.sort_dir === dir.value}
          disabled={prefs.sort_key === 'smart'}
          onSelect={() => updatePrefs((current) => (current.sort_dir === dir.value ? current : { ...current, sort_dir: dir.value }))}
        />
      ))}
    </>
  )
}

/** 显示页：密度 + 详情面板（开关 + 位置）。 */
function DisplayPage() {
  const t = useT()
  const { prefs, updatePrefs } = useDownloads()
  return (
    <>
      <SectionTitle>{t('viewSectionDensity')}</SectionTitle>
      {DENSITIES.map((density) => (
        <OptionRow
          key={density.value}
          label={t(density.labelKey)}
          checked={prefs.density === density.value}
          onSelect={() => updatePrefs((current) => (current.density === density.value ? current : { ...current, density: density.value }))}
        />
      ))}
      <Separator />
      <SectionTitle>{t('viewSectionHeaderAlign')}</SectionTitle>
      {HEADER_ALIGNS.map((align) => (
        <OptionRow
          key={align.value}
          label={t(align.labelKey)}
          checked={prefs.header_align === align.value}
          onSelect={() =>
            updatePrefs((current) => (current.header_align === align.value ? current : { ...current, header_align: align.value }))
          }
        />
      ))}
      <Separator />
      <SectionTitle>{t('viewSectionDetailPanel')}</SectionTitle>
      <OptionRow
        role="menuitemcheckbox"
        label={t('viewDetailShow')}
        checked={prefs.detail_open}
        onSelect={() => updatePrefs((current) => ({ ...current, detail_open: !current.detail_open }))}
      />
      {PLACEMENTS.map((placement) => (
        <OptionRow
          key={placement.value}
          label={t(placement.labelKey)}
          checked={prefs.detail_placement === placement.value}
          onSelect={() =>
            updatePrefs((current) =>
              current.detail_placement === placement.value ? current : { ...current, detail_placement: placement.value },
            )
          }
        />
      ))}
    </>
  )
}

export function ViewMenu({ page, onPageChange }: { page: ViewMenuPage; onPageChange: (page: ViewMenuPage) => void }) {
  const t = useT()
  return (
    <div className="flex w-full flex-col">
      <div className="flex px-3 pb-2 pt-3 mobile:px-0 mobile:pt-0">
        <SegmentedTabs
          items={PAGES.map((item) => ({ value: item.value, label: t(item.labelKey) }))}
          value={page}
          onValueChange={onPageChange}
          aria-label={t('viewOptionsTitle')}
        />
      </div>
      <div
        className="flex max-h-[min(420px,calc(100dvh-120px))] w-full flex-col overflow-y-auto pb-1 mobile:max-h-[60dvh]"
      >
        {page === 'columns' ? <ColumnsPage /> : null}
        {page === 'group' ? <GroupPage /> : null}
        {page === 'sort' ? <SortPage /> : null}
        {page === 'display' ? <DisplayPage /> : null}
      </div>
    </div>
  )
}
