import { Copy, Download, Layers, Link2 } from 'lucide-react'
import { useT } from '../../../i18n'
import { usePrefBool } from '../../../lib/rpc'
import { Button, Icon, Tooltip } from '../../../ui'
import { copyText } from '../../../lib/copy'
import { useDownloads } from '../state'
import { taskStateLabel } from '../table/text'
import { copyUrls, downloadViewsFiles, isDownloadable } from '../model/actions'
import { MAX_ETA_SECS, PROTOCOL_LABEL, formatBytes, formatDateTime, sourcePageUrl, sourceSite } from '../model/task'
import { localizeTaskError } from '../model/taskError'
import type { DownloadTaskView, TaskState } from '../model/task'
import { canChangeSegments, canChangeUrl } from '../model/taskEdit'
import { HIDE_TASK_SOURCE_PREF } from '../model/viewPrefs'
import { openChangeSegments, openChangeUrl, openGroupDetail } from '../dialogs'
import { routeLabel } from '../../../lib/taskRoute'
import { CopyButton, SourcePageLink } from './CopyButton'
import { FileHashRow } from './FileHashRow'
import { InfoHashV2Row } from './InfoHashV2Row'
import { btMetadataReady } from './btStatus'
import { DetailRow } from './DetailRow'
import { SourcesSection } from './SourcesSection'

const STATE_TEXT: Record<TaskState, string> = {
  pending: 'text-status-queued',
  downloading: 'text-status-downloading',
  paused: 'text-status-paused',
  completed: 'text-status-completed',
  failed: 'text-status-failed',
}

/** 剩余时间：与 GPUI `format_eta` 一致（<60s 秒 / <1h 分 / 否则小时 1 位小数）。 */
function formatEta(t: ReturnType<typeof useT>, seconds: number): string {
  if (seconds < 60) return t('etaSeconds', { n: String(seconds) })
  if (seconds < 3600) return t('etaMinutes', { n: String(Math.floor(seconds / 60)) })
  return t('etaHours', { n: (seconds / 3600).toFixed(1) })
}

export function GeneralTab({ view }: { view: DownloadTaskView }) {
  const t = useT()
  const { queueName } = useDownloads()
  const hideSource = usePrefBool(HIDE_TASK_SOURCE_PREF, false)
  const dto = view.dto
  // 期望哈希只在填写过时展示（与 GPUI 一致：多数任务没有，「未设置」一行是噪音）。
  const checksum = dto?.checksum ?? ''
  const proxy = dto && dto.proxyUrl !== '' ? dto.proxyUrl : t('detailFollowGlobal')
  const queueLabel = view.queueId === '' ? '' : (queueName(view.queueId) ?? view.queueId)
  const completed = view.state === 'completed'
  const downloading = view.state === 'downloading'

  // 响应式：宽度够（底部横向面板 / 宽窗口）时信息列与来源构成并排，右侧窄面板时
  // 来源构成按实际可用宽度折到信息列上方（wrap-reverse；反向交叉轴下 items-end 即顶对齐）——与 GPUI 同一规则（300 + 360）。
  return (
    <div className="flex flex-wrap-reverse items-end gap-x-6 gap-y-4">
      <div className="flex min-w-0 flex-[1_1_18.75rem] flex-col">
        <div className="flex flex-col gap-0.5 pb-2">
          <div className="min-w-0 truncate text-sm font-medium">{view.name}</div>
          <div className="flex min-w-0 gap-1 text-xs text-muted-foreground">
            {hideSource ? (
              <span className="shrink-0">{PROTOCOL_LABEL[view.protocol]}</span>
            ) : (
              <>
                <span className="shrink-0">{PROTOCOL_LABEL[view.protocol]} ·</span>
                <SourcePageLink text={sourceSite(view)} url={sourcePageUrl(view)} className="min-w-0 truncate" />
              </>
            )}
          </div>
          {view.boosted ? <div className="text-xs text-warning">{t('detailBoostActive')}</div> : null}
        </div>
        <DetailRow label={t('infoStatus')}>
          <span className={STATE_TEXT[view.state]}>{taskStateLabel(t, view)}</span>
        </DetailRow>
        {view.sizeBytes > 0 ? <DetailRow label={t('infoSize')}>{formatBytes(view.sizeBytes)}</DetailRow> : null}
        {!completed ? <DetailRow label={t('infoDownloaded')}>{formatBytes(view.downloadedBytes)}</DetailRow> : null}
        {downloading && view.speed !== null ? <DetailRow label={t('infoSpeed')}>{formatBytes(view.speed)}/s</DetailRow> : null}
        {downloading && view.etaSeconds !== null && view.etaSeconds <= MAX_ETA_SECS ? (
          <DetailRow label={t('infoRemaining')}>{formatEta(t, view.etaSeconds)}</DetailRow>
        ) : null}
        {view.createdAtSecs > 0 ? <DetailRow label={t('infoStartedAt')}>{formatDateTime(view.createdAtSecs)}</DetailRow> : null}
        {view.completedAtSecs > 0 ? <DetailRow label={t('infoCompletedAt')}>{formatDateTime(view.completedAtSecs)}</DetailRow> : null}
        <DetailRow label={t('infoPath')}>
          <div className="flex items-center gap-1">
            <span className="min-w-0 flex-1 break-all text-accent-text">{view.saveDir}</span>
            <Tooltip content={t('webCopy')}>
              <Button
                variant="ghost"
                iconOnly
                aria-label={t('webCopy')}
                onClick={() => copyText(view.saveDir)}
              >
                <Icon icon={Copy} />
              </Button>
            </Tooltip>
          </div>
        </DetailRow>
        <DetailRow label={t('taskQueueLabel')}>{queueLabel}</DetailRow>
        {checksum !== '' ? <DetailRow label={t('taskChecksum')}>{checksum}</DetailRow> : null}
        {view.source === 'local' && completed ? <FileHashRow view={view} /> : null}
        {view.source === 'local' && view.protocol === 'bt' ? <InfoHashV2Row taskId={view.taskId} hasMetadata={btMetadataReady(view)} /> : null}
        <DetailRow label={t('taskProxy')}>{proxy}</DetailRow>
        {dto && dto.autoRoute !== '' ? <DetailRow label={t('taskRoute')}>{routeLabel(t, dto.autoRoute)}</DetailRow> : null}
        {dto?.ignoreTlsErrors ? <DetailRow label={t('taskIgnoreTlsErrors')}>✓</DetailRow> : null}
        {view.errorMessage !== '' ? (
          <DetailRow label={t('infoError')}>
            <div className="flex items-center gap-1">
              <span className="min-w-0 flex-1 break-all text-destructive">{localizeTaskError(t, view.errorMessage)}</span>
              <CopyButton text={localizeTaskError(t, view.errorMessage)} label={t('detailCopyError')} copiedLabel={t('detailErrorCopied')} />
            </div>
          </DetailRow>
        ) : null}
        {view.groupId !== '' ? (
          <DetailRow label={t('groupMemberOfLabel')}>
            <Button variant="ghost" onClick={() => openGroupDetail(view.groupId)}>
              {t('openGroupInWindowAction')}
            </Button>
          </DetailRow>
        ) : null}
        <div className="flex flex-wrap gap-2 pt-3">
          {isDownloadable(view) ? (
            <Button variant="primary" icon={Download} disabled={view.fileMissing} onClick={() => downloadViewsFiles([view])}>
              {t('webDownloadFile')}
            </Button>
          ) : null}
          <Button icon={Copy} onClick={() => copyUrls([view])}>
            {t('copyUrl')}
          </Button>
          {canChangeUrl(view) ? (
            <Button icon={Link2} onClick={() => openChangeUrl(view.taskId)}>
              {t('webChangeUrl')}
            </Button>
          ) : null}
          {canChangeSegments(view) ? (
            <Button icon={Layers} onClick={() => openChangeSegments(view.taskId)}>
              {t('editThreads')}
            </Button>
          ) : null}
        </div>
      </div>
      <SourcesSection view={view} />
    </div>
  )
}
