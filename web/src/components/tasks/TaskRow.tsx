// 单条任务行。对齐 design/web/app.js taskRow()/statusMeta()/actionBtn()/iconClass()。

import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Ban, Archive, Check, Download, FileText, Image as ImageIcon, Loader2, Package2, Pause, Play, RotateCcw, Film, Music, File as FileIcon, Zap } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { api, taskFileUrl } from '../../lib/api'
import { CopyButton } from '../CopyButton'
import { cn } from '../../lib/cn'
import { fileType, fmtBytes, fmtEta, fmtSpeed, fmtTime, protoLabel, queueDisplayName, type FileType as FT } from '../../lib/format'
import { translateBackendMessage, useI18n } from '../../lib/i18n'
import { extractSiteLabel } from '../../lib/site'
import { priorityStore, useStore, useTaskPluginActivity } from '../../lib/ws'
import type { QueueDto } from '../../lib/types'
import type { TaskColumnId, ViewDensity } from '../../lib/view-prefs'
import { TaskContextMenu } from './TaskContextMenu'
import { useTasksUi } from './context'
import type { ViewTask } from './useViewTasks'

export const TYPE_ICONS: Record<FT, LucideIcon> = {
  video: Film,
  audio: Music,
  document: FileText,
  image: ImageIcon,
  program: Package2,
  archive: Archive,
  other: FileIcon,
}

/** 插件系统失败任务的错误消息前缀（引擎/hub/server 固定格式，逃生舱按钮据此判断）。 */
const PLUGIN_ERROR_PREFIX = '[插件]'

export function statusIconClass(status: ViewTask['status']): string {
  if (status === 3) return 'done'
  if (status === 4) return 'err'
  if (status === 2 || status === 0) return 'pause'
  return ''
}

function TaskMeta({ t }: { t: ViewTask }) {
  const { t: tr } = useI18n()
  const pluginActive = useTaskPluginActivity(t.taskId)
  const sep = <span className="sep">·</span>
  if (t.status === 1) {
    return (
      <>
        <span>
          {fmtBytes(t.downloadedBytes)} / {fmtBytes(t.totalBytes)}
        </span>
        {sep}
        <span className="speed">{fmtSpeed(t.speed)}</span>
        {sep}
        <span>{tr('status.eta', { eta: fmtEta(t.totalBytes - t.downloadedBytes, t.speed) })}</span>
      </>
    )
  }
  if (t.status === 5) return <span>{tr('status.preparingEllipsis')}</span>
  if (t.status === 2) {
    return (
      <>
        <span>
          {fmtBytes(t.downloadedBytes)} / {fmtBytes(t.totalBytes)}
        </span>
        {sep}
        <span className="paused-t">{tr('status.paused')}</span>
      </>
    )
  }
  if (t.status === 3) {
    return (
      <>
        <span className="ok">{tr('status.completed')}</span>
        {sep}
        <span>{fmtBytes(t.totalBytes)}</span>
        {pluginActive && (
          <>
            {sep}
            <span className="plugin-activity">
              <Loader2 size={11} className="animate-spin" />
              {tr('status.pluginProcessing')}
            </span>
          </>
        )}
      </>
    )
  }
  if (t.status === 4)
    return (
      <>
        <span className="err">{t.errorMessage ? translateBackendMessage(t.errorMessage) : tr('status.downloadFailed')}</span>
        {t.groupId && (
          <>
            {sep}
            <span className="group-expire-hint">{tr('group.memberExpiredResolve')}</span>
          </>
        )}
      </>
    )
  return <span>{tr('status.pending')}</span>
}

export function TaskActionButton({
  t,
  onPause,
  onContinue,
  onIgnorePluginRetry,
}: {
  t: ViewTask
  onPause: () => void
  onContinue: () => void
  onIgnorePluginRetry: () => void
}) {
  const { t: tr } = useI18n()
  if (t.status === 1 || t.status === 5)
    return (
      <button
        type="button"
        className="task-act"
        title={tr('task.pause')}
        onClick={(e) => {
          e.stopPropagation()
          onPause()
        }}
      >
        <Pause size={15} />
      </button>
    )
  if (t.status === 2 || t.status === 0)
    return (
      <button
        type="button"
        className="task-act"
        title={tr('task.resume')}
        onClick={(e) => {
          e.stopPropagation()
          onContinue()
        }}
      >
        <Play size={15} />
      </button>
    )
  if (t.status === 4)
    return (
      <>
        <button
          type="button"
          className="task-act retry"
          title={tr('task.retry')}
          onClick={(e) => {
            e.stopPropagation()
            onContinue()
          }}
        >
          <RotateCcw size={15} />
        </button>
        {t.errorMessage.startsWith(PLUGIN_ERROR_PREFIX) && (
          <button
            type="button"
            className="task-act"
            title={tr('task.ignorePluginRetry')}
            onClick={(e) => {
              e.stopPropagation()
              onIgnorePluginRetry()
            }}
          >
            <Ban size={15} />
          </button>
        )}
      </>
    )
  return (
    <span className="task-act done">
      <Check size={15} />
    </span>
  )
}

/** 「附加信息」列（size/created/protocol/source/queue，可选，见 lib/task-columns.ts 顶部
 *  说明）：作为额外 meta 片段追加在既有状态文案之后。 */
function ExtraColumns({ t, queues, columns }: { t: ViewTask; queues: QueueDto[]; columns: Set<TaskColumnId> }) {
  const { t: tr } = useI18n()
  const sep = <span className="sep">·</span>
  const segs: string[] = []
  if (columns.has('size')) segs.push(fmtBytes(t.totalBytes))
  if (columns.has('created')) segs.push(fmtTime(t.createdAt))
  if (columns.has('protocol')) segs.push(protoLabel(t.url))
  if (columns.has('source')) segs.push(extractSiteLabel(t.url, tr('view.siteBt')))
  if (columns.has('queue')) {
    const q = queues.find((q) => q.queueId === t.queueId)
    segs.push(q ? queueDisplayName(q) : tr('view.ungroupedQueue'))
  }
  return (
    <>
      {segs.map((s, i) => (
        <span className="extra-col" key={i}>
          {sep}
          {s}
        </span>
      ))}
    </>
  )
}

export function TaskRow({
  task: t,
  queues,
  density = 'comfortable',
  protocolBadges = true,
  columns,
}: {
  task: ViewTask
  queues: QueueDto[]
  density?: ViewDensity
  protocolBadges?: boolean
  columns?: Set<TaskColumnId>
}) {
  const { t: tr } = useI18n()
  const { selectTask, currentTaskId, selected, setSelected } = useTasksUi()
  const priority = useStore(priorityStore)
  const qc = useQueryClient()
  const invalidate = () => qc.invalidateQueries({ queryKey: ['tasks'] })

  const pauseMut = useMutation({ mutationFn: () => api.pauseTask(t.taskId), onSuccess: invalidate })
  const continueMut = useMutation({ mutationFn: () => api.continueTask(t.taskId), onSuccess: invalidate })
  const boostMut = useMutation({ mutationFn: () => api.boostTask(t.taskId), onSuccess: invalidate })
  const deleteMut = useMutation({ mutationFn: (deleteFiles: boolean) => api.deleteTask(t.taskId, deleteFiles), onSuccess: invalidate })
  const moveMut = useMutation({ mutationFn: (queueId: string) => api.moveTaskToQueue(t.taskId, queueId), onSuccess: invalidate })
  const ignorePluginRetryMut = useMutation({ mutationFn: () => api.ignorePluginRetry(t.taskId), onSuccess: invalidate })

  const Icon = TYPE_ICONS[fileType(t.fileName, t.url)]
  const pct = t.totalBytes > 0 ? Math.round((t.downloadedBytes / t.totalBytes) * 100) : 0
  const cls = statusIconClass(t.status)
  const isBoost = priority.priorityTaskId === t.taskId

  function toggleSelected(checked: boolean) {
    setSelected((prev) => {
      const next = new Set(prev)
      if (checked) next.add(t.taskId)
      else next.delete(t.taskId)
      return next
    })
  }

  return (
    <TaskContextMenu
      task={t}
      queues={queues}
      onSelect={() => selectTask(t.taskId)}
      onPause={() => pauseMut.mutate()}
      onContinue={() => continueMut.mutate()}
      onBoost={() => boostMut.mutate()}
      onDelete={(deleteFiles) => deleteMut.mutate(deleteFiles)}
      onMove={(queueId) => moveMut.mutate(queueId)}
    >
      <div className={cn('task-row', density === 'compact' && 'compact', currentTaskId === t.taskId && 'selected')} onClick={() => selectTask(t.taskId)}>
        <label className="mcheck trow-check" onClick={(e) => e.stopPropagation()}>
          <input type="checkbox" checked={selected.has(t.taskId)} onChange={(e) => toggleSelected(e.target.checked)} />
          <i />
        </label>
        <span className={cn('trow-icon', cls)}>
          <Icon size={19} />
        </span>
        <div className="trow-main">
          <div className="trow-name">
            <b>{t.fileName || t.url}</b>
            {protocolBadges && <span className="proto">{protoLabel(t.url)}</span>}
            {isBoost && (
              <span className="boost">
                <Zap size={9} />
                BOOST
              </span>
            )}
          </div>
          <div className="trow-meta">
            <TaskMeta t={t} />
            {columns && columns.size > 0 && <ExtraColumns t={t} queues={queues} columns={columns} />}
          </div>
          <div className={cn('trow-bar', cls)}>
            <i style={{ width: `${pct}%` }} />
          </div>
        </div>
        <div className="trow-side">
          <span className="trow-pct">{pct}%</span>
          {/* hover 披露的快捷操作（桌面 §4.3 行操作簇的 web 对等物：「打开文件夹」
              换成 web 可执行操作——已完成任务「保存到本地」+ 任意状态「复制链接」） */}
          {t.status === 3 && (
            <button
              type="button"
              className="task-act hover-act"
              title={tr('task.saveToLocal')}
              onClick={(e) => {
                e.stopPropagation()
                location.href = taskFileUrl(t.taskId)
              }}
            >
              <Download size={15} />
            </button>
          )}
          <CopyButton value={t.url} title={tr('task.copyUrl')} className="task-act hover-act" />
          <TaskActionButton
            t={t}
            onPause={() => pauseMut.mutate()}
            onContinue={() => continueMut.mutate()}
            onIgnorePluginRetry={() => ignorePluginRetryMut.mutate()}
          />
        </div>
      </div>
    </TaskContextMenu>
  )
}
