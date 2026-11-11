import { Users } from 'lucide-react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { ContextMenuArea, EmptyState } from '../../../ui'
import { formatBytes } from '../model/task'
import type { Translate } from '../table/text'
import { copyEntry, PollError, RowCopyButton, SkeletonRows, useMeasuredRoot, useNowSecs } from './btParts'
import { nextAnnounceText } from './btStatus'
import { ed2kLayout, ed2kPeerColumns, mapEd2kPeers, partsText, peerOriginKey, peerStateLabel, peerStateTone } from './ed2kStatus'
import type { Ed2kLayout, Ed2kPeerColumn, Ed2kPeerRow } from './ed2kStatus'
import { useEd2kStatus } from './useEd2kStatus'

const DASH = '—'

const COLUMN_WIDTH: Record<Ed2kPeerColumn, string> = {
  addr: 'minmax(10rem,1.3fr)',
  origin: '4rem',
  client: 'minmax(0,1fr)',
  state: 'minmax(7rem,1fr)',
  parts: '4.5rem',
  downloaded: '4.75rem',
  speed: '5.5rem',
}

const HEADER_KEY: Record<Ed2kPeerColumn, string> = {
  addr: 'ed2kPeerAddr',
  origin: 'ed2kPeerOrigin',
  client: 'ed2kPeerClient',
  state: 'ed2kPeerState',
  parts: 'ed2kPeerParts',
  downloaded: 'ed2kPeerDownloaded',
  speed: 'ed2kPeerSpeed',
}

const NUMERIC: Partial<Record<Ed2kPeerColumn, true>> = { parts: true, downloaded: true, speed: true }

/** 失败时冷却 / 失败原因作 tooltip，可被复制（原始英文错误文本原样展示）。 */
function StateCell({ row, t, now }: { row: Ed2kPeerRow; t: Translate; now: number }) {
  const label = peerStateLabel(row, now)
  const text =
    label.retryAt !== undefined ? t(label.key, { time: nextAnnounceText(t, label.retryAt, now) ?? DASH }) : t(label.key, label.params)
  return (
    <span className="flex min-w-0 flex-col">
      <span title={row.message || text} className={cn('truncate text-xs font-medium', peerStateTone(row.state))}>
        {text}
      </span>
      {row.message === '' ? null : (
        <span title={row.message} className="truncate text-caption text-text-tertiary">
          {row.message}
        </span>
      )}
    </span>
  )
}

function AddrText({ row, t }: { row: Ed2kPeerRow; t: Translate }) {
  if (row.addr !== '') {
    return (
      <span title={row.addr} className="tabular block truncate font-mono text-xs text-foreground">
        {row.addr}
      </span>
    )
  }
  return (
    <span title={t('ed2kPeerLowIdHint')} className="tabular block cursor-help truncate font-mono text-xs text-muted-foreground">
      {row.lowId === null ? DASH : t('ed2kPeerLowId', { id: row.lowId })}
    </span>
  )
}

function ClientText({ row }: { row: Ed2kPeerRow }) {
  const title = [row.client, row.nickname].filter((s) => s !== '').join('\n')
  return (
    <span title={title} className="flex min-w-0 flex-col">
      <span className="truncate whitespace-pre text-xs text-muted-foreground">{row.client === '' ? DASH : row.client}</span>
      {row.nickname === '' ? null : <span className="truncate whitespace-pre text-caption text-text-tertiary">{row.nickname}</span>}
    </span>
  )
}

const rate = (bps: number): string => (bps > 0 ? `${formatBytes(bps)}/s` : DASH)

function Cell({ column, row, t, now, partsTotal }: { column: Ed2kPeerColumn; row: Ed2kPeerRow; t: Translate; now: number; partsTotal: number }) {
  switch (column) {
    case 'addr':
      return null
    case 'origin':
      return <span className="truncate text-xs text-muted-foreground">{t(peerOriginKey(row.origin))}</span>
    case 'client':
      return <ClientText row={row} />
    case 'state':
      return <StateCell row={row} t={t} now={now} />
    case 'parts':
      return <span className="tabular truncate text-right text-xs text-muted-foreground">{partsText(row.parts, partsTotal) ?? DASH}</span>
    case 'downloaded':
      return <span className="tabular truncate text-right text-xs text-muted-foreground">{row.downloaded > 0 ? formatBytes(row.downloaded) : DASH}</span>
    case 'speed':
      return <span className={cn('tabular truncate text-right text-xs', row.downBps > 0 ? 'text-foreground' : 'text-text-tertiary')}>{rate(row.downBps)}</span>
  }
}

function Row({ row, layout, columns, t, now, partsTotal }: { row: Ed2kPeerRow; layout: Ed2kLayout; columns: readonly Ed2kPeerColumn[]; t: Translate; now: number; partsTotal: number }) {
  const menu = row.addr === '' ? [] : [copyEntry(t('btCopyPeerAddr'), row.addr, null, t)]
  const copy = row.addr === '' ? null : <RowCopyButton label={t('btCopyPeerAddr')} value={row.addr} doneKey={null} />

  if (layout === 'card') {
    return (
      <ContextMenuArea entries={menu} title={row.addr} className="group/row border-b border-hairline px-1 py-2 hover:bg-row-hover">
        <div className="flex items-center gap-2">
          <div className="min-w-0 flex-1">
            <AddrText row={row} t={t} />
          </div>
          {copy}
        </div>
        <div className="mt-1 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <StateCell row={row} t={t} now={now} />
          </div>
          <span className={cn('tabular shrink-0 text-xs', row.downBps > 0 ? 'text-foreground' : 'text-text-tertiary')}>{rate(row.downBps)}</span>
        </div>
        <div className="tabular mt-0.5 flex flex-wrap gap-x-3 text-xs text-text-tertiary">
          <span>{t(peerOriginKey(row.origin))}</span>
          <span>{partsText(row.parts, partsTotal) ?? DASH}</span>
          {row.client === '' ? null : (
            <span title={row.client} className="min-w-0 truncate whitespace-pre">
              {row.client}
            </span>
          )}
        </div>
      </ContextMenuArea>
    )
  }

  return (
    <ContextMenuArea entries={menu} title={row.addr} className="group/row border-b border-hairline hover:bg-row-hover">
      <div className="grid items-center gap-x-3 px-1 py-1.5" style={{ gridTemplateColumns: columns.map((c) => COLUMN_WIDTH[c]).join(' ') }}>
        {columns.map((column) =>
          column === 'addr' ? (
            <div key={column} className="flex min-w-0 items-center gap-1.5">
              <div className="min-w-0 flex-1">
                <AddrText row={row} t={t} />
                {layout === 'compact' && row.client !== '' ? (
                  <span title={row.client} className="block truncate whitespace-pre text-xs text-muted-foreground">
                    {row.client}
                  </span>
                ) : null}
              </div>
              {copy}
            </div>
          ) : (
            <Cell key={column} column={column} row={row} t={t} now={now} partsTotal={partsTotal} />
          ),
        )}
      </div>
    </ContextMenuArea>
  )
}

export function Ed2kSourcesTab({ taskId }: { taskId: string }) {
  const t = useT()
  const { data, error } = useEd2kStatus(taskId)
  const now = useNowSecs()
  const { ref, width } = useMeasuredRoot()
  const layout = ed2kLayout(width)
  const columns = ed2kPeerColumns(layout)
  const rows = data ? mapEd2kPeers(data.peers) : []
  const counts = data?.peerCounts

  return (
    <div ref={ref} className="flex flex-col gap-2">
      {counts ? <div className="tabular text-xs text-muted-foreground">{t('ed2kPeersSummary', { ...counts })}</div> : null}
      <PollError message={error === null ? null : `${t('ed2kStatusLoadFailed')}: ${error}`} />
      {data === null ? (
        error === null ? <SkeletonRows /> : null
      ) : rows.length === 0 ? (
        <EmptyState icon={Users} title={t(data.phase === 'idle' ? 'ed2kIdleHint' : 'ed2kPeersEmpty')} className="py-6" />
      ) : (
        <div className="flex flex-col">
          {layout === 'card' ? null : (
            <div className="grid gap-x-3 border-b border-hairline px-1 pb-1 text-xs text-text-tertiary" style={{ gridTemplateColumns: columns.map((c) => COLUMN_WIDTH[c]).join(' ') }}>
              {columns.map((column) => (
                <span key={column} className={cn('truncate', NUMERIC[column] && 'text-right')}>
                  {t(HEADER_KEY[column])}
                </span>
              ))}
            </div>
          )}
          {rows.map((row) => (
            <Row key={row.key} row={row} layout={layout} columns={columns} t={t} now={now} partsTotal={data.partsTotal} />
          ))}
        </div>
      )}
    </div>
  )
}
