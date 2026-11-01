import { usePrefBool } from '../lib/rpc/hooks'
import { ACTIVITY_ENTRIES } from './activity'
import type { ActivityEntry } from './activity'

/** 条目可见性判定（偏好缺省 = 显示；无偏好键的条目固定显示）。桌面外壳无论条目放在哪个区域都按它过滤。 */
export function useActivityEntryVisible(): (entry: ActivityEntry) => boolean {
  const rss = usePrefBool('ui.show_activity_rss', true)
  const webhooks = usePrefBool('ui.show_activity_webhooks', true)
  const account = usePrefBool('ui.show_activity_account', true)
  const visible: Record<string, boolean> = {
    'ui.show_activity_rss': rss,
    'ui.show_activity_webhooks': webhooks,
    'ui.show_activity_account': account,
  }
  return (entry) => entry.prefKey === undefined || visible[entry.prefKey] !== false
}

/** 各可选入口的可见性（移动端底部标签栏用；桌面外壳走布局区域）。 */
export function useVisibleActivityEntries(): ActivityEntry[] {
  const visible = useActivityEntryVisible()
  return ACTIVITY_ENTRIES.filter(visible)
}
