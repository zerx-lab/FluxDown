// 专属插件：`agent.privatePlugin.list` 拉取 FluxCloud 私有商店中为当前账号开通的条目，
// 行 / 权限确认 / 安装后授权与公开市场共用（`MarketEntryRow` + `useMarketInstall`），安装走 `agent.privatePlugin.install`。
// 目录只进内存：进入插件页懒加载一次，不轮询；登录态变化时重拉；「插件市场」刷新按钮同时强制重拉；已登录但无授权整区不展示。

import { CircleAlert, Search } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useI18n } from '../../../../i18n'
import { rpc, useAgent } from '../../../../lib/rpc'
import type { InstalledPlugin, PluginDto } from '../../../../lib/rpc'
import { Button, EmptyState, Icon, Spinner } from '../../../../ui'
import { LoginDialog } from '../account/AuthDialogs'
import { BlockTitle, ListCard, MarketSearchInput } from './common'
import type { PluginDetail } from './detail'
import {
  INITIAL_PRIVATE_CATALOG,
  MARKET_PAGE_SIZE,
  beginPrivateLoad,
  failedPrivateLoad,
  filterPrivate,
  installedVersionYanked,
  latestPerPlugin,
  marketAction,
  pageOf,
  privateExpiryText,
  privateSearchVisible,
  privateSectionView,
  yankedLabelKey,
} from './logic'
import type { ConsentSet, PrivateCatalogState } from './logic'
import { MarketEntryRow } from './MarketEntryRow'
import { useMarketInstall } from './useMarketInstall'

/** 本地化短日期；locale 不被 Intl 接受时退回浏览器默认。 */
function shortDate(locale: string, date: Date): string {
  try {
    return date.toLocaleDateString(locale, { dateStyle: 'medium' })
  } catch {
    return date.toLocaleDateString()
  }
}

export function PrivateSection({
  stale,
  active,
  plugins,
  onInstalled,
  onInstallFailed,
  onShowDetail,
  refreshNonce,
}: {
  stale: boolean
  /** 插件页当前可见；首次可见且连接就绪时拉一次。 */
  active: boolean
  plugins: readonly PluginDto[]
  onInstalled: (result: InstalledPlugin, confirmed: ConsentSet | null, updating: boolean) => void | Promise<void>
  onInstallFailed: (error: unknown) => void
  onShowDetail: (detail: PluginDetail) => void
  /** 「插件市场」刷新按钮每点一次加一：触发专属目录强制重拉。 */
  refreshNonce: number
}) {
  const { t, locale } = useI18n()
  const [catalog, setCatalog] = useState<PrivateCatalogState>(INITIAL_PRIVATE_CATALOG)
  const [limit, setLimit] = useState(MARKET_PAGE_SIZE)
  const [query, setQuery] = useState('')
  const [loginOpen, setLoginOpen] = useState(false)
  const requested = useRef(false)
  /** 只采纳最后一次请求的应答（登录态切换 / 重试会让更早的在途请求作废）。 */
  const generation = useRef(0)
  const signedIn = useAgent((snapshot) => snapshot.session !== null && snapshot.session !== undefined, false)

  /** `reset`：丢弃已有目录回到加载态（首次 / 重试 / 登录态变化）；否则保留旧列表静默刷新（刷新按钮 / 版本已变）。 */
  const load = async (reset: boolean) => {
    const mine = ++generation.current
    requested.current = true
    // 首次 / 重试 / 换账号从第一页开始；静默刷新（含安装后重拉）保持已展开的页数。
    if (reset) setLimit(MARKET_PAGE_SIZE)
    setCatalog((current) => beginPrivateLoad(current, reset))
    try {
      const result = await rpc.agent.privatePlugin.list()
      if (generation.current !== mine) return
      setCatalog({ loading: false, failed: false, signedIn: result.signedIn, entries: result.entries })
    } catch {
      if (generation.current !== mine) return
      setCatalog(failedPrivateLoad)
      setLimit(MARKET_PAGE_SIZE)
    }
  }
  const loadRef = useRef(load)
  loadRef.current = load

  useEffect(() => {
    if (!active || stale || requested.current) return
    void loadRef.current(true)
  }, [active, stale])

  const lastSignedIn = useRef(signedIn)
  useEffect(() => {
    if (lastSignedIn.current === signedIn) return
    lastSignedIn.current = signedIn
    if (requested.current) void loadRef.current(true)
  }, [signedIn])

  // 「插件市场」刷新按钮递增 `refreshNonce`：不论当前是已加载 / 失败 / 加载中都强制重拉；
  // 在途请求由 `generation` 作废后重发，已有条目保持显示直到新结果替换。未发起过加载（连接未就绪）时不抢跑。
  const lastRefresh = useRef(refreshNonce)
  useEffect(() => {
    if (lastRefresh.current === refreshNonce) return
    lastRefresh.current = refreshNonce
    if (!stale && requested.current) void loadRef.current(false)
  }, [refreshNonce, stale])

  const { pending, install, dialog } = useMarketInstall({
    run: (entry) => rpc.agent.privatePlugin.install({ pluginId: entry.pluginId, version: entry.version }),
    onInstalled,
    onInstallFailed,
    reloadCatalog: () => load(false),
  })

  const installedById = useMemo(() => new Map(plugins.map((plugin) => [plugin.identity, plugin])), [plugins])
  const catalogEntries = useMemo(() => latestPerPlugin(catalog.entries), [catalog.entries])
  const view = privateSectionView(catalog)
  const login = loginOpen ? <LoginDialog onClose={() => setLoginOpen(false)} /> : null

  if (view === 'hidden') {
    return (
      <>
        {dialog}
        {login}
      </>
    )
  }

  let body
  if (view === 'loading') {
    body = (
      <div className="flex items-center gap-2 py-3 text-xs text-muted-foreground">
        <Spinner size="md" />
        {t('privatePluginsLoading')}
      </div>
    )
  } else if (view === 'error') {
    body = (
      <div className="flex items-start gap-2 text-xs text-muted-foreground">
        <Icon icon={CircleAlert} size="md" className="mt-0.5 text-destructive" />
        <span className="min-w-0 flex-1 break-words">{t('privatePluginsLoadFailed')}</span>
        <Button variant="outline" disabled={stale} onClick={() => void load(true)}>
          {t('privatePluginsRetry')}
        </Button>
      </div>
    )
  } else if (view === 'signIn') {
    body = (
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 text-xs text-muted-foreground">
        <span className="min-w-0 flex-1 break-words">{t('privatePluginsSignInHint')}</span>
        <Button variant="outline" disabled={stale} onClick={() => setLoginOpen(true)}>
          {t('accountLogin')}
        </Button>
      </div>
    )
  } else {
    const searchVisible = privateSearchVisible(catalogEntries.length)
    const filtered = filterPrivate(catalogEntries, query)
    const { visible, remaining } = pageOf(filtered, limit)
    body = (
      <>
        {searchVisible ? (
          <MarketSearchInput
            value={query}
            onChange={(next) => {
              setQuery(next)
              setLimit(MARKET_PAGE_SIZE)
            }}
          />
        ) : null}
        {visible.length === 0 ? (
          <EmptyState icon={Search} title={t('marketSearchNoResult')} />
        ) : (
          <ListCard>
            {visible.map((entry) => {
              const installedPlugin = installedById.get(entry.pluginId)
              const installedYanked = installedPlugin ? installedVersionYanked(catalog.entries, installedPlugin) : null
              return (
                <MarketEntryRow
                  key={entry.pluginId}
                  entry={entry}
                  action={marketAction(entry, installedPlugin)}
                  pending={pending.has(entry.pluginId)}
                  installDisabled={stale}
                  installedYankedKey={installedYanked ? yankedLabelKey(installedYanked) : null}
                  privateBadge
                  expiresText={privateExpiryText(entry.expiresAt, (date) => shortDate(locale, date))}
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
      <BlockTitle title={t('privatePluginsTitle')} description={t('privatePluginsDesc')} />
      {body}
      {dialog}
      {login}
    </section>
  )
}
