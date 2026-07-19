// 任务组折叠行「活卡片」—— 火花条 + SUM 条 + 状态计数行（失败可点直达）+ 组操作。
// UI 实现细节以桌面端 lib/src/widgets/task_group_card.dart（TaskGroupRow/_GroupSparkline/
// _GroupCountsLine/buildGroupSumBar/buildGroupIcon）为准，按 web design.css class 体系
// 简化适配，不逐像素照搬。全部完成时降噪为灰色摘要行（隐藏火花条/SUM 条）。

import { Fragment, type ReactNode } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ChevronRight, Layers, Pause, Play } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { fmtEta, fmtSpeed } from '../../lib/format'
import { useI18n } from '../../lib/i18n'
import {
  computeGroupAggregate,
  groupDisplayName,
  groupMemberDirPath,
  isActiveStatus,
  memberStatusClass,
  sampleSparkline,
  type GroupMemberCounts,
} from '../../lib/task-group'
import type { GroupDto } from '../../lib/types'
import type { ViewDensity } from '../../lib/view-prefs'
import { GroupContextMenu } from './GroupContextMenu'
import { useTasksUi } from './context'
import type { ViewTask } from './useViewTasks'

export function GroupSparkline({ members }: { members: ViewTask[] }) {
  const sampled = sampleSparkline(members, 24)
  return (
    <div className="gspark">
      {sampled.map((m) => (
        <i key={m.taskId} className={memberStatusClass(m.status)} />
      ))}
    </div>
  )
}

export function GroupCountsLine({ counts, eta, onJumpToFail }: { counts: GroupMemberCounts; eta: string | null; onJumpToFail?: () => void }) {
  const { t } = useI18n()
  const segs: ReactNode[] = [t('group.itemsCount', { n: counts.total })]
  if (counts.done > 0) segs.push(t('group.doneCount', { n: counts.done }))
  if (counts.downloading > 0)
    segs.push(
      <span className="dl" key="dl">
        {t('group.downloadingCount', { n: counts.downloading })}
      </span>,
    )
  if (counts.pending > 0) segs.push(t('group.pendingCount', { n: counts.pending }))
  if (counts.paused > 0) segs.push(t('group.pausedCount', { n: counts.paused }))
  if (counts.failed > 0)
    segs.push(
      <span
        className="fail"
        key="fail"
        onClick={(e) => {
          e.stopPropagation()
          onJumpToFail?.()
        }}
      >
        {t('group.failedCount', { n: counts.failed })} ⚠
      </span>,
    )
  segs.push(t('group.doneOfTotal', { done: counts.done, total: counts.total }))
  if (eta) segs.push(eta)

  return (
    <div className="grow-counts ellip">
      {segs.map((s, i) => (
        <Fragment key={i}>
          {i > 0 && ' · '}
          {s}
        </Fragment>
      ))}
    </div>
  )
}

export function GroupRow({ group, members, density = 'comfortable' }: { group: GroupDto; members: ViewTask[]; density?: ViewDensity }) {
  const { t } = useI18n()
  const { expandedGroups, toggleGroupExpand, jumpToGroupMember, selectGroup, selectedGroupId } = useTasksUi()
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
  const expanded = expandedGroups.has(group.groupId)
  const hasActive = members.some((m) => isActiveStatus(m.status))
  const done = agg.statusBucket === 3
  const pct = Math.round(agg.progress * 100)
  const remaining = agg.totalBytes - agg.downloadedBytes
  const eta = agg.speedBytesPerSec > 0 && remaining > 0 ? t('group.etaRemaining', { eta: fmtEta(remaining, agg.speedBytesPerSec) }) : null
  const name = groupDisplayName(group)
  const firstFailed = members.find((m) => m.status === 4)
  const firstFailedDirPath = firstFailed ? groupMemberDirPath(firstFailed.saveDir, group.saveDir) : undefined

  return (
    <GroupContextMenu
      group={group}
      hasActive={hasActive}
      onPauseAll={() => pauseMut.mutate()}
      onResumeAll={() => continueMut.mutate()}
      onDelete={(deleteFiles) => deleteMut.mutate(deleteFiles)}
    >
      <div
        className={cn('grow', density === 'compact' && 'compact', done && 'done', selectedGroupId === group.groupId && 'selected')}
        onClick={() => selectGroup(group.groupId)}
      >
        <span
          className="grow-chevron"
          onClick={(e) => {
            e.stopPropagation()
            toggleGroupExpand(group.groupId)
          }}
        >
          <ChevronRight size={13} style={{ transform: expanded ? 'rotate(90deg)' : undefined }} />
        </span>
        <span className="gicon">
          <Layers size={17} />
          <span className="gnum">{members.length}</span>
        </span>
        <div className="grow-main">
          <div className="grow-name">
            <b>{name}</b>
            <span className="grow-badge">{t('group.pluginBadge')}</span>
          </div>
          <GroupCountsLine
            counts={agg.counts}
            eta={eta}
            onJumpToFail={
              firstFailed ? () => jumpToGroupMember(group.groupId, firstFailed.taskId, firstFailedDirPath) : undefined
            }
          />
          {!done && (
            <div className="grow-bar">
              <i style={{ width: `${pct}%` }} />
            </div>
          )}
        </div>
        {/* 火花条为行级列（原型 §4.6：main 之后、pct 之前）——放 main 内竖排会把行撑破 64px 节奏 */}
        {!done && <GroupSparkline members={members} />}
        <div className="grow-side">
          <span className="grow-pct">{pct}%</span>
          <span className="grow-speed">{agg.speedBytesPerSec > 0 ? fmtSpeed(agg.speedBytesPerSec) : '—'}</span>
          <button
            type="button"
            className="grow-act"
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
