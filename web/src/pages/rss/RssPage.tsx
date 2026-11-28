// RSS 页（GPUI `crates/rss`）：订阅源列（桌面 240px 可拖拽）| 条目区。
// 移动端（<=820px）为「订阅列表 → 条目详情」两级导航，详情里标题栏带返回键。
// 该页在 GPUI 里没有标题栏插槽，因此桌面标题栏保持空白。

import { ChevronLeft, Plus, Rss } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../i18n'
import { useAgent, useConnection, useDaemon } from '../../lib/rpc'
import type { PluginDto, QueueDto, RssSourceDto } from '../../lib/rpc'
import { TitleBarSlot } from '../../shell'
import { useColumnWidth } from '../../lib/useColumnWidth'
import { Button, ColumnResizer, EmptyState, Icon, useIsMobile } from '../../ui'
import { sourceDisplayName } from './filter'
import { ItemPane } from './ItemPane'
import { SourceColumn } from './SourceColumn'
import { SourceEditor } from './SourceEditor'
import { useNowSeconds } from './useNow'

const EMPTY_SOURCES: readonly RssSourceDto[] = []
const EMPTY_QUEUES: readonly QueueDto[] = []
const EMPTY_PLUGINS: readonly PluginDto[] = []
const COLUMN_STORAGE_KEY = 'fluxdown.web.rss.sourceColumnWidth'

export function RssPage() {
  const t = useT()
  const mobile = useIsMobile()
  const rawSources = useDaemon((daemon) => daemon.rssSources, EMPTY_SOURCES)
  const queues = useDaemon((daemon) => daemon.queues, EMPTY_QUEUES)
  const plugins = useDaemon((daemon) => daemon.plugins, EMPTY_PLUGINS)
  const daemonConnected = useAgent((snapshot) => snapshot.daemonConnected, false)
  const { phase } = useConnection()
  const stale = !daemonConnected || phase !== 'ready'
  const now = useNowSeconds()
  const { width, handleProps } = useColumnWidth(COLUMN_STORAGE_KEY)

  const sources = useMemo(() => [...rawSources].sort((a, b) => a.position - b.position), [rawSources])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [detailOpen, setDetailOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [oldestFirst, setOldestFirst] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const [editor, setEditor] = useState<{ key: number; source: RssSourceDto | null } | null>(null)

  const selected = sources.find((source) => source.sourceId === selectedId)
  // 所选订阅被删除 / 尚未选择：桌面回落到第一个；移动端回到列表。
  const active = selected ?? sources[0]
  const showDetail = mobile ? detailOpen && selected !== undefined : true

  useEffect(() => {
    if (mobile && detailOpen && selected === undefined) setDetailOpen(false)
  }, [mobile, detailOpen, selected])

  const openEditor = (source: RssSourceDto | null) => setEditor((prev) => ({ key: (prev?.key ?? 0) + 1, source }))
  const select = (sourceId: string) => {
    setSelectedId(sourceId)
    setDetailOpen(true)
  }

  const column = (
    <SourceColumn
      sources={sources}
      selectedId={active?.sourceId ?? null}
      refreshingId={refreshing && active ? active.sourceId : null}
      now={now}
      disabled={stale}
      onSelect={select}
      onAdd={() => openEditor(null)}
    />
  )
  const onRefreshingChange = useCallback((value: boolean) => setRefreshing(value), [])

  let pane: ReactNode
  if (active) {
    pane = (
      <ItemPane
        key={active.sourceId}
        source={active}
        stale={stale}
        now={now}
        mobile={mobile}
        query={query}
        onQueryChange={setQuery}
        oldestFirst={oldestFirst}
        onToggleSort={() => setOldestFirst((value) => !value)}
        onEdit={openEditor}
        onRefreshingChange={onRefreshingChange}
      />
    )
  } else {
    pane = (
      <EmptyState
        className="h-full"
        icon={Rss}
        title={t('rssAddSource')}
        description={t('rssSidebarEmptyHint')}
        action={
          <Button variant="primary" icon={Plus} disabled={stale} onClick={() => openEditor(null)}>
            {t('rssAddSource')}
          </Button>
        }
      />
    )
  }

  return (
    <div className="flex h-full min-h-0 w-full">
      {mobile && showDetail && selected ? (
        <TitleBarSlot>
          <Button variant="ghost" iconOnly aria-label={t('back')} onClick={() => setDetailOpen(false)}>
            <Icon icon={ChevronLeft} />
          </Button>
          <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{sourceDisplayName(selected)}</span>
        </TitleBarSlot>
      ) : null}
      {mobile ? (
        <div className="h-full min-h-0 min-w-0 flex-1">{showDetail ? pane : column}</div>
      ) : (
        <>
          <aside className="h-full shrink-0 bg-chrome" style={{ width }}>
            {column}
          </aside>
          <ColumnResizer width={width} handleProps={handleProps} />
          <div className="h-full min-h-0 min-w-0 flex-1">{pane}</div>
        </>
      )}
      {editor ? (
        <SourceEditor
          key={editor.key}
          open
          source={editor.source}
          queues={queues}
          plugins={plugins}
          onOpenChange={(open) => {
            if (!open) setEditor(null)
          }}
        />
      ) : null}
    </div>
  )
}
