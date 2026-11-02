// 应用外壳：
//   桌面：标题栏(40) / [活动栏(48) | 内容区 / 状态栏] ；移动端：标题栏 / 内容区 / 底部标签栏。
// 桌面端标题栏、活动栏、状态栏由六个可自定义区域（titleLeading/Trailing、railTop/Bottom、statusLeading/Trailing）组成，
// 布局存 localStorage，右键 → 「自定义布局」进入编辑态拖动重排；状态栏属于外壳，所有路由可见。
// 移动端不可自定义：底部标签栏 + 下载页自带的状态栏，行为不变。
// 内容区背景 surface；活动栏、标题栏、状态栏与（页面自己的）侧栏为 chrome。

import { Outlet, useLocation } from '@tanstack/react-router'
import { useEffect } from 'react'
import { useT } from '../i18n'
import { isPluginEntryActive } from '../pages/plugins/contributions'
import { usePluginActivityEntries, usePluginShellBridge } from '../pages/plugins/usePluginContributions'
import { useIsMobile } from '../ui'
import { AccountGuards } from './AccountGuards'
import { ActivityRail } from './ActivityRail'
import { BottomTabBar } from './BottomTabBar'
import { ChromeEditBanner, ChromeMenuArea } from './ChromeEdit'
import { ChromeStatusBar } from './ChromeStatusBar'
import { setChromeEditing, useChromeEdit } from './chromeStore'
import { ChromeZoneView } from './ChromeZone'
import { ConnectionBanner, ConnectionDot } from './ConnectionStatus'
import { TitleBar, TitleBarSlotProvider } from './TitleBar'
import { PluginNotices } from './PluginNotices'
import { UpdateBanner, UpdateRestartOverlay } from './UpdateStatus'

const TITLE_ZONE = 'flex shrink-0 items-center gap-1'

function useDefaultTitle(): string {
  const t = useT()
  const { pathname } = useLocation()
  const pluginEntries = usePluginActivityEntries()
  if (pathname.startsWith('/plugins/')) {
    const entry = pluginEntries.find((item) => isPluginEntryActive(item, pathname))
    if (entry) return entry.label
  }
  if (pathname.startsWith('/rss')) return t('sidebarRss')
  if (pathname.startsWith('/webhooks')) return t('pushNavTitle')
  if (pathname.startsWith('/settings')) return t('settings')
  return t('mobileNavDownloads')
}

/** 编辑态：Esc 退出；窗口缩到移动端宽度（不可自定义）时自动退出。 */
function useExitChromeEditing(mobile: boolean): void {
  const { editing } = useChromeEdit()
  useEffect(() => {
    if (!editing) return
    if (mobile) {
      setChromeEditing(false)
      return
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !document.querySelector('[role="dialog"]')) setChromeEditing(false)
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [editing, mobile])
}

export function AppShell() {
  const defaultTitle = useDefaultTitle()
  const mobile = useIsMobile()
  useExitChromeEditing(mobile)
  usePluginShellBridge()
  const titleBar = (
    <TitleBar
      defaultTitle={defaultTitle}
      leading={mobile ? null : <ChromeZoneView zone="titleLeading" className={TITLE_ZONE} />}
      trailing={
        <>
          {mobile ? null : <ChromeZoneView zone="titleTrailing" className={TITLE_ZONE} />}
          <ConnectionDot />
        </>
      }
    />
  )
  return (
    <TitleBarSlotProvider>
      <div className="flex h-dvh w-full flex-col overflow-hidden bg-background text-foreground">
        {mobile ? titleBar : <ChromeMenuArea>{titleBar}</ChromeMenuArea>}
        {mobile ? null : <ChromeEditBanner />}
        <ConnectionBanner />
        <UpdateBanner />
        <div className="flex min-h-0 flex-1">
          {mobile ? null : <ActivityRail />}
          <div className="flex min-w-0 flex-1 flex-col">
            <main className="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden bg-surface">
              <Outlet />
            </main>
            {mobile ? null : <ChromeStatusBar />}
          </div>
        </div>
        {mobile ? <BottomTabBar /> : null}
        <AccountGuards />
        <PluginNotices />
        <UpdateRestartOverlay />
      </div>
    </TitleBarSlotProvider>
  )
}
