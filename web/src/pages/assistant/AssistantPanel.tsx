// AI 助手面板（GPUI `crates/assistant`）：窗口右侧全高面板（桌面，左缘可拖动调宽）/ 全屏浮层（移动端）。
// 顶栏与标题栏同高：当前标题（可改名）、每日用量、改名 / 导出 / 新对话 / 历史会话 / 收起；主体是对话视图或历史会话视图。
// 门禁（未登录 / 加载中 / 未开放 / 套餐不含 / 服务异常）显示居中的说明页，门禁开放才显示对话与历史。
// 面板首次打开后一直挂载（当前会话、草稿、滚动位置、chat / history 视图都保留）；收起时 `invisible` + `inert` 且脱离布局（`fixed`），
// 不用 `display:none`（会丢滚动位置与输入框高度）。开关状态见 `shell/assistantPanelStore`。
// 移动端是模态对话框：`role="dialog"`、焦点留在面板内（Tab 循环）、Esc / 系统返回键关闭，关闭后焦点还给入口按钮。

import { CircleAlert, Clock, Crown, Pencil, PanelRight, Plus, Sparkles, User } from 'lucide-react'
import { useEffect, useRef, useState, useSyncExternalStore } from 'react'
import type { KeyboardEvent, PointerEvent, ReactNode } from 'react'
import { useBlocker, useNavigate } from '@tanstack/react-router'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import {
  PANEL_MAIN_MIN_WIDTH,
  PANEL_MAX_WIDTH,
  PANEL_MIN_WIDTH,
  clampPanelWidth,
  closeAssistantPanel,
  setAssistantPanelWidth,
  showAssistantChat,
  showAssistantHistory,
  useAssistantPanel,
} from '../../shell/assistantPanelStore'
import { ASSISTANT_PANEL_TRIGGER_ATTR, BUTTON_ACTIVE } from '../../shell/ActivityItem'
import { Button, EmptyState, Icon, Spinner, Tooltip, useIsMobile } from '../../ui'
import { ChatPane } from './ChatPane'
import { EMPTY_COMPOSER } from './composer'
import type { ComposerState } from './composer'
import { currentSession, gate, sessionTitle } from './model'
import type { Gate } from './model'
import { ExportMenu, RenameField } from './SessionActions'
import { SessionColumn } from './SessionColumn'
import { useAssistant } from './useAssistant'

/** 键盘调宽步长（px）。 */
const KEYBOARD_STEP = 16

const PANEL_ID = 'assistant-panel'

const FOCUSABLE = 'a[href],button:not([disabled]),textarea:not([disabled]),input:not([disabled]):not([type=hidden]),select:not([disabled]),[tabindex]:not([tabindex="-1"])'

function subscribeViewport(listener: () => void): () => void {
  window.addEventListener('resize', listener)
  return () => window.removeEventListener('resize', listener)
}

/** 窗口宽度（拖动条的 `aria-valuemax` 随窗口收窄）。 */
function useViewportWidth(): number {
  return useSyncExternalStore(subscribeViewport, () => window.innerWidth, () => PANEL_MAX_WIDTH + PANEL_MAIN_MIN_WIDTH)
}

/** 左缘拖动条：指针拖动 / 左右方向键调宽（向左变宽）；窗口宽度参与上限，保证主区至少 360px。 */
function PanelResizer({ width, label }: { width: number; label: string }) {
  const viewport = useViewportWidth()
  const drag = useRef<{ startX: number; startWidth: number } | null>(null)
  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    drag.current = { startX: event.clientX, startWidth: width }
    event.currentTarget.setPointerCapture(event.pointerId)
  }
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const state = drag.current
    if (state) setAssistantPanelWidth(state.startWidth + state.startX - event.clientX, window.innerWidth)
  }
  const onPointerUp = () => {
    drag.current = null
  }
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return
    event.preventDefault()
    setAssistantPanelWidth(width + (event.key === 'ArrowLeft' ? KEYBOARD_STEP : -KEYBOARD_STEP), window.innerWidth)
  }
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-controls={PANEL_ID}
      aria-valuemin={PANEL_MIN_WIDTH}
      aria-valuemax={clampPanelWidth(PANEL_MAX_WIDTH, viewport)}
      aria-valuenow={width}
      tabIndex={0}
      className="group absolute inset-y-0 -left-1 z-20 w-2 cursor-col-resize touch-none outline-none"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onKeyDown={onKeyDown}
    >
      <div className="mx-auto h-full w-px bg-transparent transition-colors group-hover:bg-primary/60 group-focus-visible:bg-primary group-active:bg-primary" />
    </div>
  )
}

function HeaderButton({
  label,
  icon,
  pressed,
  disabled,
  onClick,
}: {
  label: string
  icon: typeof Plus
  pressed?: boolean
  disabled?: boolean
  onClick: () => void
}) {
  return (
    <Tooltip content={label} side="bottom">
      <Button
        variant="ghost"
        iconOnly
        aria-label={label}
        aria-pressed={pressed}
        disabled={disabled}
        className={cn('shrink-0', pressed && BUTTON_ACTIVE)}
        onClick={onClick}
      >
        <Icon icon={icon} />
      </Button>
    </Tooltip>
  )
}

function PanelBody({ mobile, active }: { mobile: boolean; active: boolean }) {
  const t = useT()
  const navigate = useNavigate()
  const { view: panelView } = useAssistantPanel()
  const { controller, view, ready, now } = useAssistant()
  const [composer, setComposer] = useState<ComposerState>(EMPTY_COMPOSER)
  const [renamingId, setRenamingId] = useState<string | null>(null)
  const bodyRef = useRef<HTMLDivElement>(null)

  const { env } = view
  // 快照到手之前登录态未知：先显示加载，避免闪出「请登录」。
  const current: Gate = ready ? gate(env.signedIn, env.state, view.configFailed) : { kind: 'loading' }
  const open = current.kind === 'open'
  // 门禁没开放时没有历史可看，始终显示说明页。
  const showHistory = open && panelView === 'history'

  // 桌面：每次打开 / 切回对话视图 / 门禁转为开放时显式聚焦输入框（`autoFocus` 只在输入框挂载时生效）。
  useEffect(() => {
    if (!active || mobile || !open || showHistory) return
    bodyRef.current?.querySelector<HTMLTextAreaElement>('textarea[data-assistant-composer]')?.focus()
  }, [active, mobile, open, showHistory])

  // 已登录且面板可见时：拉取配置，门禁开放后拉取会话列表（幂等）。
  useEffect(() => {
    if (ready) controller.ensureLoaded()
  }, [controller, ready, env.signedIn, env.stale, env.state.config, view.configFailed, view.sessions.load])

  // 账号相关内容被清空（登出）：草稿不再属于任何会话，附图一并丢弃。
  useEffect(() => {
    if (!env.signedIn) setComposer(EMPTY_COMPOSER)
  }, [env.signedIn])

  const goAccount = () => {
    void navigate({ to: '/settings/$category', params: { category: 'account' } })
    // 移动端面板是全屏浮层，跳转后要收起才看得到设置页。
    if (mobile) closeAssistantPanel()
  }

  const selectSession = (sessionId: string) => {
    controller.clearFailure()
    controller.selectSession(sessionId)
    showAssistantChat()
  }
  const newChat = () => {
    controller.clearFailure()
    controller.newChat()
    showAssistantChat()
  }
  const toggleHistory = () => {
    controller.clearFailure()
    if (showHistory) showAssistantChat()
    else showAssistantHistory()
  }

  const session = currentSession(view.current, view.detail.session, view.sessions.items)
  const title = session ? sessionTitle(session.title) : null
  const heading = showHistory ? t('assistantHistory') : view.current === null ? t('assistantNewChat') : (title ?? t('assistantUntitled'))
  const config = env.state.config
  const usage = open && config && config.dailyLimit > 0 ? t('assistantUsage', { used: config.usedToday, limit: config.dailyLimit }) : null
  // 当前会话可改名 / 导出（已有云端会话的对话视图）。
  const manageable = open && !showHistory && view.current !== null
  const renaming = manageable && renamingId === view.current

  let main: ReactNode
  switch (current.kind) {
    case 'signedOut':
      main = (
        <EmptyState
          className="h-full"
          icon={User}
          title={t('assistantSignedOutTitle')}
          description={t('assistantSignedOutDesc')}
          action={
            <Button variant="primary" onClick={goAccount}>
              {t('assistantSignIn')}
            </Button>
          }
        />
      )
      break
    case 'loading':
      main = (
        <div role="status" className="flex h-full items-center justify-center gap-2 text-xs text-text-tertiary">
          <Spinner size="md" />
          {t('assistantLoading')}
        </div>
      )
      break
    case 'notOpen':
      main = <EmptyState className="h-full" icon={Sparkles} title={t('assistantUnavailableTitle')} description={t('assistantUnavailableDesc')} />
      break
    case 'planDisabled':
      main = (
        <EmptyState
          className="h-full"
          icon={Crown}
          title={t('assistantPlanTitle')}
          description={t('assistantPlanDesc')}
          action={
            <Button variant="primary" onClick={goAccount}>
              {t('assistantViewPlans')}
            </Button>
          }
        />
      )
      break
    case 'serviceError':
      main = (
        <EmptyState
          className="h-full"
          icon={CircleAlert}
          title={t('assistantServiceTitle')}
          description={t('assistantServiceDesc')}
          action={
            <Button variant="primary" loading={view.configLoading} disabled={env.stale || view.configLoading} onClick={() => controller.fetchConfig(true)}>
              {t('assistantRetry')}
            </Button>
          }
        />
      )
      break
    case 'open':
      main = showHistory ? (
        <SessionColumn view={view} controller={controller} open={open} onSelect={selectSession} />
      ) : (
        <ChatPane
          view={view}
          controller={controller}
          now={now}
          quotaExhausted={current.quotaExhausted}
          composer={composer}
          onComposerChange={setComposer}
          autoFocusComposer={!mobile}
          onOpenSession={selectSession}
        />
      )
      break
  }

  return (
    <>
      <header
        className={cn(
          'flex h-title-bar shrink-0 items-center gap-1 border-b border-hairline bg-chrome box-content pt-safe pl-3 pr-2',
          mobile && 'pl-[max(0.75rem,env(safe-area-inset-left))] pr-[max(0.5rem,env(safe-area-inset-right))]',
        )}
      >
        {renaming && view.current !== null ? (
          <RenameField
            sessionId={view.current}
            initial={title ?? ''}
            controller={controller}
            stale={env.stale}
            onDone={() => setRenamingId(null)}
          />
        ) : (
          <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground" title={heading}>
            {heading}
          </span>
        )}
        {usage !== null ? <span className="shrink-0 pr-1 text-xs text-text-tertiary tabular">{usage}</span> : null}
        {manageable && view.current !== null ? (
          <>
            <HeaderButton label={t('assistantRename')} icon={Pencil} disabled={env.stale || renaming} onClick={() => setRenamingId(view.current)} />
            <ExportMenu sessionId={view.current} view={view} controller={controller} iconSize="md" className="shrink-0" />
          </>
        ) : null}
        <HeaderButton label={t('assistantNewChat')} icon={Plus} disabled={!open || env.stale} onClick={newChat} />
        <HeaderButton label={t('assistantHistory')} icon={Clock} pressed={showHistory} disabled={!open} onClick={toggleHistory} />
        <HeaderButton label={t('assistantPanelClose')} icon={PanelRight} onClick={closeAssistantPanel} />
      </header>
      <div ref={bodyRef} className={cn('min-h-0 min-w-0 flex-1', mobile && 'pb-safe')}>
        {main}
      </div>
    </>
  )
}

/** Tab 在面板内循环（移动端模态对话框的焦点陷阱）。 */
function trapTab(event: KeyboardEvent<HTMLElement>): void {
  if (event.key !== 'Tab') return
  const items = [...event.currentTarget.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((element) => element.getClientRects().length > 0)
  const first = items[0]
  const last = items[items.length - 1]
  if (first === undefined || last === undefined) {
    event.preventDefault()
    return
  }
  const active = document.activeElement
  if (event.shiftKey && (active === first || active === event.currentTarget)) {
    event.preventDefault()
    last.focus()
  } else if (!event.shiftKey && active === last) {
    event.preventDefault()
    first.focus()
  }
}

export function AssistantPanel() {
  const t = useT()
  const mobile = useIsMobile()
  const { open, width } = useAssistantPanel()
  const asideRef = useRef<HTMLElement>(null)
  const trigger = useRef<HTMLElement | null>(null)
  const wasOpen = useRef(false)
  // 首次打开才挂载，之后常驻。
  const [mounted, setMounted] = useState(open)
  if (open && !mounted) setMounted(true)
  const modal = mobile && open

  // 系统返回键（浏览器后退）先关面板，不切换底下的路由。
  useBlocker({
    shouldBlockFn: ({ action }) => {
      if (action !== 'BACK') return false
      closeAssistantPanel()
      return true
    },
    enableBeforeUnload: false,
    disabled: !modal,
  })

  // 打开时记住入口（通常是触发按钮），关闭后把焦点还回去；`inert` 会把面板内的焦点丢到 body。
  // 移动端模态对话框打开后焦点进入面板。
  useEffect(() => {
    if (open === wasOpen.current) return
    wasOpen.current = open
    const aside = asideRef.current
    if (open) {
      const active = document.activeElement
      trigger.current = active instanceof HTMLElement && active !== document.body && !aside?.contains(active) ? active : null
      if (mobile) aside?.focus()
      return
    }
    const active = document.activeElement
    if (active !== null && active !== document.body && !aside?.contains(active)) return
    const target = trigger.current?.isConnected ? trigger.current : document.querySelector<HTMLElement>(`[${ASSISTANT_PANEL_TRIGGER_ATTR}]`)
    target?.focus()
  }, [open, mobile, mounted])

  if (!mounted) return null

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (!modal) return
    // 菜单 / 弹层经 portal 渲染在面板之外，它们自己处理 Esc。
    if (event.key === 'Escape' && !event.defaultPrevented && event.currentTarget.contains(event.target as Node)) {
      event.preventDefault()
      closeAssistantPanel()
      return
    }
    trapTab(event)
  }

  return (
    <aside
      ref={asideRef}
      id={PANEL_ID}
      aria-label={t('assistantNavTitle')}
      role={modal ? 'dialog' : undefined}
      aria-modal={modal ? true : undefined}
      tabIndex={modal ? -1 : undefined}
      inert={!open}
      onKeyDown={onKeyDown}
      className={cn(
        'flex min-w-0 flex-col bg-surface text-foreground outline-none',
        mobile ? 'fixed inset-0 z-30 h-dvh w-full' : 'relative h-full shrink-0 border-l border-hairline',
        // 收起：不可见、不可交互、不占布局（桌面脱离文档流，移动端本就是浮层），但保留布局与滚动位置。
        !open && 'invisible',
        !open && !mobile && 'fixed inset-y-0 right-0',
      )}
      // 窗口缩小时主区仍至少保留 360px（拖动时另按窗口宽度夹取，这里兜底）。
      style={mobile ? undefined : { width, maxWidth: `calc(100% - ${PANEL_MAIN_MIN_WIDTH}px)` }}
    >
      {mobile ? null : <PanelResizer width={width} label={t('assistantPanelResize')} />}
      <PanelBody mobile={mobile} active={open} />
    </aside>
  )
}
