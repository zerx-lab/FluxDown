// 设置 · 扩展：插件 / 站点默认 / 组件三个子页（对应 GPUI `crates/extensions`）。
// 子页首次访问才挂载（版本列表 / 市场懒加载），之后保持挂载以保留输入、进度与列表状态。

import { useState } from 'react'
import { useT } from '../../../../i18n'
import { useAgent, useConnection } from '../../../../lib/rpc'
import { SegmentedTabs } from '../../../../ui'
import { SettingsPage } from '../../kit/layout'
import { ComponentsTab } from './ComponentsTab'
import { PluginsTab } from './PluginsTab'
import { SiteDefaultsTab } from './SiteDefaultsTab'

type ExtensionsTab = 'plugins' | 'siteDefaults' | 'components'

const selectDaemonConnected = (snapshot: { daemonConnected: boolean }) => snapshot.daemonConnected

export function ExtensionsSettings() {
  const t = useT()
  const [tab, setTab] = useState<ExtensionsTab>('plugins')
  const [visited, setVisited] = useState<ReadonlySet<ExtensionsTab>>(new Set<ExtensionsTab>(['plugins']))

  // 写操作在连接未就绪 / daemon 断开时禁用（对应 GPUI controller 的 stale）。
  const phase = useConnection().phase
  const daemonConnected = useAgent(selectDaemonConnected, true)
  const stale = phase !== 'ready' || !daemonConnected

  const show = (next: ExtensionsTab) => {
    setTab(next)
    setVisited((current) => (current.has(next) ? current : new Set(current).add(next)))
  }

  return (
    <SettingsPage title={t('settingsCatExtensions')} description={t('settingsCatExtensionsDesc')}>
      <SegmentedTabs
        className="self-start"
        value={tab}
        onValueChange={show}
        items={[
          { value: 'plugins', label: t('settingsCatPlugins') },
          { value: 'siteDefaults', label: t('pluginSiteDefaultsTab') },
          { value: 'components', label: t('settingsCatComponents') },
        ]}
      />
      <div hidden={tab !== 'plugins'}>
        <PluginsTab stale={stale} active={tab === 'plugins'} onGoToComponents={() => show('components')} />
      </div>
      {visited.has('siteDefaults') ? (
        <div hidden={tab !== 'siteDefaults'}>
          <SiteDefaultsTab stale={stale} active={tab === 'siteDefaults'} />
        </div>
      ) : null}
      {visited.has('components') ? (
        <div hidden={tab !== 'components'}>
          <ComponentsTab stale={stale} />
        </div>
      ) : null}
    </SettingsPage>
  )
}
