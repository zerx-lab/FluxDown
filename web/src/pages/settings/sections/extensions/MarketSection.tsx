// 插件市场：`daemon.plugin.marketList` 拉取索引，前端关键字过滤 + 分页展开，`marketInstall` 安装。

import { CircleAlert, Package, Search, Star } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useT } from '../../../../i18n'
import { RpcError, rpc, useAgent } from '../../../../lib/rpc'
import type { InstalledPlugin, MarketEntryDto, PluginDto } from '../../../../lib/rpc'
import { Button, EmptyState, Icon, Select, Spinner } from '../../../../ui'
import { BlockTitle, ListCard, MarketSearchInput } from './common'
import type { PluginDetail } from './detail'
import { extensionErrorText } from './errors'
import {
  MARKET_PAGE_SIZE,
  arrangeMarket,
  filterMarket,
  installedVersionYanked,
  latestPerPlugin,
  marketAction,
  marketNeedsLoad,
  pageOf,
  yankedLabelKey,
} from './logic'
import type { ConsentSet, MarketLoadState, MarketSort } from './logic'
import { MarketEntryRow } from './MarketEntryRow'
import { useMarketInstall } from './useMarketInstall'

interface MarketState {
  loading: boolean
  error: string | null
  entries: MarketEntryDto[]
}

export function MarketSection({
  stale,
  active,
  plugins,
  onInstalled,
  onInstallFailed,
  onShowDetail,
  onRefresh,
}: {
  stale: boolean
  /** 插件页当前可见（组件页隐藏时为 false）；进入时检查目录是否过期。 */
  active: boolean
  plugins: readonly PluginDto[]
  onInstalled: (result: InstalledPlugin, confirmed: ConsentSet | null, updating: boolean) => void | Promise<void>
  onInstallFailed: (error: unknown) => void
  onShowDetail: (detail: PluginDetail) => void
  /** 点「刷新」时同步通知外层（触发专属插件目录一起重拉）。 */
  onRefresh: () => void
}) {
  const t = useT()
  const [market, setMarket] = useState<MarketState>({ loading: false, error: null, entries: [] })
  const [query, setQuery] = useState('')
  const [limit, setLimit] = useState(MARKET_PAGE_SIZE)
  const loadState = useRef<MarketLoadState>({ requested: false, retryOnReconnect: false, failed: false, loadedAt: null })
  const inflight = useRef<Promise<void> | null>(null)
  const signedIn = useAgent((snapshot) => snapshot.session !== null && snapshot.session !== undefined, false)
  const [sort, setSort] = useState<MarketSort>('default')
  const [favoritesOnlyRaw, setFavoritesOnly] = useState(false)
  const [counts, setCounts] = useState<Record<string, number>>({})
  const [mine, setMine] = useState<string[]>([])
  const [favorites, setFavorites] = useState({ failed: false })
  const [favPending, setFavPending] = useState<ReadonlySet<string>>(new Set())
  const loadFavorites = async () => {
    try {
      const result = await rpc.agent.pluginFavorite.list()
      setCounts(result.counts)
      setMine(result.mine)
      setFavorites({ failed: false })
    } catch {
      // 收藏失败不影响市场列表：计数清空、星标禁用并给出内联提示。
      setCounts({})
      setMine([])
      setFavorites({ failed: true })
    }
  }

  const fetchCatalog = async () => {
    loadState.current = { ...loadState.current, requested: true, retryOnReconnect: false, failed: false }
    setMarket((current) => ({ ...current, loading: true, error: null }))
    void loadFavorites()
    try {
      const entries = await rpc.daemon.plugin.marketList()
      loadState.current = { ...loadState.current, failed: false, loadedAt: Date.now() }
      setMarket({ loading: false, error: null, entries })
    } catch (error) {
      // 断连 / 超时类失败：连接恢复后自动重试一次；其他失败保持手动刷新。
      const transport = error instanceof RpcError && (error.appCode === 'unavailable' || error.appCode === 'timeout')
      loadState.current = { ...loadState.current, failed: true, retryOnReconnect: transport }
      setMarket((current) => ({ ...current, loading: false, error: extensionErrorText(t, error) }))
    }
  }
  /** 同一时刻只有一个目录请求：在途时复用其 Promise。 */
  const load = (): Promise<void> => {
    inflight.current ??= fetchCatalog().finally(() => {
      inflight.current = null
    })
    return inflight.current
  }
  const loadRef = useRef(load)
  loadRef.current = load
  const loadFavoritesRef = useRef(loadFavorites)
  loadFavoritesRef.current = loadFavorites

  // 登录态变化（登录 / 登出）且市场已请求过时重新拉收藏。
  const lastSignedIn = useRef(signedIn)
  useEffect(() => {
    if (lastSignedIn.current === signedIn) return
    lastSignedIn.current = signedIn
    if (loadState.current.requested) void loadFavoritesRef.current()
  }, [signedIn])

  // 进入插件页（含从组件页切回）且连接就绪时按需拉取：从未请求、断连失败待重试、或目录已超过
  // `MARKET_MAX_AGE_MS`（与 GPUI `ensure_market_loaded` 一致）；在途请求不重复发起。
  useEffect(() => {
    if (!active || stale || inflight.current !== null) return
    if (marketNeedsLoad(loadState.current, Date.now())) void loadRef.current()
  }, [active, stale])

  // 用户确认的版本已不是最新：刷新目录，让下一次点击按新版本重新确认权限。
  const { pending, install, dialog } = useMarketInstall({
    run: (entry) => rpc.daemon.plugin.marketInstall({ pluginId: entry.pluginId, version: entry.version }),
    onInstalled,
    onInstallFailed,
    reloadCatalog: load,
  })

  const installedById = useMemo(() => new Map(plugins.map((plugin) => [plugin.identity, plugin])), [plugins])
  const mineSet = useMemo(() => new Set(mine), [mine])
  const favoriteDisabled = !signedIn || favorites.failed
  const favoriteTitle = (pluginId: string) =>
    t(!signedIn ? 'marketFavoriteLoginRequired' : mineSet.has(pluginId) ? 'marketFavoriteRemove' : 'marketFavoriteAdd')

  const toggleFavorite = async (pluginId: string) => {
    if (favPending.has(pluginId)) return
    const target = !mineSet.has(pluginId)
    setFavPending((current) => new Set(current).add(pluginId))
    try {
      const result = await rpc.agent.pluginFavorite.set({ pluginId, favorited: target })
      setMine((current) => {
        const rest = current.filter((id) => id !== pluginId)
        return result.favorited ? [pluginId, ...rest] : rest
      })
      setCounts((current) => ({ ...current, [pluginId]: result.count }))
    } catch (error) {
      onInstallFailed(error)
    } finally {
      setFavPending((current) => {
        const next = new Set(current)
        next.delete(pluginId)
        return next
      })
    }
  }

  const favoritesOnly = favoritesOnlyRaw && signedIn && !favorites.failed
  const filtered = useMemo(
    () => arrangeMarket(filterMarket(latestPerPlugin(market.entries), query), sort, counts, mineSet, favoritesOnly),
    [market.entries, query, sort, counts, mineSet, favoritesOnly],
  )
  const { visible, remaining } = pageOf(filtered, limit)

  let body
  if (market.loading && market.entries.length === 0) {
    body = (
      <div className="flex items-center gap-2 py-3 text-xs text-muted-foreground">
        <Spinner size="md" />
        {t('pluginCommonLoading')}
      </div>
    )
  } else if (market.error) {
    body = (
      <div className="flex items-start gap-2 text-xs text-muted-foreground">
        <Icon icon={CircleAlert} size="md" className="mt-0.5 text-destructive" />
        <span className="min-w-0 flex-1 break-words">{t('marketLoadFailed', { message: market.error })}</span>
      </div>
    )
  } else if (market.entries.length === 0) {
    body = <EmptyState icon={Package} title={t('marketEmpty')} />
  } else {
    body = (
      <>
        <MarketSearchInput
          value={query}
          onChange={(next) => {
            setQuery(next)
            setLimit(MARKET_PAGE_SIZE)
          }}
        />
        <div className="flex flex-wrap items-center gap-2">
          <div className="w-44">
            <Select<MarketSort>
              value={sort}
              aria-label={t('marketSortLabel')}
              options={[
                { value: 'default', label: t('marketSortDefault') },
                { value: 'newest', label: t('marketSortNewest') },
                { value: 'favorites', label: t('marketSortFavorites') },
              ]}
              onValueChange={(next) => {
                setSort(next)
                setLimit(MARKET_PAGE_SIZE)
              }}
            />
          </div>
          <Button
            variant={favoritesOnly ? 'primary' : 'outline'}
            icon={Star}
            aria-pressed={favoritesOnly}
            disabled={!signedIn || favorites.failed}
            title={signedIn ? undefined : t('marketFavoriteLoginRequired')}
            onClick={() => {
              setFavoritesOnly((current) => !current)
              setLimit(MARKET_PAGE_SIZE)
            }}
          >
            {t('marketFavoritesOnly')}
          </Button>
          {favorites.failed ? <span className="text-xs text-muted-foreground">{t('marketFavoritesLoadFailed')}</span> : null}
        </div>
        {filtered.length === 0 ? (
          <EmptyState icon={favoritesOnly && query.trim() === '' ? Star : Search} title={t(favoritesOnly && query.trim() === '' ? 'marketFavoritesEmpty' : 'marketSearchNoResult')} />
        ) : (
          <ListCard>
            {visible.map((entry) => {
              const installedPlugin = installedById.get(entry.pluginId)
              const installedYanked = installedPlugin ? installedVersionYanked(market.entries, installedPlugin) : null
              return (
                <MarketEntryRow
                  key={entry.pluginId}
                  entry={entry}
                  action={marketAction(entry, installedPlugin)}
                  pending={pending.has(entry.pluginId)}
                  installDisabled={stale}
                  installedYankedKey={installedYanked ? yankedLabelKey(installedYanked) : null}
                  favorite={{
                    title: favoriteTitle(entry.pluginId),
                    pressed: mineSet.has(entry.pluginId),
                    disabled: favoriteDisabled,
                    loading: favPending.has(entry.pluginId),
                    count: counts[entry.pluginId] ?? 0,
                    onToggle: () => void toggleFavorite(entry.pluginId),
                  }}
                  onInstall={() => void install(entry, installedPlugin)}
                  onShowDetail={onShowDetail}
                />
              )
            })}
          </ListCard>
        )}
        {remaining > 0 ? (
          <div className="flex justify-center">
            <Button variant="ghost" onClick={() => setLimit((current) => current + MARKET_PAGE_SIZE)}>
              {t('marketShowMore', { count: remaining })}
            </Button>
          </div>
        ) : null}
      </>
    )
  }

  return (
    <section className="flex w-full flex-col gap-2 pt-4">
      <BlockTitle
        title={t('marketSectionTitle')}
        description={t('marketSectionDesc')}
        trailing={
          <Button
            variant="ghost"
            loading={market.loading}
            disabled={stale}
            onClick={() => {
              void load()
              // 刷新 = 公开市场 + 专属目录一起重拉（与 GPUI 一致）。
              onRefresh()
            }}
          >
            {t('marketRefreshTooltip')}
          </Button>
        }
      />
      {body}
      {dialog}
    </section>
  )
}
