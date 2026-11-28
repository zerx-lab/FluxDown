// 对话区（GPUI `render_chat` / `render_conversation` / `render_turn`）：断线 / 待确认提示、
// 对话流（提问气泡与附图、过程、Markdown 回答、回答下方的复制 / 重新生成 / 评分、会员卡、问题单）与输入区。
// 对话流在贴近底部时跟随新内容，切换会话 / 发送时强制到底；它是 `role="log"` 的 live region，读屏器会播报流式回答与确认卡。

import { ArrowDown, CircleAlert, Pencil, RefreshCw, Sparkles, ThumbsDown, ThumbsUp } from 'lucide-react'
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import type { KeyboardEvent, ReactNode } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import { Button, EmptyState, Icon, Spinner, Tooltip } from '../../ui'
import { AttachmentImage } from './AttachmentImage'
import { Composer } from './ComposerCard'
import { startEdit } from './composer'
import type { ComposerState } from './composer'
import type { AssistantController, ViewState } from './controller'
import { CopyButton } from './CopyButton'
import { PreviewableImage } from './ImagePreview'
import { IssueCard } from './IssueCard'
import { Markdown } from './MarkdownView'
import { MembershipCard } from './MembershipCard'
import {
  MAX_RATING_COMMENT_CHARS,
  awaitsHistory,
  buildTimeline,
  canResend,
  charCount,
  pendingElsewhere,
  retryPlan,
  runActive,
  runErrorKey,
  sessionLimitState,
  turnTranscript,
  validRatingComment,
} from './model'
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

/** 点踩后的可选说明：Ctrl / ⌘+Enter 提交，Esc 跳过；提交与跳过都收起，提交成功后显示感谢。 */
function RatingComment({ messageId, view, controller }: { messageId: string; view: ViewState; controller: AssistantController }) {
  const t = useT()
  const [text, setText] = useState('')
  const count = charCount(text.trim())
  const over = count > MAX_RATING_COMMENT_CHARS
  const busy = view.ratingBusy.has(messageId)
  const submittable = validRatingComment(text) !== null && !busy && !view.env.stale
  const submit = () => {
    if (submittable) void controller.submitRatingComment(messageId, text)
  }
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Escape') {
      event.preventDefault()
      controller.dismissRatingComment()
      return
    }
    if (event.key === 'Enter' && (event.metaKey || event.ctrlKey) && !event.nativeEvent.isComposing && event.keyCode !== 229) {
      event.preventDefault()
      submit()
    }
  }
  return (
    <div role="group" aria-label={t('assistantRateCommentPrompt')} className="flex min-w-0 flex-col gap-1.5 rounded-md bg-muted p-2">
      <div className="text-xs text-text-tertiary">{t('assistantRateCommentPrompt')}</div>
      <textarea
        autoFocus
        rows={2}
        value={text}
        aria-label={t('assistantRateCommentPrompt')}
        aria-invalid={over || undefined}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={onKeyDown}
        className="w-full resize-none rounded-sm border border-input bg-surface px-2 py-1.5 text-sm leading-relaxed text-foreground outline-none focus:border-ring"
      />
      <div className="flex flex-wrap items-center gap-2">
        <span className={cn('min-w-0 flex-1 text-xs tabular', over ? 'text-destructive' : 'text-text-tertiary')}>
          {t('assistantRenameCount', { count, max: MAX_RATING_COMMENT_CHARS })}
        </span>
        <Button variant="ghost" className="h-7 px-2 text-xs" disabled={busy} onClick={() => controller.dismissRatingComment()}>
          {t('assistantRateCommentSkip')}
        </Button>
        <Button variant="primary" className="h-7 px-2 text-xs" loading={busy} disabled={!submittable} onClick={submit}>
          {t('assistantRateCommentSubmit')}
        </Button>
      </div>
    </div>
  )
}

function QuestionImages({ turn, view, controller }: { turn: Turn; view: ViewState; controller: AssistantController }) {
  const t = useT()
  if (turn.localImages.length > 0) {
    return (
      <>
        {turn.localImages.map((image, index) => (
          <PreviewableImage
            key={index}
            src={image}
            alt={t('assistantImageLabel', { n: index + 1 })}
            number={index + 1}
            imageClassName="max-h-40 max-w-full rounded-md border border-hairline object-contain"
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
  // 失败的最后一轮给一键重试（已落库的替换重答；提问从未被接受的才原样重发）；有它就不再重复显示「重新生成」。
  const retry = rewritable && !running ? retryPlan(turn, isLast) : null
  // 提问已被接受、历史还没读到：不能重发（会重复提问），只能重读历史，读到后才有替换重答。
  const reloadable = !running && isLast && retry === null && awaitsHistory(turn)
  const canRegenerate = canEdit && isLast && retry === null
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
        {running && live?.observing === true ? <div className="text-xs text-text-tertiary">{t('assistantRunObserving')}</div> : null}
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
            {retry !== null ? (
              <Button variant="outline" className="mt-1 h-7 self-start px-2 text-xs" onClick={() => void controller.retry(retry)}>
                <Icon icon={RefreshCw} size="sm" />
                {t('assistantRetry')}
              </Button>
            ) : reloadable ? (
              <Button
                variant="outline"
                className="mt-1 h-7 self-start px-2 text-xs"
                loading={view.detail.load === 'loading'}
                disabled={view.env.stale}
                onClick={() => view.current !== null && controller.loadDetail(view.current)}
              >
                <Icon icon={RefreshCw} size="sm" />
                {t('assistantRetry')}
              </Button>
            ) : null}
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
        {canRate && turn.answerId !== null && view.commentPrompt === turn.answerId && turn.rating === 'down' ? (
          <RatingComment key={turn.answerId} messageId={turn.answerId} view={view} controller={controller} />
        ) : null}
        {turn.answerId !== null && view.commentThanks === turn.answerId ? (
          <div role="status" className="text-xs text-text-tertiary">
            {t('assistantRateCommentThanks')}
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
  quotaExhausted,
  turns,
  onEdit,
}: {
  view: ViewState
  controller: AssistantController
  now: number
  quotaExhausted: boolean
  turns: readonly Turn[]
  onEdit: (turn: Turn) => void
}) {
  const t = useT()
  const scrollRef = useRef<HTMLDivElement>(null)
  const follow = useRef(true)
  /** 已离开底部（不再跟随新内容）：显示「回到底部」。 */
  const [away, setAway] = useState(false)

  const onScroll = () => {
    const element = scrollRef.current
    if (!element) return
    const near = element.scrollHeight - element.scrollTop - element.clientHeight <= FOLLOW_BOTTOM_SLACK
    follow.current = near
    setAway(!near)
  }
  const jumpToBottom = () => {
    const element = scrollRef.current
    if (!element) return
    follow.current = true
    element.scrollTop = element.scrollHeight
    setAway(false)
  }
  // 切换会话 / 发送 / 首次载入历史：强制到底部并恢复跟随。
  useLayoutEffect(() => {
    follow.current = true
    const element = scrollRef.current
    if (element) element.scrollTop = element.scrollHeight
    setAway(false)
  }, [view.scrollToken])
  // 跟随时贴底；不跟随但内容缩到不再可滚动时恢复跟随并收起「回到底部」。
  const syncBottom = useCallback(() => {
    const element = scrollRef.current
    if (!element) return
    if (follow.current) element.scrollTop = element.scrollHeight
    else if (element.scrollHeight <= element.clientHeight) {
      follow.current = true
      setAway(false)
    }
  }, [])
  // 内容变化（流式增量、历史载入）时，贴近底部就继续跟随。
  useLayoutEffect(() => {
    syncBottom()
  }, [syncBottom, turns, view.stepsChoice, view.argsOpen, view.detail.load, view.commentPrompt, view.commentThanks])

  const historyLoading = view.current !== null && view.detail.load === 'loading' && turns.length === 0
  const historyFailed = view.current !== null && view.detail.load === 'failed' && turns.length === 0
  // 尺寸变化不经过上面的依赖：输入区增高（多行输入、附图、提示行）会挤小对话区，图片加载完 / 折叠区展开会撑高内容。
  // 监听滚动容器与它的内容，跟随时重新贴底，免得最新内容滑出视野而「回到底部」又不出现。
  const bodyKind = historyLoading ? 'loading' : historyFailed ? 'failed' : turns.length === 0 ? 'empty' : 'turns'
  useEffect(() => {
    const element = scrollRef.current
    if (!element || typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(syncBottom)
    observer.observe(element)
    if (element.firstElementChild !== null) observer.observe(element.firstElementChild)
    return () => observer.disconnect()
  }, [syncBottom, bodyKind])
  // 提问已被云端接受、历史里还没有它（常见于网络失败后）：自动重读一次历史，读到后失败轮次才能走「替换」重试；
  // 每个轮次只自动重读一次（再失败交给轮次里的重试按钮），免得离线时反复请求。
  const last = turns[turns.length - 1]
  const awaitingKey = last !== undefined && awaitsHistory(last) ? last.key : null
  const reloaded = useRef(new Set<string>())
  const { current: currentSession, env } = view
  const detailLoad = view.detail.load
  useEffect(() => {
    if (awaitingKey === null || currentSession === null || env.stale || detailLoad === 'loading') return
    const id = `${currentSession}:${awaitingKey}`
    if (reloaded.current.has(id)) return
    reloaded.current.add(id)
    controller.loadDetail(currentSession)
  }, [awaitingKey, currentSession, env.stale, detailLoad, controller])
  const activeRun = controller.activeRun()
  // 已达单会话提问上限：重新生成 / 编辑重发 / 重试都会被服务端拒绝，入口隐藏。
  const limitReached = sessionLimitState(view.env.state.config, view.current, view.detail.questionsUsed, activeRun !== undefined).kind === 'reached'
  // 再提一问的动作（重新生成 / 编辑重发 / 失败重试）与提问同一套门控：额度用尽、冷却中、会话上限都不显示。
  const rewritable = canResend({
    gate: { kind: 'open', quotaExhausted },
    stale: view.env.stale,
    sending: view.sending,
    cooldown: view.cooldown,
    hasActiveRun: activeRun !== undefined,
    hasSession: view.current !== null,
    limitReached,
  })
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
    <div className="relative flex min-h-0 w-full flex-1 flex-col">
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
      {away && turns.length > 0 ? (
        <div className="pointer-events-none absolute inset-x-0 bottom-3 flex justify-center">
          <Tooltip content={t('assistantScrollToBottom')}>
            <Button
              variant="outline"
              iconOnly
              aria-label={t('assistantScrollToBottom')}
              className="pointer-events-auto rounded-full bg-surface shadow-md"
              onClick={jumpToBottom}
            >
              <Icon icon={ArrowDown} size="md" />
            </Button>
          </Tooltip>
        </div>
      ) : null}
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
      <Conversation view={view} controller={controller} now={now} quotaExhausted={quotaExhausted} turns={turns} onEdit={edit} />
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
