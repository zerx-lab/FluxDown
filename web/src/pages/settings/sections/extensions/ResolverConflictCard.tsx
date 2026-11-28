// 插件页「多个插件可处理同一链接」卡（对应 GPUI `crates/extensions/src/pages/resolver_conflicts.rs`）：
// 只列还需要处理的事项；已确认覆盖全部冲突插件的站点移出，待授权配对只在安装 / 启用提示里预告。

import { Check, Info, TriangleAlert } from 'lucide-react'
import { useState } from 'react'
import { useT } from '../../../../i18n'
import { rpc } from '../../../../lib/rpc'
import type { PluginDto, PluginSiteDefaultDto } from '../../../../lib/rpc'
import { Badge, Button, Card, Select, Tooltip, toast } from '../../../../ui'
import { extensionErrorText } from './errors'
import { conflictReport, pluginDisplayName, siteResolution } from './logic'

export function ResolverConflictCard({
  plugins,
  defaults,
  defaultsLoaded,
  devMode,
  stale,
  onChanged,
}: {
  plugins: readonly PluginDto[]
  defaults: readonly PluginSiteDefaultDto[]
  /** 首次读取站点默认前不渲染站点行，避免已处理的站点闪现。 */
  defaultsLoaded: boolean
  devMode: boolean
  stale: boolean
  onChanged: () => Promise<void>
}) {
  const t = useT()
  const [busySite, setBusySite] = useState<string | null>(null)
  const report = conflictReport(plugins)
  const openSites = defaultsLoaded
    ? report.sites
        .map((group) => ({ group, resolution: siteResolution(group, defaults) }))
        .filter(({ resolution }) => resolution.kind !== 'confirmed')
    : []
  const duplicates = devMode ? report.duplicates : []
  if (openSites.length === 0 && report.unscoped.length === 0 && duplicates.length === 0) return null

  const name = (identity: string) => pluginDisplayName(plugins, identity)
  const join = t('pluginResolverConflictJoin')

  const choose = async (site: string, identity: string, candidates: string[]) => {
    if (busySite !== null) return
    setBusySite(site)
    try {
      await rpc.daemon.plugin.setSiteDefault({ site, identity, candidates })
    } catch (error) {
      toast.error(t('pluginSiteDefaultSaveFailed', { error: extensionErrorText(t, error) }))
    } finally {
      await onChanged()
      setBusySite(null)
    }
  }

  const patternsHint = (items: readonly { identity: string; pattern: string }[]) => (
    <Tooltip
      content={
        <div className="whitespace-pre-wrap [overflow-wrap:anywhere]">
          {[t('pluginResolverConflictPatterns'), ...items.map((item) => `${name(item.identity)}: ${item.pattern}`)].join('\n')}
        </div>
      }
    >
      <span
        tabIndex={0}
        role="img"
        className="inline-flex shrink-0 text-muted-foreground"
        aria-label={[t('pluginResolverConflictPatterns'), ...items.map((item) => `${name(item.identity)}: ${item.pattern}`)].join(' ')}
      >
        <Info className="size-4" />
      </span>
    </Tooltip>
  )

  return (
    <Card className="flex w-full flex-col overflow-hidden [&>*+*]:border-t [&>*+*]:border-hairline">
      {openSites.length > 0 || report.unscoped.length > 0 ? (
        <div className="flex items-start gap-2 px-3 py-2">
          <TriangleAlert className="mt-0.5 size-4 shrink-0 text-warning" />
          <div className="flex min-w-0 flex-col gap-0.5">
            <div className="text-sm font-medium text-foreground">{t('pluginResolverConflictsHeading')}</div>
            <div className="text-xs text-muted-foreground">{t('pluginResolverConflictsDesc')}</div>
          </div>
        </div>
      ) : null}
      {openSites.map(({ group, resolution }) => {
        const automatic = resolution.kind === 'automatic' ? resolution.identity : null
        const disabled = stale || busySite !== null
        return (
          <div key={group.site} className="flex flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2">
            <div className="flex min-w-0 flex-[1_1_12rem] flex-col gap-0.5">
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <span className="min-w-0 break-all text-sm font-medium text-foreground">{group.site}</span>
                {automatic !== null ? <Badge tone="warning">{t('pluginSiteDefaultAutomatic')}</Badge> : null}
              </div>
              <div className="text-xs text-muted-foreground">
                {t('pluginResolverConflictHandledBy', { plugins: group.plugins.map(name).join(join) })}
              </div>
            </div>
            <div className="ml-auto flex shrink-0 items-center gap-2">
              {patternsHint(group.patterns)}
              <Select
                className="w-48"
                aria-label={group.site}
                value={automatic ?? ''}
                placeholder={t('pluginResolverConflictPick')}
                disabled={disabled}
                options={group.plugins.map((identity) => ({ value: identity, label: name(identity) }))}
                onValueChange={(identity) => {
                  if (identity !== automatic) void choose(group.site, identity, group.plugins)
                }}
              />
              {automatic !== null ? (
                <Button variant="primary" icon={Check} disabled={disabled} onClick={() => void choose(group.site, automatic, group.plugins)}>
                  {t('pluginSiteDefaultConfirm')}
                </Button>
              ) : null}
            </div>
          </div>
        )
      })}
      {report.unscoped.map((pair) => (
        <div key={JSON.stringify([pair.owner, pair.pattern, pair.other, pair.otherPattern])} className="flex flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2">
          <div className="flex min-w-0 flex-[1_1_12rem] flex-col gap-0.5">
            <div className="break-words text-sm font-medium text-foreground">
              {t('pluginResolverConflictPair', { first: name(pair.owner), second: name(pair.other) })}
            </div>
            <div className="text-xs text-muted-foreground">{t('pluginResolverConflictAnyHint')}</div>
          </div>
          <div className="ml-auto flex shrink-0 items-center">
            {patternsHint([
              { identity: pair.owner, pattern: pair.pattern },
              { identity: pair.other, pattern: pair.otherPattern },
            ])}
          </div>
        </div>
      ))}
      {duplicates.length > 0 ? (
        <div className="flex flex-col gap-0.5 px-3 py-2">
          <div className="text-sm font-medium text-foreground">{t('pluginResolverConflictDuplicates')}</div>
          {duplicates.map((pair) => (
            <div key={JSON.stringify([pair.owner, pair.pattern, pair.otherPattern])} className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
              {`${name(pair.owner)}: ${pair.pattern} = ${pair.otherPattern}`}
            </div>
          ))}
        </div>
      ) : null}
    </Card>
  )
}
