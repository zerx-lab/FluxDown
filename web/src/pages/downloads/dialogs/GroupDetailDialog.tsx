import { useByteFormatter } from '../../../lib/fileSizeUnits'
// 任务组详情：概览 + 成员表（暂停 / 继续 / 重试失败 / 删除）。对应 GPUI `pages/group_detail.rs`。

import { Copy, Download, MoreHorizontal, Pause, Play, RotateCw, Trash2 } from 'lucide-react'
import { useEffect, useMemo } from 'react'
import { useT } from '../../../i18n'
import { TASK_STATUS, downloadTaskFile, rpc, useDaemon, useTasks, useTasksLoading } from '../../../lib/rpc'
import type { GroupDto, TaskDto } from '../../../lib/rpc'
import { ActionMenu, Badge, Button, Dialog, EmptyState, Icon, ProgressBar, SectionHeader, Tooltip, confirmDialog, toast } from '../../../ui'
import { toastRpcError } from '../../../lib/rpcToast'
import { copyText } from '../../../lib/copy'
import type { MenuEntry } from '../../../ui'
import { closeGroupDetail } from './store'
import { localizeTaskError } from '../model/taskError'
import { formatBytes } from './utils'


interface GroupSummary {
  total: number
  completed: number
  failed: number
  downloading: number
  paused: number
  pending: number
  progress: number
}

function summarize(members: readonly TaskDto[]): GroupSummary {
  const summary: GroupSummary = { total: members.length, completed: 0, failed: 0, downloading: 0, paused: 0, pending: 0, progress: 0 }
  let downloaded = 0
  let total = 0
  for (const task of members) {
    if (task.status === TASK_STATUS.completed) summary.completed += 1
    else if (task.status === TASK_STATUS.error) summary.failed += 1
    else if (task.status === TASK_STATUS.downloading) summary.downloading += 1
    else if (task.status === TASK_STATUS.paused) summary.paused += 1
    else summary.pending += 1
    downloaded += task.status === TASK_STATUS.completed && task.totalBytes <= 0 ? 0 : task.downloadedBytes
    total += task.totalBytes
  }
  summary.progress = total > 0 ? Math.min(1, downloaded / total) : summary.total > 0 ? summary.completed / summary.total : 0
  return summary
}

export function GroupDetailDialog({ groupId }: { groupId: string }) {
  const t = useT()
  const group = useDaemon((daemon) => daemon.groups.find((item) => item.groupId === groupId), undefined)
  const tasks = useTasks()
  const loading = useTasksLoading()
  const members = useMemo(() => tasks.filter((task) => task.groupId === groupId), [tasks, groupId])
  const summary = useMemo(() => summarize(members), [members])

  // 组被删除（本端或远端）后自动关闭。
  useEffect(() => {
    if (!group) closeGroupDetail()
  }, [group])
  if (!group) return null

  return (
    <Dialog open onOpenChange={(open) => !open && closeGroupDetail()} size="lg" title={group.name || group.groupId} description={loading ? t('tasksLoadingTitle') : statusLine(t, summary)}>
      <div className="flex flex-col gap-3 pb-2">
        <Overview group={group} summary={summary} />
        <SectionHeader>{t('groupDetailMembersTab')}</SectionHeader>
        {loading ? (
          <EmptyState icon={Download} title={t('tasksLoadingTitle')} className="py-6" />
        ) : members.length === 0 ? (
          <EmptyState icon={Download} title={t('groupDetailNoMembers')} className="py-6" />
        ) : (
          <MemberTable members={members} />
        )}
      </div>
    </Dialog>
  )
}

function statusLine(t: (key: string, params?: Record<string, string | number>) => string, summary: GroupSummary): string {
  if (summary.total === 0) return t('groupDetailNoMembers')
  return `${summary.completed}/${summary.total} · ${Math.round(summary.progress * 100)}%`
}

function Overview({ group, summary }: { group: GroupDto; summary: GroupSummary }) {
  const t = useT()
  const failed = useTasks().filter((task) => task.groupId === group.groupId && task.status === TASK_STATUS.error)

  const run = async (action: () => Promise<unknown>) => {
    try {
      await action()
    } catch (error) {
      toastRpcError(error)
    }
  }

  const deleteGroup = async (deleteFiles: boolean) => {
    if (deleteFiles) {
      const ok = await confirmDialog({
        title: t('groupDeleteWithFiles'),
        description: t('deleteConfirmDescWithFile', { fileName: group.name || group.groupId }),
        okLabel: t('groupDeleteWithFiles'),
        intent: 'destructive',
      })
      if (!ok) return
    }
    await run(() => rpc.daemon.group.delete({ groupId: group.groupId, deleteFiles }))
  }

  const deleteMenu: MenuEntry[] = [
    { type: 'item', key: 'delete', label: t('groupDelete'), icon: Trash2, destructive: true, onSelect: () => void deleteGroup(false) },
    { type: 'item', key: 'delete-files', label: t('groupDeleteWithFiles'), icon: Trash2, destructive: true, onSelect: () => void deleteGroup(true) },
  ]

  const copySource = () => {
    copyText(group.sourceUrl)
    toast.key('urlCopied', 'success')
  }

  const created = Number(group.createdAt)

  return (
    <div className="flex flex-col gap-3">
      <dl className="flex flex-col divide-y divide-hairline rounded-md border border-hairline text-sm">
        <DetailRow label={t('groupDetailSource')}>
          <span className="min-w-0 flex-1 break-all">{group.sourceUrl || '—'}</span>
          {group.sourceUrl ? (
            <Tooltip content={t('groupCopySourceLink')}>
              <Button variant="ghost" iconOnly aria-label={t('groupCopySourceLink')} onClick={copySource}>
                <Icon icon={Copy} />
              </Button>
            </Tooltip>
          ) : null}
        </DetailRow>
        <DetailRow label={t('groupDetailSaveDir')}>
          <span className="min-w-0 flex-1 break-all">{group.saveDir || '—'}</span>
        </DetailRow>
        {Number.isFinite(created) && created > 0 ? (
          <DetailRow label={t('groupDetailCreatedAt')}>
            <span className="tabular">{new Date(created * 1000).toLocaleString()}</span>
          </DetailRow>
        ) : null}
      </dl>

      {summary.total > 0 ? (
        <div className="flex flex-col gap-2">
          <ProgressBar value={summary.progress} />
          <div className="flex flex-wrap gap-1.5">
            <Badge tone="success">{t('groupDoneCount', { n: summary.completed })}</Badge>
            {summary.downloading > 0 ? <Badge tone="accent">{t('groupDownloadingCount', { n: summary.downloading })}</Badge> : null}
            {summary.pending > 0 ? <Badge>{t('groupPendingCount', { n: summary.pending })}</Badge> : null}
            {summary.paused > 0 ? <Badge>{t('groupPausedCount', { n: summary.paused })}</Badge> : null}
            {summary.failed > 0 ? <Badge tone="destructive">{t('groupFailedCount', { n: summary.failed })}</Badge> : null}
          </div>
        </div>
      ) : null}

      <div className="flex flex-wrap items-center gap-2">
        <Button icon={Pause} onClick={() => void run(() => rpc.daemon.group.pause({ groupId: group.groupId }))}>
          {t('groupPauseAll')}
        </Button>
        <Button icon={Play} onClick={() => void run(() => rpc.daemon.group.resume({ groupId: group.groupId }))}>
          {t('groupResumeAll')}
        </Button>
        <Button
          icon={RotateCw}
          disabled={failed.length === 0}
          onClick={() => void run(() => Promise.all(failed.map((task) => rpc.daemon.task.resume({ taskId: task.taskId }))))}
        >
          {t('groupRetryFailed')}
        </Button>
        <ActionMenu
          title={t('groupDelete')}
          entries={deleteMenu}
          trigger={
            <Button iconOnly title={t('groupDelete')} aria-label={t('groupDelete')}>
              <Icon icon={MoreHorizontal} />
            </Button>
          }
        />
      </div>
    </div>
  )
}

function DetailRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex min-h-control items-center gap-3 px-3 py-1.5 coarse:min-h-touch">
      <dt className="w-24 shrink-0 text-xs text-muted-foreground">{label}</dt>
      <dd className="flex min-w-0 flex-1 items-center gap-1">{children}</dd>
    </div>
  )
}

// ── 成员表 ──

function statusKey(status: number): string {
  switch (status) {
    case TASK_STATUS.downloading:
      return 'statusDownloading'
    case TASK_STATUS.paused:
      return 'statusPaused'
    case TASK_STATUS.completed:
      return 'statusCompleted'
    case TASK_STATUS.error:
      return 'statusError'
    case TASK_STATUS.preparing:
      return 'statusPreparing'
    default:
      return 'statusPending'
  }
}

const STATUS_COLOR: Record<number, string> = {
  [TASK_STATUS.downloading]: 'text-status-downloading',
  [TASK_STATUS.error]: 'text-status-failed',
  [TASK_STATUS.paused]: 'text-status-paused',
}

function MemberTable({ members }: { members: readonly TaskDto[] }) {
  const t = useT()
  return (
    <div className="rounded-md border border-hairline">
      <div className="hidden grid-cols-[minmax(0,1fr)_84px_150px_110px_36px] items-center gap-3 border-b border-hairline px-3 py-1.5 text-caption font-medium text-text-tertiary desktop:grid">
        <span>{t('colFileName')}</span>
        <span className="text-right">{t('colSize')}</span>
        <span>{t('colProgress')}</span>
        <span>{t('colStatus')}</span>
        <span />
      </div>
      {members.map((task) => (
        <MemberRow key={task.taskId} task={task} />
      ))}
    </div>
  )
}

function MemberRow({ task }: { task: TaskDto }) {
  const fmtBytes = useByteFormatter(formatBytes)
  const t = useT()
  const done = task.status === TASK_STATUS.completed
  const progress = done ? 1 : task.totalBytes > 0 ? Math.min(1, task.downloadedBytes / task.totalBytes) : null
  const percent = progress === null ? '' : `${Math.round(progress * 100)}%`
  const size = task.totalBytes > 0 ? fmtBytes(task.totalBytes) : '—'

  const act = async (action: () => Promise<unknown>) => {
    try {
      await action()
    } catch (error) {
      toastRpcError(error)
    }
  }

  let action: { label: string; icon: typeof Play; run: () => void } | null = null
  if (task.status === TASK_STATUS.downloading || task.status === TASK_STATUS.pending) {
    action = { label: t('pause'), icon: Pause, run: () => void act(() => rpc.daemon.task.pause({ taskId: task.taskId })) }
  } else if (task.status === TASK_STATUS.paused) {
    action = { label: t('resume'), icon: Play, run: () => void act(() => rpc.daemon.task.resume({ taskId: task.taskId })) }
  } else if (task.status === TASK_STATUS.error) {
    action = { label: t('mobileRetry'), icon: RotateCw, run: () => void act(() => rpc.daemon.task.resume({ taskId: task.taskId })) }
  } else if (done && !task.fileMissing) {
    action = { label: t('webDownloadFile'), icon: Download, run: () => downloadTaskFile(task.taskId) }
  }

  return (
    <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b border-hairline px-3 py-2 last:border-b-0 desktop:grid-cols-[minmax(0,1fr)_84px_150px_110px_36px]">
      <div className="min-w-0">
        <div className="truncate text-sm" title={task.fileName}>
          {task.fileName || task.url}
        </div>
        <div className="tabular text-xs text-muted-foreground desktop:hidden">
          {size} · <span className={STATUS_COLOR[task.status] ?? 'text-text-tertiary'}>{t(statusKey(task.status))}</span>
          {percent ? ` · ${percent}` : ''}
        </div>
        {task.status === TASK_STATUS.error && task.errorMessage ? <div className="truncate text-xs text-destructive" title={localizeTaskError(t, task.errorMessage)}>{localizeTaskError(t, task.errorMessage)}</div> : null}
      </div>
      <span className="tabular hidden text-right text-xs text-muted-foreground desktop:block">{size}</span>
      <div className="col-span-2 flex items-center gap-2 empty:hidden desktop:col-span-1 desktop:empty:flex">
        {done ? null : (
          <>
            <ProgressBar value={progress} className="flex-1" />
            <span className="tabular w-9 shrink-0 text-right text-xs text-muted-foreground">{percent}</span>
          </>
        )}
      </div>
      <span className={`hidden text-xs desktop:block ${STATUS_COLOR[task.status] ?? 'text-text-tertiary'}`}>{t(statusKey(task.status))}</span>
      <div className="row-start-1 col-start-2 flex justify-end desktop:row-start-auto desktop:col-start-auto">
        {action ? (
          <Tooltip content={action.label}>
            <Button variant="ghost" iconOnly aria-label={action.label} onClick={action.run}>
              <Icon icon={action.icon} />
            </Button>
          </Tooltip>
        ) : null}
      </div>
    </div>
  )
}
