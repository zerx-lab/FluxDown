import { useT } from '../../../i18n'
import { sourcePageUrl } from '../model/task'
import type { DownloadTaskView } from '../model/task'
import { routeLabel } from '../../../lib/taskRoute'
import { CopyButton, SourcePageLink } from './CopyButton'
import { DetailRow } from './DetailRow'

export function AdvancedTab({ view }: { view: DownloadTaskView }) {
  const t = useT()
  const dto = view.dto
  return (
    <div className="flex flex-col">
      <DetailRow label={t('infoSourcePage')}>
        {view.referrer === '' ? (
          t('detailNotSet')
        ) : (
          <div className="flex items-center gap-1">
            <SourcePageLink text={view.referrer} url={sourcePageUrl(view)} className="min-w-0 flex-1 break-all" />
            <CopyButton text={view.referrer} label={t('detailCopySourcePage')} copiedLabel={t('detailSourcePageCopied')} />
          </div>
        )}
      </DetailRow>
      <DetailRow label={t('taskProxy')}>{dto && dto.proxyUrl !== '' ? dto.proxyUrl : t('detailFollowGlobal')}</DetailRow>
      {dto && dto.autoRoute !== '' ? <DetailRow label={t('taskRoute')}>{routeLabel(t, dto.autoRoute)}</DetailRow> : null}
      <DetailRow label={t('taskIgnoreTlsErrors')}>{dto?.ignoreTlsErrors ? '✓' : '—'}</DetailRow>
    </div>
  )
}
