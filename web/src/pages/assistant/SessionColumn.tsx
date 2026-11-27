// 历史会话视图（GPUI `AssistantView::render_sidebar`）：会话列表（分页加载、原位二次点击删除、清空全部），铺满面板宽度；「新对话」在面板顶栏。

import { Trash2 } from 'lucide-react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import { isAssistantRunActive } from '../../lib/rpc'
import type { AssistantSessionDto } from '../../lib/rpc'
import { Button, Icon, Spinner } from '../../ui'
import type { AssistantController, ViewState } from './controller'
import { SESSION_DATE_CHARS, sessionTitle } from './model'

function SessionRow({
  session,
  selected,
  running,
  confirming,
  stale,
  controller,
  onSelect,
}: {
  session: AssistantSessionDto
  selected: boolean
  running: boolean
  confirming: boolean
  stale: boolean
  controller: AssistantController
  onSelect: (sessionId: string) => void
}) {
  const t = useT()
  const title = sessionTitle(session.title) ?? t('assistantUntitled')
  const date = session.updatedAt.slice(0, SESSION_DATE_CHARS)
  return (
    <div
      onMouseLeave={() => controller.cancelDelete(session.id)}
      className={cn(
        'flex w-full min-w-0 items-center gap-0.5 rounded-[var(--fx-components-nav-item-radius)]',
        selected ? 'bg-nav-selected' : 'hover:bg-nav-hover',
      )}
    >
      <button
        type="button"
        aria-current={selected ? 'true' : undefined}
        title={title}
        onClick={() => onSelect(session.id)}
        className="flex min-h-0 min-w-0 flex-1 flex-col gap-0.5 px-2 py-1.5 text-left coarse:min-h-touch coarse:justify-center"
      >
        <span className={cn('truncate text-sm', selected ? 'font-medium text-nav-selected-foreground' : 'text-foreground')}>{title}</span>
        {confirming ? (
          <span className="truncate text-xs text-destructive">{t('assistantDeleteAgain')}</span>
        ) : (
          <span className="flex min-w-0 items-center gap-1.5 text-xs text-text-tertiary">
            <span className="tabular">{date}</span>
            {session.kind === 'feedback' ? <span className="truncate">{t('assistantFeedbackBadge')}</span> : null}
            {running ? <Spinner size="sm" /> : null}
          </span>
        )}
      </button>
      <Button
        variant="ghost"
        iconOnly
        className={cn('mr-0.5 h-6 w-6 min-w-0', confirming && 'text-destructive hover:bg-destructive/10')}
        aria-label={confirming ? t('assistantDeleteAgain') : t('assistantDelete')}
        title={confirming ? t('assistantDeleteAgain') : t('assistantDelete')}
        disabled={stale}
        onBlur={() => controller.cancelDelete(session.id)}
        onClick={() => (confirming ? controller.deleteSession(session.id) : controller.requestDelete(session.id))}
      >
        <Icon icon={Trash2} size="sm" />
      </Button>
    </div>
  )
}

export function SessionColumn({
  view,
  controller,
  open,
  onSelect,
}: {
  view: ViewState
  controller: AssistantController
  /** 门禁开放（未开放时列表为空，不能新建对话）。 */
  open: boolean
  onSelect: (sessionId: string) => void
}) {
  const t = useT()
  const { sessions, env } = view
  const stale = env.stale
  const runningSessions = new Set(env.state.runs.filter((run) => isAssistantRunActive(run.status)).map((run) => run.sessionId))
  const empty = sessions.items.length === 0

  let body = null
  if (open) {
    if ((sessions.load === 'idle' || sessions.load === 'loading') && empty) {
      body = (
        <div className="flex items-center gap-2 p-3 text-xs text-text-tertiary">
          <Spinner size="sm" />
          {t('assistantSessionsLoading')}
        </div>
      )
    } else if (sessions.load === 'failed' && empty) {
      body = (
        <div className="flex flex-col items-start gap-2 p-3">
          <span className="text-xs text-text-tertiary">{t('assistantSessionsLoadFailed')}</span>
          <Button variant="outline" disabled={stale} onClick={() => controller.reloadSessions()}>
            {t('assistantRetry')}
          </Button>
        </div>
      )
    } else {
      body = (
        <>
          {empty ? <div className="p-3 text-xs text-text-tertiary">{t('assistantSessionsEmpty')}</div> : null}
          {sessions.items.map((session) => (
            <SessionRow
              key={session.id}
              session={session}
              selected={view.current === session.id}
              running={runningSessions.has(session.id)}
              confirming={view.deletePending === session.id}
              stale={stale}
              controller={controller}
              onSelect={onSelect}
            />
          ))}
          {sessions.items.length < sessions.total ? (
            <Button variant="ghost" loading={sessions.moreLoading} disabled={stale || sessions.moreLoading} onClick={() => controller.loadMoreSessions()}>
              {t('assistantLoadMore')}
            </Button>
          ) : null}
        </>
      )
    }
  }

  return (
    <div className="flex h-full min-h-0 w-full flex-col bg-chrome">
      <div className="flex shrink-0 flex-col gap-2 p-3">
        <div className="flex min-w-0 items-center justify-between gap-2">
          <span className="min-w-0 truncate text-xs font-medium text-text-tertiary">{t('assistantSessionsTitle')}</span>
          {open && (!empty || view.clearing) ? (
            <Button
              variant="ghost"
              className={cn(
                '-mr-1 h-5 px-1.5 text-xs',
                view.clearPending ? 'text-destructive hover:bg-destructive/10' : 'text-text-tertiary hover:text-foreground',
              )}
              disabled={stale || view.clearing}
              onMouseLeave={() => controller.cancelClear()}
              onBlur={() => controller.cancelClear()}
              onClick={() => (view.clearPending ? controller.clearSessions() : controller.requestClear())}
            >
              {view.clearing ? t('assistantClearingSessions') : view.clearPending ? t('assistantClearSessionsAgain') : t('assistantClearSessions')}
            </Button>
          ) : null}
        </div>
      </div>
      <div className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto px-2 pb-2">{body}</div>
    </div>
  )
}
