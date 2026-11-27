// 活动入口按钮（路由 / 动作 / 账户）：可放在活动栏（纯图标 + 右侧气泡）或标题栏 / 状态栏（紧凑图标按钮）。
// 选中项：中性底 `navSelected` + 图标 `navSelectedIcon`（强调色只落在图标上）。

import { Link, useLocation } from '@tanstack/react-router'
import { useT } from '../i18n'
import { isPluginEntryActive } from '../pages/plugins/contributions'
import type { PluginActivityEntry } from '../pages/plugins/contributions'
import { cn } from '../lib/cn'
import { ACTION_BUTTON, RAIL_BUTTON, resolveSide } from '../pages/downloads/statusbar/widget'
import type { WidgetProps } from '../pages/downloads/statusbar/widget'
import { Icon, MaskIcon, Tooltip } from '../ui'
import { AccountAvatar } from './AccountAvatar'
import { useAccountAvatar } from './accountAvatarState'
import { AccountMenu } from './AccountMenu'
import { isActivityActive } from './activity'
import type { ActivityEntry } from './activity'
import { toggleAssistantPanel, useAssistantPanel } from './assistantPanelStore'

/** 选中项悬停不变色（GPUI `activity_button`：选中态 hover 与常态一致）。 */
export const BUTTON_ACTIVE = 'bg-nav-selected text-nav-selected-foreground hover:bg-nav-selected hover:text-nav-selected-foreground'

export function RouteItem({ entry, orientation = 'horizontal', side }: WidgetProps & { entry: ActivityEntry }) {
  const t = useT()
  const { pathname } = useLocation()
  const active = isActivityActive(entry, pathname)
  const rail = orientation === 'rail'
  const label = t(entry.labelKey)
  return (
    <Tooltip content={label} side={resolveSide(orientation, side)}>
      <Link
        to={entry.to ?? '/'}
        aria-label={label}
        aria-current={active ? 'page' : undefined}
        className={cn(rail ? RAIL_BUTTON : ACTION_BUTTON, active && BUTTON_ACTIVE)}
      >
        <Icon icon={entry.icon} size={rail ? 'xl' : 'lg'} className={cn(active && 'text-nav-selected-icon')} />
      </Link>
    </Tooltip>
  )
}

/** AI 助手入口：不是路由，点击开关窗口右侧面板；面板打开时呈选中态。 */
export function AssistantPanelItem({ entry, orientation = 'horizontal', side }: WidgetProps & { entry: ActivityEntry }) {
  const t = useT()
  const { open } = useAssistantPanel()
  const rail = orientation === 'rail'
  const label = t(entry.labelKey)
  return (
    <Tooltip content={label} side={resolveSide(orientation, side)}>
      <button
        type="button"
        aria-label={label}
        aria-pressed={open}
        className={cn(rail ? RAIL_BUTTON : ACTION_BUTTON, open && BUTTON_ACTIVE)}
        onClick={toggleAssistantPanel}
      >
        <Icon icon={entry.icon} size={rail ? 'xl' : 'lg'} className={cn(open && 'text-nav-selected-icon')} />
      </button>
    </Tooltip>
  )
}

/** 插件活动页入口：标签是插件视图标题原文，图标为插件声明的 SVG（未声明时 Sparkles）。 */
export function PluginItem({ entry, orientation = 'horizontal', side }: WidgetProps & { entry: PluginActivityEntry }) {
  const { pathname } = useLocation()
  const active = isPluginEntryActive(entry, pathname)
  const rail = orientation === 'rail'
  return (
    <Tooltip content={entry.label} side={resolveSide(orientation, side)}>
      <Link
        to="/plugins/$pluginId/$viewId"
        params={{ pluginId: entry.identity, viewId: entry.viewId }}
        aria-label={entry.label}
        aria-current={active ? 'page' : undefined}
        className={cn(rail ? RAIL_BUTTON : ACTION_BUTTON, active && BUTTON_ACTIVE)}
      >
        {entry.iconMask === null ? (
          <Icon icon={entry.icon} size={rail ? 'xl' : 'lg'} className={cn(active && 'text-nav-selected-icon')} />
        ) : (
          <MaskIcon mask={entry.iconMask} size={rail ? 'xl' : 'lg'} className={cn(active && 'text-nav-selected-icon')} />
        )}
      </Link>
    </Tooltip>
  )
}

export function AccountItem({ entry, orientation = 'horizontal', side }: WidgetProps & { entry: ActivityEntry }) {
  const t = useT()
  const { pathname } = useLocation()
  const account = useAccountAvatar()
  const active = isActivityActive(entry, pathname)
  const label = account.label ?? t('accountLogin')
  return (
    <AccountMenu
      avatar={account}
      side={orientation === 'rail' ? 'right' : 'bottom'}
      trigger={(open) => (
        <Tooltip content={label} side={resolveSide(orientation, side)}>
          {/* 头像 24px，两种朝向都用 32px 方钮（账户只允许放活动栏 / 标题栏，40px 标题栏放得下）。 */}
          <button type="button" aria-label={label} className={cn(RAIL_BUTTON, (active || open) && BUTTON_ACTIVE)}>
            <AccountAvatar state={account} />
          </button>
        </Tooltip>
      )}
    />
  )
}
