import { Users } from 'lucide-react'
import { useState } from 'react'
import { useI18n, useT } from '../../../i18n'
import { FLAG_URLS } from '../../../lib/flags/flagUrls'
import { flagTitle, resolveFlagUrl } from '../../../lib/flags/flags'
import { cn } from '../../../lib/cn'
import { ContextMenuArea, EmptyState } from '../../../ui'
import { formatBytes } from '../model/task'
import type { TaskState } from '../model/task'
import type { Translate } from '../table/text'
import { copyEntry, PollError, RowCopyButton, SkeletonRows, useMeasuredRoot } from './btParts'
import { PeerActionCell, PeerEvidenceDialog, PeerGuardSummary, SuspicionCell } from './BtPeerGuard'
import { peerGuardMenu } from './peerGuardActions'
import { hasGuardData, mapPeerRows, peerColumns, peerLayout, peersEmptyKey, transportLabel } from './btStatus'
import type { PeerColumn, PeerLayout, PeerRow } from './btStatus'
import { useBtStatus } from './useBtStatus'

const DASH = '—'

const COLUMN_WIDTH: Record<PeerColumn, string> = {
  addr: 'minmax(11rem,1.3fr)',
  client: 'minmax(0,1fr)',
  transport: '3.5rem',
  downloaded: '4.75rem',
  uploaded: '4.75rem',
  downBps: '5.5rem',
  upBps: '5.5rem',
  groupDelivered: '5.5rem',
  suspicion: 'minmax(8rem,1fr)',
  action: 'minmax(5rem,0.7fr)',
}

const HEADER_KEY: Record<PeerColumn, string> = {
  addr: 'btPeerAddr',
  client: 'btPeerClient',
  transport: 'btPeerTransport',
  downloaded: 'btPeerDownloaded',
  uploaded: 'btPeerUploaded',
  downBps: 'btPeerDownSpeed',
  upBps: 'btPeerUpSpeed',
  groupDelivered: 'btGuardPeerUploaded',
  suspicion: 'btGuardPeerSuspicion',
  action: 'btGuardPeerAction',
}

const NUMERIC: Record<PeerColumn, boolean> = {
  addr: false,
  client: false,
  transport: false,
  downloaded: true,
  uploaded: true,
  downBps: true,
  upBps: true,
  groupDelivered: true,
  suspicion: false,
  action: false,
}

const rate = (bps: number): string => (bps > 0 ? `${formatBytes(bps)}/s` : DASH)

function TransportChip({ row }: { row: PeerRow }) {
  return (
    <span className="inline-flex shrink-0 justify-center rounded-sm bg-muted px-1.5 py-px text-caption font-medium text-muted-foreground">
      {transportLabel(row.transport)}
    </span>
  )
}

/** 速度单元：非零用正文色，0 用三级文字色。 */
function RateCell({ bps }: { bps: number }) {
  return <span className={cn('tabular truncate text-right text-xs', bps > 0 ? 'text-foreground' : 'text-text-tertiary')}>{rate(bps)}</span>
}

function Cell({ column, row }: { column: PeerColumn; row: PeerRow }) {
  switch (column) {
    case 'addr':
      return null
    case 'client':
      return (
        <span title={row.client} className="truncate whitespace-pre text-xs text-muted-foreground">
          {row.client === '' ? DASH : row.client}
        </span>
      )
    case 'transport':
      return <TransportChip row={row} />
    case 'downloaded':
      return <span className="tabular truncate text-right text-xs text-muted-foreground">{formatBytes(row.downloaded)}</span>
    case 'uploaded':
      return <span className="tabular truncate text-right text-xs text-muted-foreground">{formatBytes(row.uploaded)}</span>
    case 'downBps':
      return <RateCell bps={row.downBps} />
    case 'upBps':
      return <RateCell bps={row.upBps} />
    case 'groupDelivered':
      return (
        <span title={row.guard.ipGroup} className="tabular truncate text-right text-xs text-muted-foreground">
          {row.guard.ipGroup === '' ? DASH : formatBytes(row.guard.groupDeliveredBytes)}
        </span>
      )
    case 'suspicion':
      return <SuspicionCell view={row.guard} />
    case 'action':
      return <PeerActionCell view={row.guard} />
  }
}

/** 国旗槽：宽高恒定（4:3），无国家时留空，国家码晚到不引起行内布局跳动。 */
function PeerFlag({ country }: { country: string }) {
  const { locale } = useI18n()
  const url = resolveFlagUrl(FLAG_URLS, country)
  if (url === null) return <span aria-hidden className="inline-block h-3 w-4 shrink-0" />
  return (
    <img
      src={url}
      alt={country}
      title={flagTitle(country, locale)}
      width={16}
      height={12}
      draggable={false}
      className="inline-block h-3 w-4 shrink-0 rounded-xs object-cover ring-1 ring-hairline"
    />
  )
}

function Row({ row, layout, columns, t, onEvidence }: { row: PeerRow; layout: PeerLayout; columns: readonly PeerColumn[]; t: Translate; onEvidence: (row: PeerRow) => void }) {
  const menu = [copyEntry(t('btCopyPeerAddr'), row.addr, null, t), ...peerGuardMenu(row, t, onEvidence)]
  const template = columns.map((c) => COLUMN_WIDTH[c]).join(' ')

  if (layout === 'card') {
    return (
      <ContextMenuArea entries={menu} title={row.addr} className="group/row border-b border-hairline px-1 py-2 hover:bg-row-hover">
        <div className="flex items-center gap-2">
          <PeerFlag country={row.country} />
          <span title={row.addr} className="tabular min-w-0 truncate font-mono text-xs text-foreground">
            {row.addr}
          </span>
          <TransportChip row={row} />
          <span className="ml-auto" />
          <RowCopyButton label={t('btCopyPeerAddr')} value={row.addr} doneKey={null} />
        </div>
        <div className="tabular mt-1 flex flex-wrap items-center gap-x-3 text-xs text-text-tertiary">
          <span className={row.downBps > 0 ? 'text-foreground' : undefined}>↓ {rate(row.downBps)}</span>
          <span className={row.upBps > 0 ? 'text-foreground' : undefined}>↑ {rate(row.upBps)}</span>
          <span>{formatBytes(row.downloaded)} / {formatBytes(row.uploaded)}</span>
          {row.client === '' ? null : (
            <span title={row.client} className="min-w-0 truncate whitespace-pre text-muted-foreground">
              {row.client}
            </span>
          )}
        </div>
        <PeerGuardSummary view={row.guard} />
      </ContextMenuArea>
    )
  }

  return (
    <ContextMenuArea entries={menu} title={row.addr} className="group/row border-b border-hairline hover:bg-row-hover">
      <div className="grid items-center gap-x-3 px-1 py-1.5" style={{ gridTemplateColumns: template }}>
        {columns.map((column) =>
          column === 'addr' ? (
            <div key={column} className="flex min-w-0 items-center gap-1.5">
              <PeerFlag country={row.country} />
              <div className="min-w-0 flex-1">
                <span title={row.addr} className="tabular block truncate font-mono text-xs text-foreground">
                  {row.addr}
                </span>
                {layout === 'compact' && row.client !== '' ? (
                  <span title={row.client} className="block truncate whitespace-pre text-xs text-muted-foreground">
                    {row.client}
                  </span>
                ) : null}
                {layout === 'compact' ? <PeerGuardSummary view={row.guard} showAction={false} /> : null}
              </div>
              <RowCopyButton label={t('btCopyPeerAddr')} value={row.addr} doneKey={null} />
            </div>
          ) : (
            <Cell key={column} column={column} row={row} />
          ),
        )}
      </div>
    </ContextMenuArea>
  )
}

function Header({ columns, t }: { columns: readonly PeerColumn[]; t: Translate }) {
  return (
    <div
      className="grid gap-x-3 border-b border-hairline px-1 pb-1 text-xs text-text-tertiary"
      style={{ gridTemplateColumns: columns.map((c) => COLUMN_WIDTH[c]).join(' ') }}
    >
      {columns.map((column) => (
        <span key={column} className={cn('truncate', NUMERIC[column] && 'text-right')}>
          {t(HEADER_KEY[column])}
        </span>
      ))}
    </div>
  )
}

export function BtPeersTab({ taskId, state, seeding }: { taskId: string; state: TaskState; seeding: boolean }) {
  const t = useT()
  const { data, error } = useBtStatus(taskId)
  const { ref, width } = useMeasuredRoot()
  const { rows } = data ? mapPeerRows(data.peers) : { rows: [] }
  const guard = hasGuardData(rows)
  const layout = peerLayout(width, guard)
  const columns = peerColumns(layout, guard)
  const [evidenceRow, setEvidenceRow] = useState<PeerRow | null>(null)
  const counts = data?.peerCounts

  return (
    <div ref={ref} className="flex flex-col gap-2">
      {counts ? (
        <div className="tabular text-xs text-muted-foreground">
          {t('btPeersSummary', { live: counts.live, connecting: counts.connecting, queued: counts.queued, seen: counts.seen, dead: counts.dead })}
        </div>
      ) : null}
      <PollError message={error} />
      {data === null ? (
        error === null ? <SkeletonRows /> : null
      ) : rows.length === 0 ? (
        <EmptyState icon={Users} title={t(peersEmptyKey(state, seeding))} className="py-6" />
      ) : (
        <div className="flex flex-col">
          {layout === 'card' ? null : <Header columns={columns} t={t} />}
          {rows.map((row) => (
            <Row key={row.key} row={row} layout={layout} columns={columns} t={t} onEvidence={setEvidenceRow} />
          ))}
        </div>
      )}
      <PeerEvidenceDialog row={evidenceRow} onClose={() => setEvidenceRow(null)} />
    </div>
  )
}
