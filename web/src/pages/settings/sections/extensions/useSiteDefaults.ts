// 站点默认记录的读取：站点默认子页与插件页冲突卡共用。
// 成功读到（或 daemon 不支持插件）才算已加载；其余失败保持上一次列表。

import { useCallback, useEffect, useState } from 'react'
import { RpcError, rpc } from '../../../../lib/rpc'
import type { PluginDto, PluginSiteDefaultDto } from '../../../../lib/rpc'

export function useSiteDefaults(plugins: readonly PluginDto[], active: boolean) {
  const [entries, setEntries] = useState<readonly PluginSiteDefaultDto[]>([])
  const [loaded, setLoaded] = useState(false)

  const refresh = useCallback(async () => {
    try {
      setEntries(await rpc.daemon.plugin.siteDefaults())
      setLoaded(true)
    } catch (error) {
      if (error instanceof RpcError && error.appCode === 'unsupported') {
        setEntries([])
        setLoaded(true)
      }
    }
  }, [])

  // 进入页面 / 插件列表变化时重新拉取。
  useEffect(() => {
    if (active) void refresh()
  }, [refresh, plugins, active])

  return { entries, loaded, refresh }
}
