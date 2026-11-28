// 历史会话视图（GPUI `AssistantView::render_sidebar`）：搜索、会话列表（分页加载、改名、导出、原位二次点击删除、
// 清空全部），铺满面板宽度；「新对话」在面板顶栏。删除 / 清空 / 加载更多 / 改名 / 导出失败的提示显示在本视图顶部
// （历史视图没有输入区）。

import { Pencil, Search, Trash2, X } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import { isAssistantRunActive } from '../../lib/rpc'
import type { AssistantSessionDto } from '../../lib/rpc'
import { Button, FieldError, Icon, Input, Spinner } from '../../ui'
import type { AssistantController, ViewState } from './controller'
import { ACTION_FAILURE_KEY, SESSION_DATE_CHARS, sessionTitle } from './model'
import { ExportMenu, RenameField } from './SessionActions'

/** 搜索输入停顿多久后才发请求。 */
const SEARCH_DEBOUNCE_MS = 300

const ROW_BUTTON = 'h-6 w-6 min-w-0 shrink-0 text-text-tertiary hover:text-foreground'

function SessionRow({
  session,
  view,
  selected,
  running,
  confirming,
  controller,
  onSelect,
}: {
  session: AssistantSessionDto
  view: ViewState
  selected: boolean
  running: boolean
  confirming: boolean
  controller: AssistantController
  onSelect: (sessionId: string) => void
}) {
  const t = useT()
  const [renaming, setRenaming] = useState(false)
  const stale = view.env.stale
  const title = sessionTitle(session.title) ?? t('assistantUntitled')
  const date = session.updatedAt.slice(0, SESSION_DATE_CHARS)

  if (renaming) {
    return (
      <div className={cn('flex w-full min-w-0 items-center rounded-[var(--fx-components-nav-item-radius)] px-1 py-1', selected && 'bg-nav-selected')}>
        <RenameField sessionId={session.id} initial={title} controller={controller} stale={stale} onDone={() => setRenaming(false)} />
      </div>
    )
  }

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
      {confirming ? null : (
        <>
          <Button
            variant="ghost"
            iconOnly
            className={ROW_BUTTON}
            aria-label={t('assistantRename')}
            title={t('assistantRename')}
            disabled={stale}
            onClick={() => setRenaming(true)}
          >
            <Icon icon={Pencil} size="sm" />
          </Button>
          <ExportMenu sessionId={session.id} view={view} controller={controller} className={ROW_BUTTON} />
        </>
      )}
      <Button
        variant="ghost"
        iconOnly
        className={cn('mr-0.5 h-6 w-6 min-w-0 shrink-0', confirming ? 'text-destructive hover:bg-destructive/10' : 'text-text-tertiary hover:text-foreground')}
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

function SearchBox({ query, controller, disabled }: { query: string; controller: AssistantController; disabled: boolean }) {
  const t = useT()
  const [text, setText] = useState(query)

  // 停顿后才发请求；控制器按去空白后的词去重。
  useEffect(() => {
    if (text.trim() === query) return
    const timer = setTimeout(() => controller.setQuery(text), SEARCH_DEBOUNCE_MS)
    return () => clearTimeout(timer)
  }, [text, query, controller])

  return (
    <div className="relative min-w-0">
      <Icon icon={Search} size="sm" className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-text-tertiary" />
      <Input
        type="search"
        value={text}
        disabled={disabled}
        placeholder={t('assistantSearchPlaceholder')}
        aria-label={t('assistantSearchPlaceholder')}
        className="pr-8 pl-8 text-sm"
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Escape' && text !== '') {
            event.preventDefault()
            event.stopPropagation()
            setText('')
            controller.setQuery('')
          }
        }}
      />
      {text !== '' ? (
        <Button
          variant="ghost"
          iconOnly
          className="absolute top-1/2 right-1 h-6 w-6 min-w-0 -translate-y-1/2"
          aria-label={t('assistantCancel')}
          onClick={() => {
            setText('')
            controller.setQuery('')
          }}
        >
          <Icon icon={X} size="sm" />
        </Button>
      ) : null}
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
  const searching = sessions.query !== ''
  const failureKey = view.actionFailure === null ? null : ACTION_FAILURE_KEY[view.actionFailure]

  let body = null
  if (open) {
    if ((sessions.load === 'idle' || sessions.load === 'loading') && empty) {
      body = (
        <div role="status" className="flex items-center gap-2 p-3 text-xs text-text-tertiary">
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
          {empty ? <div className="p-3 text-xs text-text-tertiary">{t(searching ? 'assistantSearchEmpty' : 'assistantSessionsEmpty')}</div> : null}
          {sessions.items.map((session) => (
            <SessionRow
              key={session.id}
              session={session}
              view={view}
              selected={view.current === session.id}
              running={runningSessions.has(session.id)}
              confirming={view.deletePending === session.id}
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
          {open && (!empty || view.clearing) && !searching ? (
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
        {open ? <SearchBox query={sessions.query} controller={controller} disabled={stale} /> : null}
        {failureKey !== null ? <FieldError>{t(failureKey)}</FieldError> : null}
      </div>
      <div className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto px-2 pb-2">{body}</div>
    </div>
  )
}
