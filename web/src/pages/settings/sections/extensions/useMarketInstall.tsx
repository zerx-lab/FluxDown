// 市场安装共用流程：权限确认对话框（consent）→ 调用方给定的安装调用 → 安装后授权（onInstalled）。
// 公开市场（`daemon.plugin.marketInstall`）与专属插件（`agent.privatePlugin.install`）只在 `run` 与
// 「版本已变」时刷新哪个目录上不同；确认文案与权限差集规则（`permissionsToConfirm` / `hostsToConfirm`）同一份。

import { useState } from 'react'
import { useT } from '../../../../i18n'
import { RpcError } from '../../../../lib/rpc'
import type { InstalledPlugin, MarketEntryDto, PluginDto } from '../../../../lib/rpc'
import { hostsToConfirm, permissionsToConfirm, replacesLocalPlugin, riskOf } from './logic'
import type { ConsentSet } from './logic'
import { PluginConsentDialog } from './PluginConsentDialog'
import type { ConsentView } from './PluginConsentDialog'

export interface MarketInstallOptions {
  /** 发起真正的安装调用；`entry.version` 即用户确认权限时看到的版本。 */
  run: (entry: MarketEntryDto) => Promise<InstalledPlugin>
  onInstalled: (result: InstalledPlugin, confirmed: ConsentSet | null, updating: boolean) => void | Promise<void>
  onInstallFailed: (error: unknown) => void
  /** 用户确认的版本已不是最新（`marketVersionChanged`）：刷新对应目录，让下一次点击按新版本重新确认。 */
  reloadCatalog: () => Promise<void>
}

export function useMarketInstall({ run, onInstalled, onInstallFailed, reloadCatalog }: MarketInstallOptions) {
  const t = useT()
  const [pending, setPending] = useState<ReadonlySet<string>>(new Set())
  const [confirm, setConfirm] = useState<{ view: ConsentView; resolve: (ok: boolean) => void } | null>(null)

  const install = async (entry: MarketEntryDto, installed: PluginDto | undefined) => {
    const pluginId = entry.pluginId
    if (pending.has(pluginId)) return
    const permissions = permissionsToConfirm(entry, installed)
    const networkHosts = hostsToConfirm(entry, installed)
    const confirmed: ConsentSet = { permissions, networkHosts }
    // 私有安装将替换本机同 id 的非私有插件时，即使没有新增权限也要弹确认（带替换警示）。
    const replaces = replacesLocalPlugin(entry, installed)
    if (permissions.length > 0 || networkHosts.length > 0 || replaces) {
      const updating = installed !== undefined
      const name = entry.name === '' ? pluginId : entry.name
      const ok = await new Promise<boolean>((resolve) => {
        setConfirm({
          resolve,
          view: {
            title: t(updating ? 'pluginPermConfirmUpdateTitle' : 'pluginPermConfirmInstallTitle', { name }),
            intro:
              t(updating ? 'pluginPermConfirmUpdateBody' : 'pluginPermConfirmInstallBody', { version: entry.version }) +
              (replaces ? `\n\n${t('privatePluginReplaceWarning')}` : ''),
            permissions,
            networkHosts,
            risk: riskOf(entry.permissions),
            okLabel: t(updating ? 'pluginPermConfirmUpdateOk' : 'pluginPermConfirmInstallOk'),
          },
        })
      })
      if (!ok) return
    }
    setPending((current) => new Set(current).add(pluginId))
    try {
      await onInstalled(await run(entry), confirmed, installed !== undefined)
    } catch (error) {
      if (error instanceof RpcError && error.reason === 'marketVersionChanged') await reloadCatalog()
      onInstallFailed(error)
    } finally {
      setPending((current) => {
        const next = new Set(current)
        next.delete(pluginId)
        return next
      })
    }
  }

  const dialog = (
    <PluginConsentDialog
      consent={confirm?.view ?? null}
      onCancel={() => {
        confirm?.resolve(false)
        setConfirm(null)
      }}
      onOk={() => {
        confirm?.resolve(true)
        setConfirm(null)
      }}
    />
  )

  return { pending, install, dialog }
}
