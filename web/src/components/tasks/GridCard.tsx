// 网格 · bento 卡片（形态=网格时的渲染单元）。
// UI 依据 design/desktop-task-views DESIGN.md §4.5 + styles.css `.gcard`（单任务卡 1×、
// 组卡 2× 跨列；行装箱与跨列宽度由 TaskList 负责）；交互对齐桌面
// lib/src/widgets/task_list.dart _TaskGridCard / TaskGroupCard：点击选中 → 详情面板、
// hover 披露操作簇（右上角）、右键菜单、管理模式复选框在卡左上角；
// 网格无组内展开机制（「网格降级」）——组卡点击即打开组详情面板。

import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowDown, Check, Layers, Loader2, Pause, Play, TriangleAlert } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { fileType, fmtBytes, fmtSpeed, protoLabel } from '../../lib/format'
import { translateBackendMessage, useI18n } from '../../lib/i18n'
import { computeGroupAggregate, groupDisplayName, isActiveStatus } from '../../lib/task-group'
import type { GroupDto, QueueDto } from '../../lib/types'
import { GroupContextMenu } from './GroupContextMenu'
import { GroupCountsLine, GroupSparkline } from './GroupRow'
import { statusIconClass, TaskActionButton, TYPE_ICONS } from './TaskRow'
import { TaskContextMenu } from './TaskContextMenu'
import { useTasksUi } from './context'
import type { ViewTask } from './useViewTasks'

/** 卡片右上角状态图标（状态双编码 P5：颜色 + 图标）。 */
function StatusIcon({ status }: { status: ViewTask['status'] }) {
  const icon =
    status === 3 ? (
      <Check size={13} />
    ) : status === 4 ? (
      <TriangleAlert size={13} />
    ) : status === 5 ? (
      <Loader2 size={13} className="animate-spin" />
    ) : status === 1 ? (
      <ArrowDown size={13} />
    ) : (
      <Pause size={13} />
    )
  return <span className={cn('gcard-st', statusIconClass(status))}>{icon}</span>
}

/** 单任务网格卡 1×（design-proto `.gcard`）。 */
export function TaskGridCard({ task: t, queues, protocolBadges }: { task: ViewTask; queues: QueueDto[]; protocolBadges: boolean }) {
  const { t: tr } = useI18n()
  const { selectTask, currentTaskId, selected, setSelected } = useTasksUi()
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
  const name = t.fileName || t.url

  function toggleSelected(checked: boolean) {
    setSelected((prev) => {
      const next = new Set(prev)
      if (checked) next.add(t.taskId)
      else next.delete(t.taskId)
      return next
    })
  }

  const meta =
    t.status === 1 ? (
      <>
        <span className="active">{fmtSpeed(t.speed)}</span>
        <span>
          {pct}% · {fmtBytes(t.totalBytes)}
        </span>
      </>
    ) : t.status === 4 ? (
      <span className="errt">{t.errorMessage ? translateBackendMessage(t.errorMessage) : tr('status.downloadFailed')}</span>
    ) : (
      <>
        <span>
          {t.status === 3
            ? tr('status.completed')
            : t.status === 2
              ? tr('status.paused')
              : t.status === 5
                ? tr('status.preparingEllipsis')
                : tr('status.pending')}
        </span>
        <span>{fmtBytes(t.totalBytes)}</span>
      </>
    )

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
      <div
        className={cn('gcard', t.status === 3 && 'is-done-card', t.status === 4 && 'is-err-card', currentTaskId === t.taskId && 'selected')}
        onClick={() => selectTask(t.taskId)}
      >
        <label className="mcheck gcard-check" onClick={(e) => e.stopPropagation()}>
          <input type="checkbox" checked={selected.has(t.taskId)} onChange={(e) => toggleSelected(e.target.checked)} />
          <i />
        </label>
        <div className="gcard-top">
          <span className={cn('trow-icon', cls)}>
            <Icon size={19} />
          </span>
          {protocolBadges && <span className="proto">{protoLabel(t.url)}</span>}
          <StatusIcon status={t.status} />
        </div>
        <div className="gcard-name" title={name}>
          {name}
        </div>
        <div className={cn('gcard-bar', cls && `is-${cls}`)}>
          <i style={{ width: `${pct}%` }} />
        </div>
        <div className="gcard-meta">{meta}</div>
        <div className="gcard-acts">
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

/** 任务组网格卡 2× 跨列（design-proto `.gcard.group`；完成组同形态，仅状态呈现完成 v1.3）。 */
export function GroupGridCard({ group, members }: { group: GroupDto; members: ViewTask[] }) {
  const { t } = useI18n()
  const { selectGroup, selectedGroupId } = useTasksUi()
  const qc = useQueryClient()
  const invalidateTasks = () => qc.invalidateQueries({ queryKey: ['tasks'] })
  const pauseMut = useMutation({ mutationFn: () => api.pauseGroup(group.groupId), onSuccess: invalidateTasks })
  const continueMut = useMutation({ mutationFn: () => api.continueGroup(group.groupId), onSuccess: invalidateTasks })
  const deleteMut = useMutation({
    mutationFn: (deleteFiles: boolean) => api.deleteGroup(group.groupId, deleteFiles),
    onSuccess: () => {
      invalidateTasks()
      void qc.invalidateQueries({ queryKey: ['groups'] })
    },
  })

  const agg = computeGroupAggregate(members)
  const done = agg.statusBucket === 3
  const hasActive = members.some((m) => isActiveStatus(m.status))
  const pct = Math.round(agg.progress * 100)
  const name = groupDisplayName(group)

  const footLabel =
    agg.speedBytesPerSec > 0
      ? `↓ ${fmtSpeed(agg.speedBytesPerSec)}`
      : done
        ? t('status.completed')
        : agg.statusBucket === 4
          ? t('status.downloadFailed')
          : agg.statusBucket === 2
            ? t('status.paused')
            : '—'

  return (
    <GroupContextMenu
      group={group}
      hasActive={hasActive}
      onPauseAll={() => pauseMut.mutate()}
      onResumeAll={() => continueMut.mutate()}
      onDelete={(deleteFiles) => deleteMut.mutate(deleteFiles)}
    >
      <div
        className={cn('gcard group', done && 'is-done-card', selectedGroupId === group.groupId && 'selected')}
        onClick={() => selectGroup(group.groupId)}
      >
        <div className="gcard-top">
          <span className="gicon">
            <Layers size={15} />
            <span className="gnum">{members.length}</span>
          </span>
          <div className="gcard-gmain">
            <div className="gcard-name" title={name}>
              {name}
            </div>
            <GroupCountsLine counts={agg.counts} eta={null} />
          </div>
          <span className="gcard-gpct">{pct}%</span>
        </div>
        <GroupSparkline members={members} />
        <div className={cn('gcard-bar', done && 'is-done')}>
          <i style={{ width: `${pct}%` }} />
        </div>
        <div className="gcard-foot">
          <span className={cn(agg.speedBytesPerSec > 0 && 'active')}>{footLabel}</span>
          <span>
            {fmtBytes(agg.downloadedBytes)} / {fmtBytes(agg.totalBytes)}
          </span>
        </div>
        <div className="gcard-acts">
          <button
            type="button"
            className="task-act"
            title={hasActive ? t('group.pauseAll') : t('group.resumeAll')}
            onClick={(e) => {
              e.stopPropagation()
              if (hasActive) pauseMut.mutate()
              else continueMut.mutate()
            }}
          >
            {hasActive ? <Pause size={15} /> : <Play size={15} />}
          </button>
        </div>
      </div>
    </GroupContextMenu>
  )
}
