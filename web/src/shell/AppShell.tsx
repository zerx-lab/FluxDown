// 应用外壳：
//   桌面：[标题栏(40) / [活动栏(48) | 内容区 / 状态栏]] + 右侧全高 AI 助手面板（打开时，左缘可拖动调宽）；
//   移动端：标题栏 / 内容区 / 底部标签栏，AI 助手为全屏浮层。
// 桌面端标题栏、活动栏、状态栏由六个可自定义区域（titleLeading/Trailing、railTop/Bottom、statusLeading/Trailing）组成，
// 布局存 localStorage，右键 → 「自定义布局」进入编辑态拖动重排；状态栏属于外壳，所有路由可见。
// 移动端不可自定义：底部标签栏 + 下载页自带的状态栏，行为不变。
// 内容区背景 surface；活动栏、标题栏、状态栏与（页面自己的）侧栏为 chrome。
// AI 助手不是路由：入口是可布局的 `assistantPanel` 条目（默认状态栏右侧），开关状态见 `assistantPanelStore`；⌘K / Ctrl+K 直接开关（Web 没有命令面板）。

import { Outlet, useLocation } from '@tanstack/react-router'
import { useEffect } from 'react'
import { useT } from '../i18n'
import { useDaemon } from '../lib/rpc'
import type { SelectionRequestDto } from '../lib/rpc'
import { FileConflictDialog } from '../pages/downloads/dialogs/FileConflictDialog'
import { NewDownloadDialog } from '../pages/downloads/dialogs/NewDownloadDialog'
import { SeedLocalDialog } from '../pages/downloads/dialogs/SeedLocalDialog'
import { SelectionHost } from '../pages/downloads/dialogs/SelectionDialogs'
import { useDialogsState } from '../pages/downloads/dialogs/store'
import { isPluginEntryActive } from '../pages/plugins/contributions'
import { usePluginActivityEntries, usePluginShellBridge } from '../pages/plugins/usePluginContributions'
import { useIsMobile } from '../ui'
import { AccountGuards } from './AccountGuards'
import { AssistantPanel } from '../pages/assistant/AssistantPanel'
import { ActivityRail } from './ActivityRail'
import { BottomTabBar } from './BottomTabBar'
import { isAssistantShortcut, toggleAssistantPanel } from './assistantPanelStore'
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

function isMacPlatform(): boolean {
  const platform = (navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData?.platform ?? navigator.platform
  return /mac|iphone|ipad|ipod/i.test(platform)
}

/** ⌘K（macOS）/ Ctrl+K（其它）开关 AI 助手面板；输入框内同样生效，有模态对话框时不响应。 */
function useAssistantShortcut(): void {
  useEffect(() => {
    const mac = isMacPlatform()
    const onKeyDown = (event: KeyboardEvent) => {
      if (!isAssistantShortcut(event, mac) || document.querySelector('[role="dialog"]')) return
      event.preventDefault()
      toggleAssistantPanel()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [])
}

const EMPTY_SELECTIONS: readonly SelectionRequestDto[] = []

/** Keep task creation and its follow-up dialogs available across shell route changes. */
function DownloadCreationHost() {
  const { newDownload, seedLocal } = useDialogsState()
  const selections = useDaemon((daemon) => daemon.pendingSelections, EMPTY_SELECTIONS)
  return (
    <>
      {newDownload ? <NewDownloadDialog key={newDownload.session} session={newDownload} /> : null}
      {seedLocal ? <SeedLocalDialog /> : null}
      <SelectionHost requests={selections} />
      <FileConflictDialog />
    </>
  )
}

export function AppShell() {
  const defaultTitle = useDefaultTitle()
  const mobile = useIsMobile()
  useExitChromeEditing(mobile)
  useAssistantShortcut()
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
      <div className="flex h-dvh w-full overflow-hidden bg-background text-foreground">
        <div className="flex min-w-0 flex-1 flex-col">
          {mobile ? titleBar : <ChromeMenuArea>{titleBar}</ChromeMenuArea>}
          {mobile ? null : <ChromeEditBanner />}
          <ConnectionBanner />
          {mobile ? <UpdateBanner /> : null}
          <div className="flex min-h-0 flex-1">
            {mobile ? null : <ActivityRail />}
            <div className="flex min-w-0 flex-1 flex-col">
              <main className="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden bg-surface">
                <Outlet />
              </main>
              {mobile ? null : <ChromeStatusBar />}
            </div>
          </div>
          <DownloadCreationHost />
          {mobile ? <BottomTabBar /> : null}
          <AccountGuards />
          <PluginNotices />
          <UpdateRestartOverlay />
        </div>
        <AssistantPanel />
      </div>
    </TitleBarSlotProvider>
  )
}
