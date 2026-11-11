import { useVirtualizer } from '@tanstack/react-virtual'
import { ArrowDown, ArrowUp, Radio, RotateCw } from 'lucide-react'
import { useMemo, useRef, useState } from 'react'
import type { RefObject } from 'react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { Button, ContextMenuArea, EmptyState, RevealCallout } from '../../../ui'
import { formatBytes } from '../model/task'
import { reannounceTask } from '../model/actions'
import type { Translate } from '../table/text'
import { copyEntry, PollError, RowCopyButton, SkeletonRows, useAncestorScroller, useMeasuredRoot, useNowSecs } from './btParts'
import {
  countText,
  lastOkText,
  mapTrackerRows,
  nextAnnounceText,
  nextTrackerSort,
  showNoneWorking,
  splitMiddle,
  TRACKER_DOT,
  TRACKER_FILTERS,
  TRACKER_TEXT,
  trackerCounts,
  trackerLayout,
  trackerMessageColumn,
  trackerStatsItems,
  trackerStatusKey,
  webSeedStateKey,
  trackerView,
} from './btStatus'
import type { TrackerCounts, TrackerFilter, TrackerLayout, TrackerRow, TrackerSort, TrackerSortKey } from './btStatus'
import type { BtTrackerStatus, BtWebSeedDto } from '../../../lib/rpc'
import { useBtStatus } from './useBtStatus'

const DASH = '—'

const GRID: Record<Exclude<TrackerLayout, 'card'>, string> = {
  table: 'grid-cols-[minmax(7.5rem,9.5rem)_minmax(0,1fr)_3.25rem_6rem_6.5rem]',
  wide: 'grid-cols-[minmax(7.5rem,9.5rem)_minmax(0,1.4fr)_3.25rem_minmax(0,1fr)_6rem_6.5rem]',
  wideStats: 'grid-cols-[minmax(7.5rem,9.5rem)_minmax(0,1.4fr)_3.25rem_2.5rem_3.5rem_3.5rem_4rem_minmax(0,1fr)_6rem_6.5rem]',
}

type StatLabels = readonly [string, string, string, string]

const statLabels = (t: Translate): StatLabels => [t('btTrackerTier'), t('btTrackerSeeders'), t('btTrackerLeechers'), t('btTrackerDownloaded')]

/** 统计不单独成列的版式（卡片 / 表格 / 窄宽版）在 URL 下方放一行统计；`wideStats` 各占一列。 */
function StatsLine({ row, t }: { row: TrackerRow; t: Translate }) {
  return (
    <div className="tabular flex flex-wrap gap-x-3 text-xs text-text-tertiary">
      {trackerStatsItems(statLabels(t), row).map((item) => (
        <span key={item}>{item}</span>
      ))}
    </div>
  )
}

function StatusCell({ status, label }: { status: BtTrackerStatus; label: string }) {
  return (
    <span className={cn('flex min-w-0 items-center gap-1.5 text-xs font-medium', TRACKER_TEXT[status])}>
      <span aria-hidden className={cn('size-2 shrink-0 rounded-full', TRACKER_DOT[status])} />
      <span className="truncate">{label}</span>
    </span>
  )
}

/** URL：头部按宽度截断、尾部固定，视觉上是中间省略；文本仍连续可选。 */
function UrlText({ url, title }: { url: string; title: string }) {
  const { head, tail } = splitMiddle(url)
  return (
    <span title={title} className="flex min-w-0 font-mono text-xs text-foreground">
      <span className="min-w-0 truncate whitespace-pre">{head}</span>
      {tail === '' ? null : <span className="shrink-0 whitespace-pre">{tail}</span>}
    </span>
  )
}

function SourceChip({ t, source, hint }: { t: Translate; source: TrackerRow['source']; hint: string }) {
  return (
    <span title={hint} className="shrink-0 cursor-help rounded-sm bg-muted px-1.5 py-px text-caption text-text-tertiary">
      {t(source === 'torrent' ? 'btTrackerSourceTorrent' : 'btTrackerSourceSession')}
    </span>
  )
}

function Row({ row, layout, t, now }: { row: TrackerRow; layout: TrackerLayout; t: Translate; now: number }) {
  const status = t(trackerStatusKey(row.status))
  const peers = row.peers === null ? DASH : String(row.peers)
  const next = nextAnnounceText(t, row.nextAnnounceAt, now) ?? DASH
  const last = lastOkText(t, row.lastOkAt, now)
  const message = row.message
  const copyError = row.detail === '' ? null : row.detail
  const messageLine =
    message === '' ? null : (
      <span className="flex min-w-0 items-center gap-1">
        <span
          title={copyError ?? message}
          className={cn('block min-w-0 truncate text-xs', row.status === 'notWorking' ? 'text-destructive' : 'text-muted-foreground')}
        >
          {message}
        </span>
        {copyError === null ? null : <RowCopyButton label={t('btCopyTrackerError')} value={copyError} doneKey="btTrackerErrorCopied" />}
      </span>
    )
  const menu = [
    copyEntry(t('btCopyTrackerUrl'), row.url, 'btTrackerUrlCopied', t),
    ...(copyError === null ? [] : [{ ...copyEntry(t('btCopyTrackerError'), copyError, 'btTrackerErrorCopied', t), key: 'copyError' }]),
  ]
  // 悬停 URL：完整地址 + 来源说明（表格版式不放来源徽标，说明只在这里）。
  const hint = t(row.source === 'torrent' ? 'btTrackerSourceTorrentHint' : 'btTrackerSourceSessionHint')
  const urlTitle = `${row.url}\n${hint}`

  if (layout === 'card') {
    return (
      <ContextMenuArea entries={menu} title={row.url} className="group/row border-b border-hairline px-1 py-2 hover:bg-row-hover">
        <div className="flex items-center gap-2">
          <StatusCell status={row.status} label={status} />
          <SourceChip t={t} source={row.source} hint={hint} />
          <span className="tabular ml-auto shrink-0 text-xs text-muted-foreground">{peers}</span>
          <RowCopyButton label={t('btCopyTrackerUrl')} value={row.url} doneKey="btTrackerUrlCopied" />
        </div>
        <div className="mt-1 flex">
          <UrlText url={row.url} title={urlTitle} />
        </div>
        {messageLine ? <div className="mt-0.5">{messageLine}</div> : null}
        <div className="mt-1">
          <StatsLine row={row} t={t} />
        </div>
        <div className="tabular mt-1 flex flex-wrap gap-x-3 text-xs text-text-tertiary">
          <span>
            {t('btTrackerNextAnnounce')}: {next}
          </span>
          <span>
            {t('btTrackerLastOk')}: {last}
          </span>
        </div>
      </ContextMenuArea>
    )
  }

  return (
    <ContextMenuArea entries={menu} title={row.url} className="group/row border-b border-hairline hover:bg-row-hover">
      <div className={cn('grid items-center gap-x-3 px-1 py-1.5', GRID[layout])}>
        <StatusCell status={row.status} label={status} />
        <div className="min-w-0">
          <div className="flex min-w-0 items-center gap-1.5">
            <UrlText url={row.url} title={urlTitle} />
            {trackerMessageColumn(layout) ? <SourceChip t={t} source={row.source} hint={hint} /> : null}
            <RowCopyButton label={t('btCopyTrackerUrl')} value={row.url} doneKey="btTrackerUrlCopied" />
          </div>
          {layout === 'table' ? messageLine : null}
          {layout === 'wideStats' ? null : <StatsLine row={row} t={t} />}
        </div>
        <span className="tabular text-right text-xs text-muted-foreground">{peers}</span>
        {layout === 'wideStats' ? (
          <>
            <span className="tabular text-right text-xs text-muted-foreground">{row.tier}</span>
            <span className="tabular text-right text-xs text-muted-foreground">{countText(row.seeders)}</span>
            <span className="tabular text-right text-xs text-muted-foreground">{countText(row.leechers)}</span>
            <span className="tabular text-right text-xs text-muted-foreground">{countText(row.downloaded)}</span>
          </>
        ) : null}
        {trackerMessageColumn(layout) ? <div className="min-w-0">{messageLine}</div> : null}
        <span className="tabular truncate text-xs text-muted-foreground">{next}</span>
        <span className="tabular truncate text-xs text-muted-foreground">{last}</span>
      </div>
    </ContextMenuArea>
  )
}

interface SortProps {
  sort: TrackerSort
  onSort: (key: TrackerSortKey) => void
}

function SortButton({ label, sortKey, sort, onSort, t, className }: SortProps & { label: string; sortKey: TrackerSortKey; t: Translate; className?: string }) {
  const active = sort !== null && sort.key === sortKey
  const Icon = active && sort.desc ? ArrowDown : ArrowUp
  return (
    <button
      type="button"
      title={t('btTrackerSortHint')}
      aria-sort={active ? (sort.desc ? 'descending' : 'ascending') : 'none'}
      onClick={() => onSort(sortKey)}
      className={cn(
        'flex min-w-0 items-center gap-0.5 text-left transition-colors hover:text-foreground',
        active && 'text-foreground',
        className,
      )}
    >
      <span className="truncate">{label}</span>
      <Icon aria-hidden className={cn('size-3 shrink-0', active ? 'opacity-100' : 'opacity-0')} />
    </button>
  )
}

function Header({ layout, t, sort, onSort }: { layout: Exclude<TrackerLayout, 'card'>; t: Translate } & SortProps) {
  const common = { sort, onSort, t }
  return (
    <div className={cn('grid gap-x-3 border-b border-hairline px-1 pb-1 text-xs text-text-tertiary', GRID[layout])}>
      <SortButton {...common} sortKey="status" label={t('btTrackerStatus')} />
      <SortButton {...common} sortKey="url" label={t('btTrackerUrl')} />
      <SortButton {...common} sortKey="peers" label={t('btTrackerPeers')} className="justify-end" />
      {layout === 'wideStats'
        ? statLabels(t).map((label) => (
            <span key={label} title={label} className="truncate text-right">
              {label}
            </span>
          ))
        : null}
      {trackerMessageColumn(layout) ? <span>{t('btTrackerMessage')}</span> : null}
      <SortButton {...common} sortKey="nextAnnounce" label={t('btTrackerNextAnnounce')} />
      <SortButton {...common} sortKey="lastOk" label={t('btTrackerLastOk')} />
    </div>
  )
}

/** HTTP 源（BEP 19 webseed）：数量很少，直接平铺；URL 中间省略，悬停看全文。 */
function WebSeedsSection({ seeds, t }: { seeds: readonly BtWebSeedDto[]; t: Translate }) {
  return (
    <section className="flex flex-col gap-1 pt-2" aria-label={t('btWebSeedsSection')}>
      <h3 className="text-xs font-medium text-text-tertiary">{t('btWebSeedsSection')}</h3>
      {seeds.map((seed, index) => {
        const key = webSeedStateKey(seed.state)
        const state = key === null ? seed.state : t(key)
        const message = (seed.message ?? '').trim()
        return (
          <ContextMenuArea
            key={`${seed.url}#${index}`}
            entries={[copyEntry(t('btCopyTrackerUrl'), seed.url, 'btTrackerUrlCopied', t)]}
            title={seed.url}
            className="group/row border-b border-hairline px-1 py-1.5 hover:bg-row-hover"
          >
            <div className="flex min-w-0 items-center gap-2">
              <span title={t('btTrackerStatus')} className="shrink-0 rounded-sm bg-muted px-1.5 py-px text-caption text-text-tertiary">
                {state}
              </span>
              <UrlText url={seed.url} title={`${t('btWebSeedUrl')}: ${seed.url}`} />
              <span title={t('btPeerDownloaded')} className="tabular ml-auto shrink-0 text-xs text-muted-foreground">
                {formatBytes(seed.downloaded ?? 0)}
              </span>
              <RowCopyButton label={t('btCopyTrackerUrl')} value={seed.url} doneKey="btTrackerUrlCopied" />
            </div>
            {message === '' ? null : (
              <span title={message} className="mt-0.5 block min-w-0 truncate text-xs text-muted-foreground">
                {message}
              </span>
            )}
          </ContextMenuArea>
        )
      })}
    </section>
  )
}

function FilterTabs({
  t,
  counts,
  filter,
  onFilter,
}: {
  t: Translate
  counts: TrackerCounts
  filter: TrackerFilter
  onFilter: (filter: TrackerFilter) => void
}) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {TRACKER_FILTERS.filter((f) => f === 'all' || counts[f] > 0 || f === filter).map((f) => {
        const active = f === filter
        const label = f === 'all' ? t('btTrackerFilterAll') : t(trackerStatusKey(f))
        return (
          <button
            key={f}
            type="button"
            aria-pressed={active}
            onClick={() => onFilter(f)}
            className={cn(
              'tabular rounded-sm px-2 py-0.5 text-xs transition-colors',
              active ? 'bg-muted font-medium text-foreground' : 'text-text-tertiary hover:bg-row-hover hover:text-foreground',
            )}
          >
            {label} {counts[f]}
          </button>
        )
      })}
    </div>
  )
}

const ROW_OVERSCAN = 8
const ROW_ESTIMATE: Record<TrackerLayout, number> = { card: 96, table: 44, wide: 44, wideStats: 36 }

/** 虚拟化行列表：滚动容器是祖先面板，`scrollMargin` = 列表顶部相对滚动内容顶部的偏移。 */
function VirtualRows({ rows, layout, t, now, rootRef }: { rows: TrackerRow[]; layout: TrackerLayout; t: Translate; now: number; rootRef: RefObject<HTMLDivElement | null> }) {
  const listRef = useRef<HTMLDivElement | null>(null)
  const { scrollEl, scrollMargin } = useAncestorScroller(listRef, rootRef)

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollEl,
    estimateSize: () => ROW_ESTIMATE[layout],
    getItemKey: (index) => rows[index]?.key ?? index,
    overscan: ROW_OVERSCAN,
    scrollMargin,
  })

  return (
    <div ref={listRef} className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
      {virtualizer.getVirtualItems().map((item) => {
        const row = rows[item.index]
        if (row === undefined) return null
        return (
          <div
            key={item.key}
            ref={virtualizer.measureElement}
            data-index={item.index}
            className="absolute left-0 top-0 w-full"
            style={{ transform: `translateY(${item.start - scrollMargin}px)` }}
          >
            <Row row={row} layout={layout} t={t} now={now} />
          </div>
        )
      })}
    </div>
  )
}

function DhtRow({ enabled, layout, t }: { enabled: boolean; layout: TrackerLayout; t: Translate }) {
  const status: BtTrackerStatus = enabled ? 'working' : 'notContacted'
  const label = t(enabled ? 'btTrackerDhtOn' : 'btTrackerDhtOff')
  if (layout === 'card') {
    return (
      <div className="flex items-center gap-2 border-b border-hairline px-1 py-2">
        <StatusCell status={status} label={label} />
        <span className="ml-auto font-mono text-xs text-muted-foreground">{t('btTrackerDht')}</span>
      </div>
    )
  }
  return (
    <div className={cn('grid items-center gap-x-3 border-b border-hairline px-1 py-1.5', GRID[layout])}>
      <StatusCell status={status} label={label} />
      <span className="font-mono text-xs text-muted-foreground">{t('btTrackerDht')}</span>
    </div>
  )
}

/** `running`：任务在下载中或做种中（GPUI `bt_task_running`），决定「强制汇报」是否可用。 */
export function BtTrackersTab({ taskId, running }: { taskId: string; running: boolean }) {
  const t = useT()
  const { data, error } = useBtStatus(taskId)
  const now = useNowSecs()
  const { ref, width } = useMeasuredRoot()
  const layout = trackerLayout(width)
  const [filter, setFilter] = useState<TrackerFilter>('all')
  const [sort, setSort] = useState<TrackerSort>(null)
  const rows = useMemo(() => (data ? mapTrackerRows(data.trackers) : []), [data])
  const counts = useMemo(() => trackerCounts(rows), [rows])
  const view = useMemo(() => trackerView(rows, filter, sort).flatMap((index) => rows[index] ?? []), [rows, filter, sort])
  const warn = data !== null && showNoneWorking(data.trackers)
  const onSort = (key: TrackerSortKey) => setSort((current) => nextTrackerSort(current, key))
  const [reannouncing, setReannouncing] = useState(false)
  const onReannounce = () => {
    setReannouncing(true)
    void reannounceTask(taskId).finally(() => setReannouncing(false))
  }
  const webSeeds = data?.webSeeds ?? []

  return (
    <div ref={ref} className="flex flex-col gap-2">
      <RevealCallout visible={warn} tone="warning" title={t('btTabTrackers')} body={t('btTrackersNoneWorking')} contentClassName="" />
      <PollError message={error} />
      {data === null ? (
        error === null ? <SkeletonRows /> : null
      ) : (
        <>
          <div className="flex items-center gap-2">
            <div className="min-w-0 flex-1">{rows.length === 0 ? null : <FilterTabs t={t} counts={counts} filter={filter} onFilter={setFilter} />}</div>
            <Button variant="ghost" icon={RotateCw} loading={reannouncing} disabled={!running || reannouncing} onClick={onReannounce}>
              {t('btReannounce')}
            </Button>
          </div>
          <div className="flex flex-col">
            {layout === 'card' ? null : <Header layout={layout} t={t} sort={sort} onSort={onSort} />}
            <DhtRow enabled={data.dhtEnabled} layout={layout} t={t} />
            {rows.length === 0 ? (
              <EmptyState icon={Radio} title={t('btTrackersEmpty')} className="py-6" />
            ) : view.length === 0 ? (
              <EmptyState icon={Radio} title={t('btTrackersFilterEmpty')} className="py-6" />
            ) : (
              <VirtualRows key={layout} rows={view} layout={layout} t={t} now={now} rootRef={ref} />
            )}
          </div>
          {webSeeds.length === 0 ? null : <WebSeedsSection seeds={webSeeds} t={t} />}
        </>
      )}
    </div>
  )
}
