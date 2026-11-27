// 「执行过程」折叠区、工具调用行与确认卡片（GPUI `render_steps` / `render_tool` / `render_confirm`）。
// 工具名、参数 JSON、输出预览都是模型 / 用户数据：单行摘要截断，展开后的 JSON 在块内横向滚动，不撑破版面。

import { ChevronDown, ChevronRight, CircleAlert, CircleCheck, Clock, Globe, Pen, X } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import type { AssistantToolCallDto } from '../../lib/rpc'
import { Badge, Button, Icon, Spinner } from '../../ui'
import type { AssistantController, ViewState } from './controller'
import {
  argumentsPretty,
  argumentsSummary,
  confirmView,
  formatCountdown,
  formatDuration,
  formatToolOutput,
  remainingSeconds,
  stepsOpen,
  toolCount,
  toolState,
} from './model'
import type { ConfirmView, ToolState, Turn } from './model'

interface Shared {
  view: ViewState
  controller: AssistantController
  now: number
}

/** 图标 + 文字的结果标签（已允许 / 已过期 / 已提交等）。 */
export function ResultLabel({ icon, text, tone }: { icon: LucideIcon; text: string; tone: string }) {
  return (
    <span className={cn('inline-flex min-w-0 items-center gap-1.5 text-xs', tone)}>
      <Icon icon={icon} size="sm" />
      <span className="min-w-0 break-words">{text}</span>
    </span>
  )
}

function ConfirmCard({ call, confirmation, view, controller }: { call: AssistantToolCallDto; confirmation: ConfirmView } & Shared) {
  const t = useT()
  const requestId = call.requestId ?? null
  const busy = requestId !== null && view.confirming.has(requestId)
  const enabled = confirmation.kind === 'pending' && !view.env.stale && !busy && requestId !== null

  let status = null
  switch (confirmation.kind) {
    case 'pending':
      status =
        confirmation.remainingMs === null ? null : (
          <span className="text-xs text-text-tertiary tabular">
            {t('assistantConfirmRemaining', { time: formatCountdown(remainingSeconds(confirmation.remainingMs)) })}
          </span>
        )
      break
    case 'approved':
      status = <ResultLabel icon={CircleCheck} text={t('assistantConfirmApproved')} tone="text-success" />
      break
    case 'declined':
      status = <ResultLabel icon={X} text={t('assistantConfirmDeclined')} tone="text-muted-foreground" />
      break
    case 'expired':
      status = <ResultLabel icon={Clock} text={t('assistantConfirmExpired')} tone="text-warning" />
      break
  }

  const effect =
    call.effect === 'network'
      ? { icon: Globe, key: 'assistantEffectNetwork' }
      : call.effect === 'write'
        ? { icon: Pen, key: 'assistantEffectWrite' }
        : null
  const summary = argumentsSummary(call.arguments)
  const argsOpen = view.argsOpen.has(call.callId)
  const hasArguments = call.arguments !== undefined && call.arguments !== null

  return (
    <div className="flex w-full min-w-0 flex-col gap-3 rounded-[var(--fx-components-card-radius)] border border-warning/50 bg-surface p-3">
      <div className="flex min-w-0 flex-col gap-2">
        <div className="flex flex-wrap items-center gap-1.5">
          <Icon icon={CircleAlert} size="sm" className="text-warning" />
          <span className="text-sm font-medium text-foreground">{t('assistantConfirmTitle')}</span>
          {effect ? (
            <Badge>
              <span className="inline-flex items-center gap-1">
                <Icon icon={effect.icon} size="sm" />
                {t(effect.key)}
              </span>
            </Badge>
          ) : null}
        </div>
        <div className="font-mono text-sm break-all text-foreground">{call.name}</div>
        {summary !== null ? (
          <div className={cn('font-mono text-xs text-text-tertiary', argsOpen ? 'break-all whitespace-pre-wrap' : 'truncate')} title={argsOpen ? undefined : summary}>
            {summary}
          </div>
        ) : null}
        {hasArguments ? (
          <>
            <button
              type="button"
              aria-expanded={argsOpen}
              onClick={() => controller.toggleArguments(call.callId)}
              className="flex w-fit items-center gap-1 text-xs text-accent-text coarse:min-h-touch"
            >
              <Icon icon={argsOpen ? ChevronDown : ChevronRight} size="sm" />
              {t(argsOpen ? 'assistantArgsHide' : 'assistantArgsShow')}
            </button>
            {argsOpen && call.arguments !== undefined && call.arguments !== null ? (
              <pre className="max-h-64 w-full max-w-full overflow-auto rounded-sm bg-muted p-2 font-mono text-xs whitespace-pre text-foreground">
                {argumentsPretty(call.arguments)}
              </pre>
            ) : null}
          </>
        ) : null}
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-0 flex-1">{status}</div>
        <Button variant="outline" disabled={!enabled} onClick={() => requestId !== null && controller.confirmTool(requestId, false)}>
          {t('assistantDeny')}
        </Button>
        <Button variant="primary" loading={busy} disabled={!enabled} onClick={() => requestId !== null && controller.confirmTool(requestId, true)}>
          {t('assistantAllow')}
        </Button>
      </div>
    </div>
  )
}

/** 状态文案与图标：待确认 / 未执行优先于调用状态本身。 */
function toolPresentation(state: ToolState, confirmation: ConfirmView | null): { labelKey: string; tone: string; icon: 'spinner' | LucideIcon } {
  if (confirmation?.kind === 'pending') return { labelKey: 'assistantToolWaiting', tone: 'text-warning', icon: Clock }
  if (confirmation?.kind === 'declined' || confirmation?.kind === 'expired') {
    return { labelKey: 'assistantToolSkipped', tone: 'text-text-tertiary', icon: CircleAlert }
  }
  switch (state) {
    case 'running':
      return { labelKey: 'assistantToolRunning', tone: 'text-text-tertiary', icon: 'spinner' }
    case 'succeeded':
      return { labelKey: 'assistantToolDone', tone: 'text-success', icon: CircleCheck }
    case 'failed':
      return { labelKey: 'assistantToolFailed', tone: 'text-destructive', icon: CircleAlert }
    case 'interrupted':
      return { labelKey: 'assistantToolInterrupted', tone: 'text-warning', icon: CircleAlert }
  }
}

function DetailSection({ title, text }: { title: string; text: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <div className="text-xs font-medium text-text-tertiary">{title}</div>
      <pre className="m-0 font-mono text-xs break-all whitespace-pre-wrap text-foreground">{text}</pre>
    </div>
  )
}

function ToolRow({ call, runActive, view, controller, now }: { call: AssistantToolCallDto; runActive: boolean } & Shared) {
  const t = useT()
  const state = toolState(call, runActive)
  const confirmation = confirmView(call, runActive, now)
  const client = call.kind === 'client'
  const { labelKey, tone, icon } = toolPresentation(state, confirmation)
  const detailKey = `detail:${call.callId}`
  const detailArguments = client && call.arguments !== undefined && call.arguments !== null && argumentsSummary(call.arguments) !== null ? argumentsPretty(call.arguments) : null
  const detailOutput = client && call.outputPreview && call.outputPreview.trim() !== '' ? formatToolOutput(call.outputPreview) : null
  const expandable = confirmation === null && (detailArguments !== null || detailOutput !== null)
  const open = expandable && view.argsOpen.has(detailKey)

  const header = (
    <>
      {icon === 'spinner' ? <Spinner size="sm" /> : <Icon icon={icon} size="sm" className={tone} />}
      <span className="min-w-0 font-mono text-sm break-all text-foreground">{call.name}</span>
      {client ? <Badge>{t('assistantToolLocal')}</Badge> : null}
      <span className={cn('text-xs', tone)}>{t(labelKey)}</span>
      {call.durationMs !== undefined && call.durationMs !== null ? (
        <span className="text-xs text-text-tertiary tabular">{formatDuration(call.durationMs)}</span>
      ) : null}
    </>
  )

  return (
    <div className="flex w-full min-w-0 flex-col gap-1">
      {expandable ? (
        <button
          type="button"
          aria-expanded={open}
          aria-label={t(open ? 'assistantToolDetailsHide' : 'assistantToolDetailsShow')}
          title={t(open ? 'assistantToolDetailsHide' : 'assistantToolDetailsShow')}
          onClick={() => controller.toggleArguments(detailKey)}
          className="flex w-full min-w-0 flex-wrap items-center gap-1.5 rounded-sm px-1 py-0.5 text-left hover:bg-muted coarse:min-h-touch"
        >
          {header}
          <Icon icon={open ? ChevronDown : ChevronRight} size="sm" className="ml-auto text-text-tertiary" />
        </button>
      ) : (
        <div className="flex min-w-0 flex-wrap items-center gap-1.5 px-1 py-0.5">{header}</div>
      )}
      {open ? (
        <div className="flex min-w-0 flex-col gap-2 rounded-sm bg-muted p-2">
          {detailArguments !== null ? <DetailSection title={t('assistantToolArguments')} text={detailArguments} /> : null}
          {detailOutput !== null ? <DetailSection title={t('assistantToolOutput')} text={detailOutput} /> : null}
        </div>
      ) : null}
      {confirmation !== null ? <ConfirmCard call={call} confirmation={confirmation} view={view} controller={controller} now={now} /> : null}
    </div>
  )
}

export function Steps({ turn, view, controller, now }: { turn: Turn } & Shared) {
  const t = useT()
  const running = turn.live !== null && (turn.live.status === 'starting' || turn.live.status === 'running')
  const open = stepsOpen(turn, view.stepsChoice.get(turn.key), now)
  const tools = toolCount(turn)

  return (
    <div className="flex w-full min-w-0 flex-col gap-0.5">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => controller.toggleSteps(turn.key, open)}
        className="flex w-fit max-w-full items-center gap-1 py-0.5 text-left coarse:min-h-touch"
      >
        <Icon icon={open ? ChevronDown : ChevronRight} size="sm" className="text-text-tertiary" />
        <span className="text-xs font-medium text-text-tertiary">{t('assistantStepsTitle')}</span>
        {tools > 0 ? <span className="text-xs text-text-tertiary">{t('assistantStepsCount', { count: tools })}</span> : null}
        {running && turn.answer === null ? <Spinner size="sm" /> : null}
      </button>
      {open ? (
        <div className="ml-1 flex min-w-0 flex-col gap-2 border-l border-hairline pl-3">
          {turn.steps.map((block, index) => {
            if (block.type === 'text') {
              return (
                <div key={index} className="text-sm break-words whitespace-pre-wrap text-muted-foreground">
                  {block.text}
                </div>
              )
            }
            // 问题单草稿在 `Turn.issues` 单独渲染，不进过程列表。
            if (block.type === 'tool') {
              return <ToolRow key={block.call.callId} call={block.call} runActive={running} view={view} controller={controller} now={now} />
            }
            return null
          })}
        </div>
      ) : null}
    </div>
  )
}
