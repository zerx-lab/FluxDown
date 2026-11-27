// 对话区（GPUI `render_chat` / `render_conversation` / `render_turn`）：断线 / 待确认提示、
// 对话流（提问气泡、过程、Markdown 回答、问题单、按条评分）与输入区。对话流在贴近底部时跟随新内容，切换会话 / 发送时强制到底。

import { CircleAlert, Sparkles, ThumbsDown, ThumbsUp } from 'lucide-react'
import { useLayoutEffect, useMemo, useRef } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../i18n'
import { Button, EmptyState, Icon, Spinner, Tooltip } from '../../ui'
import { Composer } from './ComposerCard'
import type { ComposerState } from './composer'
import type { AssistantController, ViewState } from './controller'
import { IssueCard } from './IssueCard'
import { Markdown } from './MarkdownView'
import { buildTimeline, pendingElsewhere, runActive } from './model'
import type { Turn } from './model'
import { Steps } from './Steps'

/** 距底部多近算「跟随最新内容」（GPUI `FOLLOW_BOTTOM_SLACK`）。 */
const FOLLOW_BOTTOM_SLACK = 40

function Banner({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex flex-none items-center gap-3 border-b border-hairline bg-warning/10 px-4 py-1.5">
      <Icon icon={CircleAlert} size="sm" className="text-warning" />
      <span className="min-w-0 flex-1 text-sm break-words text-foreground">{children}</span>
      {action}
    </div>
  )
}

function RatingRow({ turn, view, controller }: { turn: Turn; view: ViewState; controller: AssistantController }) {
  const t = useT()
  const messageId = turn.answerId
  if (messageId === null) return null
  const disabled = view.env.stale || view.ratingBusy.has(messageId)
  const choices = [
    { rating: 'up' as const, icon: ThumbsUp, label: t('assistantRateUp') },
    { rating: 'down' as const, icon: ThumbsDown, label: t('assistantRateDown') },
  ]
  return (
    <div role="group" aria-label={t('assistantRatePrompt')} className="flex items-center gap-1">
      {choices.map(({ rating, icon, label }) => (
        <Tooltip key={rating} content={label}>
          <Button
            variant={turn.rating === rating ? 'outline' : 'ghost'}
            iconOnly
            aria-label={label}
            aria-pressed={turn.rating === rating}
            disabled={disabled}
            onClick={() => controller.rate(messageId, turn.rating, rating)}
          >
            <Icon icon={icon} size="md" />
          </Button>
        </Tooltip>
      ))}
    </div>
  )
}

function TurnView({ turn, saved, view, controller, now }: { turn: Turn; saved: boolean; view: ViewState; controller: AssistantController; now: number }) {
  const t = useT()
  const running = runActive(turn)
  const live = turn.live
  // 只在已落库、已结束的回答下评分（评价按回答消息记录）。
  const canRate = saved && !running && turn.answer !== null && turn.answerId !== null

  return (
    <div className="flex w-full min-w-0 flex-col gap-3">
      <div className="flex min-w-0 flex-col items-end gap-1">
        {turn.question !== '' ? (
          <div className="max-w-[85%] rounded-lg bg-muted px-3 py-2 text-sm break-words whitespace-pre-wrap text-foreground">{turn.question}</div>
        ) : null}
        {turn.images.length > 0 ? (
          <div className="flex max-w-[85%] flex-wrap justify-end gap-1.5">
            {turn.images.map((image, index) => (
              <img
                key={index}
                src={image}
                alt={t('assistantImageLabel', { n: index + 1 })}
                className="max-h-40 max-w-full rounded-md border border-hairline object-contain"
              />
            ))}
          </div>
        ) : null}
      </div>
      <div className="flex w-full min-w-0 flex-col gap-2">
        {turn.steps.length > 0 ? <Steps turn={turn} view={view} controller={controller} now={now} /> : null}
        {turn.answer !== null ? (
          <Markdown text={turn.answer} />
        ) : running && turn.steps.length === 0 ? (
          <div className="flex items-center gap-2 text-xs text-text-tertiary">
            <Spinner size="sm" />
            {t('assistantThinking')}
          </div>
        ) : null}
        {live?.status === 'failed' ? (
          <div className="flex min-w-0 flex-col gap-0.5">
            <div className="text-sm text-destructive">{t('assistantRunFailed')}</div>
            {live.error ? <div className="text-xs break-words whitespace-pre-wrap text-text-tertiary">{live.error}</div> : null}
          </div>
        ) : null}
        {live?.status === 'canceled' ? <div className="text-xs text-text-tertiary">{t('assistantStopped')}</div> : null}
        {turn.issues.map((issue) => (
          <IssueCard key={issue.id} issue={issue} view={view} controller={controller} />
        ))}
        {canRate ? <RatingRow turn={turn} view={view} controller={controller} /> : null}
      </div>
    </div>
  )
}

function Notice({ title, description, action }: { title: string; description?: string; action?: ReactNode }) {
  return <EmptyState className="h-full" icon={CircleAlert} title={title} description={description} action={action} />
}

function Conversation({
  view,
  controller,
  now,
  turns,
}: {
  view: ViewState
  controller: AssistantController
  now: number
  turns: readonly Turn[]
}) {
  const t = useT()
  const scrollRef = useRef<HTMLDivElement>(null)
  const follow = useRef(true)

  const onScroll = () => {
    const element = scrollRef.current
    if (element) follow.current = element.scrollHeight - element.scrollTop - element.clientHeight <= FOLLOW_BOTTOM_SLACK
  }
  // 切换会话 / 发送 / 首次载入历史：强制到底部并恢复跟随。
  useLayoutEffect(() => {
    follow.current = true
    const element = scrollRef.current
    if (element) element.scrollTop = element.scrollHeight
  }, [view.scrollToken])
  // 内容变化（流式增量、历史载入）时，贴近底部就继续跟随。
  useLayoutEffect(() => {
    const element = scrollRef.current
    if (element && follow.current) element.scrollTop = element.scrollHeight
  })

  const historyLoading = view.current !== null && view.detail.load === 'loading' && turns.length === 0
  const historyFailed = view.current !== null && view.detail.load === 'failed' && turns.length === 0
  let body: ReactNode
  if (historyLoading) {
    body = (
      <div className="flex h-full items-center justify-center gap-2 text-xs text-text-tertiary">
        <Spinner size="md" />
        {t('assistantLoading')}
      </div>
    )
  } else if (historyFailed) {
    const sessionId = view.current
    body = (
      <Notice
        title={t('assistantDetailFailed')}
        description={t('assistantServiceDesc')}
        action={
          <Button variant="outline" disabled={view.env.stale} onClick={() => sessionId !== null && controller.loadDetail(sessionId)}>
            {t('assistantRetry')}
          </Button>
        }
      />
    )
  } else if (turns.length === 0) {
    body = <EmptyState className="h-full" icon={Sparkles} title={t('assistantEmptyTitle')} description={t('assistantEmptyDesc')} />
  } else {
    body = (
      <div className="mx-auto flex w-full max-w-3xl min-w-0 flex-col gap-6 px-4 py-4">
        {turns.map((turn) => (
          <TurnView key={turn.key} turn={turn} saved={view.current !== null} view={view} controller={controller} now={now} />
        ))}
      </div>
    )
  }

  return (
    <div ref={scrollRef} onScroll={onScroll} className="min-h-0 w-full flex-1 overflow-y-auto">
      {body}
    </div>
  )
}

export function ChatPane({
  view,
  controller,
  now,
  quotaExhausted,
  composer,
  onComposerChange,
  autoFocusComposer,
  onOpenSession,
}: {
  view: ViewState
  controller: AssistantController
  now: number
  quotaExhausted: boolean
  composer: ComposerState
  onComposerChange: (value: ComposerState) => void
  /** 挂载时聚焦输入框。 */
  autoFocusComposer: boolean
  onOpenSession: (sessionId: string) => void
}) {
  const t = useT()
  const { state } = view.env
  const config = state.config ?? undefined
  const { current, detail, issueOverrides, ratingOverrides } = view
  const turns = useMemo(
    () => buildTimeline(current ?? '', detail.messages, detail.issues, state.runs, issueOverrides, ratingOverrides),
    [current, detail.messages, detail.issues, state.runs, issueOverrides, ratingOverrides],
  )
  const other = pendingElsewhere(state, view.current, now)

  return (
    <div className="flex h-full min-h-0 w-full min-w-0 flex-col">
      {view.env.stale ? <Banner>{t('localServiceDisconnected')}</Banner> : null}
      {other !== null ? (
        <Banner
          action={
            <Button variant="outline" onClick={() => onOpenSession(other)}>
              {t('assistantPendingElsewhereOpen')}
            </Button>
          }
        >
          {t('assistantPendingElsewhere')}
        </Banner>
      ) : null}
      <Conversation view={view} controller={controller} now={now} turns={turns} />
      <Composer
        view={view}
        controller={controller}
        config={config}
        quotaExhausted={quotaExhausted}
        newChat={view.current === null && turns.length === 0}
        value={composer}
        onChange={onComposerChange}
        autoFocus={autoFocusComposer}
      />
    </div>
  )
}
