// 状态栏小部件（下行 / 上行速度、全部暂停 / 全部开始、剩余空间）。
// 状态直接读全局 store（`useRuntimeStats` / `useDaemonReady`），动作直接走 rpc（`pauseAll` / `resumeAll`），
// 所以既能放进外壳的任意区域，也能用在移动端页面级状态栏。
// `rail` 朝向：纯图标，数值写进气泡（活动栏只有 48px 宽，放不下文字）。

import { ArrowDown, ArrowUp, HardDrive, Pause, Play, RotateCw } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { useTaskTableStatus, useTasks } from '../../../lib/rpc'
import { Icon, Tooltip } from '../../../ui'
import { pauseAll, resumeAll, retryFailedViews } from '../model/actions'
import { buildLocalView, formatBytes, stateOfStatus } from '../model/task'
import { ACTION_BUTTON, RAIL_BUTTON, resolveSide, useDaemonReady, useRuntimeStats } from './widget'
import type { WidgetProps } from './widget'

const RAIL_CELL = 'inline-flex size-8 coarse:size-touch shrink-0 items-center justify-center text-muted-foreground'

/** 只读文本小部件：横排 = 图标 + 文字；活动栏 = 图标 + 气泡文字。 */
function IconCell({
  icon,
  text,
  orientation = 'horizontal',
  side,
  className,
}: WidgetProps & { icon: LucideIcon; text: string; className?: string }) {
  if (orientation === 'rail') {
    return (
      <Tooltip content={text} side={resolveSide(orientation, side)}>
        <span role="img" aria-label={text} className={RAIL_CELL}>
          <Icon icon={icon} size="xl" />
        </span>
      </Tooltip>
    )
  }
  return (
    <span className={cn('inline-flex shrink-0 items-center gap-0.5 whitespace-nowrap px-1 text-caption tabular-nums text-muted-foreground', className)}>
      <Icon icon={icon} />
      {text}
    </span>
  )
}

/** 全局下行 / 上行速度。 */
export function SpeedCell({ direction, ...rest }: WidgetProps & { direction: 'down' | 'up' }) {
  const stats = useRuntimeStats()
  const bps = direction === 'down' ? stats.totalDownloadBps : stats.totalUploadBps
  return <IconCell icon={direction === 'down' ? ArrowDown : ArrowUp} text={`${formatBytes(Math.max(0, bps))}/s`} {...rest} />
}

/** 剩余磁盘空间；daemon 未上报时不渲染（`preview` 供布局编辑态占位，便于拖动）。 */
export function DiskFreeCell({ preview = false, ...rest }: WidgetProps & { preview?: boolean }) {
  const t = useT()
  const free = useRuntimeStats().diskFreeBytes
  if (free == null && !preview) return null
  const size = free == null ? '—' : formatBytes(free)
  return <IconCell icon={HardDrive} className="narrow:hidden" text={t('diskSpaceFreeLabel', { size })} {...rest} />
}

function GlobalActionButton({
  icon,
  labelKey,
  action,
  orientation = 'horizontal',
  side,
}: WidgetProps & { icon: LucideIcon; labelKey: 'pauseAll' | 'resumeAll'; action: () => Promise<boolean> }) {
  const t = useT()
  const disabled = !useDaemonReady()
  const label = t(labelKey)
  const rail = orientation === 'rail'
  return (
    <Tooltip content={label} side={resolveSide(orientation, side)}>
      <button type="button" aria-label={label} disabled={disabled} onClick={() => void action()} className={rail ? RAIL_BUTTON : ACTION_BUTTON}>
        <Icon icon={icon} size={rail ? 'xl' : 'lg'} />
      </button>
    </Tooltip>
  )
}

export function PauseAllButton(props: WidgetProps) {
  return <GlobalActionButton icon={Pause} labelKey="pauseAll" action={pauseAll} {...props} />
}

export function ResumeAllButton(props: WidgetProps) {
  return <GlobalActionButton icon={Play} labelKey="resumeAll" action={resumeAll} {...props} />
}

/** 全局失败任务重试：点击时从完整本机任务快照重新筛选，绕过页面筛选 / 搜索。 */
export function RetryFailedButton({ orientation = 'horizontal', side }: WidgetProps) {
  const t = useT()
  const tasks = useTasks()
  const table = useTaskTableStatus()
  const ready = useDaemonReady()
  const hasFailed = tasks.some((task) => stateOfStatus(task.status) === 'failed')
  const disabled = !ready || table.loading || !hasFailed
  const label = hasFailed ? t('retryFailed') : t('retryFailedUnavailable')
  const rail = orientation === 'rail'
  return (
    <Tooltip content={label} side={resolveSide(orientation, side)}>
      <button
        type="button"
        aria-label={label}
        disabled={disabled}
        onClick={() => {
          const views = tasks
            .filter((task) => stateOfStatus(task.status) === 'failed')
            .map((task) => buildLocalView(task, null, false, null, undefined, ready))
          void retryFailedViews(views)
        }}
        className={rail ? RAIL_BUTTON : ACTION_BUTTON}
      >
        <Icon icon={RotateCw} size={rail ? 'xl' : 'lg'} />
      </button>
    </Tooltip>
  )
}
