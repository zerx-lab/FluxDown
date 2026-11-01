// 活动入口注册表：路由 / 账户入口的 id、文案键、图标与可选入口偏好键；数组顺序也是移动端底部标签栏的顺序。
// 桌面端条目放在标题栏 / 活动栏 / 状态栏的哪个区域由 chromeCatalog（默认区域与可放置的族）与用户布局决定，不在此处；
// 可选入口偏好缺省视为显示。

import { BellRing, CircleUser, Download, Rss, Settings } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'

/** 内置入口 id 之外还有插件活动页（`plugin:<identity>:<viewId>`），故放宽为 string。 */
export type ActivityId = string

export interface ActivityEntry {
  id: ActivityId
  labelKey: string
  icon: LucideIcon
  /** 路由条目的目标路径；账户条目由 `AccountAvatar` 自绘并链到设置的账户分类。 */
  to?: '/' | '/rss' | '/webhooks' | '/settings'
  /** 可选入口的可见性偏好键；undefined = 固定显示。偏好关闭后无论条目被摆在哪个区域都不显示。 */
  prefKey?: string
}

export const ACTIVITY_ENTRIES: readonly ActivityEntry[] = [
  { id: 'downloads', labelKey: 'mobileNavDownloads', icon: Download, to: '/' },
  { id: 'rss', labelKey: 'sidebarRss', icon: Rss, to: '/rss', prefKey: 'ui.show_activity_rss' },
  { id: 'webhooks', labelKey: 'pushNavTitle', icon: BellRing, to: '/webhooks', prefKey: 'ui.show_activity_webhooks' },
  { id: 'account', labelKey: 'settingsCatAccount', icon: CircleUser, prefKey: 'ui.show_activity_account' },
  { id: 'settings', labelKey: 'settings', icon: Settings, to: '/settings' },
]

/** 账户入口所在路径（设置的账户分类）。 */
export const ACCOUNT_PATH = '/settings/account'

/** 当前路径是否命中该条目（Downloads 精确匹配 `/`，账户匹配账户分类，其余前缀匹配；账户分类不再点亮设置）。 */
export function isActivityActive(entry: ActivityEntry, pathname: string): boolean {
  if (entry.id === 'account') return pathname === ACCOUNT_PATH
  if (!entry.to) return false
  if (entry.to === '/') return pathname === '/'
  if (pathname === ACCOUNT_PATH) return false
  return pathname === entry.to || pathname.startsWith(`${entry.to}/`)
}
