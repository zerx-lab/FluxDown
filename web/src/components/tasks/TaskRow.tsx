// 单条任务行。对齐 design/web/app.js taskRow()/statusMeta()/actionBtn()/iconClass()。

import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Archive, Check, FileText, Image as ImageIcon, Pause, Play, RotateCcw, Film, Music, File as FileIcon, Zap } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { fileType, fmtBytes, fmtEta, fmtSpeed, fmtTime, protoLabel, type FileType as FT } from '../../lib/format'
import { translateBackendMessage, useI18n } from '../../lib/i18n'
import { priorityStore, useStore } from '../../lib/ws'
import type { QueueDto } from '../../lib/types'
import { TaskContextMenu } from './TaskContextMenu'
import { useTasksUi } from './context'
import type { ViewTask } from './useViewTasks'

const TYPE_ICONS: Record<FT, LucideIcon> = {
  video: Film,
  audio: Music,
  document: FileText,
  image: ImageIcon,
  archive: Archive,
  other: FileIcon,
}

function statusIconClass(status: ViewTask['status']): string {
  if (status === 3) return 'done'
  if (status === 4) return 'err'
  if (status === 2 || status === 0) return 'pause'
  return ''
}

function TaskMeta({ t }: { t: ViewTask }) {
  const { t: tr } = useI18n()
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
        {sep}
        <span>{fmtTime(t.createdAt)}</span>
      </>
    )
  }
  if (t.status === 4) return <span className="err">{t.errorMessage ? translateBackendMessage(t.errorMessage) : tr('status.downloadFailed')}</span>
  return <span>{tr('status.pending')}</span>
}

function TaskActionButton({ t, onPause, onContinue }: { t: ViewTask; onPause: () => void; onContinue: () => void }) {
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
    )
  return (
    <span className="task-act done">
      <Check size={15} />
    </span>
  )
}

export function TaskRow({ task: t, queues }: { task: ViewTask; queues: QueueDto[] }) {
  const { selectTask, currentTaskId, selected, setSelected } = useTasksUi()
  const priority = useStore(priorityStore)
  const qc = useQueryClient()
  const invalidate = () => qc.invalidateQueries({ queryKey: ['tasks'] })

  const pauseMut = useMutation({ mutationFn: () => api.pauseTask(t.taskId), onSuccess: invalidate })
  const continueMut = useMutation({ mutationFn: () => api.continueTask(t.taskId), onSuccess: invalidate })
  const boostMut = useMutation({ mutationFn: () => api.boostTask(t.taskId), onSuccess: invalidate })
  const deleteMut = useMutation({ mutationFn: (deleteFiles: boolean) => api.deleteTask(t.taskId, deleteFiles), onSuccess: invalidate })
  const moveMut = useMutation({ mutationFn: (queueId: string) => api.moveTaskToQueue(t.taskId, queueId), onSuccess: invalidate })

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
      <div className={cn('task-row', currentTaskId === t.taskId && 'selected')} onClick={() => selectTask(t.taskId)}>
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
            <span className="proto">{protoLabel(t.url)}</span>
            {isBoost && (
              <span className="boost">
                <Zap size={9} />
                BOOST
              </span>
            )}
          </div>
          <div className="trow-meta">
            <TaskMeta t={t} />
          </div>
          <div className={cn('trow-bar', cls)}>
            <i style={{ width: `${pct}%` }} />
          </div>
        </div>
        <div className="trow-side">
          <span className="trow-pct">{pct}%</span>
          <TaskActionButton t={t} onPause={() => pauseMut.mutate()} onContinue={() => continueMut.mutate()} />
        </div>
      </div>
    </TaskContextMenu>
  )
}
