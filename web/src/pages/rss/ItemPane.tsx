// 右侧条目区（GPUI `RssView::render` 的 main 部分）：反馈横幅 / 订阅头 / 工具栏 /
// 「选择可见」+ 批量条 / 虚拟列表 / 空状态。桌面批量按钮在「选择可见」行内；
// 移动端批量条吸在内容区底部（内容区之下就是底部标签栏，天然避开）。

import { ArrowUpDown, Check, CircleAlert, RotateCw, Rss, Search, Settings, Trash2 } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import { RSS_ITEM_STATUS, rpc } from '../../lib/rpc'
import type { RssItemDto, RssSourceDto } from '../../lib/rpc'
import { Button, Checkbox, EmptyState, Icon, Input, Tooltip, confirmDialog } from '../../ui'
import { sourceDisplayName } from './filter'
import { sourceStatusLine, visibleIndices } from './format'
import { ItemList } from './ItemList'
import { ItemImagesDialog } from './ItemImagesDialog'
import { useRssItems } from './useRssItems'
import type { RssItemsState } from './useRssItems'

function BatchActions({
  count,
  ignoreDisabled,
  downloadDisabled,
  onClear,
  onIgnore,
  onDownload,
}: {
  count: number
  ignoreDisabled: boolean
  downloadDisabled: boolean
  onClear: () => void
  onIgnore: () => void
  onDownload: () => void
}) {
  const t = useT()
  return (
    <div className="flex min-w-0 flex-1 flex-wrap items-center gap-2 mobile:justify-end">
      <span className="text-xs text-muted-foreground tabular">{t('rssSelectedCount', { n: count })}</span>
      <span className="flex-1" />
      <Button variant="ghost" onClick={onClear}>
        {t('rssClearSelection')}
      </Button>
      <Button variant="outline" disabled={ignoreDisabled} onClick={onIgnore}>
        {t('rssIgnoreSelected')}
      </Button>
      <Button variant="primary" disabled={downloadDisabled} onClick={onDownload}>
        {t('rssDownloadSelected')}
      </Button>
    </div>
  )
}

export function ItemPane({
  source,
  stale,
  now,
  mobile,
  query,
  onQueryChange,
  oldestFirst,
  onToggleSort,
  onEdit,
  onRefreshingChange,
}: {
  source: RssSourceDto
  stale: boolean
  now: number
  mobile: boolean
  query: string
  onQueryChange: (query: string) => void
  oldestFirst: boolean
  onToggleSort: () => void
  onEdit: (source: RssSourceDto) => void
  onRefreshingChange: (refreshing: boolean) => void
}) {
  const t = useT()
  const state: RssItemsState = useRssItems(source.sourceId, stale)
  const { items, selected, busy, refreshBusy, readBusy } = state
  const [deleteBusy, setDeleteBusy] = useState(false)
  const [imageItem, setImageItem] = useState<RssItemDto | null>(null)

  useEffect(() => {
    if (stale) setImageItem(null)
  }, [stale])

  useEffect(() => {
    onRefreshingChange(refreshBusy)
    return () => onRefreshingChange(false)
  }, [refreshBusy, onRefreshingChange])

  const visible = useMemo(() => visibleIndices(items, query, oldestFirst), [items, query, oldestFirst])
  const rows = useMemo(() => visible.flatMap((index) => (items[index] ? [items[index]] : [])), [visible, items])
  const visibleSelected = rows.filter((item) => selected.has(item.guid)).length
  const allVisible = rows.length > 0 && visibleSelected === rows.length
  const anyVisible = visibleSelected > 0
  const selectedGuids = useMemo(() => [...selected], [selected])
  const ignoreGuids = useMemo(
    () => items.filter((item) => item.status === RSS_ITEM_STATUS.new && selected.has(item.guid)).map((item) => item.guid),
    [items, selected],
  )
  const ignoreDisabled = stale || ignoreGuids.every((guid) => busy.has(guid))
  const downloadDisabled = stale || selectedGuids.every((guid) => busy.has(guid))
  const unhealthy = source.lastError !== ''
  const name = sourceDisplayName(source)
  const error = stale ? t('localServiceDisconnected') : state.actionError

  const confirmDelete = async () => {
    if (stale || deleteBusy) return
    const title = t('rssDeleteSource')
    const ok = await confirmDialog({ title, description: t('rssDeleteConfirmDesc', { name }), okLabel: title, intent: 'destructive' })
    if (!ok) return
    setDeleteBusy(true)
    try {
      await rpc.daemon.rss.deleteSource({ sourceId: source.sourceId })
    } catch {
      state.fail()
    } finally {
      setDeleteBusy(false)
    }
  }

  const batch = (
    <BatchActions
      count={selected.size}
      ignoreDisabled={ignoreDisabled}
      downloadDisabled={downloadDisabled}
      onClear={state.clearSelection}
      onIgnore={() => void state.act(ignoreGuids, 'ignore')}
      onDownload={() => void state.act(selectedGuids, 'download')}
    />
  )

  let body: ReactNode
  if (state.loading && rows.length === 0) {
    body = <EmptyState className="flex-1" icon={Rss} title={t('rssEmptyFetching')} description={t('rssEmptyFetchingHint')} />
  } else if (rows.length === 0) {
    if (query.trim() !== '' && items.length > 0) {
      body = <EmptyState className="flex-1" icon={Search} title={t('rssNoMatch', { query: query.trim().toLowerCase() })} />
    } else if (unhealthy || state.loadError) {
      body = (
        <EmptyState
          className="flex-1 [&_svg]:text-destructive"
          icon={CircleAlert}
          title={t('rssEmptyError')}
          description={t('rssEmptyErrorHint')}
          action={
            <div className="flex flex-wrap justify-center gap-2 pt-1">
              <Button variant="outline" disabled={stale} onClick={() => void (state.loadError ? state.reload() : state.refresh())}>
                {t('rssEmptyRetry')}
              </Button>
              <Button variant="outline" disabled={stale} onClick={() => onEdit(source)}>
                {t('rssCheckConfig')}
              </Button>
            </div>
          }
        />
      )
    } else if (!source.seeded && source.enabled) {
      body = <EmptyState className="flex-1" icon={Rss} title={t('rssEmptyFetching')} description={t('rssEmptyFetchingHint')} />
    } else {
      body = <EmptyState className="flex-1" icon={Rss} title={t('rssEmptyTitle')} description={t('rssEmptyDesc')} />
    }
  } else {
    body = (
      <ItemList
        rows={rows}
        selected={selected}
        busy={busy}
        taskStates={state.taskStates}
        stale={stale}
        mobile={mobile}
        onToggle={state.toggle}
        onAct={(guids, action) => void state.act(guids, action)}
        onViewImages={setImageItem}
      />
    )
  }

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col bg-surface">
      {error ? <div className="shrink-0 bg-destructive/10 px-4 py-1.5 text-xs text-destructive">{error}</div> : null}
      {state.feedback ? (
        <div className="flex shrink-0 items-center gap-3 bg-accent px-4 py-0.5">
          <span className="min-w-0 flex-1 text-xs text-foreground">{state.feedback}</span>
          <Button variant="ghost" onClick={state.dismissFeedback}>
            {t('close')}
          </Button>
        </div>
      ) : null}

      <div className="flex shrink-0 flex-col gap-3 border-b border-hairline px-4 py-3">
        <div className="flex items-center gap-2">
          <Icon icon={Rss} className={unhealthy ? 'text-destructive' : 'text-muted-foreground'} />
          <div className="flex min-w-0 flex-1 flex-col">
            <span className="truncate text-sm font-semibold text-foreground mobile:hidden" title={name}>
              {name}
            </span>
            <span className={cn('truncate text-xs tabular', unhealthy ? 'text-destructive' : 'text-text-tertiary')}>
              {sourceStatusLine(t, source, refreshBusy, now)}
            </span>
          </div>
          <Tooltip content={t('rssManageTitle')}>
            <Button
              variant="ghost"
              iconOnly
              aria-label={t('rssManageTitle')}
              className="text-muted-foreground hover:text-foreground"
              disabled={stale}
              onClick={() => onEdit(source)}
            >
              <Icon icon={Settings} size="md" />
            </Button>
          </Tooltip>
          <Tooltip content={t('rssDeleteSource')}>
            <Button
              variant="ghost"
              iconOnly
              aria-label={t('rssDeleteSource')}
              className="text-destructive"
              disabled={stale || deleteBusy}
              onClick={() => void confirmDelete()}
            >
              <Icon icon={Trash2} size="md" />
            </Button>
          </Tooltip>
        </div>
        <div className="flex flex-wrap items-center gap-1">
          <Button variant="ghost" icon={ArrowUpDown} onClick={onToggleSort}>
            {t(oldestFirst ? 'rssSortOldest' : 'rssSortNewest')}
          </Button>
          <Button variant="ghost" icon={Check} disabled={stale || readBusy} onClick={() => void state.readAll()}>
            {t('rssMarkAllRead')}
          </Button>
          <Button variant="ghost" icon={RotateCw} disabled={stale || refreshBusy} onClick={() => void state.refresh()}>
            {t(refreshBusy ? 'rssRefreshing' : 'rssRefreshNow')}
          </Button>
          <span className="min-w-2 flex-1" />
          <div className="relative w-[200px] mobile:order-last mobile:w-full">
            <Icon icon={Search} size="md" className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-text-tertiary" />
            <Input
              type="search"
              value={query}
              placeholder={t('rssSearchHint')}
              aria-label={t('rssSearchHint')}
              className="pl-8"
              onChange={(event) => onQueryChange(event.target.value)}
            />
          </div>
        </div>
      </div>

      {rows.length > 0 || selected.size > 0 ? (
        <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-hairline px-4 py-0.5">
          <label className="flex h-control cursor-pointer items-center gap-3 coarse:min-h-touch">
            <Checkbox
              checked={allVisible ? true : anyVisible ? 'indeterminate' : false}
              onCheckedChange={() => state.setMany(rows.map((item) => item.guid), !allVisible)}
              aria-label={t('rssSelectVisible')}
            />
            <span className="text-xs font-medium text-text-tertiary">{t('rssSelectVisible')}</span>
          </label>
          {selected.size > 0 && !mobile ? batch : null}
        </div>
      ) : null}

      {body}

      {imageItem && !stale ? (
        <ItemImagesDialog key={imageItem.guid} item={imageItem} onClose={() => setImageItem(null)} />
      ) : null}

      {selected.size > 0 && mobile ? (
        <div className="shrink-0 border-t border-hairline bg-chrome px-3 py-2">{batch}</div>
      ) : null}
    </div>
  )
}
