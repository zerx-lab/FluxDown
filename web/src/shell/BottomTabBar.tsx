// 移动端底部标签栏（<=820px）：替代活动栏。44px 触摸目标、图标 + caption 文字标签，
// 底部避开 Home 指示条（safe-area-inset-bottom）。条目与桌面活动栏同源（activity.ts）。

import { Link, useLocation } from '@tanstack/react-router'
import type { ReactNode } from 'react'
import { useT } from '../i18n'
import { cn } from '../lib/cn'
import { Icon } from '../ui'
import { AccountAvatar } from './AccountAvatar'
import { useAccountAvatar } from './accountAvatarState'
import { AccountMenu } from './AccountMenu'
import { isActivityActive } from './activity'
import type { ActivityEntry } from './activity'
import { openAssistantPanel } from './assistantPanelStore'
import { useVisibleActivityEntries } from './useVisibleActivityEntries'

/** 标签栏内容高度（不含安全区）。与 index.css 的 `--fx-bottom-bar` 保持一致。 */
export const BOTTOM_BAR_HEIGHT = 56

const TAB = 'flex min-h-touch min-w-touch flex-1 flex-col items-center justify-center gap-0.5 px-1 active:bg-nav-hover'

export function BottomTabBar() {
  const t = useT()
  const location = useLocation()
  const account = useAccountAvatar()
  const entries = useVisibleActivityEntries()

  const renderEntry = (entry: ActivityEntry) => {
    const active = isActivityActive(entry, location.pathname)
    const content = (glyph: ReactNode, label: string, highlighted: boolean) => (
      <>
        {glyph}
        <span className={cn('max-w-full truncate text-caption', highlighted ? 'font-medium text-foreground' : 'text-muted-foreground')}>
          {label}
        </span>
      </>
    )
    if (entry.id === 'account') {
      // 标签只放短标题；完整的昵称 · 徽标文字走无障碍标签。点击在底部 Sheet 展开账户卡片。
      return (
        <div key={entry.id} className="flex flex-1 [&>div]:flex-1">
          <AccountMenu
            avatar={account}
            trigger={(open) => (
              <button type="button" aria-label={account.label ?? t('accountLogin')} className={cn(TAB, 'w-full')}>
                {content(
                  <span className="flex h-6 items-center"><AccountAvatar state={account} /></span>,
                  t(account.label === null ? 'accountLogin' : entry.labelKey),
                  active || open,
                )}
              </button>
            )}
          />
        </div>
      )
    }
    if (entry.id === 'assistantPanel') {
      // 动作条目：打开全屏 AI 助手浮层（浮层盖住标签栏，关闭按钮返回）。
      return (
        <button key={entry.id} type="button" className={TAB} onClick={openAssistantPanel}>
          {content(<Icon icon={entry.icon} size="xl" />, t(entry.labelKey), false)}
        </button>
      )
    }
    return (
      <Link key={entry.id} to={entry.to ?? '/'} aria-current={active ? 'page' : undefined} className={TAB}>
        {content(<Icon icon={entry.icon} size="xl" className={cn(active && 'text-nav-selected-icon')} />, t(entry.labelKey), active)}
      </Link>
    )
  }

  return (
    <nav
      aria-label="Activity"
      className="flex shrink-0 items-stretch border-t border-hairline bg-chrome pb-safe pl-safe pr-safe desktop:hidden"
      style={{ minHeight: BOTTOM_BAR_HEIGHT }}
    >
      {entries.map(renderEntry)}
    </nav>
  )
}
