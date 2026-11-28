// 对话区（GPUI `render_chat` / `render_conversation` / `render_turn`）：断线 / 待确认提示、
// 对话流（提问气泡与附图、过程、Markdown 回答、回答下方的复制 / 重新生成 / 评分、会员卡、问题单）与输入区。
// 对话流在贴近底部时跟随新内容，切换会话 / 发送时强制到底；它是 `role="log"` 的 live region，读屏器会播报流式回答与确认卡。

import { CircleAlert, Pencil, RefreshCw, Sparkles, ThumbsDown, ThumbsUp } from 'lucide-react'
import { useLayoutEffect, useMemo, useRef } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import { Button, EmptyState, Icon, Spinner, Tooltip } from '../../ui'
import { AttachmentImage } from './AttachmentImage'
import { Composer } from './ComposerCard'
import { startEdit } from './composer'
import type { ComposerState } from './composer'
import type { AssistantController, ViewState } from './controller'
import { CopyButton } from './CopyButton'
import { IssueCard } from './IssueCard'
import { Markdown } from './MarkdownView'
import { MembershipCard } from './MembershipCard'
import { buildTimeline, pendingElsewhere, runActive, runErrorKey, sessionLimitState, turnTranscript } from './model'
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

/** 评分：只变图标颜色（赞 = 强调色，踩 = 危险色，未选 = 弱化），没有边框（GPUI `render_rating`）。 */
function RatingButtons({ turn, view, controller }: { turn: Turn; view: ViewState; controller: AssistantController }) {
  const t = useT()
  const messageId = turn.answerId
  if (messageId === null) return null
  const disabled = view.env.stale || view.ratingBusy.has(messageId)
  const choices = [
    { rating: 'up' as const, icon: ThumbsUp, label: t('assistantRateUp'), selected: 'text-accent-text' },
    { rating: 'down' as const, icon: ThumbsDown, label: t('assistantRateDown'), selected: 'text-destructive' },
  ]
  return (
    <div role="group" aria-label={t('assistantRatePrompt')} className="flex items-center">
      {choices.map(({ rating, icon, label, selected }) => (
        <Tooltip key={rating} content={label}>
          <Button
            variant="ghost"
            iconOnly
            aria-label={label}
            aria-pressed={turn.rating === rating}
            disabled={disabled}
            className={turn.rating === rating ? selected : 'text-muted-foreground'}
            onClick={() => controller.rate(messageId, turn.rating, rating)}
          >
            <Icon icon={icon} size="md" />
          </Button>
        </Tooltip>
      ))}
    </div>
  )
}

function QuestionImages({ turn, view, controller }: { turn: Turn; view: ViewState; controller: AssistantController }) {
  const t = useT()
  if (turn.localImages.length > 0) {
    return (
      <>
        {turn.localImages.map((image, index) => (
          <img
            key={index}
            src={image}
            alt={t('assistantImageLabel', { n: index + 1 })}
            className="max-h-40 max-w-full rounded-md border border-hairline object-contain"
          />
        ))}
      </>
    )
  }
  if (turn.images.length > 0) {
    return (
      <>
        {turn.images.map((attachmentId, index) => (
          <AttachmentImage key={`${index}:${attachmentId}`} attachmentId={attachmentId} number={index + 1} controller={controller} disabled={view.env.stale} />
        ))}
      </>
    )
  }
  // 执行中且本地没有缩略图（别的设备发起 / 页面刷新后）：只知道张数。
  if (turn.imageCount > 0) return <span className="text-xs text-text-tertiary">{t('assistantImages', { count: turn.imageCount })}</span>
  return null
}

function TurnView({
  turn,
  isLast,
  rewritable,
  onEdit,
  view,
  controller,
  now,
}: {
  turn: Turn
  isLast: boolean
  /** 当前会话允许重写历史（已保存、没有进行中的执行 / 发送、已连接）。 */
  rewritable: boolean
  onEdit: (turn: Turn) => void
  view: ViewState
  controller: AssistantController
  now: number
}) {
  const t = useT()
  const running = runActive(turn)
  const live = turn.live
  const hasContent = turn.answer !== null || turn.steps.length > 0
  // 只在已落库、已结束的回答下评分（评价按回答消息记录）。
  const canRate = view.current !== null && !running && turn.answer !== null && turn.answerId !== null
  const questionId = turn.questionId
  const canEdit = rewritable && !running && questionId !== null && turn.question !== ''
  const canRegenerate = canEdit && isLast
  const imageBlock = turn.localImages.length > 0 || turn.images.length > 0 || turn.imageCount > 0

  return (
    <div className="flex w-full min-w-0 flex-col gap-3">
      <div className="group flex min-w-0 flex-col items-end gap-1">
        {turn.question !== '' ? (
          <div className="max-w-[85%] rounded-lg bg-muted px-3 py-2 text-sm break-words whitespace-pre-wrap text-foreground">{turn.question}</div>
        ) : null}
        {imageBlock ? (
          <div className="flex max-w-[85%] flex-wrap items-center justify-end gap-1.5">
            <QuestionImages turn={turn} view={view} controller={controller} />
          </div>
        ) : null}
        {canEdit ? (
          <div className="flex opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100 coarse:opacity-100">
            <Tooltip content={t('assistantEditResend')}>
              <Button variant="ghost" iconOnly aria-label={t('assistantEditResend')} className="text-muted-foreground" onClick={() => onEdit(turn)}>
                <Icon icon={Pencil} size="md" />
              </Button>
            </Tooltip>
          </div>
        ) : null}
      </div>
      <div className="flex w-full min-w-0 flex-col gap-2">
        {turn.steps.length > 0 ? <Steps turn={turn} view={view} controller={controller} now={now} /> : null}
        {turn.answer !== null ? (
          <>
            <Markdown text={turn.answer} />
            {turn.answerInterrupted ? <div className="text-xs text-warning">{t('assistantAnswerInterrupted')}</div> : null}
          </>
        ) : running && turn.steps.length === 0 ? (
          <div className="flex items-center gap-2 text-xs text-text-tertiary">
            <Spinner size="sm" />
            {t('assistantThinking')}
          </div>
        ) : null}
        {live?.status === 'failed' ? (
          <div className="flex min-w-0 flex-col gap-0.5">
            <div className="text-sm text-destructive">{t('assistantRunFailed')}</div>
            <div className="text-xs break-words text-text-tertiary">{t(runErrorKey(live.errorReason))}</div>
            {live.error ? <div className="text-xs break-words whitespace-pre-wrap text-text-tertiary">{live.error}</div> : null}
          </div>
        ) : null}
        {live?.status === 'canceled' ? <div className="text-xs text-text-tertiary">{t('assistantStopped')}</div> : null}
        {!running && (hasContent || live !== null) ? (
          <div className="-ml-2 flex flex-wrap items-center">
            <CopyButton label={t('assistantCopyTurn')} getText={() => turnTranscript(turn, now, (key) => t(key))} />
            {turn.answer !== null ? <CopyButton label={t('assistantCopyAnswer')} getText={() => turn.answer ?? ''} /> : null}
            {canRegenerate ? (
              <Tooltip content={t('assistantRegenerate')}>
                <Button
                  variant="ghost"
                  iconOnly
                  aria-label={t('assistantRegenerate')}
                  className={cn('text-muted-foreground')}
                  onClick={() => questionId !== null && void controller.regenerate(questionId, turn.question, turn.imageCount > 0)}
                >
                  <Icon icon={RefreshCw} size="md" />
                </Button>
              </Tooltip>
            ) : null}
            {canRate ? <RatingButtons turn={turn} view={view} controller={controller} /> : null}
          </div>
        ) : null}
        {turn.membershipOffer ? <MembershipCard disabled={view.env.stale} /> : null}
        {turn.issues.map((issue) => (
          <IssueCard key={issue.id} issue={issue} view={view} controller={controller} />
        ))}
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
  onEdit,
}: {
  view: ViewState
  controller: AssistantController
  now: number
  turns: readonly Turn[]
  onEdit: (turn: Turn) => void
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
  const activeRun = controller.activeRun()
  // 已达单会话提问上限：重新生成 / 编辑重发都会被服务端拒绝，入口隐藏。
  const limitReached = sessionLimitState(view.env.state.config, view.current, view.detail.questionsUsed, activeRun !== undefined).kind === 'reached'
  const rewritable = view.current !== null && !view.env.stale && !view.sending && activeRun === undefined && !limitReached
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
        {turns.map((turn, index) => (
          <TurnView
            key={turn.key}
            turn={turn}
            isLast={index === turns.length - 1}
            rewritable={rewritable}
            onEdit={onEdit}
            view={view}
            controller={controller}
            now={now}
          />
        ))}
      </div>
    )
  }

  return (
    <div
      ref={scrollRef}
      onScroll={onScroll}
      role="log"
      aria-live="polite"
      aria-relevant="additions text"
      aria-label={t('assistantNavTitle')}
      className="min-h-0 w-full flex-1 overflow-y-auto"
    >
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
  const { current, detail, issueOverrides, ratingOverrides, sent } = view
  const turns = useMemo(
    () => buildTimeline(current ?? '', detail.messages, detail.issues, state.runs, issueOverrides, ratingOverrides, sent),
    [current, detail.messages, detail.issues, state.runs, issueOverrides, ratingOverrides, sent],
  )
  const other = pendingElsewhere(state, view.current, now)

  const edit = (turn: Turn) => {
    if (current === null || turn.questionId === null) return
    onComposerChange(startEdit(composer, { sessionId: current, questionId: turn.questionId, text: turn.question, imageCount: turn.imageCount }))
  }

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
      <Conversation view={view} controller={controller} now={now} turns={turns} onEdit={edit} />
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
