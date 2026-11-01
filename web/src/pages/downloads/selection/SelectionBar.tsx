import { Download, Pause, Play, Trash2, X } from 'lucide-react'
import type { ReactNode } from 'react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { ActionMenu, Icon, Tooltip } from '../../../ui'
import type { MenuEntry } from '../../../ui'
import { confirmDeleteWithFiles, deleteViews, downloadViewsFiles, pauseViews, resumeViews } from '../model/actions'
import { useDownloads } from '../state'

const BTN =
  'inline-flex h-control min-w-control shrink-0 items-center justify-center gap-1 rounded-md px-1.5 text-sm text-foreground hover:bg-nav-hover active:bg-nav-selected disabled:pointer-events-none disabled:opacity-50 mobile:min-h-touch mobile:min-w-touch'
/** 表头内联变体：按钮不超过表头行高（28px）。 */
const BTN_INLINE = 'h-6 min-w-6'

function Action({
  label,
  onClick,
  disabled,
  className,
  children,
}: {
  label: string
  onClick?: () => void
  disabled?: boolean
  className?: string
  children: ReactNode
}) {
  // 置灰按钮不触发指针事件：tooltip 挂在外层 span 上，原因仍可见。
  return (
    <Tooltip content={label} side="top">
      <span className="inline-flex">
        <button type="button" aria-label={label} disabled={disabled} onClick={onClick} className={cn(BTN, className)}>
          {children}
        </button>
      </span>
    </Tooltip>
  )
}

const Sep = () => <span className="mx-1 h-4 w-px shrink-0 bg-hairline" />

/** 批量操作内容（已选 N 项 | 操作 | ×），浮动条与表头覆盖条共用；`inline` 时按钮收紧到表头行高。 */
function SelectionActions({ inline }: { inline?: boolean }) {
  const t = useT()
  const { summary, selectedViews, clearSelection } = useDownloads()
  const btn = inline ? BTN_INLINE : undefined

  const deleteEntries: MenuEntry[] = [
    { type: 'item', key: 'delete-task', label: t('deleteTask'), icon: Trash2, onSelect: () => void deleteViews(selectedViews, false) },
    { type: 'item', key: 'delete-files', label: t('deleteTaskAndFile'), icon: Trash2, onSelect: () => void confirmDeleteWithFiles(selectedViews) },
  ]

  return (
    <>
      <span className="shrink-0 whitespace-nowrap px-1 tabular-nums text-foreground">{t('selectedCount', { n: summary.count })}</span>
      <Sep />
      {summary.anyResumable && (
        <Action label={t('resume')} className={btn} onClick={() => void resumeViews(selectedViews)}>
          <Icon icon={Play} size="md" />
        </Action>
      )}
      {summary.anyActive && (
        <Action label={t('pause')} className={btn} onClick={() => void pauseViews(selectedViews)}>
          <Icon icon={Pause} size="md" />
        </Action>
      )}
      {summary.anyLocal && (
        <Action label={t('webDownloadFile')} className={btn} disabled={!summary.allDownloadable} onClick={() => downloadViewsFiles(selectedViews)}>
          <Icon icon={Download} size="md" />
        </Action>
      )}
      <ActionMenu
        title={t('delete')}
        align="start"
        entries={deleteEntries}
        trigger={
          <button type="button" aria-label={t('delete')} className={cn(BTN, btn, 'text-destructive')}>
            <Icon icon={Trash2} size="md" />
          </button>
        }
      />
      <Sep />
      <Action label={t('deselectAll')} className={btn} onClick={clearSelection}>
        <Icon icon={X} size="md" />
      </Action>
    </>
  )
}

/** 桌面表头覆盖条：铺满表头选择列右侧，与表头同底色、无边框/阴影；由 TaskTable 在选中 ≥1 项时替换列标题。 */
export function SelectionHeaderBar() {
  return (
    <div role="toolbar" className="flex h-full min-w-0 flex-1 items-center gap-0.5 overflow-x-auto bg-surface pr-2 text-xs [scrollbar-width:none]">
      <SelectionActions inline />
    </div>
  )
}

/** 移动端卡片布局（无表头）的底部浮动选择条：选中 ≥1 项时出现，承载批量操作。 */
export function SelectionBar() {
  const { summary, detailOpen } = useDownloads()
  // 详情面板正展示唯一选中的任务时，不再显示批量条。
  if (!summary.any || (detailOpen && summary.count === 1)) return null

  return (
    <div
      className={cn(
        'absolute bottom-4 left-1/2 z-20 flex h-9 -translate-x-1/2 items-center gap-0.5 overflow-x-auto rounded-lg border border-hairline bg-surface px-2 text-xs shadow-md',
        'max-w-[calc(100vw-16px)] [scrollbar-width:none]',
        'mobile:fixed mobile:bottom-[calc(var(--fx-bottom-bar,0px)+0.5rem)] mobile:h-12 mobile:px-1.5',
      )}
    >
      <SelectionActions />
    </div>
  )
}
