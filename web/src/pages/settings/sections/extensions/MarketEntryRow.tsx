// 市场条目行：公开市场与专属插件共用。收藏星标（仅公开市场）、专属角标 / 到期（仅专属）均为可选。

import { Info, Star } from 'lucide-react'
import { cn } from '../../../../lib/cn'
import type { MarketEntryDto } from '../../../../lib/rpc'
import { useT } from '../../../../i18n'
import { Badge, Button, Icon, Tooltip } from '../../../../ui'
import { ExtLink, IconButton, ListRow } from './common'
import { detailFromMarket } from './detail'
import type { PluginDetail } from './detail'
import { riskOf, yankedLabelKey } from './logic'
import type { MarketAction } from './logic'
import { RiskBadge } from './PluginConsentDialog'

/** 收藏星标的全部状态；缺省 = 该行不带收藏（专属插件）。 */
export interface FavoriteControl {
  title: string
  pressed: boolean
  disabled: boolean
  loading: boolean
  count: number
  onToggle: () => void
}

export function MarketEntryRow({
  entry,
  action,
  pending,
  installDisabled,
  installedYankedKey,
  favorite,
  privateBadge = false,
  expiresText,
  onInstall,
  onShowDetail,
}: {
  entry: MarketEntryDto
  action: MarketAction
  pending: boolean
  installDisabled: boolean
  /** 已装版本被撤回时的标签键（已由 `yankedLabelKey` 解析）。 */
  installedYankedKey: string | null
  favorite?: FavoriteControl
  /** 专属插件角标（`privatePluginBadge`）。 */
  privateBadge?: boolean
  /** 已本地化的「授权至 …」；永久授权不传。 */
  expiresText?: string | null
  onInstall: () => void
  onShowDetail: (detail: PluginDetail) => void
}) {
  const t = useT()
  const yankedKey = yankedLabelKey(entry.yanked)
  return (
    <ListRow
      info={
        <>
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span className="min-w-0 break-words text-sm font-medium text-foreground">{entry.name === '' ? entry.pluginId : entry.name}</span>
            <span className="tabular text-xs text-muted-foreground">v{entry.version}</span>
            {entry.author ? <span className="text-xs text-muted-foreground">{entry.author}</span> : null}
            {privateBadge ? <Badge tone="accent">{t('privatePluginBadge')}</Badge> : null}
            {yankedKey ? <Badge tone="destructive">{t(yankedKey)}</Badge> : null}
            {installedYankedKey ? <Badge tone="destructive">{t('pluginInstalledVersionYanked', { label: t(installedYankedKey) })}</Badge> : null}
            <RiskBadge risk={riskOf(entry.permissions)} />
          </div>
          {entry.homepage ? <ExtLink href={entry.homepage} /> : null}
          {expiresText ? <div className="break-words text-xs text-muted-foreground">{t('privatePluginExpires', { date: expiresText })}</div> : null}
          {entry.description ? <div className="line-clamp-2 break-words text-xs text-muted-foreground">{entry.description}</div> : null}
        </>
      }
      actions={
        <>
          {favorite ? (
            <Tooltip content={favorite.title}>
              <Button
                variant="ghost"
                aria-label={favorite.title}
                aria-pressed={favorite.pressed}
                disabled={favorite.disabled || favorite.loading}
                loading={favorite.loading}
                onClick={favorite.onToggle}
                className={cn('gap-1 px-2 tabular', favorite.pressed ? 'text-accent-text' : 'text-muted-foreground hover:text-foreground')}
              >
                {favorite.loading ? null : <Icon icon={Star} size="md" className={favorite.pressed ? 'fill-current' : undefined} />}
                {favorite.count > 0 ? <span className="text-xs">{favorite.count}</span> : null}
              </Button>
            </Tooltip>
          ) : null}
          <IconButton icon={Info} label={t('pluginDetailDescription')} onClick={() => onShowDetail(detailFromMarket(entry))} />
          <Button variant="outline" loading={pending} disabled={action === 'installed' || action === 'unavailable' || installDisabled} onClick={onInstall}>
            {pending
              ? t(action === 'update' ? 'marketUpdatingButton' : 'marketInstallingButton')
              : t(
                  action === 'install'
                    ? 'marketInstallButton'
                    : action === 'update'
                      ? 'marketUpdateButton'
                      : action === 'installed'
                        ? 'marketInstalledButton'
                        : 'marketUnavailableButton',
                )}
          </Button>
        </>
      }
    />
  )
}
