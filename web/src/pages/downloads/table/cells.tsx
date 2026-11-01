// 任务表 / 卡片共用的单元格与文案（移植 task_table.rs 的 status_label / status_detail / render_*_cell）。

import { LoaderCircle } from 'lucide-react'
import { cn } from '../../../lib/cn'
import { FileIcon } from '../../../lib/icon-pack'
import { Badge, Icon, Tooltip } from '../../../ui'
import { reopenFileConflicts } from '../dialogs/fileConflict'
import { percentLabel, sourceSite } from '../model/task'
import { localizeTaskError } from '../model/taskError'
import type { DownloadTaskView } from '../model/task'
import type { ViewDensity } from '../model/viewPrefs'
import { SegmentProgress } from './SegmentProgress'
import { kindLabel, relaxedMeta, statusDetail, statusLabel, taskStatusClass } from './text'
import type { Translate } from './text'

/** 文件图标边长：常规与 GPUI 选择列紧凑档同为 `icon.lg`，宽松密度放进 32px 底块。 */
const GLYPH_SIZE = 'var(--fx-icon-lg)'
const TILE_GLYPH_SIZE = 'calc(var(--fx-icon-lg) + 2px)'

export function KindGlyph({ view, className, tile = false }: { view: DownloadTaskView; className?: string; tile?: boolean }) {
  const glyph = view.metadataPending ? (
    <Icon icon={LoaderCircle} size="md" className={cn('animate-spin text-muted-foreground', className)} />
  ) : (
    <FileIcon
      name={view.name}
      kind={view.kind}
      size={tile ? TILE_GLYPH_SIZE : GLYPH_SIZE}
      className={cn('text-muted-foreground', view.state === 'completed' && view.fileMissing && 'opacity-50', className)}
    />
  )
  return tile ? <div className="flex size-8 items-center justify-center rounded-md bg-progress-track">{glyph}</div> : glyph
}

export function FileCell({ t, view, density, conflict }: { t: Translate; view: DownloadTaskView; density: ViewDensity; conflict: boolean }) {
  const twoLine = density !== 'compact'
  const relaxed = density === 'relaxed'
  const site = sourceSite(view)
  const category = kindLabel(t, view.kind)
  // 宽松（旧 Flutter 默认布局）：名称中等字重 + 新副标题；其余密度：类别 · 站点。
  const subtitle = relaxed ? relaxedMeta(t, view) : site === '' ? category : `${category} · ${site}`
  return (
    <div className="flex min-w-0 flex-col justify-center">
      <FileName t={t} view={view} className={relaxed ? 'text-sm font-medium' : 'text-sm'} conflict={conflict} />
      {twoLine && !view.metadataPending ? <div className="tabular truncate text-xs text-text-tertiary">{subtitle}</div> : null}
    </div>
  )
}

/** 「待确认」角标（任务有待选的「文件已存在」）：点击重新打开冲突对话框，不触发行选中。 */
export function ConflictBadge({ t }: { t: Translate }) {
  return (
    <Tooltip content={t('fileConflictPendingTooltip')}>
      <button
        type="button"
        aria-label={t('fileConflictPendingTooltip')}
        onClick={(event) => {
          event.stopPropagation()
          reopenFileConflicts()
        }}
        onDoubleClick={(event) => event.stopPropagation()}
        className="shrink-0 rounded-full"
      >
        <Badge tone="warning">{t('fileConflictPending')}</Badge>
      </button>
    </Tooltip>
  )
}

function FileName({ t, view, className, conflict }: { t: Translate; view: DownloadTaskView; className: string; conflict: boolean }) {
  const name = (
    <div
      className={cn('truncate', className, view.metadataPending ? 'text-muted-foreground' : 'text-foreground')}
      title={view.metadataPending ? undefined : view.name}
    >
      {view.metadataPending ? t('statusPreparing') : view.name}
    </div>
  )
  if (!conflict) return name
  return (
    <div className="flex min-w-0 items-center gap-1.5">
      <div className="min-w-0 flex-1">{name}</div>
      <ConflictBadge t={t} />
    </div>
  )
}

export function StatusCell({ t, view, density }: { t: Translate; view: DownloadTaskView; density: ViewDensity }) {
  const twoLine = density !== 'compact'
  const detail = statusDetail(t, view)
  const failedError = view.state === 'failed' && view.errorMessage.trim() !== ''
  const tooltip = failedError ? localizeTaskError(t, view.errorMessage) : twoLine ? undefined : (detail ?? undefined)
  return (
    <div className="tabular flex min-w-0 flex-col justify-center text-xs" title={tooltip}>
      <div className="flex min-w-0 items-center gap-1.5">
        <span className={cn('truncate', taskStatusClass(view))}>{statusLabel(t, view)}</span>
      </div>
      {twoLine && detail ? <div className="truncate text-xs text-text-tertiary">{detail}</div> : null}
    </div>
  )
}

/** 进度单元格：分段条 + 整数百分比；完成态整格留空。任务行传任务视图，组行传聚合（无分段运行时）。 */
export function ProgressCell({ view, barWidth }: { view: Pick<DownloadTaskView, 'state' | 'progress' | 'runtime'>; barWidth: number }) {
  if (view.state === 'completed') return null
  return (
    <div className="flex min-w-0 items-center gap-2">
      <SegmentProgress runtime={view.runtime} progress={view.progress} state={view.state} width={barWidth} />
      <span className="tabular w-8 shrink-0 text-right text-xs text-muted-foreground">{percentLabel(view.progress)}</span>
    </div>
  )
}
