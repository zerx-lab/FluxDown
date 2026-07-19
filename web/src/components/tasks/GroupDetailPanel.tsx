// 任务组详情面板（2 Tab：概览 / 成员）。
// 对齐 lib/src/widgets/group_detail_panel.dart（_GroupDetailPanelState）：大号 SUM 进度 +
// 计数行 + 放大火花条 + 组操作行 + 信息字段（来源/保存目录/创建时间/队列/解析插件懒续期
// 提示）+ 成员迷你列表（点击下钻任务详情）。web 侧简化为单列纵向布局（侧栏 340px 宽，
// 无桌面「底部布局」横向双栏概念），Tab 结构/字段/操作与桌面对齐；REST 无 retry-failed/
// 打开文件夹端点，组操作仅保留全部暂停/恢复 + 删除（+删除文件），来源链接走复制而非
// 启动外部浏览器（web 场景更合适）。

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Layers, Pause, Play, Trash2, X } from 'lucide-react'
import { api } from '../../lib/api'
import { cn } from '../../lib/cn'
import { fmtBytes, fmtEta, fmtTime, queueDisplayName } from '../../lib/format'
import { useI18n } from '../../lib/i18n'
import { confirmDialog } from '../../lib/confirm'
import { computeGroupAggregate, groupDisplayName, groupMemberDirPath, memberStatusClass, sampleSparkline } from '../../lib/task-group'
import { DField, statusText } from './DetailPanel'
import { useTasksUi } from './context'
import { useViewTasks, type ViewTask } from './useViewTasks'

type GroupTab = 'overview' | 'members'

export function GroupDetailPanel() {
  const { t } = useI18n()
  const { selectedGroupId, groupDetailOpen, closeGroupDetail, selectTask } = useTasksUi()
  const [tab, setTab] = useState<GroupTab>('overview')
  const { data: groups = [] } = useQuery({ queryKey: ['groups'], queryFn: api.listGroups })
  const { data: queues = [] } = useQuery({ queryKey: ['queues'], queryFn: api.listQueues })
  const tasks = useViewTasks()
  const group = groups.find((g) => g.groupId === selectedGroupId)
  const open = groupDetailOpen && !!group
  const members = group ? tasks.filter((task) => task.groupId === group.groupId) : []

  const qc = useQueryClient()
  const invalidateTasks = () => qc.invalidateQueries({ queryKey: ['tasks'] })
  const pauseMut = useMutation({ mutationFn: () => api.pauseGroup(group?.groupId ?? ''), onSuccess: invalidateTasks })
  const continueMut = useMutation({ mutationFn: () => api.continueGroup(group?.groupId ?? ''), onSuccess: invalidateTasks })
  const deleteMut = useMutation({
    mutationFn: (deleteFiles: boolean) => api.deleteGroup(group?.groupId ?? '', deleteFiles),
    onSuccess: () => {
      invalidateTasks()
      void qc.invalidateQueries({ queryKey: ['groups'] })
      closeGroupDetail()
    },
  })

  return (
    <aside className={cn('detail', !open && 'hidden')}>
      {group && (
        <>
          <header className="detail-head">
            <b className="ellip">{groupDisplayName(group)}</b>
            <button type="button" className="icon-btn sm" title={t('detail.collapse')} onClick={closeGroupDetail}>
              <X size={14} />
            </button>
          </header>
          <div className="detail-tabs">
            <button type="button" className={cn('dtab', tab === 'overview' && 'active')} onClick={() => setTab('overview')}>
              {t('group.detailOverviewTab')}
            </button>
            <button type="button" className={cn('dtab', tab === 'members' && 'active')} onClick={() => setTab('members')}>
              {t('group.detailMembersTab')}
            </button>
          </div>
          <div className="detail-body">
            {tab === 'overview' ? (
              <OverviewTab
                group={group}
                members={members}
                queues={queues}
                hasActive={members.some((m) => m.status === 0 || m.status === 1 || m.status === 5)}
                onPauseAll={() => pauseMut.mutate()}
                onResumeAll={() => continueMut.mutate()}
                onDelete={(deleteFiles) => deleteMut.mutate(deleteFiles)}
                onJumpToFail={() => setTab('members')}
              />
            ) : (
              <MembersTab group={group} members={members} onSelectTask={selectTask} />
            )}
          </div>
        </>
      )}
    </aside>
  )
}

function GroupSparklineLg({ members }: { members: ViewTask[] }) {
  const sampled = sampleSparkline(members, 24)
  return (
    <div className="gspark lg">
      {sampled.map((m) => (
        <i key={m.taskId} className={memberStatusClass(m.status)} />
      ))}
    </div>
  )
}

function OverviewTab({
  group,
  members,
  queues,
  hasActive,
  onPauseAll,
  onResumeAll,
  onDelete,
  onJumpToFail,
}: {
  group: { groupId: string; name: string; sourceUrl: string; saveDir: string; createdAt: string }
  members: ViewTask[]
  queues: { queueId: string; name: string }[]
  hasActive: boolean
  onPauseAll: () => void
  onResumeAll: () => void
  onDelete: (deleteFiles: boolean) => void
  onJumpToFail: () => void
}) {
  const { t } = useI18n()
  const agg = computeGroupAggregate(members)
  const pct = Math.round(agg.progress * 100)
  const remaining = agg.totalBytes - agg.downloadedBytes
  const eta = agg.speedBytesPerSec > 0 && remaining > 0 ? t('group.etaRemaining', { eta: fmtEta(remaining, agg.speedBytesPerSec) }) : null
  const queueId = members[0]?.queueId ?? ''
  const groupQueue = queues.find((q) => q.queueId === queueId)
  const queueName = groupQueue ? queueDisplayName(groupQueue) : t('detail.defaultQueue')

  return (
    <>
      <div className="gd-summary">
        <span className="gicon">
          <Layers size={19} />
          <span className="gnum">{members.length}</span>
        </span>
        <div>
          <b className="ellip">{groupDisplayName(group)}</b>
          <span>{t('group.detailSubtitle', { status: statusText(agg.statusBucket) })}</span>
        </div>
      </div>
      <div className="d-progress">
        <div className="d-progress-num">
          <b>{pct}%</b>
          <span>
            {fmtBytes(agg.downloadedBytes)} / {fmtBytes(agg.totalBytes)}
          </span>
        </div>
        <div className="d-bar">
          <i style={{ width: `${pct}%` }} />
        </div>
      </div>
      <div className={cn('grow-counts', agg.counts.failed > 0 && 'clickable')} onClick={agg.counts.failed > 0 ? onJumpToFail : undefined}>
        {t('group.itemsCount', { n: agg.counts.total })}
        {agg.counts.done > 0 ? ` · ${t('group.doneCount', { n: agg.counts.done })}` : ''}
        {agg.counts.downloading > 0 ? ` · ${t('group.downloadingCount', { n: agg.counts.downloading })}` : ''}
        {agg.counts.pending > 0 ? ` · ${t('group.pendingCount', { n: agg.counts.pending })}` : ''}
        {agg.counts.paused > 0 ? ` · ${t('group.pausedCount', { n: agg.counts.paused })}` : ''}
        {agg.counts.failed > 0 ? ` · ${t('group.failedCount', { n: agg.counts.failed })} ⚠` : ''}
        {` · ${t('group.doneOfTotal', { done: agg.counts.done, total: agg.counts.total })}`}
        {eta ? ` · ${eta}` : ''}
      </div>
      <div className="gd-spark-wrap">
        <GroupSparklineLg members={members} />
      </div>
      <div className="d-actions">
        <button type="button" className="btn primary sm" onClick={hasActive ? onPauseAll : onResumeAll}>
          {hasActive ? <Pause size={15} /> : <Play size={15} />}
          {hasActive ? t('group.pauseAll') : t('group.resumeAll')}
        </button>
        <button
          type="button"
          className="btn danger sm"
          onClick={async () => {
            const name = groupDisplayName(group)
            if (await confirmDialog({ title: t('group.deleteTitle'), message: t('group.deleteMsg', { name }), danger: true })) onDelete(false)
          }}
        >
          <Trash2 size={15} />
          {t('group.delete')}
        </button>
      </div>
      <DField label={t('group.detailSource')} value={group.sourceUrl || '—'} copy />
      <DField label={t('group.detailSaveDir')} value={group.saveDir} />
      <DField label={t('group.detailCreatedAt')} value={fmtTime(group.createdAt)} />
      <DField label={t('group.detailQueue')} value={queueName} />
      <p className="seg-note">
        <b>{t('group.detailResolverPlugin')}</b> · {t('group.detailLazyRenewHint')}
      </p>
    </>
  )
}

function MembersTab({
  group,
  members,
  onSelectTask,
}: {
  group: { groupId: string; saveDir: string }
  members: ViewTask[]
  onSelectTask: (id: string) => void
}) {
  const { t } = useI18n()
  if (members.length === 0) return <p className="seg-note">{t('group.detailNoMembers')}</p>
  return (
    <>
      {members.map((m) => {
        const dir = groupMemberDirPath(m.saveDir, group.saveDir)
        const name = dir ? `${dir}/${m.fileName}` : m.fileName
        const pct = m.totalBytes > 0 ? Math.round((m.downloadedBytes / m.totalBytes) * 100) : 0
        return (
          <div key={m.taskId} className="gmember-row" onClick={() => onSelectTask(m.taskId)}>
            <i className={cn('gmember-dot', memberStatusClass(m.status))} />
            <span className="ellip">{name}</span>
            <em className={m.status === 4 ? 'err' : undefined}>{m.status === 4 ? t('status.error') : `${pct}%`}</em>
          </div>
        )
      })}
    </>
  )
}
