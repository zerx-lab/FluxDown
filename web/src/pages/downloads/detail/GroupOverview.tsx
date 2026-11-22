import { useByteFormatter } from '../../../lib/fileSizeUnits'
// 停靠面板「组概览」（规格 §5）：聚焦任务组且选区恰好等于其通过成员时显示。聚合取全组成员（不受侧栏筛选 / 搜索影响）。

import { AppWindow, FolderOpen, Layers, Pause, Play, RotateCw } from 'lucide-react'
import { useMemo } from 'react'
import { useT } from '../../../i18n'
import { Button, Icon, ProgressBar } from '../../../ui'
import { canOpenGroupFolder, openGroupFolder, pauseGroup, resumeGroup, retryFailedInGroup } from '../model/actions'
import { anyActive, anyResumable, displayName } from '../model/groupTree'
import { formatBytes, formatDateTime, MAX_ETA_SECS, percentLabel } from '../model/task'
import { openGroupDetail } from '../dialogs'
import { useDownloads, stateLabel } from '../state'
import { groupCountSegments } from '../table/groupFacts'
import { CountsLine } from '../table/groupText'
import { formatEta, STATUS_TEXT } from '../table/text'
import { DetailRow, Note } from './DetailRow'

export function GroupOverview({ groupId }: { groupId: string }) {
  const fmtBytes = useByteFormatter(formatBytes)
  const t = useT()
  const { groups, groupTotals, allViews, queueName, tasksLoading, contentVersion } = useDownloads()
  const group = groups.find((item) => item.groupId === groupId)
  // 读取放在渲染里：`contentVersion` 随进度帧递增，使聚合跟随最新数据。
  void contentVersion
  const totals = groupTotals(groupId)
  const memberCount = totals?.counts.total ?? 0

  // 队列：全组成员都在同一队列才显示队列名，否则 `—`（成员数变化时重算，不随进度帧遍历全部任务）。
  const queueId = useMemo(() => {
    if (tasksLoading) return null
    let found: string | null = null
    for (const view of allViews()) {
      if (view.source !== 'local' || view.groupId !== groupId) continue
      if (found === null) found = view.queueId
      else if (found !== view.queueId) return null
    }
    return found
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 只在成员数变化时重算
  }, [allViews, groupId, tasksLoading, memberCount])

  if (!group || !totals) return null
  const name = displayName(group.name, group.saveDir, groupId)
  const status = stateLabel(t, totals.state)
  const created = Number(group.createdAt)
  const sizeText = totals.sizeBytes > 0 ? `${fmtBytes(totals.downloadedBytes)} / ${fmtBytes(totals.sizeBytes)}` : fmtBytes(totals.downloadedBytes)
  const speedLine =
    totals.speed > 0
      ? [
          `${fmtBytes(totals.speed)}/s`,
          totals.etaSeconds !== null && totals.etaSeconds <= MAX_ETA_SECS
            ? t('groupEtaRemaining', { eta: formatEta(t, totals.etaSeconds) })
            : null,
        ]
          .filter((part) => part !== null)
          .join(' · ')
      : null
  const notSet = t('detailNotSet')

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-3">
      <div className="flex items-center gap-2">
        <div className="flex size-9 shrink-0 items-center justify-center rounded-md bg-accent-text/12">
          <Icon icon={Layers} size="lg" className="text-accent-text" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm font-semibold text-foreground" title={name}>
            {name}
          </div>
          <div className="truncate text-xs text-muted-foreground">{t('groupDetailSubtitle', { status })}</div>
        </div>
      </div>

      <div className="flex flex-col gap-1.5">
        <div className="tabular text-3xl font-semibold text-foreground">{percentLabel(totals.progress)}</div>
        <div className="tabular text-xs text-muted-foreground">{sizeText}</div>
        <ProgressBar value={totals.progress} />
        <div className={speedLine ? 'tabular text-xs text-muted-foreground' : `text-xs ${STATUS_TEXT[totals.state]}`}>{speedLine ?? status}</div>
        <CountsLine segments={groupCountSegments(t, { ...totals, etaSeconds: null })} className="text-xs text-text-tertiary" />
      </div>

      <div className="flex flex-wrap items-center gap-2">
        {anyActive(totals.counts) ? (
          <Button icon={Pause} onClick={() => void pauseGroup(groupId)}>
            {t('groupPauseAll')}
          </Button>
        ) : null}
        {anyResumable(totals.counts) ? (
          <Button icon={Play} onClick={() => void resumeGroup(groupId)}>
            {t('groupResumeAll')}
          </Button>
        ) : null}
        {totals.counts.failed > 0 ? (
          <Button icon={RotateCw} onClick={() => void retryFailedInGroup(allViews(), groupId)}>
            {t('groupRetryFailed')}
          </Button>
        ) : null}
        {canOpenGroupFolder() ? (
          <Button icon={FolderOpen} onClick={() => void openGroupFolder(allViews(), groupId, group.saveDir)}>
            {t('groupOpenFolder')}
          </Button>
        ) : null}
        <Button icon={AppWindow} onClick={() => openGroupDetail(groupId)}>
          {t('openGroupInWindowAction')}
        </Button>
      </div>

      <div className="flex flex-col">
        <DetailRow label={t('groupDetailSource')}>{group.sourceUrl || notSet}</DetailRow>
        <DetailRow label={t('groupDetailSaveDir')}>{group.saveDir || notSet}</DetailRow>
        <DetailRow label={t('groupDetailCreatedAt')}>{Number.isFinite(created) && created > 0 ? formatDateTime(created) : notSet}</DetailRow>
        <DetailRow label={t('groupDetailQueue')}>{queueId === null ? notSet : queueName(queueId)}</DetailRow>
      </div>
      {memberCount === 0 ? <Note>{t('groupDetailNoMembers')}</Note> : null}
    </div>
  )
}
