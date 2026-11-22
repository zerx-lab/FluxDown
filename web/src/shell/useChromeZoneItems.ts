import { useDaemon } from '../lib/rpc'
import { useVersionBadge } from '../lib/update'
import { ACTIVITY_ENTRIES } from './activity'
import type { ChromeZone } from './chromeLayout'
import { useChromeEdit, useChromeLayout } from './chromeStore'
import { useActivityEntryVisible } from './useVisibleActivityEntries'

const selectDiskKnown = (daemon: { runtimeStats: { diskFreeBytes: number | null } }) => daemon.runtimeStats.diskFreeBytes != null

/**
 * 某区域里当前应显示的条目 id（布局顺序）：
 * - `ui.show_activity_*` 偏好关闭的入口无论放在哪都不显示；
 * - 手动隐藏的条目非编辑态不显示（编辑态保留，用于切换回来）；
 * - 剩余空间 daemon 未上报时不占位（编辑态保留，便于拖动）；
 * - 版本条目被隐藏时，有更新状态（新版本 / 下载中 / 待重启 / 失败）仍临时显示，更新提示不随之丢失。
 */
export function useChromeZoneItems(zone: ChromeZone): readonly string[] {
  const layout = useChromeLayout()
  const { editing } = useChromeEdit()
  const entryVisible = useActivityEntryVisible()
  const diskKnown = useDaemon(selectDiskKnown, false)
  const updateAttention = useVersionBadge() !== null
  return layout[zone].filter((id) => {
    if (!editing && layout.hidden.includes(id) && !(id === 'appVersion' && updateAttention)) return false
    const entry = ACTIVITY_ENTRIES.find((item) => item.id === id)
    if (entry) return entryVisible(entry)
    return id !== 'diskFree' || diskKnown || editing
  })
}
