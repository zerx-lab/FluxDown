// 条目 id → 实际控件。同一批小部件既用于外壳区域，也用于移动端页面级状态栏（见 `StatusBar.tsx`）。

import { ArrowDown, ArrowUp } from 'lucide-react'
import { usePluginActivityEntries } from '../pages/plugins/usePluginContributions'
import { ProxyControl } from '../pages/downloads/statusbar/ProxyControl'
import { SpeedLimitControl } from '../pages/downloads/statusbar/SpeedLimitControl'
import { DiskFreeCell, PauseAllButton, ResumeAllButton, RetryFailedButton, SpeedCell } from '../pages/downloads/statusbar/StatusWidgets'
import type { WidgetProps } from '../pages/downloads/statusbar/widget'
import { ACTIVITY_ENTRIES } from './activity'
import { AccountItem, PluginItem, RouteItem } from './ActivityItem'

export function ChromeItem({ id, preview = false, ...position }: WidgetProps & { id: string; preview?: boolean }) {
  const pluginEntries = usePluginActivityEntries()
  switch (id) {
    case 'downloadSpeed':
      return <SpeedCell direction="down" {...position} />
    case 'uploadSpeed':
      return <SpeedCell direction="up" {...position} />
    case 'pauseAll':
      return <PauseAllButton {...position} />
    case 'resumeAll':
      return <ResumeAllButton {...position} />
    case 'retryFailed':
      return <RetryFailedButton {...position} />
    case 'downloadLimit':
      return <SpeedLimitControl icon={ArrowDown} titleKey="speedLimitTitle" configKey="speed_limit_bytes" {...position} />
    case 'uploadLimit':
      return <SpeedLimitControl icon={ArrowUp} titleKey="uploadLimit" configKey="upload_limit_bytes" {...position} />
    case 'proxy':
      return <ProxyControl {...position} />
    case 'diskFree':
      return <DiskFreeCell preview={preview} {...position} />
    default: {
      const pluginEntry = pluginEntries.find((item) => item.id === id)
      if (pluginEntry) return <PluginItem entry={pluginEntry} {...position} />
      const entry = ACTIVITY_ENTRIES.find((item) => item.id === id)
      if (!entry) return null
      return entry.id === 'account' ? <AccountItem entry={entry} {...position} /> : <RouteItem entry={entry} {...position} />
    }
  }
}
