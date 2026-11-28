// 插件活动页 `/plugins/$pluginId/$viewId`：页头（插件名 + 视图标题 + palette 命令按钮）+ 可移植视图。
// 只有桌面实现的视图不在 Web 出现，直接访问其地址按「不存在」处理。

import { useParams } from '@tanstack/react-router'
import { MonitorSmartphone } from 'lucide-react'
import { useState } from 'react'
import { useT } from '../../i18n'
import type { PluginCommandDeclDto } from '../../lib/rpc'
import { Button, EmptyState } from '../../ui'
import { runPluginCommand } from './commands'
import { derivePaletteCommands } from './contributions'
import { usePlugins } from './usePluginContributions'
import { PluginViewHost } from './view/PluginViewHost'

function CommandButton({ identity, command }: { identity: string; command: PluginCommandDeclDto }) {
  const t = useT()
  const [busy, setBusy] = useState(false)
  return (
    <Button
      variant="outline"
      loading={busy}
      onClick={() => {
        setBusy(true)
        void runPluginCommand(t, identity, command.id).finally(() => setBusy(false))
      }}
    >
      {command.title}
    </Button>
  )
}

export function PluginViewPage() {
  const t = useT()
  const { pluginId, viewId } = useParams({ strict: false }) as { pluginId?: string; viewId?: string }
  const plugins = usePlugins()
  const plugin = plugins.find((item) => item.identity === pluginId)
  const view = plugin?.contributions.views.find((item) => item.id === viewId && item.portable)

  if (!plugin || !view) {
    return <EmptyState icon={MonitorSmartphone} title={t('pluginViewClosed')} className="flex-1" />
  }
  const commands = derivePaletteCommands(plugin)
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex shrink-0 items-center gap-3 border-b border-hairline px-4 py-2">
        <div className="min-w-0 flex-1">
          <div className="truncate text-xs text-muted-foreground">{plugin.name}</div>
          <h1 className="truncate text-title font-semibold text-foreground">{view.title}</h1>
        </div>
        {commands.map((command) => (
          <CommandButton key={command.id} identity={plugin.identity} command={command} />
        ))}
      </header>
      <div className="view-viewport min-h-0 flex-1 overflow-auto p-4">
        <PluginViewHost key={`${plugin.identity}/${view.id}`} identity={plugin.identity} view={view.id} />
      </div>
    </div>
  )
}
